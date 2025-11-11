#[derive(Clone, Debug, Default)]
pub struct Libp2pStatusSnapshot {
    pub enabled: bool,
    pub role: Option<String>,
    pub peer_id: Option<String>,
    pub listen_addrs: Vec<String>,
    pub private_inbound_target: Option<usize>,
    pub relay_inbound_limit: Option<usize>,
}

pub trait Libp2pStatusProvider: Send + Sync {
    fn snapshot(&self) -> Libp2pStatusSnapshot;
}

pub struct NoopLibp2pStatusProvider;

impl Libp2pStatusProvider for NoopLibp2pStatusProvider {
    fn snapshot(&self) -> Libp2pStatusSnapshot {
        Libp2pStatusSnapshot::default()
    }
}
