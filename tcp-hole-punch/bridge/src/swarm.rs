use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::{
    channel::{mpsc, oneshot},
    future::FutureExt,
    sink::Sink,
    stream::FuturesUnordered,
    SinkExt, StreamExt,
};
use kaspa_core::{debug, error, info, warn};
use libp2p::{
    core::connection::ConnectedPoint,
    dcutr, identify,
    multiaddr::Protocol,
    noise, relay,
    swarm::{behaviour::toggle::Toggle, NetworkBehaviour, StreamProtocol, Swarm, SwarmEvent},
    yamux, Multiaddr, PeerId, SwarmBuilder,
};
use libp2p_stream::{self as lpstream, AlreadyRegistered, OpenStreamError};
use std::task::{Context, Poll};
use tokio::task::JoinHandle;

use crate::{
    stream::{Libp2pConnectInfo, Libp2pStreamHandle},
    BridgeError, Result,
};

const BRIDGE_PROTOCOL_ID: &str = "/kaspa/p2p/bridge/1.0";

/// Transport-level configuration for the libp2p swarm.
#[derive(Clone, Debug)]
pub struct TransportConfig {
    /// Enable the QUIC transport in addition to TCP.
    pub enable_quic: bool,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self { enable_quic: false }
    }
}

/// Configuration for relay client behaviour.
#[derive(Clone, Debug)]
pub struct RelayConfig {
    /// Toggle the relay client behaviour.
    pub enabled: bool,
    /// Maximum total reservations to keep active.
    pub max_reservations: u16,
    /// Maximum relayed circuits we keep for a single relay peer.
    pub max_circuits_per_peer: u16,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self { enabled: true, max_reservations: 8, max_circuits_per_peer: 4 }
    }
}

/// Configuration for running a relay server (hop) behaviour.
#[derive(Clone, Debug)]
pub struct RelayServerConfig {
    /// Toggle the relay server behaviour.
    pub enabled: bool,
}

impl Default for RelayServerConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

/// Configuration for hole-punching behaviour.
#[derive(Clone, Debug)]
pub struct HolePunchConfig {
    /// Toggle libp2p DCUtR hole punching support.
    pub enable_dcutr: bool,
}

impl Default for HolePunchConfig {
    fn default() -> Self {
        Self { enable_dcutr: true }
    }
}

/// Aggregate swarm configuration.
#[derive(Clone, Debug, Default)]
pub struct SwarmConfig {
    pub transport: TransportConfig,
    pub relay: RelayConfig,
    pub hole_punch: HolePunchConfig,
    pub relay_server: RelayServerConfig,
    pub external_addresses: Vec<Multiaddr>,
}

/// Public command sender wrapper used by tonic integration.
pub struct SwarmCommandSender {
    inner: mpsc::Sender<SwarmCommand>,
    tracker: Arc<CommandTracker>,
}

impl SwarmCommandSender {
    fn new(label: impl Into<String>, inner: mpsc::Sender<SwarmCommand>) -> Self {
        let tracker = Arc::new(CommandTracker::new(label));
        tracker.inc("init");
        Self { inner, tracker }
    }

    fn tracker(&self) -> &CommandTracker {
        &self.tracker
    }

    fn clone_with_reason(&self, reason: &str) -> Self {
        self.tracker.inc(reason);
        Self { inner: self.inner.clone(), tracker: self.tracker.clone() }
    }
}

impl Clone for SwarmCommandSender {
    fn clone(&self) -> Self {
        self.clone_with_reason("clone")
    }
}

impl Drop for SwarmCommandSender {
    fn drop(&mut self) {
        self.tracker.dec("drop");
    }
}

impl Sink<SwarmCommand> for SwarmCommandSender {
    type Error = mpsc::SendError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: SwarmCommand) -> std::result::Result<(), Self::Error> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_close(cx)
    }
}

