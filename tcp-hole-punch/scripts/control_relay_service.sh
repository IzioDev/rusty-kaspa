#!/usr/bin/env bash
set -euo pipefail
SESSION="relay"
BIN="/root/hole-punch-relay"
LOG="/root/relay.log"
PORT="4011"
SEED="42"
case "${1:-}" in
    start)
        tmux has-session -t "$SESSION" 2>/dev/null && tmux kill-session -t "$SESSION"
        tmux new-session -d -s "$SESSION" "RUST_LOG=info $BIN --port $PORT --secret-key-seed $SEED | tee -a $LOG"
        ;;
    stop)
        tmux has-session -t "$SESSION" 2>/dev/null && tmux kill-session -t "$SESSION"
        ;;
    status)
        if tmux has-session -t "$SESSION" 2>/dev/null; then
            echo "running"
        else
            echo "stopped"
        fi
        ;;
    *)
        echo "Usage: $0 {start|stop|status}"
        exit 1
        ;;
esac
