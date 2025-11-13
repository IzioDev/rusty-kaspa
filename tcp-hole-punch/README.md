# TCP Hole Punch Workspace

This directory collects everything built around the libp2p/DCUtR proof-of-concept for Kaspa.

| Path | Purpose |
| --- | --- |
| `plan.md` | Phase tracker with status of each milestone and the follow-up work queued after Phase 4. |
| `final-report.md` | End-to-end write‑up covering architecture changes, troubleshooting, the successful remote run, and future recommendations. |
| `final-verification.md` | Concise checklist summarising log evidence, tests, and reproduction steps for the PoC. |
| `design/architecture.md` | Detailed design notes for the bridge layer, stream wrapper, and operational guidance. |
| `logs/` | Sanitised captures from local and remote rehearsals plus the mixed-NAT runbook (`phase4-remote-success.md`). |
| `bridge/` | Libp2p⇄tonic bridge crate sources and tests. |
| `relay/` | Helper crate/scripts for running the standalone relay binary. |
| `scripts/` | Utilities for controlling the remote relay service. |
| `research/` | Scratch notes and reference material gathered during investigation. |
| `research/test-scenarios.md` | Archived Phase 1 libp2p example runs (baseline punch success/failure cases). |

For a quick orientation:
1. Start with `final-report.md` to understand what was built and how the PoC succeeded.
2. Review `design/architecture.md` for implementation specifics.
3. Use the runbook in `logs/phase4-remote-success.md` if you need to replay the mixed-NAT handshake.

## Running the end-to-end VPS validation

The workflow keeps a tmux session per remote host so SSH connections persist:
- Local tmux window `relay_vps` → SSH to the relay
- Local tmux window `client_vps` → SSH to the Vultr client
- Local tmux window `phase6_server` (optional) → run the server locally

### 1. Create local tmux sessions & log in
```bash
tmux new -s relay_vps    # in the first terminal
ssh relay-vps            # inside the session
```
Open a second terminal:
```bash
tmux new -s client_vps
ssh client-vps
```
Back in your main shell, either reuse an existing tmux window or create one (`tmux new -s phase6_server`) for the local server command.

### 2. Sync code to the VPS hosts
```bash
# From the project root
rsync -az --delete --exclude '.git' --exclude 'target' . relay-vps:/root/rusty-kaspa
rsync -az --delete --exclude '.git' --exclude 'target' . client-vps:/root/rusty-kaspa
```

### 3. Rebuild and restart the relay (inside `relay_vps` tmux)
```bash
cd /root/rusty-kaspa/tcp-hole-punch/relay
cargo build --release
cd /root
./control_relay_service.sh stop
./control_relay_service.sh start
```

### 4. Start the Kaspa server (local window `phase6_server`)
```bash
export LIBP2P_LISTEN_MULTIADDRS="/ip4/0.0.0.0/tcp/4012,/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY/p2p-circuit"
export LIBP2P_RELAY_MULTIADDRS="/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY"
cargo run --release -p kaspa-p2p-lib --bin kaspa_p2p_server --features libp2p-bridge \
  2>&1 | tee tcp-hole-punch/logs/phase6-server-session.log
```

### 5. Start the Kaspa client (inside `client_vps` tmux)
```bash
cd /root/rusty-kaspa
export LIBP2P_REMOTE_PEER_ID=12D3KooWR7QpfbpcFBJH32w3tqcY6erAg4xgZsN9QYmGBhw7vUBC
export LIBP2P_REMOTE_MULTIADDRS="/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY/p2p-circuit"
cargo run --release --bin kaspa_p2p_client --features libp2p-bridge \
  2>&1 | tee tcp-hole-punch/logs/phase6-client-session.log
```

Watch for:
- Server log: `Relay reservation requested…` and `accepting incoming libp2p stream…`.
- Client log: `Client connected via libp2p… relay=true`.
- Relay log: `CircuitReqAccepted` for the server/client peer IDs.
The consolidated runbook lives in `tcp-hole-punch/logs/phase6-remote-validation.md`, and the sanitised logs mirror what we committed for the Phase 6 evidence. Continuous replays can follow the same steps; feel free to adapt them into scripts or `just` recipes to shorten the loop further.

## Node Configuration Cheat Sheet

When running a production node, `kaspad` exposes dedicated libp2p flags:

- `--libp2p-relay-mode={auto,on,off}` – defaults to `auto`, which starts the bridge relay only when `AddressManager` reports a public interface (from `--externalip`, routable `--listen`, or UPnP). Use `on` / `off` to force behaviour.
- `--libp2p-relay-port=<port>` – binds the relay listener to a port that’s independent from the TCP P2P socket (default: `p2p_port + 1`).
- `--libp2p-private-inbound-target=<n>` – cap libp2p inbound sessions when the node is private (default 8). The daemon automatically lowers the global inbound limit to this value when no public address is detected.
- `--libp2p-relay-inbound-limit=<n>` – maximum number of hole-punched peers accepted per relay host (default 2) to avoid eclipsing via a single intermediary.

You can verify the bridge at runtime via RPC:

```bash
kaspa-cli getlibpstatus
```

The command reports whether libp2p is enabled, the active role (client-only vs public relay), the current peer ID, and the configured inbound caps.

### Relay capability metadata

From Phase 10 onwards every Kaspa node running protocol version ≥ 9 advertises a dedicated service bit in its `Version` message plus the libp2p listener port it is willing to broker. The metadata flows through the P2P address gossip and is persisted by the RocksDB address manager so that private nodes always know which peers can act as relays even before they meet on the network.

- `kaspa-cli getpeeraddresses --json` now returns entries with `services` and `relayPort`. A relay-capable peer exposes bit `0x1` and a non-zero `relayPort`.
- `kaspa-cli getconnectedpeerinfo --json` surfaces the same data for currently connected peers, alongside the existing libp2p telemetry (peer ID, multiaddr, whether a relay was used for the connection).
- Older nodes (protocol < 9) neither advertise nor consume this metadata; do not rely on them as relays for private nodes.

Example snippet:

```json
{
  "address": "203.0.113.42:16211",
  "services": 1,
  "relayPort": 18111
}
```

Use this output, together with `getlibpstatus`, to confirm that your public nodes are advertising the expected relay port and that private nodes keep at least two relay connections for anti-eclipse safety.