impl Deref for SwarmCommandSender {
    type Target = mpsc::Sender<SwarmCommand>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for SwarmCommandSender {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[derive(Debug)]
struct CommandTracker {
    label: String,
    active: AtomicUsize,
}

impl CommandTracker {
    fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), active: AtomicUsize::new(0) }
    }

    fn inc(&self, reason: &str) -> usize {
        let new = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        eprintln!("command-tx[{}]: ++ ({reason}) -> {new}", self.label);
        new
    }

    fn dec(&self, reason: &str) -> usize {
        let new = self.active.fetch_sub(1, Ordering::SeqCst) - 1;
        eprintln!("command-tx[{}]: -- ({reason}) -> {new}", self.label);
        new
    }

    fn count(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }
}

pub type IncomingStreamReceiver = mpsc::Receiver<Libp2pStreamHandle>;

/// Commands that can be issued to the libp2p swarm actor.
pub enum SwarmCommand {
    Dial { peer: PeerId, addrs: Vec<Multiaddr>, response: oneshot::Sender<Result<Libp2pStreamHandle>> },
    ListenOn { addr: Multiaddr, response: Option<oneshot::Sender<Result<()>>> },
    Shutdown,
}

/// Handle returned to the bridge so it can interact with the swarm actor.
pub struct SwarmHandle {
    command_tx: SwarmCommandSender,
    incoming_rx: Option<IncomingStreamReceiver>,
    join_handle: JoinHandle<()>,
    peer_id: PeerId,
}

impl SwarmHandle {
    pub fn command_tx(&self) -> SwarmCommandSender {
        self.command_tx.clone_with_reason("SwarmHandle::command_tx")
    }

