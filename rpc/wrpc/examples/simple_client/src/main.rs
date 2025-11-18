// Example of simple client to connect with Kaspa node using wRPC connection and collect some node and network basic data

use kaspa_rpc_core::{
    api::rpc::RpcApi, GetBlockDagInfoResponse, GetConnectedPeerInfoResponse, GetLibpStatusResponse, GetPeerAddressesResponse,
    GetServerInfoResponse, RpcContextualPeerAddress,
};
use kaspa_wrpc_client::{
    client::{ConnectOptions, ConnectStrategy},
    prelude::NetworkId,
    prelude::NetworkType,
    result::Result,
    KaspaRpcClient, Resolver, WrpcEncoding,
};
use std::process::ExitCode;
use std::time::Duration;

#[tokio::main]
async fn main() -> ExitCode {
    match check_node_status().await {
        Ok(_) => {
            println!("Well done! You successfully completed your first client connection to Kaspa node!");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("An error occurred: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn check_node_status() -> Result<()> {
    // Allow overriding the target URL/encoding via environment variables so we can hit custom nodes during testing.
    let env_snapshot: Vec<_> = std::env::vars().collect();
    println!("ENV SNAPSHOT: {:?}", env_snapshot);
    for (key, value) in env_snapshot.iter().filter(|(key, _)| key.starts_with("KASPA_")) {
        println!("ENV {key}={value}");
    }

    let url = std::env::var("KASPA_WSRPC_URL").ok();
    if let Some(url) = &url {
        println!("Connecting to custom wRPC endpoint: {url}");
    }
    let encoding = match std::env::var("KASPA_WSRPC_ENCODING").as_deref() {
        Ok("json") => WrpcEncoding::SerdeJson,
        _ => WrpcEncoding::Borsh,
    };
    let resolver = if url.is_some() { None } else { Some(Resolver::default()) };

    // Define the network your Kaspa node is connected to
    // You can select NetworkType::Mainnet, NetworkType::Testnet, NetworkType::Devnet, NetworkType::Simnet
    let network_type = NetworkType::Mainnet;
    let selected_network = Some(NetworkId::new(network_type));

    // Advanced options
    let subscription_context = None;

    // Create new wRPC client with parameters defined above
    let client = KaspaRpcClient::new(encoding, url.as_deref(), resolver, selected_network, subscription_context)?;

    // Advanced connection options
    let timeout = 5_000;
    let options = ConnectOptions {
        block_async_connect: true,
        connect_timeout: Some(Duration::from_millis(timeout)),
        strategy: ConnectStrategy::Fallback,
        ..Default::default()
    };

    // Connect to selected Kaspa node
    client.connect(Some(options)).await?;

    // Optionally force-add a peer (e.g., our relay) to ensure the private node learns it immediately.
    if let Ok(peer) = std::env::var("KASPA_ADD_PEER") {
        match peer.parse::<RpcContextualPeerAddress>() {
            Ok(addr) => {
                println!("Adding peer {peer} via RPC");
                if let Err(err) = client.add_peer(addr, true).await {
                    println!("add_peer failed: {err}");
                }
            }
            Err(err) => println!("Invalid KASPA_ADD_PEER value {peer}: {err}"),
        }
    }

    // Retrieve and show Kaspa node information
    let GetServerInfoResponse { is_synced, server_version, network_id, has_utxo_index, .. } = client.get_server_info().await?;

    println!("Node version: {server_version}");
    println!("Network: {network_id}");
    println!("Node is synced: {is_synced}");
    println!("Node is indexing UTXOs: {has_utxo_index}");

    // Retrieve libp2p status so we can validate relay/private roles
    match client.get_libp_status().await {
        Ok(GetLibpStatusResponse { enabled, role, listen_addresses, private_inbound_target, relay_inbound_limit, .. }) => {
            println!("libp2p enabled: {enabled}, role: {:?}", role);
            println!("listen addresses: {listen_addresses:?}");
            println!("private inbound target: {:?}, relay inbound limit: {:?}", private_inbound_target, relay_inbound_limit);
        }
        Err(err) => println!("get_libp_status RPC failed: {err}"),
    }

    // Retrieve and show Kaspa network information
    let GetBlockDagInfoResponse {
        block_count,
        header_count,
        tip_hashes,
        difficulty,
        past_median_time,
        virtual_parent_hashes,
        pruning_point_hash,
        virtual_daa_score,
        sink,
        ..
    } = client.get_block_dag_info().await?;

    println!("Block count: {block_count}");
    println!("Header count: {header_count}");
    println!("Tip hashes:");
    for tip_hash in tip_hashes {
        println!("{tip_hash}");
    }
    println!("Difficulty: {difficulty}");
    println!("Past median time: {past_median_time}");
    println!("Virtual parent hashes:");
    for virtual_parent_hash in virtual_parent_hashes {
        println!("{virtual_parent_hash}");
    }
    println!("Pruning point hash: {pruning_point_hash}");
    println!("Virtual DAA score: {virtual_daa_score}");
    println!("Sink: {sink}");

    // Inspect peer addresses / connections when running against custom nodes
    if url.is_some() {
        match client.get_peer_addresses().await {
            Ok(GetPeerAddressesResponse { known_addresses, .. }) => {
                let relay_advertisers: Vec<_> = known_addresses
                    .into_iter()
                    .filter(|addr| addr.relay_port.is_some())
                    .map(|addr| format!("{} (relayPort={:?})", addr.to_string(), addr.relay_port))
                    .collect();
                println!("Gossiped relays: {:?}", relay_advertisers);
            }
            Err(err) => println!("get_peer_addresses RPC failed: {err}"),
        }

        match client.get_connected_peer_info().await {
            Ok(GetConnectedPeerInfoResponse { peer_info }) => {
                println!("Total connected peers: {}", peer_info.len());
                let relay_candidates: Vec<_> = peer_info.iter().filter(|p| p.relay_port.is_some()).collect();
                println!("Connected relay-capable peers (relay_port present): {}", relay_candidates.len());
                for peer in relay_candidates.iter().take(5) {
                    println!(
                        "  {} services={} relayPort={:?} is_libp2p={} libp2p_relay_used={:?}",
                        peer.address, peer.services, peer.relay_port, peer.is_libp2p, peer.libp2p_relay_used
                    );
                }

                let active_circuits: Vec<_> =
                    peer_info.iter().filter(|p| p.is_libp2p && p.libp2p_relay_used.unwrap_or(false)).collect();
                println!("Active libp2p relay circuits:");
                if active_circuits.is_empty() {
                    println!("  (none)");
                } else {
                    for peer in active_circuits {
                        println!(
                            "  peer={} multiaddr={:?} libp2p_relay_used={:?}",
                            peer.address, peer.libp2p_multiaddr, peer.libp2p_relay_used
                        );
                        if let Some(ma) = &peer.libp2p_multiaddr {
                            println!("    libp2p_multiaddr={ma}");
                        }
                    }
                }

                println!("Sample peer set:");
                for peer in peer_info.iter().take(5) {
                    println!(
                        "  {} outbound={} services={} relayPort={:?} is_libp2p={} libp2p_relay_used={:?}",
                        peer.address, peer.is_outbound, peer.services, peer.relay_port, peer.is_libp2p, peer.libp2p_relay_used
                    );
                }
            }
            Err(err) => println!("get_connected_peer_info RPC failed: {err}"),
        }
    }

    // Disconnect client from Kaspa node
    client.disconnect().await?;

    // Return function result
    Ok(())
}
