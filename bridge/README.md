This Stratum Bridge is currently in BETA. Community support is available in the Kaspa Discord’s [#mining-and-hardware](https://discord.com/channels/599153230659846165/910178666099646584) channel.

For bug reports or feature request, please open an issue at [`kaspanet/rusty-kaspa` issues](https://github.com/kaspanet/rusty-kaspa/issues) and prefix your issue title with `[Bridge]`.

The bridge can operate in two modes:

1. (Default) **In-process** node: both bridge and a kaspa node are started
2. **External** node: only bridge is started. Requires an external node to connect to.

### Supported Miners

The bridge supports multiple ASIC miner types with automatic detection:

- **IceRiver** (KS2L, KS3M, KS5, etc.): Requires extranonce, single hex string job format
- **Bitmain** (Antminer, GodMiner): No extranonce, array + timestamp job format
- **BzMiner**: Requires extranonce, single hex string job format
- **Goldshell**: Requires extranonce, single hex string job format

The bridge automatically detects miner type and adjusts protocol handling accordingly.

## Getting Started

Assuming you created a new directory `kaspa` on your machine.

1. Download and extract the [release archive](https://github.com/kaspanet/rusty-kaspa/releases/latest) for your OS in `kaspa`.
2. Copy/paste the [configuration boilerplate file](https://github.com/kaspanet/rusty-kaspa/blob/master/bridge/config.yaml) in `kaspa/config.yaml`
3. Run bridge and node alongside:
   ```bash
   ./stratum-bridge --config config.yaml --node-mode inprocess -- --utxoindex --rpclisten=127.0.0.1:16110
   ```

Already have a node running?\
Make sure configured `kaspad_address` is reachable. Adapt node mode: `--node-mode external` and remove kaspad-specific arguments:

```bash
./stratum-bridge --config config.yaml --node-mode external
```

## Web Dashboard

The bridge includes a built-in web dashboard accessible at the configured `web_dashboard_port`.
It contains Real-time Statistics, Workers table, Recent found blocks, and expose data as endpoints:

- `/api/stats`: JSON stats for all workers and blocks
- `/api/status`: Bridge status information
- `/api/config`: Configuration management (read/write, requires `RKSTRATUM_ALLOW_CONFIG_WRITE=1`)

**Access:** Open `http://127.0.0.1:3030/` (or your configured port) in a web browser.

The dashboard only is started if configured.

## Default config / ports

**Note:** If no config file is found, the bridge uses code defaults:

- `kaspad_address`: `localhost:16110`
- `node_mode`: `inprocess`
- `web_dashboard_port`: empty

The sample `config.yaml` exposes these Stratum ports:

Each instance also sets a `prom_port`, which is a per-instance Prometheus HTTP endpoint.  
Scrape format: `http://<bridge_host>:<prom_port>/metrics`.

| Port    | Purpose                                                                                                        |
| ------- | -------------------------------------------------------------------------------------------------------------- |
| `:5559` | Stratum listener for very low-difficulty workers (`min_share_diff: 4`), with metrics on Prometheus `:2118`.    |
| `:5560` | Stratum listener for low-difficulty workers (`min_share_diff: 512`), with metrics on Prometheus `:2119`.       |
| `:5561` | Stratum listener for medium-difficulty workers (`min_share_diff: 1024`), with metrics on Prometheus `:2120`.   |
| `:5555` | Stratum listener for higher-difficulty workers (`min_share_diff: 2048`), with metrics on Prometheus `:2114`.   |
| `:5556` | Stratum listener for higher-difficulty workers (`min_share_diff: 4096`), with metrics on Prometheus `:2115`.   |
| `:5557` | Stratum listener for high-difficulty workers (`min_share_diff: 8192`), with metrics on Prometheus `:2116`.     |
| `:5558` | Stratum listener for highest-difficulty workers (`min_share_diff: 16384`), with metrics on Prometheus `:2117`. |

## CLI Help

For detailed command-line options:

```bash
./stratum-bridge --help
```

This will show all available bridge options and guidance for kaspad arguments (relevant for `inprocess` mode).

## Running two bridges at once (two dashboards)

If you run **two `stratum-bridge` processes** simultaneously (e.g. one in-process and one external),
they **cannot share the same**:

- `web_dashboard_port` (dashboard)
- any Stratum ports
- any per-instance Prometheus ports

Recommended setup:

- **In-process bridge**: run normally with `--config config.yaml` (uses `web_dashboard_port: ":3030"` and the configured instance ports)
- **External bridge**: do **not** reuse the same instance ports; instead, run a single custom Stratum instance on a different port and set a different web dashboard port.

Example (external bridge on `:3031` + Stratum `:16120`):

```bash
cargo run -p kaspa-stratum-bridge --release --features rkstratum_cpu_miner --bin stratum-bridge -- --config config.yaml --web-dashboard-port :3031 --node-mode external --kaspad-address 127.0.0.1:16210 --instance "port=:16120,diff=1" --internal-cpu-miner --internal-cpu-miner-address "kaspatest:address" --internal-cpu-miner-threads 1
```

Open:

- `http://127.0.0.1:3030/` for the in-process bridge
- `http://127.0.0.1:3031/` for the external bridge

## Miner / ASIC connection

- **Pool URL:** `<your_pc_IPv4>:<stratum_port>` (e.g. `192.168.1.10:5555`)
- **Username / wallet:** `kaspa:YOUR_WALLET_ADDRESS.WORKERNAME`

## Connectivity

To verify connectivity on Windows:

```powershell
netstat -ano | findstr :5555
```

To see detailed miner connection / job logs:

```powershell
$env:RUST_LOG="info,kaspa_stratum_bridge=debug"
```

On Windows, Ctrl+C may show `STATUS_CONTROL_C_EXIT` which is expected.

## Prometheus Metrics

The bridge exposes Prometheus metrics at `/metrics` for integration with monitoring systems:

- Worker share counters and difficulty tracking
- Block mining statistics
- Network hashrate and difficulty
- Worker connection status and uptime
- Internal CPU miner metrics (when feature enabled)

## Variable Difficulty (VarDiff)

The bridge supports automatic difficulty adjustment based on worker performance:

- **Target Shares Per Minute**: Configurable via `shares_per_min` in config
- **Power-of-2 Clamping**: Optional `pow2_clamp` for smoother difficulty transitions
- **Per-Worker Tracking**: Each worker's difficulty is adjusted independently
- **Real-time Display**: Current difficulty shown in web dashboard

VarDiff helps optimize mining efficiency by automatically adjusting difficulty to match each worker's hashrate.

### Internal CPU miner (feature-gated)

The internal CPU miner is a **compile-time feature**.

Build:

```bash
cargo build -p kaspa-stratum-bridge --release --features rkstratum_cpu_miner
```

Run (external node mode + internal CPU miner enabled):

```bash
cargo run -p kaspa-stratum-bridge --release --features rkstratum_cpu_miner --bin stratum-bridge -- --config bridge/config.yaml --node-mode external --internal-cpu-miner --internal-cpu-miner-address kaspa:YOUR_WALLET_ADDRESS --internal-cpu-miner-threads 1
```

## Testing

Run all bridge tests (including CPU miner tests when feature is enabled):

```bash
cargo test -p kaspa-stratum-bridge --features rkstratum_cpu_miner --bin stratum-bridge
```

Run tests without the CPU miner feature:

```bash
cargo test -p kaspa-stratum-bridge --bin stratum-bridge
```

The test suite includes:

- Configuration parsing tests
- JSON-RPC event parsing tests
- Network utilities tests
- Hasher/difficulty calculation tests
- Mining state management tests
- Miner compatibility tests (IceRiver, Bitmain, BzMiner, Goldshell)
- Share validation and PoW checking tests
- VarDiff logic tests
- Wallet address cleaning tests
- CPU miner tests (when `rkstratum_cpu_miner` feature is enabled)

The test suite is comprehensive and educational, with 175+ unit tests designed to help developers understand the codebase.
