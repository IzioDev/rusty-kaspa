# Phase 10 Diagnostic Report – 2025-11-17
## Handover Verification Session

### Executive Summary
**Status:** ❌ **BLOCKED** – Reservation protocol not functioning
**Blocker:** Private nodes request reservations from relay, but relay does not process/accept them

---

## Environment Status

### Relay (149.28.164.184)
- **Process:** Running (PID 56869, started 11:47)
- **Version:** kaspad 1.0.1
- **Flags:** `--libp2p-relay-mode=on --libp2p-relay-port=18111`
- **Ports:** 16111 (P2P), 17110/18110 (wRPC), 18111 (libp2p) ✓
- **Hop server:** Enabled (`libp2p relay server behaviour enabled` logged at 11:48:00)
- **Peer ID:** `12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG`

### Private Node A (10.0.3.26 – LAN)
- **Process:** Running (PID 87357, started 23:13)
- **Version:** kaspad 1.0.1
- **Flags:** `--libp2p-relay-mode=auto --libp2p-helper-address=127.0.0.1:38081 --libp2p-reservation=/ip4/149.28.164.184/tcp/18111/p2p/12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG/p2p-circuit`
- **Ports:** 16111 (P2P), 17110 (wRPC), 38081 (helper) ✓
- **Role:** ClientOnly (correct for private LAN node)
- **Peer ID:** `12D3KooWQH8UDJtmWgCnwGZDNA1Li14FbNPmDsaoAiNQgduNF8Jk`
- **Connectivity:** Relay port 18111 reachable (nc test succeeded)
- **Synced:** Yes ✓

### Private Node B (139.180.172.111 – VPS)
- **Process:** Running (PID 55478, started 12:14)
- **Version:** kaspad 1.0.1
- **Flags:** `--libp2p-relay-mode=auto --libp2p-helper-address=127.0.0.1:38082 --libp2p-reservation=/ip4/149.28.164.184/tcp/18111/p2p/12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG/p2p-circuit`
- **Ports:** 17110 (wRPC), 38082 (helper) ✓
- **Role:** PublicRelay (auto-detected due to public IP – unexpected but not critical)
- **Peer ID:** `12D3KooWPF3XdqHvpQ2Yba7paTctNMQbE7dg9ZPwW3PeTpBxZCkt`
- **Connectivity:** Relay port 18111 reachable (nc test succeeded)
- **Synced:** Yes ✓

---

## Diagnostic Steps Performed

### 1. Baseline Verification ✓
- Confirmed all three nodes running with correct flags
- Verified port listeners on all hosts
- Checked relay hop server enablement
- Verified wRPC probe functionality
- Verified helper endpoints bound

### 2. Relay Seeding ✓
- Node A: Added relay `149.28.164.184:16111` to P2P address book
- Node B: Added relay `149.28.164.184:16111` to P2P address book
- **Result:** Node B detected 1 relay-capable peer after seeding

### 3. Helper Dial Attempt ❌
**Command:** Node A → Node B via relay
```bash
kaspa-libp2p-circuit-helper \
  --relay-ma "/ip4/149.28.164.184/tcp/18111/p2p/12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG/p2p-circuit" \
  --relay-peer "12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG" \
  --target-peer "12D3KooWPF3XdqHvpQ2Yba7paTctNMQbE7dg9ZPwW3PeTpBxZCkt" \
  --control "127.0.0.1:38081"
```

**Error:**
```
✗ Dial failed: libp2p bridge error: dial attempt failed: Failed to negotiate transport protocol(s):
  [(/ip4/149.28.164.184/tcp/18111/p2p/12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG/p2p-circuit/p2p/12D3KooWPF3XdqHvpQ2Yba7paTctNMQbE7dg9ZPwW3PeTpBxZCkt: :
  Failed to connect to destination.: Failed to connect to destination.:
  Relay has no reservation for destination.)]
```

---

## Log Analysis

### Private Node A Logs
**Reservation request (23:13:27):**
```
[INFO] libp2p reservation targets: [/ip4/149.28.164.184/tcp/18111/p2p/12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG/p2p-circuit]
[INFO] requesting libp2p reservation via /ip4/149.28.164.184/tcp/18111/p2p/12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG/p2p-circuit
```
**No follow-up messages** (no success, no failure)

**Helper dial error (23:18:57):**
```
[WARN] Failed to open stream peer=12D3KooWPF3XdqHvpQ2Yba7paTctNMQbE7dg9ZPwW3PeTpBxZCkt
       err=dial attempt failed: ... Relay has no reservation for destination.
```

### Private Node B Logs
**Reservation request (12:14:10):**
```
[INFO] libp2p reservation targets: [/ip4/149.28.164.184/tcp/18111/p2p/12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG/p2p-circuit]
[INFO] requesting libp2p reservation via /ip4/149.28.164.184/tcp/18111/p2p/12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG/p2p-circuit
```
**No follow-up messages** (no success, no failure)