    pub fn join_handle(&self) -> &JoinHandle<()> {
        &self.join_handle
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub fn take_incoming(&mut self) -> Option<IncomingStreamReceiver> {
        self.incoming_rx.take()
    }

    pub async fn shutdown(self) -> Result<()> {
        let SwarmHandle { command_tx, mut incoming_rx, join_handle, peer_id: _ } = self;
        if let Some(rx) = incoming_rx.take() {
            drop(rx);
        }

        let before = command_tx.tracker().count();
        eprintln!("SwarmHandle::shutdown: sending Shutdown; active clones={before}");
        let mut tx = command_tx;
        tx.send(SwarmCommand::Shutdown).await.map_err(|_| BridgeError::CommandChannelClosed)?;

        eprintln!("SwarmHandle::shutdown: awaiting join");
        join_handle.await.map_err(|err| BridgeError::DialFailed(format!("swarm task join error: {err}")))?;
        eprintln!("SwarmHandle::shutdown: join complete");
        Ok(())
    }
}

/// Behaviour bundling the protocols we care about.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BridgeBehaviourEvent")]
struct BridgeBehaviour {
    identify: identify::Behaviour,
    ping: libp2p::ping::Behaviour,
    relay_client: Toggle<relay::client::Behaviour>,
    relay_server: Toggle<relay::Behaviour>,
    dcutr: dcutr::Behaviour,
    stream: lpstream::Behaviour,
}

/// Spawn the libp2p swarm actor using the default configuration.
pub fn spawn_swarm(local_key: libp2p::identity::Keypair) -> Result<SwarmHandle> {
    spawn_swarm_with_config(local_key, SwarmConfig::default())
}

/// Spawn the libp2p swarm actor with a custom configuration and return a handle for issuing commands.
pub fn spawn_swarm_with_config(local_key: libp2p::identity::Keypair, config: SwarmConfig) -> Result<SwarmHandle> {
    let peer_id_label = local_key.public().to_peer_id();
    let (raw_sender, mut command_rx) = mpsc::channel::<SwarmCommand>(128);
    let (incoming_tx, incoming_rx) = mpsc::channel::<Libp2pStreamHandle>(128);

    let command_tx = SwarmCommandSender::new(format!("swarm-{peer_id_label}"), raw_sender);

    let mut swarm = build_swarm(local_key.clone(), &config)?;
    for addr in &config.external_addresses {
        swarm.add_external_address(addr.clone());
    }
    let config = Arc::new(config);
    let mut control = swarm.behaviour_mut().stream.new_control();
    let mut incoming_streams = control.accept(stream_protocol()).map_err(|e| match e {
        AlreadyRegistered => BridgeError::DialFailed("protocol already registered".into()),
    })?;
    let dial_control = control.clone();
    drop(control);

    let config_for_join = Arc::clone(&config);
    let join_handle = tokio::spawn(async move {
        let config = config_for_join;
        let mut peer_book = PeerBook::default();
        let mut incoming_tx = incoming_tx;
        let mut pending_dials = FuturesUnordered::new();
        let mut pending_listen_acks: Vec<(String, oneshot::Sender<std::result::Result<(), BridgeError>>)> = Vec::new();
        loop {
            tokio::select! {
                cmd = command_rx.next() => {
                    match cmd {
                        Some(SwarmCommand::Dial { peer, addrs, response }) => {
                            debug!("Received dial command peer={} addrs={:?}", peer, addrs);
                            let mut reserved_relays: Vec<Multiaddr> = Vec::new();
                            if config.relay.enabled {
                                let relay_candidates: Vec<_> = addrs.iter().cloned().filter(|addr| addr_uses_relay(addr)).collect();
                                let new_relay_slots = relay_candidates.len();
                                if new_relay_slots > 0 {
                                    let total = peer_book.relay_address_count();
                                    let per_peer = peer_book.relay_address_count_for(peer);
                                    let max_total = config.relay.max_reservations as usize;
                                    let max_per_peer = config.relay.max_circuits_per_peer as usize;
                                    if total + new_relay_slots > max_total {
                                        let _ = response.send(Err(BridgeError::DialFailed(format!(
                                            "relay reservation limit reached (total {total} >= {max_total})"
                                        ))));
                                        continue;
                                    }
                                    if per_peer + new_relay_slots > max_per_peer {
                                        let _ = response.send(Err(BridgeError::DialFailed(format!(
                                            "relay reservation limit reached for peer {peer} (current {per_peer} >= {max_per_peer})"
                                        ))));
                                        continue;
                                    }
                                }
                                reserved_relays = peer_book.reserve_addresses(peer, &relay_candidates);
                            }
                            let mut control_clone = dial_control.clone();
                            if !addrs.is_empty() {
                                // Construct full multiaddrs with /p2p/{peer} suffix for dialing
                                let full_addrs: Vec<_> = addrs
                                    .iter()
                                    .cloned()
                                    .map(|mut addr| {
                                        addr.push(Protocol::P2p(peer));
                                        addr
                                    })
                                    .collect();
                                // Dial using full multiaddrs - this is the most explicit approach
                                for addr in &full_addrs {
                                    if let Err(err) = swarm.dial(addr.clone()) {
                                        warn!("Failed to dial address peer={} addr={} err={}", peer, addr, err);
                                    } else {
                                        debug!("Dial initiated successfully peer={} addr={}", peer, addr);
                                        break; // Successfully initiated one dial, that's enough
                                    }
                                }
                            }
                            // open_stream will wait for the connection to establish, then open a stream
                            pending_dials.push(async move {
                                let result = control_clone.open_stream(peer, stream_protocol()).await.map_err(map_stream_error);
                                (peer, response, result, reserved_relays)
                            }.boxed());
                        }
                        Some(SwarmCommand::ListenOn { addr, response }) => {
                            let addr_string = addr.to_string();
                            info!("Received ListenOn command addr={} has_response={}", addr_string, response.is_some());
                            match swarm.listen_on(addr) {
                                Ok(_) => {
                                    if let Some(tx) = response {
                                        pending_listen_acks.push((addr_string.clone(), tx));
                                    }
                                }
                                Err(err) => {
                                    error!("failed to listen addr={} err={}", addr_string, err);
                                    if let Some(tx) = response {
                                        let _ = tx.send(Err(BridgeError::DialFailed(format!("listen error: {err}"))));
                                    }
                                }
                            }
                        }
                        Some(SwarmCommand::Shutdown) => break,
                        None => break,
                    }
                }
                dial_result = pending_dials.next(), if !pending_dials.is_empty() => {
                    if let Some((peer, response, outcome, reserved_relays)) = dial_result {
                        match outcome {
                            Ok(stream) => {
                                debug!("Stream opened successfully peer={}", peer);
                                peer_book.confirm_addresses(peer, &reserved_relays);
                                let info = peer_book.info_for(peer);
                                let handle = Libp2pStreamHandle::new(stream, info);
                                if response.send(Ok(handle)).is_err() {
                                    debug!("dial response channel dropped peer={}", peer);
                                }
                            }
                            Err(err) => {
                                warn!("Failed to open stream peer={} err={}", peer, err);
                                peer_book.release_pending(peer, &reserved_relays);
                                if response.send(Err(err)).is_err() {
                                    debug!("dial response channel dropped (error) peer={}", peer);
                                }
                            }
                        }
                    }
                }
                inbound = incoming_streams.next() => {
                    match inbound {
                        Some((peer_id, stream)) => {
                            let info = peer_book.info_for(peer_id);
                            let handle = Libp2pStreamHandle::new(stream, info);
                            if incoming_tx.send(handle).await.is_err() {
                                warn!("incoming queue closed; dropping inbound stream peer={}", peer_id);
                            }
                        }
                        None => break,
                    }
                }
                swarm_event = swarm.select_next_some() => {
                    if let SwarmEvent::ConnectionEstablished { peer_id, .. } = &swarm_event {
                        swarm.behaviour_mut().identify.push(std::iter::once(*peer_id));
                        debug!("[IDENTIFY] queued push to peer {}", peer_id);
                    }
                    match swarm_event {
                        SwarmEvent::NewListenAddr { ref address, .. } => {
                            info!("Swarm listening address={}", address);
                            for (requested, ack) in pending_listen_acks.drain(..) {
                                info!("Resolving ListenOn command listen_addr={} requested_addr={}", address, requested);
                                let _ = ack.send(Ok(()));
                            }
                        }
                        event => handle_swarm_event(event, &mut peer_book),
                    }
                }
            }
        }
    });

    Ok(SwarmHandle { command_tx, incoming_rx: Some(incoming_rx), join_handle, peer_id: local_key.public().to_peer_id() })
}

fn build_swarm(local_key: libp2p::identity::Keypair, config: &SwarmConfig) -> Result<Swarm<BridgeBehaviour>> {
    // Configure yamux with large buffers to support HTTP/2 over libp2p
    let receive_window_size = 32 * 1024 * 1024;
    let max_buffer_size = 32 * 1024 * 1024;

    let yamux_config = {
        let mut cfg = yamux::Config::default();
        #[allow(deprecated)]
        {
            cfg.set_receive_window_size(receive_window_size);
            cfg.set_max_buffer_size(max_buffer_size);
        }
        cfg
    };

    eprintln!("✓ Yamux: receive_window={}MiB, max_buffer={}MiB", receive_window_size / (1024 * 1024), max_buffer_size / (1024 * 1024));

    let base_builder = SwarmBuilder::with_existing_identity(local_key.clone())
        .with_tokio()
        // Use only Noise for simplicity in tests - TLS negotiation can hang
        .with_tcp(Default::default(), noise::Config::new, || yamux_config.clone())
        .map_err(|e| BridgeError::DialFailed(format!("tcp transport init failed: {e}")))?;

    if config.transport.enable_quic {
        let builder = base_builder.with_quic();
        if config.relay.enabled {
            let behaviour_config = config.clone();
            let yamux_cfg = yamux_config.clone();
            let builder = builder
                .with_relay_client(noise::Config::new, move || yamux_cfg.clone())
                .map_err(|e| BridgeError::DialFailed(format!("relay client init failed: {e}")))?
                .with_behaviour(move |key, relay_behaviour| {
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(build_behaviour(key, Some(relay_behaviour), &behaviour_config))
                })
                .map_err(|e| BridgeError::DialFailed(format!("behaviour init failed: {e}")))?;
            Ok(builder.build())
        } else {
            let behaviour_config = config.clone();
            let builder = builder
                .with_behaviour(move |key| {
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(build_behaviour(key, None, &behaviour_config))
                })
                .map_err(|e| BridgeError::DialFailed(format!("behaviour init failed: {e}")))?;
            Ok(builder.build())
        }
    } else if config.relay.enabled {
        let behaviour_config = config.clone();
        let builder = base_builder
            .with_relay_client(noise::Config::new, || yamux_config.clone())
            .map_err(|e| BridgeError::DialFailed(format!("relay client init failed: {e}")))?
            .with_behaviour(move |key, relay_behaviour| {
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(build_behaviour(key, Some(relay_behaviour), &behaviour_config))
            })
            .map_err(|e| BridgeError::DialFailed(format!("behaviour init failed: {e}")))?;
        Ok(builder.build())
    } else {
        let behaviour_config = config.clone();
        let builder = base_builder
            .with_behaviour(move |key| {
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(build_behaviour(key, None, &behaviour_config))
            })
            .map_err(|e| BridgeError::DialFailed(format!("behaviour init failed: {e}")))?;
        Ok(builder.build())
    }
}

fn build_behaviour(
    key: &libp2p::identity::Keypair,
    relay_behaviour: Option<relay::client::Behaviour>,
    config: &SwarmConfig,
) -> BridgeBehaviour {
    let public = key.public();

    let relay_client = Toggle::from(relay_behaviour);
    let relay_server = if config.relay_server.enabled {
        info!("libp2p relay server behaviour enabled peer={}", public.to_peer_id());
        let mut relay_server_config = relay::Config::default();
        relay_server_config.max_reservations = 256;
        relay_server_config.max_reservations_per_peer = 32;
        relay_server_config.reservation_duration = Duration::from_secs(10 * 60);
        relay_server_config.max_circuits = 128;
        relay_server_config.max_circuits_per_peer = 32;
        relay_server_config.max_circuit_duration = Duration::from_secs(10 * 60);
        relay_server_config.max_circuit_bytes = 16 * 1024 * 1024;
        Toggle::from(Some(relay::Behaviour::new(public.to_peer_id(), relay_server_config)))
    } else {
        Toggle::from(None)
    };

    // Always enable DCUtR for hole-punching support
    info!("DCUtR behaviour ENABLED for peer={}", public.to_peer_id());
    let dcutr = dcutr::Behaviour::new(public.to_peer_id());

    let identify_cfg = identify::Config::new("/kaspa/0.1.0".into(), public).with_push_listen_addr_updates(true);

    BridgeBehaviour {
        identify: identify::Behaviour::new(identify_cfg),
        ping: libp2p::ping::Behaviour::new(libp2p::ping::Config::new()),
        relay_client,
        relay_server,
        dcutr,
        stream: lpstream::Behaviour::default(),
    }
}

#[derive(Default)]
struct PeerBook {
    active: HashMap<PeerId, VecDeque<Multiaddr>>,
    pending: HashMap<PeerId, VecDeque<Multiaddr>>,
    relay_overrides: HashSet<PeerId>,
}

impl PeerBook {
    fn record_address(&mut self, peer: PeerId, addr: Multiaddr) {
        if let Some(normalized) = normalize_multiaddr(addr) {
            let queue = self.active.entry(peer).or_default();
            push_unique(queue, normalized);
        }
    }

