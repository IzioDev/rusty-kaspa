use std::{
    fs,
    net::{Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures::{channel::oneshot, SinkExt};
use hole_punch_bridge::{
    spawn_swarm_with_config,
    swarm::{SwarmCommand, SwarmHandle},
    tonic_integration::incoming_from_handle,
    BridgeError, SwarmConfig,
};
use kaspa_core::{
    debug, info,
    task::service::{AsyncService, AsyncServiceError, AsyncServiceFuture},
    trace, warn,
};
use kaspa_p2p_flows::flow_context::{FlowContext, Libp2pRelayAdvertisement};
use kaspa_rpc_service::libp2p::{Libp2pStatusProvider, Libp2pStatusSnapshot};
use kaspa_utils::triggers::SingleTrigger;
use libp2p::{
    identity,
    multiaddr::{Multiaddr, Protocol},
};
use parking_lot::RwLock;
use thiserror::Error;
use tokio::{select, sync::Mutex, time::sleep};

/// Mode controlling whether we expose a libp2p listener for incoming hole-punched streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Libp2pRelayMode {
    Auto,
    On,
    Off,
}

impl Default for Libp2pRelayMode {
    fn default() -> Self {
        Self::Auto
    }
}

impl std::str::FromStr for Libp2pRelayMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            other => Err(format!("unsupported libp2p relay mode '{other}'")),
        }
    }
}

impl Libp2pRelayMode {
    fn should_listen(self, has_public_addr: bool) -> bool {
        match self {
            Libp2pRelayMode::On => true,
            Libp2pRelayMode::Off => false,
            Libp2pRelayMode::Auto => has_public_addr,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Libp2pRole {
    Disabled,
    ClientOnly,
    PublicRelay,
}

impl std::fmt::Display for Libp2pRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Libp2pRole::Disabled => write!(f, "disabled"),
            Libp2pRole::ClientOnly => write!(f, "client-only"),
            Libp2pRole::PublicRelay => write!(f, "public-relay"),
        }
    }
}

#[derive(Clone)]
struct Libp2pStatusInner {
    enabled: bool,
    role: Libp2pRole,
    peer_id: Option<String>,
    listen_addrs: Vec<String>,
    private_inbound_target: Option<usize>,
    relay_inbound_limit: Option<usize>,
}

impl Default for Libp2pStatusInner {
    fn default() -> Self {
        Self {
            enabled: false,
            role: Libp2pRole::Disabled,
            peer_id: None,
            listen_addrs: Vec::new(),
            private_inbound_target: None,
            relay_inbound_limit: None,
        }
    }
}

/// Shared libp2p status updated by the bridge service and exposed via RPC.
#[derive(Default)]
pub struct Libp2pStatus {
    inner: RwLock<Libp2pStatusInner>,
}

impl Libp2pStatus {
    pub fn update(&self, enabled: bool, role: Libp2pRole, peer_id: Option<String>, listen_addrs: Vec<String>) {
        let mut guard = self.inner.write();
        guard.enabled = enabled;
        guard.role = role;
        guard.peer_id = peer_id;
        guard.listen_addrs = listen_addrs;
    }

    pub fn set_limits(&self, private_inbound_target: Option<usize>, relay_inbound_limit: Option<usize>) {
        let mut guard = self.inner.write();
        guard.private_inbound_target = private_inbound_target;
        guard.relay_inbound_limit = relay_inbound_limit;
    }

    pub fn disable(&self) {
        self.update(false, Libp2pRole::Disabled, None, Vec::new());
        self.set_limits(None, None);
    }
}

impl Libp2pStatusProvider for Libp2pStatus {
    fn snapshot(&self) -> Libp2pStatusSnapshot {
        let guard = self.inner.read();
        Libp2pStatusSnapshot {
            enabled: guard.enabled,
            role: Some(guard.role.to_string()).filter(|_| guard.enabled),
            peer_id: guard.peer_id.clone(),
            listen_addrs: guard.listen_addrs.clone(),
            private_inbound_target: guard.private_inbound_target,
            relay_inbound_limit: guard.relay_inbound_limit,
        }
    }
}

