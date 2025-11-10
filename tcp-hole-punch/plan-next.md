# TCP Hole Punch – Production Integration Plan

_This document picks up after the Phase 6 PoC plan (see `plan.md`). It focuses on upstreaming the bridge, enforcing the policies Michael requested, and hardening the feature for general use._

## Phase 7 – Bridge Integration & Role Detection
- Embed the libp2p bridge service inside `kaspad`, including clean shutdown, logging, and hooks for feeding incoming streams to the existing adaptor.
- Introduce `--libp2p-relay-port` and `--libp2p-relay-mode={auto,on,off}`. Default to `auto`, which starts the relay only when `AddressManager::local_net_addresses` is non-empty (as determined today via `--externalip`, routable `--listen`, or UPnP).
- Surface the node’s current role (public relay vs. private-only) via RPC/logs so operators can verify the auto-detection.

## Phase 8 – Connection Policy & Private Caps
- Extend the connection manager to maintain separate budgets for direct public peers vs. hole-punched private peers; when dialing private peers, request relayed streams through the bridge while preferring direct TCP for public nodes.
- Enforce a “private node” inbound cap near the current 8-peer norm (configurable) plus per-relay quotas so a single public host cannot flood a home node. Public nodes retain the higher inbound limit but reserve capacity for relay-mediated traffic.
- Add lightweight local telemetry/RPC (e.g., `getLibp2pStatus`) that reports active relay sessions, punch success/fail counts, and current inbound cap usage—restricted to the local operator to avoid network-wide leakage.

## Phase 9 – Relay Capability Metadata (Awaiting Design Decision)
- Decide how relay capability is shared:  
  1. **Extend `Version.services` / address records** to advertise a relay bit + port (requires protowire + RocksDB changes, but keeps discovery decentralized).  
  2. **Sidecar relay registry** keyed by `NetAddress` (no wire/schema change, but needs a strategy to disseminate relay info without centralization).
- Implement the chosen approach, migrate existing stores if needed, and update the connection manager to rotate through the discovered relay set so each private connection uses a different public relay, as per Michael’s anti-eclipse guidance.

## Open Questions / Dependencies
- Final decision on the relay metadata storage strategy (Phase 9 blocker).
- Exact defaults for the private inbound cap, per-relay quotas, and how they interact with existing flags like `--connect`/`--addpeer`.
- File-descriptor budget impacts when every public node also runs a libp2p relay (may require updating the current FD allocation in `kaspad`).

Once the metadata decision is settled, we can expand this plan with concrete tasks/migrations and start wiring the automatic relay rotation. Until then, Phases 7–8 deliver the runtime integration, role controls, and safety limits Michael highlighted without depending on the storage choice.