    fn reserve_addresses(&mut self, peer: PeerId, addrs: &[Multiaddr]) -> Vec<Multiaddr> {
        let mut reserved = Vec::new();
        for addr in addrs {
            if let Some(normalized) = normalize_multiaddr(addr.clone()) {
                if self.is_tracking(peer, &normalized) {
                    continue;
                }
                let queue = self.pending.entry(peer).or_default();
                push_unique(queue, normalized.clone());
                reserved.push(normalized);
            }
        }
        reserved
    }

    fn confirm_addresses(&mut self, peer: PeerId, addrs: &[Multiaddr]) {
        if addrs.is_empty() {
            return;
        }

        let mut to_activate: Vec<Multiaddr> = Vec::new();
        if let Some(queue) = self.pending.get_mut(&peer) {
            for addr in addrs {
                if let Some(pos) = queue.iter().position(|existing| existing == addr) {
                    if let Some(removed) = queue.remove(pos) {
                        to_activate.push(removed);
                    }
                }
            }
            if queue.is_empty() {
                self.pending.remove(&peer);
            }
        }

        if to_activate.is_empty() {
            to_activate = addrs.to_vec();
        }

        let active_queue = self.active.entry(peer).or_default();
        for addr in to_activate {
            push_unique(active_queue, addr);
        }
    }

