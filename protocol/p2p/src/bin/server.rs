use kaspa_core::debug;
use kaspa_p2p_lib::echo::EchoFlowInitializer;
use kaspa_utils::networking::NetAddress;
use std::{str::FromStr, sync::Arc, time::Duration};

#[cfg(feature = "libp2p-bridge")]
use futures::{channel::oneshot, SinkExt, StreamExt};
#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::stream::{Libp2pConnectInfo as BridgeConnectInfo, Libp2pStream};
#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::swarm::SwarmCommand;
#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::tonic_integration::incoming_from_handle;
#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::{spawn_swarm_with_config, SwarmConfig};
#[cfg(feature = "libp2p-bridge")]
use libp2p::{identity, Multiaddr};
#[cfg(feature = "libp2p-bridge")]
use std::env;

#[tokio::main]
async fn main() {
    // [-] - init logger
    kaspa_core::log::init_logger(None, "debug");
    // [0] - init p2p-adaptor - server side
    let ip_port = NetAddress::from_str("[::1]:50051").unwrap();
    let initializer = Arc::new(EchoFlowInitializer::new());
    let adaptor = kaspa_p2p_lib::Adaptor::bidirectional(ip_port, kaspa_p2p_lib::Hub::new(), initializer, Default::default()).unwrap();
    #[cfg(feature = "libp2p-bridge")]
    let mut libp2p_handle: Option<hole_punch_bridge::swarm::SwarmHandle> = None;

    // [1] - optionally accept inbound libp2p streams via the bridge
    #[cfg(feature = "libp2p-bridge")]
    if let Ok(listen_multiaddr) = env::var("LIBP2P_LISTEN_MULTIADDR") {
        let mut swarm = spawn_swarm_with_config(identity::Keypair::generate_ed25519(), SwarmConfig::default()).expect("spawn swarm");
        let listen_addr: Multiaddr = listen_multiaddr.parse().expect("multiaddr");
        let (listen_tx, listen_rx) = oneshot::channel();
        swarm.command_tx().send(SwarmCommand::ListenOn { addr: listen_addr, response: Some(listen_tx) }).await.expect("listen");
        listen_rx.await.expect("listen ack").expect("listen ok");

        let mut incoming = incoming_from_handle(&mut swarm).expect("incoming");
        let adaptor_clone = adaptor.clone();
        tokio::spawn(async move {
            while let Some(stream) = incoming.next().await {
                match stream {
                    Ok(stream) => match connect_libp2p_stream(&adaptor_clone, stream).await {
                        Ok(peer) => debug!("Accepted libp2p stream from {peer}"),
                        Err(err) => debug!("Failed to accept libp2p stream: {err}"),
                    },
                    Err(err) => debug!("Incoming stream error: {err}"),
                }
            }
        });

        libp2p_handle = Some(swarm);
    }
    // [1] - connect to a few peers (legacy TCP path)
    let ip_port = String::from("[::1]:16111");
    for i in 0..1 {
        debug!("P2P, p2p_client::main - starting peer:{}", i);
        let _peer_key = adaptor.connect_peer_with_retries(ip_port.clone(), 16, Duration::from_secs(1)).await;
    }
    // [2] - wait for ~60 sec and terminate
    tokio::time::sleep(Duration::from_secs(64)).await;
    debug!("P2P,p2p_client::main - TERMINATE");
    adaptor.terminate_all_peers().await;
    #[cfg(feature = "libp2p-bridge")]
    if let Some(handle) = libp2p_handle {
        let _ = handle.shutdown().await;
    }
    debug!("P2P,p2p_client::main - FINISH");
    tokio::time::sleep(Duration::from_secs(10)).await;
    debug!("P2P,p2p_client::main - EXIT");
}

#[cfg(feature = "libp2p-bridge")]
fn metadata_from_info(info: &BridgeConnectInfo) -> kaspa_p2p_lib::ConnectionMetadata {
    let libp2p_info = kaspa_p2p_lib::Libp2pConnectInfo::with_address(
        info.peer_id.to_string(),
        info.remote_multiaddr.as_ref().map(|addr| addr.to_string()),
        info.relay_used,
    );
    kaspa_p2p_lib::ConnectionMetadata::new(info.synthesized_socket, Some(libp2p_info))
}

#[cfg(feature = "libp2p-bridge")]
async fn connect_libp2p_stream(
    adaptor: &kaspa_p2p_lib::Adaptor,
    stream: Libp2pStream,
) -> Result<kaspa_p2p_lib::PeerKey, kaspa_p2p_lib::ConnectionError> {
    let metadata = metadata_from_info(&stream.info);
    adaptor.connect_peer_with_stream(stream, metadata).await
}
