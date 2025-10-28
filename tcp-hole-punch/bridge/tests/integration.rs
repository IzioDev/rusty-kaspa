use std::{net::TcpListener, time::Duration};

use futures::{channel::oneshot, FutureExt, SinkExt, StreamExt};
use http::Uri;
use libp2p::{identity, Multiaddr, PeerId};
use tokio::time::timeout;
use tonic::{
    transport::{Endpoint, Server},
    Request, Response, Status,
};
use tower::Service;
use urlencoding::encode;

use hole_punch_bridge::{
    swarm::{spawn_swarm, SwarmCommand},
    tonic_integration::{incoming_from_handle, Libp2pConnector},
    BridgeError,
};

mod pb {
    tonic::include_proto!("bridge.test");
}

use pb::echo_client::EchoClient;
use pb::echo_server::{Echo, EchoServer};
use pb::{PingRequest, PingResponse};

#[derive(Default)]
struct EchoService;

#[tonic::async_trait]
impl Echo for EchoService {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse { message: request.into_inner().message }))
    }
}

fn init_tracing() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt::try_init();
    });
}

fn reserve_tcp_multiaddr() -> Multiaddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    format!("/ip4/127.0.0.1/tcp/{port}").parse().expect("multiaddr")
}

#[tokio::test]
async fn incoming_helper_returns_once() {
    let mut handle = spawn_swarm(identity::Keypair::generate_ed25519()).expect("spawn");

    let incoming = incoming_from_handle(&mut handle).expect("incoming available");
    drop(incoming);

    assert!(matches!(incoming_from_handle(&mut handle), Err(BridgeError::IncomingClosed)));

    timeout(Duration::from_secs(5), handle.shutdown()).await.expect("shutdown timed out").expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn libp2p_dial_yields_stream() {
    init_tracing();
    let mut server_handle = spawn_swarm(identity::Keypair::generate_ed25519()).expect("spawn server");
    let listen_multiaddr = reserve_tcp_multiaddr();
    let (listen_tx, listen_rx) = oneshot::channel();
    server_handle
        .command_tx()
        .send(SwarmCommand::ListenOn { addr: listen_multiaddr.clone(), response: Some(listen_tx) })
        .await
        .expect("send listen");
    listen_rx.await.expect("listen rx").expect("listen result");

    let mut incoming = incoming_from_handle(&mut server_handle).expect("incoming");

    let client_handle = spawn_swarm(identity::Keypair::generate_ed25519()).expect("spawn client");
    let dial_multiaddr = listen_multiaddr.clone();
    let (response_tx, response_rx) = oneshot::channel();
    client_handle
        .command_tx()
        .send(SwarmCommand::Dial { peer: server_handle.local_peer_id(), addrs: vec![dial_multiaddr], response: response_tx })
        .await
        .expect("send dial");

    let mut inbound_stream = timeout(Duration::from_secs(15), incoming.next())
        .await
        .expect("incoming timeout")
        .expect("incoming stream")
        .expect("stream ok");
    let mut outbound_stream = timeout(Duration::from_secs(15), response_rx)
        .await
        .expect("dial response timeout")
        .expect("dial response")
        .expect("dial ok")
        .into_stream();

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    outbound_stream.write_all(b"ping").await.expect("write outbound");
    outbound_stream.flush().await.expect("flush outbound");

    let mut buf = [0u8; 4];
    inbound_stream.read_exact(&mut buf).await.expect("read inbound");
    assert_eq!(&buf, b"ping");

    timeout(Duration::from_secs(5), client_handle.shutdown()).await.expect("client shutdown timeout").expect("client shutdown");
    timeout(Duration::from_secs(5), server_handle.shutdown()).await.expect("server shutdown timeout").expect("server shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tonic_server_accepts_libp2p_stream() {
    init_tracing();
    let mut server_handle = spawn_swarm(identity::Keypair::generate_ed25519()).expect("spawn server");
    let server_peer = server_handle.local_peer_id();
    let listen_multiaddr = reserve_tcp_multiaddr();

    let (listen_tx, listen_rx) = oneshot::channel();
    server_handle
        .command_tx()
        .send(SwarmCommand::ListenOn { addr: listen_multiaddr.clone(), response: Some(listen_tx) })
        .await
        .expect("send listen command");
    listen_rx.await.expect("listen response channel").expect("listen result");

    let incoming = incoming_from_handle(&mut server_handle).expect("incoming");

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server_task = tokio::spawn(async move {
        Server::builder()
            .add_service(EchoServer::new(EchoService::default()))
            .serve_with_incoming_shutdown(incoming, shutdown_rx.map(|_| ()))
            .await
    });

    let client_handle = spawn_swarm(identity::Keypair::generate_ed25519()).expect("spawn client");
    let dial_multiaddr = format!("{listen_multiaddr}/p2p/{server_peer}");
    let uri = format!("libp2p://{server_peer}?addr={}", encode(&dial_multiaddr));

    let channel = Endpoint::from_shared(uri)
        .expect("endpoint")
        .connect_with_connector(Libp2pConnector::new(client_handle.command_tx()))
        .await
        .expect("connect via libp2p");

    let mut client = EchoClient::new(channel);
    let response = client.ping(Request::new(PingRequest { message: "hello".into() })).await.expect("ping response");
    assert_eq!(response.into_inner().message, "hello");

    drop(client);
    shutdown_tx.send(()).ok();
    server_task.await.expect("server task").expect("server result");

    timeout(Duration::from_secs(5), client_handle.shutdown()).await.expect("client shutdown timeout").expect("client shutdown");
    timeout(Duration::from_secs(5), server_handle.shutdown()).await.expect("server shutdown timeout").expect("server shutdown");
}

#[tokio::test]
async fn connector_rejects_missing_addr() {
    let handle = spawn_swarm(identity::Keypair::generate_ed25519()).expect("spawn");
    let peer = handle.local_peer_id();
    let mut connector = Libp2pConnector::new(handle.command_tx());
    let uri: Uri = format!("libp2p://{peer}").parse().unwrap();

    let err = connector.call(uri).await.expect_err("expected error");
    assert!(matches!(err, BridgeError::DialFailed(msg) if msg.contains("missing addr")));

    timeout(Duration::from_secs(5), handle.shutdown()).await.expect("shutdown timeout").expect("shutdown");
}

#[tokio::test]
async fn connector_rejects_invalid_multiaddr() {
    let handle = spawn_swarm(identity::Keypair::generate_ed25519()).expect("spawn");
    let peer = handle.local_peer_id();
    let mut connector = Libp2pConnector::new(handle.command_tx());
    let uri: Uri = format!("libp2p://{peer}?addr=not-a-multiaddr").parse().unwrap();

    let err = connector.call(uri).await.expect_err("expected error");
    assert!(matches!(err, BridgeError::DialFailed(msg) if msg.contains("invalid multiaddr")));

    timeout(Duration::from_secs(5), handle.shutdown()).await.expect("shutdown timeout").expect("shutdown");
}

#[tokio::test]
async fn connector_rejects_multiaddr_missing_peer_suffix() {
    let handle = spawn_swarm(identity::Keypair::generate_ed25519()).expect("spawn");
    let peer = handle.local_peer_id();
    let mut connector = Libp2pConnector::new(handle.command_tx());
    let multiaddr = encode("/ip4/127.0.0.1/tcp/5000");
    let uri: Uri = format!("libp2p://{peer}?addr={multiaddr}").parse().unwrap();

    let err = connector.call(uri).await.expect_err("expected error");
    assert!(matches!(err, BridgeError::DialFailed(msg) if msg.contains("terminal /p2p")));

    timeout(Duration::from_secs(5), handle.shutdown()).await.expect("shutdown timeout").expect("shutdown");
}

#[tokio::test]
async fn connector_rejects_mismatched_peer() {
    let handle = spawn_swarm(identity::Keypair::generate_ed25519()).expect("spawn");
    let peer = handle.local_peer_id();
    let other = PeerId::random();
    let addr_source = format!("/ip4/10.0.0.5/tcp/12345/p2p/{other}");
    let addr = encode(&addr_source);
    let uri: Uri = format!("libp2p://{peer}?addr={addr}").parse().unwrap();
    let mut connector = Libp2pConnector::new(handle.command_tx());

    let err = connector.call(uri).await.expect_err("expected error");
    assert!(matches!(err, BridgeError::DialFailed(msg) if msg.contains("targets peer")));

    timeout(Duration::from_secs(5), handle.shutdown()).await.expect("shutdown timeout").expect("shutdown");
}

#[tokio::test]
async fn relay_limits_are_enforced() {
    init_tracing();

    let mut config = hole_punch_bridge::SwarmConfig::default();
    config.relay.max_reservations = 1;
    config.relay.max_circuits_per_peer = 1;

    let handle = hole_punch_bridge::spawn_swarm_with_config(identity::Keypair::generate_ed25519(), config).expect("spawn with config");
    let mut command_tx = handle.command_tx();

    let peer = PeerId::random();
    let relay_addr: Multiaddr = "/ip4/192.0.2.1/tcp/4001/p2p-circuit".parse().expect("multiaddr");

    let (first_tx, first_rx) = oneshot::channel();
    command_tx.send(SwarmCommand::Dial { peer, addrs: vec![relay_addr.clone()], response: first_tx }).await.expect("send first dial");

    let (second_tx, second_rx) = oneshot::channel();
    command_tx.send(SwarmCommand::Dial { peer, addrs: vec![relay_addr], response: second_tx }).await.expect("send second dial");

    match second_rx.await.expect("second response") {
        Ok(_) => panic!("expected limit error"),
        Err(BridgeError::DialFailed(msg)) => {
            assert!(msg.contains("relay reservation limit"), "unexpected error message: {msg}")
        }
        Err(other) => panic!("unexpected error {other:?}"),
    }

    // Ensure the first dial completes (it will likely fail once the swarm realises there is no relay).
    let _ = first_rx.await;

    timeout(Duration::from_secs(5), handle.shutdown()).await.expect("shutdown timeout").expect("shutdown");
}
