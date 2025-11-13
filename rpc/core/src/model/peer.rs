use borsh::{BorshDeserialize, BorshSerialize};
use kaspa_utils::networking::{ContextualNetAddress, IpAddress, NetAddress, PeerId};
use serde::{Deserialize, Serialize};

pub type RpcNodeId = PeerId;
pub type RpcIpAddress = IpAddress;
pub type RpcPeerAddress = NetAddress;
pub type RpcContextualPeerAddress = ContextualNetAddress;

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct RpcPeerInfo {
    pub id: RpcNodeId,
    pub address: RpcPeerAddress,
    pub last_ping_duration: u64, // NOTE: i64 in gRPC protowire

    pub is_outbound: bool,
    pub time_offset: i64,
    pub user_agent: String,

    pub advertised_protocol_version: u32,
    pub time_connected: u64, // NOTE: i64 in gRPC protowire
    pub is_ibd_peer: bool,
    pub services: u64,
    pub relay_port: Option<u16>,

    // Libp2p metadata (None when the connection was established using plain TCP)
    pub is_libp2p: bool,
    pub libp2p_peer_id: Option<String>,
    pub libp2p_multiaddr: Option<String>,
    pub libp2p_relay_used: Option<bool>,
}