#[derive(Clone)]
pub struct Libp2pBridgeConfig {
    pub relay_mode: Libp2pRelayMode,
    pub listen_port: u16,
    pub identity_path: PathBuf,
    pub has_public_address: bool,
}

impl Libp2pBridgeConfig {
    fn role(&self, has_public_addr: bool) -> Libp2pRole {
        if self.relay_mode.should_listen(has_public_addr) {
            Libp2pRole::PublicRelay
        } else {
            Libp2pRole::ClientOnly
        }
    }
}

pub struct Libp2pBridgeService {
    flow_context: Arc<FlowContext>,
    config: Libp2pBridgeConfig,
    status: Arc<Libp2pStatus>,
    shutdown: SingleTrigger,
    swarm_handle: Mutex<Option<SwarmHandle>>,
}

impl Libp2pBridgeService {
    pub fn new(flow_context: Arc<FlowContext>, config: Libp2pBridgeConfig, status: Arc<Libp2pStatus>) -> Self {
        Self { flow_context, config, status, shutdown: SingleTrigger::default(), swarm_handle: Mutex::new(None) }
    }

    async fn run_inner(self: Arc<Self>) -> Result<(), Libp2pBridgeError> {
        let identity = self.load_or_create_identity().await?;
        let mut handle = spawn_swarm_with_config(identity, SwarmConfig::default()).map_err(Libp2pBridgeError::Bridge)?;
        let peer_id = handle.local_peer_id();

        let has_public_addr =
            self.config.has_public_address || self.flow_context.address_manager.lock().best_local_address().is_some();
        let role = self.config.role(has_public_addr);

        let listen_addrs = if matches!(role, Libp2pRole::PublicRelay) { self.start_listeners(&mut handle).await? } else { Vec::new() };
        self.status.update(true, role, Some(peer_id.to_string()), listen_addrs.clone());
        if matches!(role, Libp2pRole::PublicRelay) {
            self.flow_context.set_libp2p_advertisement(Some(Libp2pRelayAdvertisement::new(self.config.listen_port)));
        } else {
            self.flow_context.set_libp2p_advertisement(None);
        }

        match role {
            Libp2pRole::PublicRelay => {
                info!("libp2p bridge running as public relay (peer {}), listening on: {}", peer_id, listen_addrs.join(", "));
            }
            Libp2pRole::ClientOnly => {
                if has_public_addr {
                    info!("libp2p bridge running in client-only mode (peer {})", peer_id);
                } else {
                    info!("libp2p bridge running in client-only mode (peer {}), no public interface detected", peer_id);
                }
            }
            Libp2pRole::Disabled => {}
        }

        let adaptor = self.wait_for_adaptor().await?;
        let incoming = incoming_from_handle(&mut handle)?;
        adaptor.serve_incoming_streams(incoming);
        trace!("libp2p bridge connected to adaptor for incoming streams");

        {
            let mut guard = self.swarm_handle.lock().await;
            *guard = Some(handle);
        }

        self.shutdown.listener.clone().await;
        self.shutdown_swarm().await;
        self.status.disable();
        self.flow_context.set_libp2p_advertisement(None);
        Ok(())
    }

    async fn shutdown_swarm(&self) {
        let mut guard = self.swarm_handle.lock().await;
        if let Some(handle) = guard.take() {
            if let Err(err) = handle.shutdown().await {
                warn!("Failed to shutdown libp2p swarm: {err}");
            }
        }
    }

