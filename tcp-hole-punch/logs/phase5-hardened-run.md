# Phase 5 – Hardened Bridge Rehearsal (2025-10-29)

Local three-node replay (relay + private server + private client) using the Phase 5 code paths.  
Relay served from `127.0.0.1:4011` with deterministic peer id `12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY`; server and client binaries ran with `libp2p-bridge` feature enabled.

## Run Notes

- Relay startup and identity: `logs/phase5-relay-session.log:5` confirms listener addresses and the deterministic peer id.
- Server requested a relay reservation and exposed the bridged listener: `logs/phase5-server-session.log:187-188`.
- Inbound libp2p stream lacked a socket, so the handler synthesised a stable IPv6 endpoint and logged it before routing: `logs/phase5-server-session.log:376`.
- Router accepted the libp2p peer and completed the Kaspa Version/Verack/Ready flow twice without duplication: `logs/phase5-server-session.log:487-488`.
- Client dial opened over the relay circuit, exercised the hardened HTTP/2 settings (`max_frame=1 MiB`, etc.), and recorded the libp2p metadata used for metrics: `logs/phase5-client-session.log:184,330`.

All three logs (`phase5-relay-session.log`, `phase5-server-session.log`, `phase5-client-session.log`) are stored alongside this summary for reference.
