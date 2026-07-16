#!/usr/bin/env bash
# stop-3val.sh — stop the WSL bare-metal devnet. Leaves data dirs intact.
set -uo pipefail
cd "$(dirname "$0")"
source ./env.sh

if [ ! -f "$RUN_DIR/pids" ]; then
    echo "no pids file ($RUN_DIR/pids) — nothing to stop"
    # belt-and-braces: kill any stray torus-node bound to our data root
    pkill -f "torus-node .*$DATA_ROOT" 2>/dev/null || true
    exit 0
fi

while read -r pid; do
    [ -n "$pid" ] || continue
    if kill -0 "$pid" 2>/dev/null; then
        echo "stopping pid $pid"
        kill "$pid" 2>/dev/null || true
    fi
done < "$RUN_DIR/pids"

# give them a moment to flush rocksdb, then hard-kill stragglers
for _ in 1 2 3 4 5 6 7 8 9 10; do
    if xargs -a "$RUN_DIR/pids" -r -I{} kill -0 {} 2>/dev/null; then sleep 1; else break; fi
done
while read -r pid; do
    [ -n "$pid" ] || continue
    kill -9 "$pid" 2>/dev/null || true
done < "$RUN_DIR/pids"

rm -f "$RUN_DIR/pids"
echo "stopped."
