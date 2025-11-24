#!/bin/bash
set -e

# Script to run prove_libp2p_circuit.sh with correct environment variables for the PVE setup

# SSH Prefixes
export NODE_A_CMD_PREFIX="ssh -o StrictHostKeyChecking=no -J root@10.0.3.11 ubuntu@192.168.1.10"
export NODE_B_CMD_PREFIX="ssh -o StrictHostKeyChecking=no -J root@10.0.3.11 ubuntu@192.168.2.10"

# Working Directories
export NODE_A_WORKDIR="/home/ubuntu/rusty-kaspa"
export NODE_B_WORKDIR="/home/ubuntu/rusty-kaspa"

# Binaries (Relative to WORKDIR)
export HELPER_BIN="./target/release/kaspa-libp2p-circuit-helper"
export PROBE_BIN="./target/release/kaspa-wrpc-simple-client-example"

# Helper Addresses (Local to the node, so 127.0.0.1 is correct for the remote process)
export NODE_A_HELPER_ADDR="127.0.0.1:38081"
export NODE_B_HELPER_ADDR="127.0.0.1:38082"

# RPC URLs (Local to the node)
export NODE_A_WSRPC_URL="ws://127.0.0.1:17110"
export NODE_B_WSRPC_URL="ws://127.0.0.1:17110"

# Peer IDs (Captured from logs)
export NODE_A_PEER_ID="12D3KooWDxS5SrHSw8AaSTW55kXAoNyFtTyX22UiGM5Yo1iahRRy"
export NODE_B_PEER_ID="12D3KooWJZ6WC3g1r9SXYx5qjRch9vbwQ6YbFzdc8CuvxNzk9AvL"

# Relay Info
export RELAY_MULTIADDR="/ip4/10.0.3.50/tcp/18111/p2p/12D3KooWNqG8aV4rdLmA2cQ5Aax9E8G5GGoX4h7eQR2zYten6Bmv/p2p-circuit"
export RELAY_PEER_ID="12D3KooWNqG8aV4rdLmA2cQ5Aax9E8G5GGoX4h7eQR2zYten6Bmv"
export ADD_PEER_ADDR="10.0.3.50:16111"

# Disable auto-restart in the script because we already started them
export NODE_A_START_CMD=""
export NODE_B_START_CMD=""

# Paths
export PROOF_DIR="/tmp/proof"
export REPO_ROOT="/tmp" # Dummy, script uses it for paths but we override BINs

# Create proof dir locally
mkdir -p $PROOF_DIR

# Run the original script (assumed to be at /tmp/prove_libp2p_circuit.sh)
/tmp/prove_libp2p_circuit.sh