    fn release_pending(&mut self, peer: PeerId, addrs: &[Multiaddr]) {
        if let Some(queue) = self.pending.get_mut(&peer) {
            for addr in addrs {
                if let Some(pos) = queue.iter().position(|existing| existing == addr) {
                    queue.remove(pos);
                }
            }
            if queue.is_empty() {
                self.pending.remove(&peer);
            }
        }
    }

    fn relay_address_count(&self) -> usize {
        self.active.values().flat_map(|queue| queue.iter()).filter(|addr| addr_uses_relay(addr)).count()
            + self.pending.values().flat_map(|queue| queue.iter()).filter(|addr| addr_uses_relay(addr)).count()
    }

    fn relay_address_count_for(&self, peer: PeerId) -> usize {
        let active = self.active.get(&peer).map(|queue| queue.iter().filter(|addr| addr_uses_relay(addr)).count()).unwrap_or(0);
        let pending = self.pending.get(&peer).map(|queue| queue.iter().filter(|addr| addr_uses_relay(addr)).count()).unwrap_or(0);
        active + pending
    }

    fn is_tracking(&self, peer: PeerId, addr: &Multiaddr) -> bool {
        self.active.get(&peer).map_or(false, |queue| queue.contains(addr))
            || self.pending.get(&peer).map_or(false, |queue| queue.contains(addr))
    }

