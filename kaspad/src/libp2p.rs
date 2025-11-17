use std::{
    convert::TryFrom,
    fs,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use futures::{channel::oneshot, SinkExt};
use hole_punch_bridge::{
    spawn_swarm_with_config,
    stream::Libp2pConnectInfo as BridgeLibp2pInfo,
    swarm::{SwarmCommand, SwarmHandle},
    tonic_integration::incoming_from_handle,
    BridgeError, SwarmConfig,
};
use kaspa_core::{
    debug, info,
    task::service::{AsyncService, AsyncServiceError, AsyncServiceFuture},
    trace, warn,
};
use kaspa_libp2p_helper_protocol::{Libp2pDialRequest, Libp2pDialResponse};
use kaspa_p2p_flows::flow_context::{FlowContext, Libp2pRelayAdvertisement};
use kaspa_p2p_lib::{ConnectionError, ConnectionMetadata, PeerKey};
use kaspa_rpc_service::libp2p::{Libp2pStatusProvider, Libp2pStatusSnapshot};
use kaspa_utils::triggers::{Listener, SingleTrigger};
use libp2p::{
    identity,
    multiaddr::{Multiaddr, Protocol},
    PeerId,
};
use parking_lot::RwLock;
use serde_json;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    select,
    sync::Mutex,
    task::JoinHandle,
    time::{sleep, timeout},
};

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
    pub helper_address: Option<SocketAddr>,
    pub reservation_multiaddrs: Vec<Multiaddr>,
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
    helper_task: Mutex<Option<JoinHandle<()>>>,
}

impl Libp2pBridgeService {
    pub fn new(flow_context: Arc<FlowContext>, config: Libp2pBridgeConfig, status: Arc<Libp2pStatus>) -> Self {
        Self {
            flow_context,
            config,
            status,
            shutdown: SingleTrigger::default(),
            swarm_handle: Mutex::new(None),
            helper_task: Mutex::new(None),
        }
    }

