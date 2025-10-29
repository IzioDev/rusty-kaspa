use kaspa_core::debug;
use kaspa_p2p_lib::echo::EchoFlowInitializer;
use std::{sync::Arc, time::Duration};

#[cfg(feature = "libp2p-bridge")]
use futures::{channel::oneshot, SinkExt};
#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::stream::{Libp2pConnectInfo as BridgeConnectInfo, Libp2pStream};
#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::swarm::SwarmCommand;
#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::{spawn_swarm_with_config, SwarmConfig};
#[cfg(feature = "libp2p-bridge")]
use libp2p::{identity, multiaddr::Protocol, Multiaddr, PeerId};
#[cfg(feature = "libp2p-bridge")]
use std::{convert::TryFrom, env, str::FromStr};

#[tokio::main]
async fn main() {
    // [-] - init logger
    kaspa_core::log::init_logger(None, "debug");
    // [0] - init p2p-adaptor
    let initializer = Arc::new(EchoFlowInitializer::new());
    let adaptor = kaspa_p2p_lib::Adaptor::client_only(kaspa_p2p_lib::Hub::new(), initializer, Default::default());
    #[cfg(feature = "libp2p-bridge")]
    let mut libp2p_handle: Option<hole_punch_bridge::swarm::SwarmHandle> = None;

    #[cfg(feature = "libp2p-bridge")]
    if let (Ok(raw_multiaddrs), Ok(remote_peer)) =
        (env::var("LIBP2P_REMOTE_MULTIADDRS").or_else(|_| env::var("LIBP2P_REMOTE_MULTIADDR")), env::var("LIBP2P_REMOTE_PEER_ID"))
    {
        let swarm = spawn_swarm_with_config(identity::Keypair::generate_ed25519(), SwarmConfig::default()).expect("spawn swarm");
        let peer_id = PeerId::from_str(&remote_peer).expect("peer id");
        debug!("libp2p client local peer id: {}", swarm.local_peer_id());

        let mut handle_result = None;
        for value in raw_multiaddrs.split(|c| c == ',' || c == ';').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            match value.parse::<Multiaddr>() {
                Ok(addr) => {
                    debug!("Attempting libp2p dial via {value}");
                    let (response_tx, response_rx) = oneshot::channel();
                    let normalized_addr = normalize_multiaddr(addr, &peer_id);
                    if let Err(err) = swarm
                        .command_tx()
                        .send(SwarmCommand::Dial { peer: peer_id, addrs: vec![normalized_addr], response: response_tx })
                        .await
                    {
                        debug!("Failed to submit dial command for {value}: {err}");
                        continue;
                    }
                    match response_rx.await {
                        Ok(Ok(handle)) => {
                            handle_result = Some(handle);
                            break;
                        }
                        Ok(Err(err)) => {
                            debug!("libp2p dial via {value} failed: {err}");
                        }
                        Err(err) => {
                            debug!("libp2p dial via {value} dropped: {err}");
                        }
                    }
                }
                Err(err) => debug!("Failed to parse remote multiaddr '{value}': {err}"),
            }
        }

        if let Some(handle) = handle_result {
            match connect_libp2p_stream(&adaptor, handle.into_stream()).await {
                Ok(peer) => {
                    debug!("Connected via libp2p to {peer}");
                    libp2p_handle = Some(swarm);
                }
                Err(err) => debug!("Failed to connect via libp2p: {err}"),
            }
        } else {
            debug!("No libp2p dial attempt succeeded");
        }
    }
    // [1] - connect 128 peers + flows
    let ip_port = String::from("[::1]:50051");
    for i in 0..1 {
        debug!("P2P, p2p_client::main - starting peer:{}", i);
        let _peer_key = adaptor.connect_peer_with_retries(ip_port.clone(), 16, Duration::from_secs(1)).await;
    }
    // [2] - wait a few seconds and terminate
    tokio::time::sleep(Duration::from_secs(5)).await;
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

#[cfg(feature = "libp2p-bridge")]
fn normalize_multiaddr(mut addr: Multiaddr, peer: &PeerId) -> Multiaddr {
    if let Some(Protocol::P2p(multi_hash)) = addr.iter().last() {
        if PeerId::try_from(multi_hash.clone()).map_or(false, |id| &id == peer) {
            addr.pop();
        }
    }
    addr
}