    async fn start_listeners(&self, handle: &mut SwarmHandle) -> Result<Vec<String>, Libp2pBridgeError> {
        let mut active = Vec::new();
        for addr in listen_multiaddrs(self.config.listen_port) {
            let addr_string = addr.to_string();
            let (response_tx, response_rx) = oneshot::channel::<Result<(), BridgeError>>();
            handle
                .command_tx()
                .send(SwarmCommand::ListenOn { addr, response: Some(response_tx) })
                .await
                .map_err(|_| Libp2pBridgeError::CommandChannelClosed)?;
            match response_rx.await {
                Ok(Ok(())) => {
                    debug!("libp2p bridge listening on {addr_string}");
                    active.push(addr_string);
                }
                Ok(Err(err)) => return Err(Libp2pBridgeError::Bridge(err)),
                Err(_) => return Err(Libp2pBridgeError::CommandChannelClosed),
            }
        }
        Ok(active)
    }

    async fn wait_for_adaptor(&self) -> Result<Arc<kaspa_p2p_lib::Adaptor>, Libp2pBridgeError> {
        let mut shutdown = self.shutdown.listener.clone();
        loop {
            if let Some(adaptor) = self.flow_context.p2p_adaptor() {
                return Ok(adaptor);
            }

            if self.shutdown.trigger.is_triggered() {
                return Err(Libp2pBridgeError::AdaptorUnavailable);
            }

            select! {
                _ = &mut shutdown => return Err(Libp2pBridgeError::AdaptorUnavailable),
                _ = sleep(Duration::from_millis(100)) => {}
            }
        }
    }

    async fn load_or_create_identity(&self) -> Result<identity::Keypair, Libp2pBridgeError> {
        let path = self.config.identity_path.clone();
        tokio::task::spawn_blocking(move || load_or_create_identity(&path))
            .await
            .map_err(|err| Libp2pBridgeError::Join(err.to_string()))?
    }
}

fn load_or_create_identity(path: &Path) -> Result<identity::Keypair, Libp2pBridgeError> {
    if let Ok(bytes) = fs::read(path) {
        identity::Keypair::from_protobuf_encoding(&bytes).map_err(|err| Libp2pBridgeError::KeyEncoding(err.to_string()))
    } else {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let keypair = identity::Keypair::generate_ed25519();
        let bytes = keypair.to_protobuf_encoding().map_err(|err| Libp2pBridgeError::KeyEncoding(err.to_string()))?;
        fs::write(path, bytes)?;
        Ok(keypair)
    }
}

fn listen_multiaddrs(port: u16) -> Vec<Multiaddr> {
    vec![
        Multiaddr::empty().with(Protocol::Ip4(Ipv4Addr::UNSPECIFIED)).with(Protocol::Tcp(port)),
        Multiaddr::empty().with(Protocol::Ip6(Ipv6Addr::UNSPECIFIED)).with(Protocol::Tcp(port)),
    ]
}

impl AsyncService for Libp2pBridgeService {
    fn ident(self: Arc<Self>) -> &'static str {
        "libp2p-bridge-service"
    }

    fn start(self: Arc<Self>) -> AsyncServiceFuture {
        Box::pin(async move {
            if let Err(err) = self.clone().run_inner().await {
                self.status.disable();
                warn!("libp2p bridge service stopped with error: {err}");
                return Err(AsyncServiceError::Service(err.to_string()));
            }
            Ok(())
        })
    }

    fn stop(self: Arc<Self>) -> AsyncServiceFuture {
        Box::pin(async move {
            self.shutdown_swarm().await;
            Ok(())
        })
    }

    fn signal_exit(self: Arc<Self>) {
        self.shutdown.trigger.trigger();
    }
}

#[derive(Error, Debug)]
enum Libp2pBridgeError {
    #[error("libp2p bridge error: {0}")]
    Bridge(#[from] BridgeError),
    #[error("command channel closed")]
    CommandChannelClosed,
    #[error("failed to load libp2p identity: {0}")]
    KeyEncoding(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("p2p adaptor is not available")]
    AdaptorUnavailable,
    #[error("task join error: {0}")]
    Join(String),
}