    fn remove_address(&mut self, peer: PeerId, addr: &Multiaddr) {
        if let Some(normalized) = normalize_multiaddr(addr.clone()) {
            if let Some(queue) = self.active.get_mut(&peer) {
                if let Some(pos) = queue.iter().position(|existing| existing == &normalized) {
                    queue.remove(pos);
                }
                if queue.is_empty() {
                    self.active.remove(&peer);
                }
            }
        }
    }

    fn info_for(&self, peer: PeerId) -> Libp2pConnectInfo {
        if let Some(queue) = self.active.get(&peer) {
            if let Some(addr) = queue.back() {
                return build_connect_info_with_override(peer, addr.clone(), self.relay_overrides.contains(&peer));
            }
        }
        if let Some(queue) = self.pending.get(&peer) {
            if let Some(addr) = queue.back() {
                return build_connect_info_with_override(peer, addr.clone(), self.relay_overrides.contains(&peer));
            }
        }
        let mut info = Libp2pConnectInfo::new(peer);
        if self.relay_overrides.contains(&peer) {
            info.relay_used = true;
        }
        info
    }

    fn mark_relay_override(&mut self, peer: PeerId) {
        self.relay_overrides.insert(peer);
    }
}

fn push_unique(queue: &mut VecDeque<Multiaddr>, addr: Multiaddr) {
    if let Some(pos) = queue.iter().position(|existing| existing == &addr) {
        queue.remove(pos);
    }
    queue.push_back(addr);
}

fn normalize_multiaddr(mut addr: Multiaddr) -> Option<Multiaddr> {
    if addr.iter().last().map_or(false, |p| matches!(p, Protocol::P2p(_))) {
        addr.pop();
    }
    if addr.is_empty() {
        None
    } else {
        Some(addr)
    }
}

fn build_connect_info_with_override(peer: PeerId, addr: Multiaddr, force_relay: bool) -> Libp2pConnectInfo {
    let relay_used = addr_uses_relay(&addr);
    let mut full_addr = addr;
    full_addr.push(Protocol::P2p(peer));
    let mut info = Libp2pConnectInfo::with_address(peer, full_addr, relay_used);
    if force_relay {
        info.relay_used = true;
    }
    info
}
fn handle_swarm_event(event: SwarmEvent<BridgeBehaviourEvent>, peer_book: &mut PeerBook) {
    match event {
        SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
            debug!("Swarm connection established peer={} endpoint={:?}", peer_id, endpoint);
            if let Some(addr) = endpoint_multiaddr(&endpoint) {
                peer_book.record_address(peer_id, addr);
            }
        }
        SwarmEvent::ConnectionClosed { peer_id, endpoint, cause, .. } => {
            let mut logged = false;
            if let Some(addr) = endpoint_multiaddr(&endpoint) {
                if addr_uses_relay(&addr) {
                    info!("Swarm relay connection closed peer={} addr={} cause={:?}", peer_id, addr, cause);
                    logged = true;
                }
                peer_book.remove_address(peer_id, &addr);
            }
            if !logged {
                debug!("Swarm connection closed peer={} endpoint={:?} cause={:?}", peer_id, endpoint, cause);
            }
        }
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            debug!("Outgoing connection error peer_id={:?} error={}", peer_id, error);
        }
        SwarmEvent::IncomingConnectionError { error, .. } => {
            debug!("Incoming connection error error={}", error);
        }
        SwarmEvent::Behaviour(BridgeBehaviourEvent::Identify(event)) => match event {
            identify::Event::Received { peer_id, ref info, .. } => {
                info!("[IDENTIFY] received from {}: protocols={:?} addrs={:?}", peer_id, info.protocols, info.listen_addrs);
                for addr in &info.listen_addrs {
                    peer_book.record_address(peer_id, addr.clone());
                }
            }
            identify::Event::Pushed { peer_id, ref info, .. } => {
                info!("[IDENTIFY] pushed to {}: protocols={:?} addrs={:?}", peer_id, info.protocols, info.listen_addrs);
            }
            other => {
                debug!("[IDENTIFY] behaviour event event={:?}", other);
            }
        },
        SwarmEvent::Behaviour(BridgeBehaviourEvent::RelayClient(event)) => {
            use libp2p::relay::client::Event;
            debug!("Relay client behaviour event event={:?}", event);
            #[allow(unreachable_patterns)]
            match event {
                Event::ReservationReqAccepted { relay_peer_id, renewal, .. } => {
                    if renewal {
                        info!("libp2p relay reservation renewed relay_peer_id={}", relay_peer_id);
                    } else {
                        info!("libp2p relay reservation accepted relay_peer_id={}", relay_peer_id);
                    }
                }
                Event::OutboundCircuitEstablished { relay_peer_id, limit } => {
                    info!("Outbound circuit established via relay relay_peer_id={} limit={:?}", relay_peer_id, limit);
                }
                Event::InboundCircuitEstablished { src_peer_id, limit } => {
                    peer_book.mark_relay_override(src_peer_id);
                    info!("Inbound circuit established src_peer_id={} limit={:?}", src_peer_id, limit);
                }
                other => {
                    warn!("Unhandled relay client event event={:?}", other);
                }
            }
        }
        SwarmEvent::Behaviour(BridgeBehaviourEvent::RelayServer(event)) => {
            use libp2p::relay::Event;
            match event {
                Event::ReservationReqAccepted { src_peer_id, renewed } => {
                    if renewed {
                        info!("Relay server: reservation renewed src_peer_id={}", src_peer_id);
                    } else {
                        info!("Relay server: reservation accepted src_peer_id={}", src_peer_id);
                    }
                }
                #[allow(deprecated)]
                Event::ReservationReqAcceptFailed { src_peer_id, error } => {
                    warn!("Relay server: reservation accept failed src_peer_id={} error={:?}", src_peer_id, error);
                }
                Event::ReservationReqDenied { src_peer_id, status } => {
                    warn!("Relay server: reservation request denied src_peer_id={} status={:?}", src_peer_id, status);
                }
                #[allow(deprecated)]
                Event::ReservationReqDenyFailed { src_peer_id, error } => {
                    debug!("Relay server: reservation deny failed src_peer_id={} error={:?}", src_peer_id, error);
                }
                Event::ReservationClosed { src_peer_id } => {
                    warn!("Relay server: reservation closed src_peer_id={}", src_peer_id);
                }
                Event::ReservationTimedOut { src_peer_id } => {
                    warn!("Relay server: reservation timed out src_peer_id={}", src_peer_id);
                }
                Event::CircuitReqAccepted { src_peer_id, dst_peer_id } => {
                    info!("Relay server: circuit request accepted src_peer_id={} dst_peer_id={}", src_peer_id, dst_peer_id);
                }
                Event::CircuitReqDenied { src_peer_id, dst_peer_id, status } => {
                    warn!(
                        "Relay server: circuit request denied src_peer_id={} dst_peer_id={} status={:?}",
                        src_peer_id, dst_peer_id, status
                    );
                }
                #[allow(deprecated)]
                Event::CircuitReqOutboundConnectFailed { src_peer_id, dst_peer_id, error } => {
                    warn!(
                        "Relay server: circuit outbound connect failed src_peer_id={} dst_peer_id={} error={:?}",
                        src_peer_id, dst_peer_id, error
                    );
                }
                #[allow(deprecated)]
                Event::CircuitReqAcceptFailed { src_peer_id, dst_peer_id, error } => {
                    debug!(
                        "Relay server: circuit accept failed src_peer_id={} dst_peer_id={} error={:?}",
                        src_peer_id, dst_peer_id, error
                    );
                }
                #[allow(deprecated)]
                Event::CircuitReqDenyFailed { src_peer_id, dst_peer_id, error } => {
                    debug!(
                        "Relay server: circuit deny failed src_peer_id={} dst_peer_id={} error={:?}",
                        src_peer_id, dst_peer_id, error
                    );
                }
                Event::CircuitClosed { src_peer_id, dst_peer_id, error } => {
                    if let Some(err) = error {
                        info!(
                            "Relay server: circuit closed with error src_peer_id={} dst_peer_id={} err={:?}",
                            src_peer_id, dst_peer_id, err
                        );
                    } else {
                        info!("Relay server: circuit closed src_peer_id={} dst_peer_id={}", src_peer_id, dst_peer_id);
                    }
                }
            }
        }
        SwarmEvent::Behaviour(BridgeBehaviourEvent::Dcutr(event)) => {
            info!("DCUtR event: {:?}", event);
        }
        SwarmEvent::ListenerClosed { addresses, reason, .. } => {
            warn!("Swarm listener closed addresses={:?} reason={:?}", addresses, reason);
        }
        SwarmEvent::ListenerError { error, .. } => {
            warn!("Swarm listener error error={}", error);
        }
        SwarmEvent::NewExternalAddrCandidate { address } => {
            debug!("New external addr candidate {}", address);
        }
        SwarmEvent::ExternalAddrConfirmed { address } => {
            debug!("External addr confirmed {}", address);
        }
        _ => {}
    }
}

