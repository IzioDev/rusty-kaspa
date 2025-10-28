use std::fmt;
use std::net::SocketAddr;

/// Additional metadata describing how a connection was established.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ConnectionMetadata {
    /// Optional socket address observed for this connection (e.g. synthesized from a multiaddr).
    pub socket_addr: Option<SocketAddr>,

    /// Optional libp2p-specific metadata describing the remote endpoint.
    pub libp2p: Option<Libp2pConnectInfo>,
}

impl ConnectionMetadata {
    pub fn new(socket_addr: Option<SocketAddr>, libp2p: Option<Libp2pConnectInfo>) -> Self {
        Self { socket_addr, libp2p }
    }

    /// Returns a concise textual description suitable for logging.
    pub fn summary(&self) -> Option<String> {
        self.libp2p.as_ref().map(|info| info.summary())
    }
}

/// Metadata describing a libp2p-backed connection.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Libp2pConnectInfo {
    pub peer_id: String,
    pub remote_multiaddr: Option<String>,
    pub relay_used: bool,
}

impl Libp2pConnectInfo {
    pub fn new(peer_id: impl Into<String>) -> Self {
        Self { peer_id: peer_id.into(), remote_multiaddr: None, relay_used: false }
    }

    pub fn with_address(peer_id: impl Into<String>, remote_multiaddr: Option<String>, relay_used: bool) -> Self {
        Self { peer_id: peer_id.into(), remote_multiaddr, relay_used }
    }

    fn summary(&self) -> String {
        let mut parts = vec![format!("peer={}", self.peer_id)];
        if let Some(addr) = &self.remote_multiaddr {
            parts.push(format!("addr={}", addr));
        }
        if self.relay_used {
            parts.push("relay=true".to_string());
        }
        parts.join(" ")
    }
}

impl fmt::Display for Libp2pConnectInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.summary())
    }
}
