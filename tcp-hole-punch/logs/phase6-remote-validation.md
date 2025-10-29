# Phase 6 – Hardened Remote Validation (2025-10-29)

Repeats the mixed-NAT scenario using the Phase 5 hardening changes. The relay runs on the Hetzner VPS (`<RELAY_IP>`), the Kaspa server on the home workstation (`<HOME_IP>` behind NAT), and the Kaspa client on the Vultr VPS (`<CLIENT_VPS_IP>`). Both binaries are built with `--release` and `--features libp2p-bridge`.

## Environment Snapshot

| Role  | Host | Key commands |
| --- | --- | --- |
| Relay | `phase6_relay` tmux → Hetzner VPS | `cd /root/rusty-kaspa/tcp-hole-punch/relay && cargo build --release`<br>`./control_relay_service.sh stop && ./control_relay_service.sh start` |
| Server | Local workstation | `LIBP2P_LISTEN_MULTIADDRS="/ip4/0.0.0.0/tcp/4012,/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY/p2p-circuit"`<br>`LIBP2P_RELAY_MULTIADDRS="/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY"`<br>`cargo run --release -p kaspa-p2p-lib --bin kaspa_p2p_server --features libp2p-bridge` |
| Client | `phase6_vultr` tmux → Vultr VPS | `LIBP2P_REMOTE_PEER_ID=12D3KooWR7QpfbpcFBJH32w3tqcY6erAg4xgZsN9QYmGBhw7vUBC`<br>`LIBP2P_REMOTE_MULTIADDRS="/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY/p2p-circuit"`<br>`cargo run --release --bin kaspa_p2p_client --features libp2p-bridge` |

## Timeline & Evidence

1. **Relay reservation established** – server log records the manual reservation immediately after startup (`phase6-server-session.log:188`).
2. **Client dial succeeds over the relay circuit** – client log shows `Client connected via libp2p … relay=true` once the reservation is in place (`phase6-client-session.log:251`).
3. **Kaspa handshake completes** – server accepts the libp2p stream, synthesises a deterministic IPv6 socket, and reaches `Ready` (`phase6-server-session.log:351` and `402-403`).
4. **Relay confirms circuit creation** – relay log reports `CircuitReqAccepted` for the source/destination peer IDs after the successful punch (`phase6-relay-session.log:295`).

## Captured Logs

- `tcp-hole-punch/logs/phase6-server-session.log` – local server session (sanitised IPs).
- `tcp-hole-punch/logs/phase6-client-session.log` – Vultr client session (sanitised IPs and metadata).
- `tcp-hole-punch/logs/phase6-relay-session.log` – relay excerpts covering the Phase 6 run.

These replace the Phase 4 evidence in the rollout checklist and demonstrate that the hardened bridge (Phase 5) operates correctly across the real mixed-NAT topology.
