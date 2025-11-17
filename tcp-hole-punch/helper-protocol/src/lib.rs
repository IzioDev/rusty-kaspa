use serde::{Deserialize, Serialize};

/// Request issued by the helper CLI to instruct the node to open a libp2p circuit.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Libp2pDialRequest {
    /// Full multiaddrs (e.g. `/ip4/relay/tcp/18111/p2p/<relay>/p2p-circuit`) allowed for dialing.
    pub relay_multiaddrs: Vec<String>,
    /// Optional relay peer id used for sanity checking when parsing the multiaddr list.
    pub relay_peer_id: Option<String>,
    /// The target peer ID that should appear at the end of the circuit.
    pub target_peer_id: String,
    /// Optional timeout in milliseconds before aborting the dial request.
    pub timeout_ms: Option<u64>,
}

impl Libp2pDialRequest {
    pub fn timeout_ms(&self) -> Option<u64> {
        self.timeout_ms
    }
}

/// Response emitted by the node after attempting to open a circuit.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Libp2pDialResponse {
    pub success: bool,
    pub message: String,
    pub target_peer_id: Option<String>,
    pub peer_key: Option<String>,
    pub libp2p_multiaddr: Option<String>,
    pub relay_used: Option<bool>,
}

impl Libp2pDialResponse {
    pub fn success(message: impl Into<String>, multiaddr: Option<String>, relay_used: Option<bool>, peer_key: Option<String>) -> Self {
        Self { success: true, message: message.into(), target_peer_id: None, peer_key, libp2p_multiaddr: multiaddr, relay_used }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            target_peer_id: None,
            peer_key: None,
            libp2p_multiaddr: None,
            relay_used: None,
        }
    }

    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target_peer_id = Some(target.into());
        self
    }
}
