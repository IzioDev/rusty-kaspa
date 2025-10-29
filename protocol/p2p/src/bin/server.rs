use kaspa_core::debug;
use kaspa_p2p_lib::echo::EchoFlowInitializer;
use kaspa_utils::networking::NetAddress;
use std::{str::FromStr, sync::Arc, time::Duration};

#[cfg(feature = "libp2p-bridge")]
use futures::{channel::oneshot, SinkExt};
#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::swarm::{SwarmCommand, SwarmHandle};
#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::tonic_integration::incoming_from_handle;
#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::{spawn_swarm_with_config, SwarmConfig};
#[cfg(feature = "libp2p-bridge")]
use libp2p::{identity, Multiaddr, PeerId};
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
    if let Some(listen_multiaddrs) = listen_multiaddrs_from_env() {
        let mut swarm = spawn_swarm_with_config(identity::Keypair::generate_ed25519(), SwarmConfig::default()).expect("spawn swarm");
        for listen_addr in listen_multiaddrs {
            let (listen_tx, listen_rx) = oneshot::channel();
            swarm.command_tx().send(SwarmCommand::ListenOn { addr: listen_addr, response: Some(listen_tx) }).await.expect("listen");
            listen_rx.await.expect("listen ack").expect("listen ok");
        }

        init_relay_reservations(&mut swarm).await;

        let local_peer = swarm.local_peer_id();
        debug!("libp2p server peer id: {local_peer}");

        let incoming = incoming_from_handle(&mut swarm).expect("incoming");
        debug!("libp2p server feeding incoming streams into tonic");
        adaptor.serve_incoming_streams(incoming);

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
async fn init_relay_reservations(handle: &mut SwarmHandle) {
    for addr in multiaddrs_from_env("LIBP2P_RELAY_MULTIADDRS", "LIBP2P_RELAY_MULTIADDR") {
        match addr {
            Ok(addr) => {
                if let Some(peer_id) = last_peer_id(&addr) {
                    let mut listen_addr = addr.clone();
                    let had_circuit = listen_addr.iter().any(|p| matches!(p, libp2p::multiaddr::Protocol::P2pCircuit));
                    if !had_circuit {
                        listen_addr.push(libp2p::multiaddr::Protocol::P2pCircuit);
                    }

                    debug!("Relay listen request via {addr} (relay {peer_id}); submitting {listen_addr}");

                    let (response_tx, response_rx) = oneshot::channel();
                    if let Err(err) = handle
                        .command_tx()
                        .send(SwarmCommand::ListenOn { addr: listen_addr.clone(), response: Some(response_tx) })
                        .await
                    {
                        debug!("Failed to request relay listen on {listen_addr}: {err}");
                        continue;
                    }

                    match response_rx.await {
                        Ok(Ok(())) => debug!("Relay reservation requested via {listen_addr}"),
                        Ok(Err(err)) => debug!("Relay listen request for {listen_addr} returned error: {err}"),
                        Err(err) => debug!("Relay listen response dropped for {listen_addr}: {err}"),
                    }
                } else {
                    debug!("Relay multiaddr {addr} missing peer component");
                }
            }
            Err(err) => debug!("Failed to parse relay multiaddr: {err}"),
        }
    }
}

#[cfg(feature = "libp2p-bridge")]
fn listen_multiaddrs_from_env() -> Option<Vec<Multiaddr>> {
    let entries = multiaddrs_from_env("LIBP2P_LISTEN_MULTIADDRS", "LIBP2P_LISTEN_MULTIADDR");
    if entries.is_empty() {
        return None;
    }

    let mut addrs = Vec::new();
    for entry in entries {
        match entry {
            Ok(addr) => addrs.push(addr),
            Err(err) => debug!("Failed to parse listen multiaddr: {err}"),
        }
    }

    if addrs.is_empty() {
        None
    } else {
        Some(addrs)
    }
}

#[cfg(feature = "libp2p-bridge")]
fn multiaddrs_from_env(primary: &str, fallback: &str) -> Vec<Result<Multiaddr, libp2p::multiaddr::Error>> {
    let Ok(raw) = env::var(primary).or_else(|_| env::var(fallback)) else {
        return Vec::new();
    };
    raw.split(|c| c == ',' || c == ';').map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::parse).collect()
}

#[cfg(feature = "libp2p-bridge")]
fn last_peer_id(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter()
        .filter_map(|p| match p {
            libp2p::multiaddr::Protocol::P2p(peer) => Some(peer),
            _ => None,
        })
        .last()
}
