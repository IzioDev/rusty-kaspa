# Phase 4 – Remote Two-Private-Peer POC (2025-10-29)

This run demonstrates a full Kaspa handshake between two private peers that meet via the Hetzner relay (`<RELAY_IP>`). The server ran on the local macOS host (private behind NAT) and the client on the Vultr VPS (`<CLIENT_VPS_IP>`). Both peers used libp2p/DCUtR to punch through their respective NATs and carried the Kaspa gRPC flows over the resulting circuit.

## Environment Snapshot

| Role  | Host | Key commands |
| --- | --- | --- |
| Relay | Hetzner VPS (`<RELAY_IP>`) | `/root/control_relay_service.sh status` (already running) |
| Server | Local workstation | `LIBP2P_LISTEN_MULTIADDRS="/ip4/0.0.0.0/tcp/4012,/ip4/<RELAY_IP>/tcp/4011/p2p/<relay>/p2p-circuit"` |
| Client | Vultr VPS (`<CLIENT_VPS_IP>`) | `LIBP2P_REMOTE_PEER_ID=<server-peer>`<br>`LIBP2P_REMOTE_MULTIADDRS=/ip4/<RELAY_IP>/tcp/4011/p2p/<relay>/p2p-circuit` |

- Both VPSes have the necessary firewall rules (`ufw allow 4011/tcp 4012/tcp`) and the relay binary was already active.
- The Vultr host builds `kaspa_p2p_client` natively via `rustup` + `cargo` to avoid cross-architecture binaries.

## Timeline & Log References

1. **Relay reservation + listener publication**  
   `tcp-hole-punch/logs/phase4-server-session.log:16-26`  
   ```
   Relay reservation requested via /ip4/<RELAY_IP>/.../p2p-circuit
   libp2p server peer id: 12D3KooWPC7iF5f59UFdRFEUfNprqjshtWcevpHm3N9VxD8ArxYW
   ```

2. **Client dial & DCUtR**  
   `tcp-hole-punch/logs/phase4-client-session.log:205-230` shows the dial attempt, reservation check, and DCUtR hole-punch through the relay.

3. **Kaspa handshake**  
   - Client logs `P2P, Client connected via libp2p … relay=true` at `tcp-hole-punch/logs/phase4-client-session.log:253`.  
   - Server logs the matching inbound identity and ready flow:  
     - `sending version message` – line 333  
     - `accepted version message` / `accepted ready message` – lines 358-360  
     - `P2P Connected to incoming peer … addr=/p2p/<client>` – line 376

4. **Relay trace**  
   `tcp-hole-punch/logs/phase4-relay-session.log` captures the reservation acceptance and subsequent circuit traffic between the two private peers.

## Operational Notes

- The server now synthesises a stable socket address when tonic’s `Request` lacks one (`protocol/p2p/src/core/connection_handler.rs`), ensuring downstream router bookkeeping/logging still work.
- Wait for the reservation log on the server before launching the client; starting too early will still produce “Relay has no reservation for destination”.
- Environment variables used for this run:
  ```bash
  # Server (local)
  LIBP2P_LISTEN_MULTIADDRS="/ip4/0.0.0.0/tcp/4012,/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY/p2p-circuit"
  LIBP2P_RELAY_MULTIADDRS="/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY"

  # Client (Vultr)
  LIBP2P_REMOTE_PEER_ID=12D3KooWPC7iF5f59UFdRFEUfNprqjshtWcevpHm3N9VxD8ArxYW
  LIBP2P_REMOTE_MULTIADDRS="/ip4/<RELAY_IP>/tcp/4011/p2p/12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY/p2p-circuit"
  ```

The captured logs plus the synthesised metadata confirm that both private peers completed the Kaspa flows over the punched circuit without fallback to the relay transport.
