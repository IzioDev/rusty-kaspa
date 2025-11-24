#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

: "${HELPER_BIN:=$REPO_ROOT/target/release/kaspa-libp2p-circuit-helper}"
: "${PROBE_BIN:=$REPO_ROOT/target/debug/kaspa-wrpc-simple-client-example}"
: "${NODE_A_CMD_PREFIX:=}"
: "${NODE_B_CMD_PREFIX:=}"
: "${NODE_A_START_CMD:=}"
: "${NODE_B_START_CMD:=}"
: "${NODE_A_WORKDIR:=$REPO_ROOT}"
: "${NODE_B_WORKDIR:=$REPO_ROOT}"
: "${NODE_A_WSRPC_URL:=ws://127.0.0.1:17110/}"
: "${NODE_B_WSRPC_URL:=ws://127.0.0.1:27110/}"
: "${NODE_A_HELPER_ADDR:=127.0.0.1:38081}"
: "${NODE_B_HELPER_ADDR:=127.0.0.1:38082}"
: "${NODE_A_PEER_ID:=12D3KooWQH8UDJtmWgCnwGZDNA1Li14FbNPmDsaoAiNQgduNF8Jk}"
: "${NODE_B_PEER_ID:=12D3KooWLLFP6CANQAaDoDSs3ZGqMMgn75EA1aY3osFPPUKAvUrk}"
: "${RELAY_MULTIADDR:=/ip4/149.28.164.184/tcp/18111/p2p/12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG/p2p-circuit}"
: "${RELAY_PEER_ID:=12D3KooWKWQMLKnDg9BizoExsXWiuebcitxtJa3LCHdcWT2jP7yG}"
: "${ADD_PEER_ADDR:=149.28.164.184:16111}"
: "${POLL_ATTEMPTS:=24}"
: "${POLL_INTERVAL:=5}"
: "${HELPER_ATTEMPTS:=5}"
: "${HELPER_RETRY_DELAY:=1}"
: "${PROOF_DIR:=$REPO_ROOT/tcp-hole-punch/proof}"
: "${RUN_REVERSE_DIAL:=1}"

mkdir -p "$PROOF_DIR"
timestamp="$(date +%Y%m%d-%H%M%S)"

require_file() {
    # if [ ! -x "$1" ]; then
    #     echo "missing executable: $1" >&2
    #     exit 1
    # fi
    return 0
}

require_file "$HELPER_BIN"
require_file "$PROBE_BIN"

run_cmd() {
    local prefix="$1"
    local workdir="$2"
    local cmd="$3"
    if [ -n "$prefix" ]; then
        $prefix "cd \"$workdir\" && $cmd"
    else
        bash -lc "cd \"$workdir\" && $cmd"
    fi
}

seed_relay() {
    local prefix="$1"
    local workdir="$2"
    local url="$3"
    echo "Seeding relay on $url"
    run_cmd "$prefix" "$workdir" "KASPA_WSRPC_URL=$url KASPA_ADD_PEER=$ADD_PEER_ADDR $PROBE_BIN >/dev/null"
}

run_helper() {
    local prefix="$1"
    local workdir="$2"
    local control="$3"
    local target="$4"
    echo "Dialing peer $target via helper on $control"
    run_cmd "$prefix" "$workdir" "$HELPER_BIN --relay-ma '$RELAY_MULTIADDR' --relay-peer '$RELAY_PEER_ID' --target-peer '$target' --control '$control'"
}

poll_for_circuit() {
    local label="$1"
    local prefix="$2"
    local workdir="$3"
    local url="$4"
    local outfile="$5"
    echo "Waiting for libp2p circuit on $label"
    for attempt in $(seq 1 "$POLL_ATTEMPTS"); do
        if run_cmd "$prefix" "$workdir" "KASPA_WSRPC_CIRCUIT_ONLY=1 KASPA_WSRPC_QUICK=1 KASPA_WSRPC_URL=$url $PROBE_BIN" >"$outfile"; then
            if grep -q "Active libp2p relay circuits:" "$outfile" && grep -q "libp2p_relay_used=Some(true)" "$outfile"; then
                if ! (grep -A1 "Active libp2p relay circuits" "$outfile" | grep -q "(none)"); then
                    echo "  ✓ detected active circuit on $label after ${attempt} attempt(s)"
                    # Re-run without quick mode so the final logs capture the full snapshot
                    run_cmd "$prefix" "$workdir" "KASPA_WSRPC_URL=$url $PROBE_BIN" >"$outfile"
                    return 0
                fi
            fi
        fi
        sleep "$POLL_INTERVAL"
    done
    echo "  ✗ timed out waiting for libp2p circuit on $label" >&2
    return 1
}

start_node_if_needed() {
    local prefix="$1"
    local workdir="$2"
    local cmd="$3"
    local label="$4"
    if [ -n "$cmd" ]; then
        echo "Restarting $label"
        run_cmd "$prefix" "$workdir" "$cmd"
    fi
}

start_node_if_needed "$NODE_A_CMD_PREFIX" "$NODE_A_WORKDIR" "$NODE_A_START_CMD" "node A"
start_node_if_needed "$NODE_B_CMD_PREFIX" "$NODE_B_WORKDIR" "$NODE_B_START_CMD" "node B"

sleep 3

node_a_log="$PROOF_DIR/node-a-$timestamp.log"
node_b_log="$PROOF_DIR/node-b-$timestamp.log"

seed_relay "$NODE_A_CMD_PREFIX" "$NODE_A_WORKDIR" "$NODE_A_WSRPC_URL"
seed_relay "$NODE_B_CMD_PREFIX" "$NODE_B_WORKDIR" "$NODE_B_WSRPC_URL"

# Capture the probes in the background before dialing so short-lived circuits are detected.
poll_for_circuit "node A" "$NODE_A_CMD_PREFIX" "$NODE_A_WORKDIR" "$NODE_A_WSRPC_URL" "$node_a_log" &
poll_a_pid=$!
poll_for_circuit "node B" "$NODE_B_CMD_PREFIX" "$NODE_B_WORKDIR" "$NODE_B_WSRPC_URL" "$node_b_log" &
poll_b_pid=$!

for attempt in $(seq 1 "$HELPER_ATTEMPTS"); do
    if ! run_helper "$NODE_A_CMD_PREFIX" "$NODE_A_WORKDIR" "$NODE_A_HELPER_ADDR" "$NODE_B_PEER_ID"; then
        echo "  ⚠ helper dial attempt $attempt from node A failed" >&2
    fi
    if [ "${RUN_REVERSE_DIAL}" -eq 1 ]; then
        if ! run_helper "$NODE_B_CMD_PREFIX" "$NODE_B_WORKDIR" "$NODE_B_HELPER_ADDR" "$NODE_A_PEER_ID"; then
            echo "  ⚠ helper dial attempt $attempt from node B failed" >&2
        fi
    fi

    poll_a_active=0
    if kill -0 "$poll_a_pid" 2>/dev/null; then
        poll_a_active=1
    fi
    poll_b_active=0
    if kill -0 "$poll_b_pid" 2>/dev/null; then
        poll_b_active=1
    fi
    if [ "$poll_a_active" -eq 0 ] && [ "$poll_b_active" -eq 0 ]; then
        break
    fi
    if [ "$attempt" -lt "$HELPER_ATTEMPTS" ]; then
        sleep "$HELPER_RETRY_DELAY"
    fi
done

wait "$poll_a_pid"
wait "$poll_b_pid"

echo "Final probe outputs stored in:"
echo "  $node_a_log"
echo "  $node_b_log"
