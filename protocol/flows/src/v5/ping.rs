use crate::{flow_context::FlowContext, flow_trait::Flow};
use kaspa_core::{debug, task::tick::TickReason};
use kaspa_p2p_lib::{
    common::ProtocolError,
    dequeue, dequeue_with_timeout, make_message,
    pb::{kaspad_message::Payload, PingMessage, PongMessage},
    IncomingRoute, Router,
};
use rand::Rng;
use std::{
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

/// Flow for managing a loop receiving pings and responding with pongs
pub struct ReceivePingsFlow {
    _ctx: FlowContext,
    router: Arc<Router>,
    incoming_route: IncomingRoute,
}

#[async_trait::async_trait]
impl Flow for ReceivePingsFlow {
    fn router(&self) -> Option<Arc<Router>> {
        Some(self.router.clone())
    }

    async fn start(&mut self) -> Result<(), ProtocolError> {
        self.start_impl().await
    }
}

impl ReceivePingsFlow {
    pub fn new(ctx: FlowContext, router: Arc<Router>, incoming_route: IncomingRoute) -> Self {
        Self { _ctx: ctx, router, incoming_route }
    }

    async fn start_impl(&mut self) -> Result<(), ProtocolError> {
        loop {
            // We dequeue without a timeout in this case, responding to pings whenever they arrive
            let ping = dequeue!(self.incoming_route, Payload::Ping)?;
            debug!("P2P Flows, got ping request with nonce {}", ping.nonce);
            let pong = make_message!(Payload::Pong, PongMessage { nonce: ping.nonce });
            self.router.enqueue(pong).await?;
        }
    }
}

pub const PING_INTERVAL: Duration = Duration::from_secs(120); // 2 minutes
const LIBP2P_FAST_PING_INTERVAL: Duration = Duration::from_secs(5);

/// Flow for managing a loop sending pings and waiting for pongs
pub struct SendPingsFlow {
    ctx: FlowContext,

    // We use a weak reference to avoid this flow from holding the router during timer waiting if the connection was closed
    router: Weak<Router>,
    peer: String,
    incoming_route: IncomingRoute,
    ping_interval: Duration,
    initial_delay: Duration,
}

#[async_trait::async_trait]
impl Flow for SendPingsFlow {
    fn router(&self) -> Option<Arc<Router>> {
        self.router.upgrade()
    }

    async fn start(&mut self) -> Result<(), ProtocolError> {
        self.start_impl().await
    }
}

impl SendPingsFlow {
    pub fn new(ctx: FlowContext, router: Arc<Router>, incoming_route: IncomingRoute) -> Self {
        let peer = router.to_string();
        let (initial_delay, ping_interval) = determine_ping_schedule(&router);
        if initial_delay.as_secs() == 0 && ping_interval == LIBP2P_FAST_PING_INTERVAL {
            debug!("P2P Flows, enabling fast ping cadence for relay-backed peer {peer}");
        }
        Self { ctx, router: Arc::downgrade(&router), peer, incoming_route, ping_interval, initial_delay }
    }

    async fn start_impl(&mut self) -> Result<(), ProtocolError> {
        let mut first = true;
        loop {
            // Wait for the next ping window (first ping may be immediate for relayed libp2p peers)
            let wait = if first { self.initial_delay } else { self.ping_interval };
            first = false;
            if !wait.is_zero() {
                if let TickReason::Shutdown = self.ctx.tick_service.tick(wait).await {
                    return Ok(());
                }
            }

            // Create a fresh random nonce for each ping
            let nonce = rand::thread_rng().gen::<u64>();
            let ping = make_message!(Payload::Ping, PingMessage { nonce });
            let ping_time = Instant::now();
            let Some(router) = self.router.upgrade() else {
                return Err(ProtocolError::ConnectionClosed);
            };
            router.enqueue(ping).await?;
            let pong = dequeue_with_timeout!(self.incoming_route, Payload::Pong)?;
            if pong.nonce != nonce {
                return Err(ProtocolError::Other("nonce mismatch between ping and pong"));
            } else {
                debug!("Successful ping with peer {} (nonce: {})", self.peer, pong.nonce);
            }
            router.set_last_ping_duration(ping_time.elapsed().as_millis() as u64);
        }
    }
}

fn determine_ping_schedule(router: &Router) -> (Duration, Duration) {
    if router.connection_metadata().and_then(|metadata| metadata.libp2p.as_ref()).map_or(false, |info| info.relay_used) {
        (Duration::from_secs(0), LIBP2P_FAST_PING_INTERVAL)
    } else {
        (PING_INTERVAL, PING_INTERVAL)
    }
}
