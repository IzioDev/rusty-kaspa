# TCP Hole Punch Workspace

This directory collects everything built around the libp2p/DCUtR proof-of-concept for Kaspa.

| Path | Purpose |
| --- | --- |
| `plan.md` | Phase tracker with status of each milestone and the follow-up work queued after Phase 4. |
| `final-report.md` | End-to-end write‑up covering architecture changes, troubleshooting, the successful remote run, and future recommendations. |
| `design/phase2-architecture.md` | Detailed design notes for the bridge layer, stream wrapper, and operational guidance. |
| `logs/` | Sanitised captures from local and remote rehearsals plus the mixed-NAT runbook (`phase4-remote-success.md`). |
| `bridge/` | Libp2p⇄tonic bridge crate sources and tests. |
| `relay/` | Helper crate/scripts for running the standalone relay binary. |
| `scripts/` | Utilities for controlling the remote relay service. |
| `research/` | Scratch notes and reference material gathered during investigation. |
| `TEST_SCENARIOS.md` | Phase 1/2 scenario notes used when validating the initial libp2p experiments. |

For a quick orientation:
1. Start with `final-report.md` to understand what was built and how the PoC succeeded.
2. Review `design/phase2-architecture.md` for implementation specifics.
3. Use the runbook in `logs/phase4-remote-success.md` if you need to replay the mixed-NAT handshake.

## Running the end-to-end VPS validation

To validate the bridge against real infrastructure:
- **Relay VPS** – tmux session `phase6_relay`
- **Kaspa server (home workstation)** – tmux session `phase6_server`
- **Client VPS** – tmux session `phase6_vultr`

### 1. Sync code to the VPS hosts
```bash
# From the project root
rsync -az --delete --exclude '.git' --exclude 'target' . relay-vps:/root/rusty-kaspa
rsync -az --delete --exclude '.git' --exclude 'target' . client-vps:/root/rusty-kaspa
```

### 2. Rebuild and restart the relay (relay VPS)
```bash
ssh relay-vps
tmux attach -t phase6_relay  # if not already inside
cd /root/rusty-kaspa/tcp-hole-punch/relay
cargo build --release
cd /root
./control_relay_service.sh stop
./control_relay_service.sh start
```

### 3. Start the Kaspa server (home workstation)
```bash
tmux attach -t phase6_server  # optional
export LIBP2P_LISTEN_MULTIADDRS="/ip4/0.0.0.0/tcp/4012,/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY/p2p-circuit"
export LIBP2P_RELAY_MULTIADDRS="/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY"
cargo run --release -p kaspa-p2p-lib --bin kaspa_p2p_server --features libp2p-bridge \
  2>&1 | tee tcp-hole-punch/logs/phase6-server-session.log
```

### 4. Start the Kaspa client (client VPS)
```bash
ssh client-vps
tmux attach -t phase6_vultr
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

### 5. Collect and sanitise logs
```bash
rsync -az relay-vps:/root/rusty-kaspa/tcp-hole-punch/logs/phase6-relay-session.log tcp-hole-punch/logs/
rsync -az client-vps:/root/rusty-kaspa/tcp-hole-punch/logs/phase6-client-session.log tcp-hole-punch/logs/
# phase6-server-session.log is already local from step 3

sed -i '' \
  -e 's/78\\.47\\.77\\.153/<RELAY_IP>/g' \
  -e 's/87\\.121\\.72\\.51/<HOME_IP>/g' \
  -e 's/208\\.167\\.242\\.80/<CLIENT_VPS_IP>/g' \
  tcp-hole-punch/logs/phase6-{server,client,relay}-session.log
```

The consolidated runbook lives in `tcp-hole-punch/logs/phase6-remote-validation.md`, and the sanitised logs mirror what we committed for the Phase 6 evidence. Continuous replays can follow the same steps; feel free to adapt them into scripts or `just` recipes to shorten the loop further.
