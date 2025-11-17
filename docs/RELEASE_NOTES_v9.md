# Protocol v9 release notes

## What changed on the wire
- Protocol v9 adds relay capability metadata to `Version`: service bit `0x1` (`LIBP2P_RELAY`) now signals that a node can accept hole-punched traffic, and `relayPort` advertises the libp2p listener separately from the TCP P2P port.
- `NetAddress` gossip includes the same `services` bit plus the relay port so private nodes can learn and rotate across public relays without manual allow-lists.

## Mixed-version behavior (compat proof)
- Older protocol versions (< v9) safely ignore both fields because they follow protobuf "unknown field" semantics. They continue to interoperate, they simply cannot act as relays.
- v9 nodes treat missing relay metadata as zero/`None`, so peers still complete handshakes even when talking to older binaries.
- Compatibility coverage lives in CI; auditors can grep the following tests:
  - `protocol/p2p-compat-v8-tests/src/tests.rs::compat_v8_ignores_v9_relay_fields_on_version` (v9 → v8 Version)
  - `protocol/p2p-compat-v8-tests/src/tests.rs::compat_v8_address_gossip_survives_relay_metadata` (v9 → v8 NetAddress)
  - `protocol/p2p-compat-v8-tests/src/tests.rs::compat_v9_handles_v8_version_without_relay_fields` (v8 → v9 Version)
  - `utils/src/networking.rs::compat_v8_net_address_legacy_deserialize_defaults` (legacy NetAddress EOF path)
  - `protocol/p2p/src/convert/messages.rs::compat_v9_version_zero_relay_port_defaults_to_none` (`relayPort == 0 ⇒ None`)

## What changed on disk
- AddressManager schema v2 extends every persisted `NetAddress` with `services` and `relay_port`. Public relays write `services = 1` and their bridge port so private nodes can persist relay metadata.
- The first v9 startup migrates the column family in-place, writes the schema marker, and logs the bump; later restarts skip the rewrite as long as the marker remains.
- Rolling back to a pre-v9 binary requires wiping the AddressManager column family (`--reset-db` or manually deleting the CF) because older builds cannot decode schema-v2 rows.

## Operator “verify in 60 seconds”
```bash
# On a public relay-capable node
kaspa-cli getlibpstatus --json
kaspa-cli getpeeraddresses --json | jq '.addresses[] | select(.services == 1) | {addr: .address, relayPort: .relayPort}'

# On a private node
kaspa-cli getlibpstatus --json | jq '{role, privateInboundTarget, relayInboundLimit}'
kaspa-cli getconnectedpeerinfo --json | jq '.peers[] | {addr: .address, services: .services}'
```
Expected: public relays show `"services": 1` with a positive `relayPort`; private nodes report `role: "client-only"` plus the private/relay inbound targets.

## Upgrade steps (safe path)
1. Stop the node.
2. Deploy the v9 binaries.
3. Start the node and watch for the AddressManager schema bump log; the one-time migration runs here, and subsequent restarts boot normally.

## Related flags (unchanged defaults)
`--libp2p-relay-mode`, `--libp2p-relay-port`, and `--libp2p-private-inbound-target` behave exactly as in previous releases; the v9 upgrade merely documents how they relate to relay metadata.