### Relay Logs
- ✅ Startup shows: `libp2p relay server behaviour enabled peer=12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG`
- ❌ **No reservation request activity logged at all**
- ❌ No mention of peer IDs `12D3KooWQH8UDJtmWgCnwGZDNA1Li14FbNPmDsaoAiNQgduNF8Jk` or `12D3KooWPF3XdqHvpQ2Yba7paTctNMQbE7dg9ZPwW3PeTpBxZCkt`
- Historical logs (10:39:54) show: `Listener: rejecting protocol: /libp2p/circuit/relay/0.2.0/hop` (before current boot)

### Error Timeline
1. **Early attempts (21:01–21:39):** "Remote does not support `/libp2p/circuit/relay/0.2.0/hop` protocol"
   → Relay hop wasn't enabled yet
2. **After relay restart (22:06+):** "Relay has no reservation for destination"
   → Hop enabled, but reservations not being processed

---

## Root Cause Hypothesis

The reservation protocol flow is incomplete:

1. ✅ Private nodes parse `--libp2p-reservation` flag
2. ✅ Private nodes log "requesting libp2p reservation"
3. ✅ Private nodes can reach relay port 18111 (TCP connectivity confirmed)
4. ❌ **Relay does not log or process reservation requests**
5. ❌ No reservation is established
6. ❌ Circuit dials fail with "Relay has no reservation for destination"

**Possible causes:**
- Reservation protocol handler not registered in relay code
- Reservation acceptance requires additional configuration/flag
- Relay silently rejecting reservation requests (no error logs)
- Protocol version mismatch or negotiation failure
- libp2p relay behaviour not fully configured for relay-v2 reservation acceptance

---

## Comparison to Phase 6 Success

Phase 6 logs (`phase6-relay-session.log:295`) show successful DCUtR circuits. Current Phase 10 setup differs:
- Phase 6 may have used different libp2p configuration or code version
- Current relay shows hop server "enabled" but no reservation activity
- Need to verify if Phase 6 relay had additional configuration

---

## Recommended Next Steps

### Option 1: Enable Debug Logging
Restart relay with `--loglevel=debug` to capture libp2p protocol-level messages:
```bash
# On relay (149.28.164.184)
kill <relay-pid>
nohup /root/kaspa/bin/kaspad \
  --appdir /root/kaspa/data \
  --loglevel=debug \
  --libp2p-relay-mode=on \
  --libp2p-relay-port=18111 \
  --rpclisten=0.0.0.0:16110 \
  --rpclisten-borsh=public \
  --rpclisten-json=public \
  > /root/kaspa/logs/kaspad-debug.log 2>&1 &
```
Then retry helper dial and check for reservation protocol messages.

### Option 2: Code Review
Investigate kaspad/libp2p integration:
- Verify reservation protocol handler is registered in relay mode
- Check if `--libp2p-relay-mode=on` fully enables relay-v2 reservation acceptance
- Compare Phase 6 code/config to current Phase 10 implementation
- Search for reservation logging code to understand why no logs appear

### Option 3: Minimal Test
Create a standalone libp2p test to verify the relay accepts reservations:
```rust
// Simplified reservation test outside kaspad
// Connect to relay:18111 and request reservation
// Log protocol negotiation and response
```

### Option 4: Check for Missing Dependency
Verify relay has all required libp2p behaviours configured:
- `Relay` behaviour with `Mode::Server`
- Circuit relay v2 support enabled
- Reservation limits configured (if required)

---

## Attached Evidence

### wRPC Probe Output (Node A before circuit)
```
libp2p enabled: true, role: Some("client-only")
private inbound target: Some(8), relay inbound limit: Some(2)
Connected relay-capable peers (relay_port present): 0
Active libp2p relay circuits: (none)
Sample peer set:
  36.50.94.101:16111 outbound=true is_libp2p=false relay_used=None
  75.46.154.168:16111 outbound=true is_libp2p=false relay_used=None
  ...
```

### Expected wRPC Probe Output (after successful circuit)
```
libp2p enabled: true, role: Some("client-only")
Active libp2p relay circuits:
  12D3KooWPF3XdqHvpQ2Yba7paTctNMQbE7dg9ZPwW3PeTpBxZCkt via /ip4/149.28.164.184/.../p2p-circuit
Connected peers with is_libp2p=true, libp2p_relay_used=Some(true)
```

---

## Session Duration
- Start: 2025-11-17 11:47 UTC (relay boot)
- Private nodes restarted: 23:13 (Node A), 12:14 (Node B)
- Helper dial attempted: 23:18
- Diagnostic completed: 23:20

## Files for Review
- Node A logs: `/home/ubuntu/kaspa/data/kaspa-mainnet/logs/rusty-kaspa.log`
- Node B logs: `/root/kaspa/data/kaspa-mainnet/logs/rusty-kaspa.log`
- Relay logs: `/root/kaspa/data/kaspa-mainnet/logs/rusty-kaspa.log`
- Helper binary: `~/rusty-kaspa/target/release/kaspa-libp2p-circuit-helper`
- Proof script: `~/rusty-kaspa/tcp-hole-punch/scripts/prove_libp2p_circuit.sh`

---

**Prepared by:** Claude (handover verification session)
**Contact:** Luke/Codex for code-level resolution
