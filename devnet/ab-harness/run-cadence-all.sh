#!/usr/bin/env bash
set -uo pipefail
cd /home/18c/torus-ab
export EMPTY_WINDOW=40 LOAD_DUR=40 BS=50 SENDERS=8 RATE=20
A=/home/18c/torus-ab/bin/torus-node-A
B=/home/18c/torus-ab/bin/torus-node-B
echo "===== ARM A (feat/pacemaker-backoff) ====="
./cadence.sh armA "$A" 0
echo; echo "===== ARM B-OFF (fsync build, TORUS_SYNC_WAL_ON_COMMIT=0) ====="
./cadence.sh armBoff "$B" 0
echo; echo "===== ARM B-ON (fsync build, TORUS_SYNC_WAL_ON_COMMIT=1) ====="
./cadence.sh armBon "$B" 1
echo "===== CADENCE MATRIX DONE ====="
