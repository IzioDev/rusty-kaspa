# TCP Hole Punch Integration Plan

## Progress Snapshot
- ✅ **Phase 1 – Baseline hole punching**  
  - Successful direct punch: see `TEST_SCENARIOS.md` (VPS dialer scenario) and raw logs in `logs/dialer_vps_scenario.log`.  
  - Negative case (expected relay fallback): documented in `logs/hotspot-scenario-summary.md`.
- ⏳ **Phase 2 – Build the libp2p ⇄ tonic bridge** (next up).
- ⏳ **Phase 3 – Extend the Kaspa adaptor/router**.
- ⏳ **Phase 4 – Two-private-peer PoC**.

## Phase 1 – Reproduce Baseline Hole Punching
- ✅ Relay, listener, and dialer binaries exercised.  
  - Positive case (VPS dialer ↔ home listener) produced a direct TCP channel – evidence in `logs/dialer_vps_scenario.log` and summary in `TEST_SCENARIOS.md`.  
  - Negative case (hotspot dialer) remained on the relay, captured in `logs/hotspot-scenario-summary.md` for reference.
- Timing, multiaddrs, and Swarm events recorded to guide Phase 2 expectations (`logs/phase1-summary.md`).

## Phase 2 – Build the libp2p ⇄ tonic Bridge
- Implement a libp2p Swarm “actor” task (Tokio loop + command channel) configured with TCP/Noise/Yamux, QUIC, AutoNAT, Relay v2, and DCUtR behaviours.
- Prototype both `libp2p-stream` and a lightweight custom behaviour (similar to `libp2p-grpc-rs`) to obtain `NegotiatedSubstream`s; keep the option that hands us a `Send + 'static` stream with the least ceremony.
- Wrap the stream in a `Libp2pStreamWrapper` that implements `AsyncRead/Write` and tonic’s `Connected`, synthesising a fallback `SocketAddr` and exposing full `PeerId`/`Multiaddr` metadata via `ConnectInfo`.
- Expose async entry points `accept_stream()` (feeding `Server::serve_with_incoming`) and `dial_stream()` (backed by `Endpoint::connect_with_connector`) to integrate with Kaspa.

## Phase 3 – Extend the Kaspa Adaptor/Router
- Add an adaptor path that consumes an owned `AsyncRead + AsyncWrite + Send + 'static` stream instead of dialing by URI, threading the optional `SocketAddr` through `ConnectionHandler::connect`/`Router::new`.
- Ensure the handshake initialisers (`echo.rs`, `flow_context.rs`) continue to run unchanged, while the new path can inject libp2p metadata (PeerId, relay status) for policy hooks.
- Verify logging, `PeerKey`, and identity management behave sanely when the address is synthesized or changes between sessions.

## Phase 4 – Deliver the Two-Private-Peer PoC
- Modify `protocol/p2p/src/bin/{server.rs,client.rs}` to source their transport from the libp2p bridge while keeping the Kaspa handshake and flow registration intact.
- Script the three-node test (public relay + two NATed Kaspa binaries) to demonstrate sustained Kaspa gossip over a hole-punched TCP connection, recording fallback behaviour (relay persistence, QUIC attempts, retry cadence).
- Document operational knobs and open policy items: relay inventory, connection/relay limits, PeerId↔PeerKey unification, and peer-store schema updates for storing Multiaddrs/observed addresses.
