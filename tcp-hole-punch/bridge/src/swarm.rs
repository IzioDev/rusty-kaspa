use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use futures::{
    channel::{mpsc, oneshot},
    future::FutureExt,
    sink::Sink,
    stream::FuturesUnordered,
    SinkExt, StreamExt,
};
use libp2p::{
    core::connection::ConnectedPoint,
    dcutr, identify,
    multiaddr::Protocol,
    noise, relay,
    swarm::{NetworkBehaviour, StreamProtocol, Swarm, SwarmEvent},
    tls, yamux, Multiaddr, PeerId, SwarmBuilder,
};
use libp2p_stream::{self as lpstream, AlreadyRegistered, OpenStreamError};
use std::task::{Context, Poll};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::{
    stream::{Libp2pConnectInfo, Libp2pStreamHandle},
    BridgeError, Result,
};

const BRIDGE_PROTOCOL_ID: &str = "/kaspa/p2p/bridge/1.0";

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
    ListenOn { addr: Multiaddr },
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
    relay: relay::client::Behaviour,
    identify: identify::Behaviour,
    ping: libp2p::ping::Behaviour,
    dcutr: dcutr::Behaviour,
    stream: lpstream::Behaviour,
}

/// Spawn the libp2p swarm actor and return a handle for issuing commands.
pub fn spawn_swarm(local_key: libp2p::identity::Keypair) -> Result<SwarmHandle> {
    let peer_id_label = local_key.public().to_peer_id();
    let (raw_sender, mut command_rx) = mpsc::channel::<SwarmCommand>(32);
    let (incoming_tx, incoming_rx) = mpsc::channel::<Libp2pStreamHandle>(32);

    let command_tx = SwarmCommandSender::new(format!("swarm-{peer_id_label}"), raw_sender);

    let mut swarm = build_swarm(local_key.clone())?;
    let mut control = swarm.behaviour_mut().stream.new_control();
    let mut incoming_streams = control.accept(stream_protocol()).map_err(|e| match e {
        AlreadyRegistered => BridgeError::DialFailed("protocol already registered".into()),
    })?;
    let dial_control = control.clone();
    drop(control);

    let join_handle = tokio::spawn(async move {
        let mut peer_book = PeerBook::default();
        let mut incoming_tx = incoming_tx;
        let mut pending_dials = FuturesUnordered::new();
        loop {
            tokio::select! {
                cmd = command_rx.next() => {
                    match cmd {
                        Some(SwarmCommand::Dial { peer, addrs, response }) => {
                            let mut control_clone = dial_control.clone();
                            for addr in &addrs {
                                swarm.add_peer_address(peer, addr.clone());
                            }
                            if !addrs.is_empty() {
                                peer_book.record_addresses(peer, addrs.clone());
                            }
                            pending_dials.push(async move {
                                let result = control_clone.open_stream(peer, stream_protocol()).await.map_err(map_stream_error);
                                (peer, response, result)
                            }.boxed());
                        }
                        Some(SwarmCommand::ListenOn { addr }) => {
                            if let Err(err) = swarm.listen_on(addr) {
                                error!(%err, "failed to listen");
                            }
                        }
                        Some(SwarmCommand::Shutdown) => break,
                        None => break,
                    }
                }
                dial_result = pending_dials.next() => {
                    if let Some((peer, response, outcome)) = dial_result {
                        match outcome {
                            Ok(stream) => {
                                let info = peer_book.info_for(peer);
                                let handle = Libp2pStreamHandle::new(stream, info);
                                if response.send(Ok(handle)).is_err() {
                                    debug!(%peer, "dial response channel dropped");
                                }
                            }
                            Err(err) => {
                                if response.send(Err(err)).is_err() {
                                    debug!(%peer, "dial response channel dropped (error)");
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
                            if incoming_tx.try_send(handle).is_err() {
                                warn!(%peer_id, "incoming queue full; dropping inbound stream");
                            }
                        }
                        None => break,
                    }
                }
                swarm_event = swarm.select_next_some() => {
                    handle_swarm_event(swarm_event, &mut peer_book);
                }
            }
        }
    });

    Ok(SwarmHandle { command_tx, incoming_rx: Some(incoming_rx), join_handle, peer_id: local_key.public().to_peer_id() })
}

fn build_swarm(local_key: libp2p::identity::Keypair) -> Result<Swarm<BridgeBehaviour>> {
    let builder = SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(Default::default(), (tls::Config::new, noise::Config::new), yamux::Config::default)
        .map_err(|e| BridgeError::DialFailed(format!("tcp transport init failed: {e}")))?;

    let builder = builder
        .with_relay_client((tls::Config::new, noise::Config::new), yamux::Config::default)
        .map_err(|e| BridgeError::DialFailed(format!("relay client init failed: {e}")))?;

    let builder = builder
        .with_behaviour(|key, relay| {
            let public = key.public();
            let peer_id = public.to_peer_id();
            BridgeBehaviour {
                relay,
                identify: identify::Behaviour::new(identify::Config::new("/kaspa/0.1.0".into(), public)),
                ping: libp2p::ping::Behaviour::new(libp2p::ping::Config::new()),
                dcutr: dcutr::Behaviour::new(peer_id),
                stream: lpstream::Behaviour::default(),
            }
        })
        .map_err(|e| BridgeError::DialFailed(format!("behaviour init failed: {e}")))?;

    Ok(builder.build())
}

#[derive(Default)]
struct PeerBook {
    addresses: HashMap<PeerId, HashSet<Multiaddr>>,
}

impl PeerBook {
    fn record_address(&mut self, peer: PeerId, addr: Multiaddr) {
        self.addresses.entry(peer).or_default().insert(addr);
    }

    fn record_addresses(&mut self, peer: PeerId, addrs: Vec<Multiaddr>) {
        for addr in addrs {
            self.record_address(peer, addr);
        }
    }

    fn remove_address(&mut self, peer: PeerId, addr: &Multiaddr) {
        if let Some(set) = self.addresses.get_mut(&peer) {
            set.remove(addr);
            if set.is_empty() {
                self.addresses.remove(&peer);
            }
        }
    }

    fn info_for(&self, peer: PeerId) -> Libp2pConnectInfo {
        if let Some(set) = self.addresses.get(&peer) {
            if let Some(addr) = set.iter().next().cloned() {
                let relay_used = addr_uses_relay(&addr);
                return Libp2pConnectInfo::with_address(peer, addr, relay_used);
            }
        }
        Libp2pConnectInfo::new(peer)
    }
}

fn handle_swarm_event(event: SwarmEvent<BridgeBehaviourEvent>, peer_book: &mut PeerBook) {
    match event {
        SwarmEvent::NewListenAddr { address, .. } => {
            info!(%address, "Swarm listening");
        }
        SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
            debug!(%peer_id, ?endpoint, "Swarm connection established");
            if let Some(addr) = endpoint_multiaddr(&endpoint) {
                peer_book.record_address(peer_id, addr);
            }
        }
        SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } => {
            debug!(%peer_id, ?endpoint, "Swarm connection closed");
            if let Some(addr) = endpoint_multiaddr(&endpoint) {
                peer_book.remove_address(peer_id, &addr);
            }
        }
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            debug!(?peer_id, %error, "Outgoing connection error");
        }
        SwarmEvent::IncomingConnectionError { error, .. } => {
            debug!(%error, "Incoming connection error");
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

enum BridgeBehaviourEvent {
    Stream,
    Relay,
    Identify,
    Ping,
    Dcutr,
}

impl From<()> for BridgeBehaviourEvent {
    fn from(_: ()) -> Self {
        Self::Stream
    }
}
impl From<relay::client::Event> for BridgeBehaviourEvent {
    fn from(value: relay::client::Event) -> Self {
        let _ = value;
        Self::Relay
    }
}
impl From<identify::Event> for BridgeBehaviourEvent {
    fn from(value: identify::Event) -> Self {
        let _ = value;
        Self::Identify
    }
}
impl From<libp2p::ping::Event> for BridgeBehaviourEvent {
    fn from(value: libp2p::ping::Event) -> Self {
        let _ = value;
        Self::Ping
    }
}
impl From<dcutr::Event> for BridgeBehaviourEvent {
    fn from(value: dcutr::Event) -> Self {
        let _ = value;
        Self::Dcutr
    }
}
