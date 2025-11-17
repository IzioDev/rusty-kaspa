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

Protocol v9 nodes now surface relay capability directly in the gossip set. Every `NetAddress` carries a `services` bitmap and a `relayPort`, and the connection manager keeps at least two distinct relays online for private nodes by default.

- `kaspa-cli getpeeraddresses --json | jq '.addresses[] | select(.services == 1)'` shows all advertised relays plus their bridge ports.
- `kaspa-cli getconnectedpeerinfo --json` reports the same data for active peers so you can confirm that private nodes rotate between relays (`services == 1` + distinct `relayPort` values).

Example snippet:

```json
{
  "address": "203.0.113.42:16211",
  "services": 1,
  "relayPort": 18111
}
```

If you see `services: 0`, the peer is either running an older protocol or operating in client-only mode and will be skipped by the private-node relay selector.

### Upgrade to v9

Follow the [Protocol v9 release notes](../docs/RELEASE_NOTES_v9.md) for the full description of wire/disk changes plus the AddressManager schema-v2 migration details. The safe path is: stop the daemon, deploy the v9 binaries, start the daemon, and confirm the one-time schema bump log before proceeding to the next node.

**Verify in 60 seconds**

```bash
# On a public relay-capable node
kaspa-cli getlibpstatus --json
kaspa-cli getpeeraddresses --json | jq '.addresses[] | select(.services == 1) | {addr: .address, relayPort: .relayPort}'

# On a private node
kaspa-cli getlibpstatus --json | jq '{role, privateInboundTarget, relayInboundLimit}'
kaspa-cli getconnectedpeerinfo --json | jq '.peers[] | {addr: .address, services: .services}'
```

Expected: public relays present `"services": 1` with a positive `relayPort`; private nodes show `role: "client-only"` plus the private/relay inbound targets reported by `getlibpstatus`.

## Libp2p circuit helper

The helper control plane is enabled automatically when running the patched `kaspad`. By default it binds to `127.0.0.1:(libp2p_relay_port + 100)` and can be overridden (or disabled) with `--libp2p-helper-address=<host:port>` / `--libp2p-helper-address=off`. The helper executable is built via:

```bash
cargo build -p kaspa-libp2p-circuit-helper --release
```

Invoke it on each private node after the daemon is up:

```bash
target/release/kaspa-libp2p-circuit-helper \
  --relay-ma "/ip4/149.28.164.184/tcp/18111/p2p/12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG/p2p-circuit" \
  --relay-peer 12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG \
  --target-peer 12D3KooWLLFP6CANQAaDoDSs3ZGqMMgn75EA1aY3osFPPUKAvUrk \
  --control 127.0.0.1:38081 \
  --timeout 30s
```

The output prints the multiaddr and peer key on success; failures return a non-zero exit status together with the error description propagated by kaspad.

## Deterministic validation script

`tcp-hole-punch/scripts/prove_libp2p_circuit.sh` orchestrates the end-to-end proof: it restarts the nodes (optional), seeds the relay once via the wRPC probe, runs the helper to open A→B (and optionally B→A) circuits, polls each node until the “Active libp2p relay circuits” section is populated, and writes timestamped probe captures under `tcp-hole-punch/proof/`.

Environment variables control the workflow:

| Variable | Purpose |
| --- | --- |
| `NODE_A_CMD_PREFIX` / `NODE_B_CMD_PREFIX` | Optional command prefix for remote execution, e.g. `ssh ubuntu@10.0.3.26`. |
| `NODE_A_WSRPC_URL` / `NODE_B_WSRPC_URL` | Local wRPC endpoints (default `ws://127.0.0.1:17110/` & `ws://127.0.0.1:27110/`). |
| `NODE_A_HELPER_ADDR` / `NODE_B_HELPER_ADDR` | Helper control endpoints exposed by kaspad (`--libp2p-helper-address`). |
| `NODE_A_PEER_ID` / `NODE_B_PEER_ID` | Static peer IDs used when dialing. |
| `NODE_A_START_CMD` / `NODE_B_START_CMD` | Optional restart commands (e.g. `systemctl restart kaspad`). |
| `RELAY_MULTIADDR`, `RELAY_PEER_ID`, `ADD_PEER_ADDR` | Relay coordinates injected into the helper/probe. |

Example invocation (local nodes):

```bash
export NODE_A_WSRPC_URL=ws://127.0.0.1:17110/
export NODE_B_WSRPC_URL=ws://127.0.0.1:27110/
export NODE_A_HELPER_ADDR=127.0.0.1:38081
export NODE_B_HELPER_ADDR=127.0.0.1:38082
tcp-hole-punch/scripts/prove_libp2p_circuit.sh
```

When running against remote hosts, prefix the commands via SSH:

```bash
export NODE_A_CMD_PREFIX="ssh ubuntu@10.0.3.26"
export NODE_B_CMD_PREFIX="ssh luke@mac-mini"
export NODE_A_WORKDIR=/home/ubuntu/rusty-kaspa
export NODE_B_WORKDIR=/Users/luke/Documents/GitHub/rusty-kaspa
tcp-hole-punch/scripts/prove_libp2p_circuit.sh
```

The script emits the paths to the captured probe logs when the active circuits appear.

## Proof artefacts

Redacted sample probe outputs demonstrating the “Active libp2p relay circuits” section live under `tcp-hole-punch/logs/sample-node-a.log` and `tcp-hole-punch/logs/sample-node-b.log`. Each file records the environment snapshot, relay gossip, connected peer summary, and the populated relay circuit section with `/p2p-circuit/…` multiaddrs.

## Reverting

To revert to the previous behaviour, remove `--libp2p-helper-address` (or set it to `off`), restart kaspad, and skip the helper/validation steps. The helper control port is only bound locally, so no firewall changes are required. After disabling the helper simply restart the daemon (`systemctl restart kaspad` or the chosen supervisor) to return to the default configuration.