    async fn run_inner(self: Arc<Self>) -> Result<(), Libp2pBridgeError> {
        let has_public_addr =
            self.config.has_public_address || self.flow_context.address_manager.lock().best_local_address().is_some();
        let role = self.config.role(has_public_addr);
        info!("libp2p bridge role at startup: {:?}", role);

        let mut swarm_config = SwarmConfig::default();
        if matches!(role, Libp2pRole::PublicRelay) {
            swarm_config.relay_server.enabled = true;
            info!("libp2p relay server enabled for public relay mode");
        }

        // Enable relay client behavior if reservations are configured
        if !self.config.reservation_multiaddrs.is_empty() {
            swarm_config.relay.enabled = true;
        }

        let identity = self.load_or_create_identity().await?;
        let mut handle = spawn_swarm_with_config(identity, swarm_config).map_err(Libp2pBridgeError::Bridge)?;
        let peer_id = handle.local_peer_id();

        let listen_addrs = if matches!(role, Libp2pRole::PublicRelay) { self.start_listeners(&mut handle).await? } else { Vec::new() };
        self.status.update(true, role, Some(peer_id.to_string()), listen_addrs.clone());
        if matches!(role, Libp2pRole::PublicRelay) {
            self.flow_context.set_libp2p_advertisement(Some(Libp2pRelayAdvertisement::new(self.config.listen_port)));
        } else {
            self.flow_context.set_libp2p_advertisement(None);
        }

        if !self.config.reservation_multiaddrs.is_empty() {
            info!("libp2p reservation targets: {:?}", self.config.reservation_multiaddrs);
            self.start_reservations(&mut handle).await?;
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
        if let Some(addr) = self.config.helper_address {
            if let Err(err) = self.start_helper_server(addr).await {
                warn!("Failed to start libp2p helper server: {err}");
            }
        }

        self.shutdown.listener.clone().await;
        self.shutdown_swarm().await;
        self.status.disable();
        self.flow_context.set_libp2p_advertisement(None);
        Ok(())
    }

    async fn shutdown_swarm(&self) {
        self.stop_helper_server().await;
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

    async fn start_reservations(&self, handle: &mut SwarmHandle) -> Result<(), Libp2pBridgeError> {
        for addr in &self.config.reservation_multiaddrs {
            let addr_string = addr.to_string();

            info!("requesting libp2p reservation via {addr_string}");

            // Create response channel to verify listen_on succeeds
            let (response_tx, response_rx) = futures::channel::oneshot::channel();
            handle
                .command_tx()
                .send(SwarmCommand::ListenOn { addr: addr.clone(), response: Some(response_tx) })
                .await
                .map_err(|_| Libp2pBridgeError::CommandChannelClosed)?;

            // Wait for listen_on to be processed
            // The actual ReservationReqAccepted event will come through the swarm event loop
            match response_rx.await {
                Ok(Ok(())) => info!("libp2p listen_on sent for {addr_string}"),
                Ok(Err(err)) => warn!("libp2p listen_on failed for {addr_string}: {err}"),
                Err(_) => warn!("libp2p listen_on response channel dropped for {addr_string}"),
            }
        }
        Ok(())
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

    async fn start_helper_server(self: &Arc<Self>, addr: SocketAddr) -> Result<(), Libp2pBridgeError> {
        let listener = TcpListener::bind(addr).await?;
        let actual_addr = listener.local_addr()?;
        info!("libp2p helper control listening on {actual_addr}");
        let service = Arc::clone(self);
        let shutdown = self.shutdown.listener.clone();
        let handle = tokio::spawn(async move { service.helper_listener_loop(listener, shutdown).await });
        *self.helper_task.lock().await = Some(handle);
        Ok(())
    }

    async fn helper_listener_loop(self: Arc<Self>, listener: TcpListener, shutdown: Listener) {
        loop {
            tokio::select! {
                _ = shutdown.clone() => break,
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, remote)) => {
                            let svc = Arc::clone(&self);
                            tokio::spawn(async move {
                                svc.handle_helper_client(stream, remote).await;
                            });
                        }
                        Err(err) => {
                            warn!("libp2p helper accept failed: {err}");
                            break;
                        }
                    }
                }
            }
        }
        info!("libp2p helper control stopped");
    }

    async fn stop_helper_server(&self) {
        let handle = self.helper_task.lock().await.take();
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
        }
    }

    async fn handle_helper_client(self: Arc<Self>, stream: TcpStream, remote: SocketAddr) {
        if let Err(err) = self.process_helper_client(stream).await {
            warn!("libp2p helper client {remote} failed: {err}");
        }
    }

    async fn process_helper_client(&self, stream: TcpStream) -> Result<(), Libp2pBridgeError> {
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            return Ok(());
        }
        let request: Libp2pDialRequest =
            serde_json::from_str(line.trim()).map_err(|err| Libp2pBridgeError::InvalidRequest(err.to_string()))?;
        let response = self.process_helper_request(request).await;
        let payload = serde_json::to_vec(&response).map_err(|err| Libp2pBridgeError::HelperSerialization(err.to_string()))?;
        writer.write_all(&payload).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }

    async fn process_helper_request(&self, request: Libp2pDialRequest) -> Libp2pDialResponse {
        match self.execute_dial(&request).await {
            Ok(outcome) => {
                let message = format!("peer {} connected via libp2p", request.target_peer_id);
                Libp2pDialResponse::success(
                    message,
                    outcome.libp2p_multiaddr.clone(),
                    Some(outcome.relay_used),
                    Some(outcome.peer_key.to_string()),
                )
                .with_target(request.target_peer_id)
            }
            Err(err) => {
                warn!("libp2p helper dial failed: {err}");
                Libp2pDialResponse::failure(err.to_string()).with_target(request.target_peer_id)
            }
        }
    }

    async fn execute_dial(&self, request: &Libp2pDialRequest) -> Result<DialOutcome, Libp2pBridgeError> {
        if request.relay_multiaddrs.is_empty() {
            return Err(Libp2pBridgeError::InvalidRequest("at least one relay multiaddr is required".into()));
        }
        let addrs = self.parse_multiaddrs(request)?;
        let target_peer = PeerId::from_str(&request.target_peer_id)
            .map_err(|err| Libp2pBridgeError::InvalidRequest(format!("invalid target peer id: {err}")))?;
        let timeout_ms = request.timeout_ms.unwrap_or(30_000);
        let timeout_duration = Duration::from_millis(timeout_ms);
        self.dial_via_swarm(target_peer, addrs, timeout_duration).await
    }

    fn parse_multiaddrs(&self, request: &Libp2pDialRequest) -> Result<Vec<Multiaddr>, Libp2pBridgeError> {
        let mut addrs = Vec::with_capacity(request.relay_multiaddrs.len());
        for raw in &request.relay_multiaddrs {
            let addr: Multiaddr =
                raw.parse().map_err(|err| Libp2pBridgeError::InvalidRequest(format!("invalid multiaddr '{raw}': {err}")))?;
            if !addr.iter().any(|component| matches!(component, Protocol::P2pCircuit)) {
                warn!("relay multiaddr '{raw}' is missing /p2p-circuit");
            }
            if let Some(expected) = &request.relay_peer_id {
                if !multiaddr_contains_peer(&addr, expected) {
                    warn!("relay multiaddr '{raw}' does not include expected relay peer id {expected}");
                }
            }
            addrs.push(addr);
        }
        Ok(addrs)
    }

    async fn dial_via_swarm(
        &self,
        target_peer: PeerId,
        addrs: Vec<Multiaddr>,
        timeout_duration: Duration,
    ) -> Result<DialOutcome, Libp2pBridgeError> {
        let mut command_tx = {
            let guard = self.swarm_handle.lock().await;
            let handle = guard.as_ref().ok_or(Libp2pBridgeError::AdaptorUnavailable)?;
            handle.command_tx()
        };
        let (response_tx, response_rx) = oneshot::channel();
        command_tx
            .send(SwarmCommand::Dial { peer: target_peer, addrs, response: response_tx })
            .await
            .map_err(|_| Libp2pBridgeError::CommandChannelClosed)?;
        let stream_handle = timeout(timeout_duration, response_rx)
            .await
            .map_err(|_| Libp2pBridgeError::DialTimeout)?
            .map_err(|_| Libp2pBridgeError::CommandChannelClosed)??;
        let adaptor = self.flow_context.p2p_adaptor().ok_or(Libp2pBridgeError::AdaptorUnavailable)?;
        let stream = stream_handle.into_stream();
        let info = stream.info.clone();
        let metadata = metadata_from_info(&info);
        let peer_key = adaptor.connect_peer_with_stream(stream, metadata).await?;
        let libp2p_multiaddr = info.remote_multiaddr.as_ref().map(|addr| addr.to_string());
        Ok(DialOutcome { peer_key, libp2p_multiaddr, relay_used: info.relay_used })
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

fn metadata_from_info(info: &BridgeLibp2pInfo) -> ConnectionMetadata {
    let libp2p_info = kaspa_p2p_lib::Libp2pConnectInfo::with_address(
        info.peer_id.to_string(),
        info.remote_multiaddr.as_ref().map(|addr| addr.to_string()),
        info.relay_used,
    );
    ConnectionMetadata::new(info.synthesized_socket, Some(libp2p_info))
}

fn multiaddr_contains_peer(addr: &Multiaddr, expected: &str) -> bool {
    if let Ok(expected_peer) = PeerId::from_str(expected) {
        for component in addr.iter() {
            if let Protocol::P2p(multihash) = component {
                if PeerId::try_from(multihash.clone()).map(|peer| peer == expected_peer).unwrap_or(false) {
                    return true;
                }
            }
        }
    }
    false
}

struct DialOutcome {
    peer_key: PeerKey,
    libp2p_multiaddr: Option<String>,
    relay_used: bool,
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
    #[error("libp2p connection error: {0}")]
    Connection(#[from] ConnectionError),
    #[error("failed to load libp2p identity: {0}")]
    KeyEncoding(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("p2p adaptor is not available")]
    AdaptorUnavailable,
    #[error("task join error: {0}")]
    Join(String),
    #[error("invalid libp2p helper request: {0}")]
    InvalidRequest(String),
    #[error("libp2p dial timed out")]
    DialTimeout,
    #[error("libp2p helper serialization error: {0}")]
    HelperSerialization(String),
}
