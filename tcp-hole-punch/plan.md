# TCP Hole Punch Integration Plan

## Progress Snapshot
- ✅ **Phase 1 – Baseline hole punching**  
  - Successful direct punch: see `TEST_SCENARIOS.md` (VPS dialer scenario) and raw logs in `logs/dialer_vps_scenario.log`.  
  - Negative case (expected relay fallback): documented in `logs/hotspot-scenario-summary.md`.
- 🔄 **Phase 2 – Build the libp2p ⇄ tonic bridge** (in progress via `bridge/` crate).
- ⏳ **Phase 3 – Extend the Kaspa adaptor/router**.
- ⏳ **Phase 4 – Two-private-peer PoC**.

## Phase 1 – Reproduce Baseline Hole Punching
- ✅ Relay, listener, and dialer binaries exercised.  
  - Positive case (VPS dialer ↔ home listener) produced a direct TCP channel – evidence in `logs/dialer_vps_scenario.log` and summary in `TEST_SCENARIOS.md`.  
  - Negative case (hotspot dialer) remained on the relay, captured in `logs/hotspot-scenario-summary.md` for reference.
- Timing, multiaddrs, and Swarm events recorded to guide Phase 2 expectations (`logs/phase1-summary.md`).

## Phase 2 – Build the libp2p ⇄ tonic Bridge
- Architecture notes captured in `design/phase2-architecture.md`.
- Libp2p swarm actor + stream wrapper implemented under `bridge/src/` with `libp2p-stream` control, tonic client connector, and an instrumented command channel wrapper for lifecycle/debug visibility.
- Helper `incoming_from_handle` added so callers can detach the single pending incoming receiver safely; `tests/integration.rs` covers the helper and shutdown path (5s timeout guard, verified under `cargo test -- --nocapture`).
- Current TODOs (handover): wire an end-to-end tonic server test using `Libp2pIncoming`, expand connector error coverage (missing/bad multiaddrs, relay fallback), and produce usage docs/README + final plan update before marking Phase 2 complete.

## Phase 3 – Extend the Kaspa Adaptor/Router
- Add an adaptor path that consumes an owned `AsyncRead + AsyncWrite + Send + 'static` stream instead of dialing by URI, threading the optional `SocketAddr` through `ConnectionHandler::connect`/`Router::new`.
- Ensure the handshake initialisers (`echo.rs`, `flow_context.rs`) continue to run unchanged, while the new path can inject libp2p metadata (PeerId, relay status) for policy hooks.
- Verify logging, `PeerKey`, and identity management behave sanely when the address is synthesized or changes between sessions.

## Phase 4 – Deliver the Two-Private-Peer PoC
- Modify `protocol/p2p/src/bin/{server.rs,client.rs}` to source their transport from the libp2p bridge while keeping the Kaspa handshake and flow registration intact.
- Script the three-node test (public relay + two NATed Kaspa binaries) to demonstrate sustained Kaspa gossip over a hole-punched TCP connection, recording fallback behaviour (relay persistence, QUIC attempts, retry cadence).
- Document operational knobs and open policy items: relay inventory, connection/relay limits, PeerId↔PeerKey unification, and peer-store schema updates for storing Multiaddrs/observed addresses.
