# TCP Hole Punch – Production Integration Plan

_This document picks up after the Phase 6 PoC plan (see `plan.md`). It focuses on upstreaming the bridge, enforcing the policies Michael requested, and hardening the feature for general use._

## Phase 7 – Bridge Integration & Role Detection
- Embed the libp2p bridge service inside `kaspad`, including clean shutdown, logging, and hooks for feeding incoming streams to the existing adaptor.
- Introduce `--libp2p-relay-port` and `--libp2p-relay-mode={auto,on,off}`. Default to `auto`, which starts the relay only when `AddressManager::local_net_addresses` is non-empty (as determined today via `--externalip`, routable `--listen`, or UPnP).
- Surface the node’s current role (public relay vs. private-only) via RPC/logs so operators can verify the auto-detection.

## Phase 8 – Connection Policy & Private Caps
- Extend the connection manager to maintain separate budgets for direct public peers vs. hole-punched private peers; when dialing private peers, request relayed streams through the bridge while preferring direct TCP for public nodes.
- Enforce a “private node” inbound cap near the current 8-peer norm (configurable) plus per-relay quotas so a single public host cannot flood a home node. Public nodes retain the higher inbound limit but reserve capacity for relay-mediated traffic.
- Add lightweight local telemetry/RPC (e.g., `getLibpStatus`) that reports active relay sessions, punch success/fail counts, and current inbound cap usage—restricted to the local operator to avoid network-wide leakage.

## Phase 9 – Usability & Telemetry Polish
- Document the new libp2p flags (`--libp2p-relay-mode`, `--libp2p-private-inbound-target`, etc.) in the operator runbook and README so the bridge rollout is self-serve.
- Add integration/unit tests around the inbound-cap logic (Libp2pLimits) to ensure future changes don’t regress quota enforcement.
- Expose a lightweight RPC/CLI (`getLibpStatus`) and metrics counters so operators and dashboards can inspect relay role, peer id, and current inbound usage without parsing `getServerInfo`.
- Add targeted logging/alerts when the bridge service fails or when libp2p caps trigger disconnects, making troubleshooting easier during the rollout phase.

## Phase 10 – Relay Capability Metadata (Protocol-Native)
- Extend `Version.services` (and associated protowire structs) with a relay capability bit plus an advertised relay port, keeping the wire format backwards-compatible (e.g., bump version fields, default new bits to zero).
- Update `AddressManager` schemas (RocksDB column family and `NetAddress` serialization) to persist the new capability bit and port for each peer; write a migration that gracefully upgrades existing stores.
- Populate the capability flag during handshake: public nodes set the bit when their `--libp2p-relay-mode` exposes a relay listener, private nodes leave it unset.
- Teach `address` flows (`RequestAddresses`, senders) to include the capability flag so the gossip-derived peer set knows which nodes can act as relays.
- Update `ConnectionManager` selection logic to consume the new metadata: maintain a pool of relay-capable peers, ensure each private outbound connection chooses a distinct relay, and retry/fallback when a relay proves unhealthy.
- Add RPC/CLI visibility (e.g., include the relay capability in `getPeerAddresses` or expose a filtered list) so operators can audit which peers are advertising relays.
- Document upgrade and compatibility notes: mixed-version behavior, expected migrations, and how operators can verify their nodes are advertising/consuming the capability.

## Open Questions / Dependencies
- Final decision on the relay metadata storage strategy (Phase 9 blocker).
- Exact defaults for the private inbound cap, per-relay quotas, and how they interact with existing flags like `--connect`/`--addpeer`.
- File-descriptor budget impacts when every public node also runs a libp2p relay (may require updating the current FD allocation in `kaspad`).

Once the metadata decision is settled, we can expand this plan with concrete tasks/migrations and start wiring the automatic relay rotation. Until then, Phases 7–8 deliver the runtime integration, role controls, and safety limits Michael highlighted without depending on the storage choice.