fn map_stream_error(error: OpenStreamError) -> BridgeError {
    match error {
        OpenStreamError::UnsupportedProtocol(proto) => BridgeError::DialFailed(format!("stream rejected protocol {proto}")),
        OpenStreamError::Io(err) => BridgeError::DialFailed(err.to_string()),
        _ => BridgeError::DialFailed(format!("stream error: {error:?}")),
    }
}

fn stream_protocol() -> StreamProtocol {
    StreamProtocol::new(BRIDGE_PROTOCOL_ID)
}

fn endpoint_multiaddr(endpoint: &ConnectedPoint) -> Option<Multiaddr> {
    match endpoint {
        ConnectedPoint::Dialer { address, .. } => Some(address.clone()),
        ConnectedPoint::Listener { send_back_addr, .. } => Some(send_back_addr.clone()),
    }
}

fn addr_uses_relay(addr: &Multiaddr) -> bool {
    addr.iter().any(|component| matches!(component, Protocol::P2pCircuit))
}

#[derive(Debug)]
enum BridgeBehaviourEvent {
    Stream,
    Identify(identify::Event),
    Ping,
    RelayClient(relay::client::Event),
    RelayServer(relay::Event),
    Dcutr(dcutr::Event),
}

impl From<()> for BridgeBehaviourEvent {
    fn from(_: ()) -> Self {
        Self::Stream
    }
}

impl From<identify::Event> for BridgeBehaviourEvent {
    fn from(value: identify::Event) -> Self {
        Self::Identify(value)
    }
}

impl From<libp2p::ping::Event> for BridgeBehaviourEvent {
    fn from(value: libp2p::ping::Event) -> Self {
        let _ = value;
        Self::Ping
    }
}

impl From<relay::client::Event> for BridgeBehaviourEvent {
    fn from(value: relay::client::Event) -> Self {
        Self::RelayClient(value)
    }
}

impl From<relay::Event> for BridgeBehaviourEvent {
    fn from(value: relay::Event) -> Self {
        Self::RelayServer(value)
    }
}

impl From<dcutr::Event> for BridgeBehaviourEvent {
    fn from(value: dcutr::Event) -> Self {
        Self::Dcutr(value)
    }
}
