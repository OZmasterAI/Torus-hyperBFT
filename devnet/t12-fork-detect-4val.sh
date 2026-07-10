#!/usr/bin/env bash
# t12-fork-detect-4val.sh — T1.2 n=4/f=1 fork-detection devnet harness (think-dev).
#
# WHY THIS EXISTS
#   think-dev carries the grandparent-lock / lock-on-parent consensus fix
#   ("finding A", commit 4a94e8b) targeting a CONFIRMED 4-validator agreement
#   violation. The unit tests (pending_justify_bypass_enforces_lock_clause,
#   justify_block_livelock_test) pass, but consensus code needs BEHAVIORAL proof:
#   a real 4-validator devnet under load where we cross-check the per-height
#   COMMITTED block hash across ALL nodes and fail loudly on any divergence (fork),
#   including across an f=1 fault (kill one validator, stay live 3-of-4, restart,
#   verify it rejoins with no disagreeing committed hash at any height).
#
# WHAT IT DOES  (native binaries, NO docker, NO cargo — a release binary is provided)
#   Phase A  — 4 validators up under load for PHASE_A_SECS.
#   Phase B  — SIGKILL validator KILL_IDX; keep load FAULT_SECS; ASSERT the chain
#              height keeps climbing (3-of-4 liveness); restart the SAME data dir;
#              wait for catch-up; run POST_SECS more under load.
#   Check    — while all 4 are still up, t12-fork-check.py pulls the committed hash
#              at every height from every node and proves no height ever has two
#              different hashes. ANY mismatch => FORK DETECTED report + non-zero exit.
#
# HASH SOURCE (verified in the think-dev worktree, see t12-fork-check.py header):
#   eth_getBlockByNumber(hex(h), false)["hash"] == keccak256(canonical block header)
#   stored at commit time (app.rs:207, eth.rs:102-114). Uncommitted height => null.
#
# ISOLATION / SAFETY
#   Everything lives under RUN_DIR (default devnet/t12-run-<ts>). Own genesis copy,
#   own data dirs, own ports (defaults chosen to avoid the live testnet node on
#   9090/8545/30333 AND the docker devnet on 8645-8648/9091-9094). Teardown kills
#   ONLY the PIDs this script recorded — never pkill/pgrep on 'torus-node', so the
#   production seed node under /home/crab/projects/Torus-hyperBFT/testnet is untouched.
#
# A false PASS is worse than a crash: the checker FAILS on any unreachable node,
# any unparseable hash, any committed-prefix gap, and any cross-node hash mismatch.
#
# Static gates (do NOT compile Rust): bash -n devnet/t12-fork-detect-4val.sh
#                                     python3 -m py_compile devnet/t12-fork-check.py
set -uo pipefail
cd "$(dirname "$0")"                      # -> <worktree>/devnet
DEVNET_DIR="$(pwd)"
WT_ROOT="$(cd .. && pwd)"

# ---------------------------------------------------------------------------
# Config (all overridable via env)
# ---------------------------------------------------------------------------
TORUS_NODE_BIN=${TORUS_NODE_BIN:-"$WT_ROOT/target/release/torus-node"}
TORUS_BENCH_BIN=${TORUS_BENCH_BIN:-"$WT_ROOT/target/release/bench-throughput"}
GENESIS_SRC=${GENESIS_SRC:-"$DEVNET_DIR/genesis.json"}   # existing 4-validator devnet genesis (read-only, copied)
TS=$(date +%Y%m%d-%H%M%S)
RUN_DIR=${RUN_DIR:-"$DEVNET_DIR/t12-run-$TS"}

# Phase durations (seconds). SMOKE shrinks everything for a quick shakeout.
SMOKE=${SMOKE:-0}
if [ "$SMOKE" = 1 ]; then
    PHASE_A_SECS=${PHASE_A_SECS:-40}
    FAULT_SECS=${FAULT_SECS:-30}
    POST_SECS=${POST_SECS:-30}
else
    PHASE_A_SECS=${PHASE_A_SECS:-240}   # ~4 min all-up under load
    FAULT_SECS=${FAULT_SECS:-120}       # ~2 min with one validator down (3-of-4)
    POST_SECS=${POST_SECS:-120}         # ~2 min after the killed node rejoins
fi

# Which validator to kill during the f=1 window. V0/V1 are the bootstrap targets
# (every other node dials them), so default to V3 — a pure leaf whose death cannot
# break anyone else's bootstrap. Must be 0..3.
KILL_IDX=${KILL_IDX:-3}

# Load shape (bench 'consensus'): pre-signed nonce window is <55s, so each burst is
# capped and the load loop re-fires bursts to sustain pressure across a whole phase.
BURST=${BURST:-45}                       # seconds per bench invocation (< 55s window)
BS=${BS:-100}                            # batch size (orders/action). moderate default
RATE=${RATE:-3}                          # actions/s per sender
SENDERS=${SENDERS:-20}                   # genesis funds 20 market-makers
MARKETS=${MARKETS:-4}                    # genesis seeds 4 markets — do not exceed
SIGN=${SIGN:-session}

# Ports (isolated from live testnet 8545/30333/9090 and docker devnet 8645+/9091+).
RPC_PORTS=(18645 18646 18647 18648)
P2P_PORTS=(31433 31434 31435 31436)
METRICS_PORTS=(19091 19092 19093 19094)
KEYS=(
    0100000000000000000000000000000000000000000000000000000000000000
    0200000000000000000000000000000000000000000000000000000000000000
    0300000000000000000000000000000000000000000000000000000000000000
    0400000000000000000000000000000000000000000000000000000000000000
)
# Deterministic libp2p peer IDs for keys 01/02 (from docker-compose.yml). The mesh
# also forms via validator-id dialing, so a stale ID still converges, but we wire
# the known IDs to make bootstrap deterministic. Everyone dials V0 and V1; V0 dials
# V1 — a full mesh emerges and the restarted leaf rejoins by dialing V0+V1.
PID_V0=12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X
PID_V1=12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3
ADDR_V0="/ip4/127.0.0.1/udp/${P2P_PORTS[0]}/quic-v1/p2p/$PID_V0"
ADDR_V1="/ip4/127.0.0.1/udp/${P2P_PORTS[1]}/quic-v1/p2p/$PID_V1"

RPC_URLS=""
for i in 0 1 2 3; do
    RPC_URLS+="${RPC_URLS:+,}v$i=http://127.0.0.1:${RPC_PORTS[$i]}"
done
# Plain (no-label) list for tools that want bare URLs; label list for the checker.
BARE_RPCS=""
for i in 0 1 2 3; do
    BARE_RPCS+="${BARE_RPCS:+,}http://127.0.0.1:${RPC_PORTS[$i]}"
done

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------
[ -x "$TORUS_NODE_BIN" ]  || { echo "FATAL: TORUS_NODE_BIN not executable: $TORUS_NODE_BIN" >&2; exit 1; }
[ -x "$TORUS_BENCH_BIN" ] || { echo "FATAL: TORUS_BENCH_BIN not executable: $TORUS_BENCH_BIN" >&2; exit 1; }
[ -r "$GENESIS_SRC" ]     || { echo "FATAL: genesis not readable: $GENESIS_SRC" >&2; exit 1; }
command -v python3 >/dev/null || { echo "FATAL: python3 required" >&2; exit 1; }
command -v curl    >/dev/null || { echo "FATAL: curl required" >&2; exit 1; }
case "$KILL_IDX" in 0|1|2|3) : ;; *) echo "FATAL: KILL_IDX must be 0..3" >&2; exit 1 ;; esac

mkdir -p "$RUN_DIR"
cp "$GENESIS_SRC" "$RUN_DIR/genesis.json"   # copy, never edit the tracked file
for i in 0 1 2 3; do mkdir -p "$RUN_DIR/v$i/data"; done
: >"$RUN_DIR/load_rpcs"                      # live-RPC set the load loop reads each burst
printf '%s\n' "$BARE_RPCS" >"$RUN_DIR/load_rpcs"
HEIGHTS_TSV="$RUN_DIR/heights.tsv"
: >"$HEIGHTS_TSV"
EVENTS_LOG="$RUN_DIR/events.log"
: >"$EVENTS_LOG"

log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$EVENTS_LOG"; }

# ---------------------------------------------------------------------------
# Teardown — kill ONLY recorded PIDs. Never pkill/pgrep by name (would match the
# production seed node). Load loop is stopped via a stop-file + its recorded PID.
# ---------------------------------------------------------------------------
teardown() {
    log "teardown: stopping load + nodes (recorded PIDs only)"
    touch "$RUN_DIR/STOP_LOAD" 2>/dev/null || true
    if [ -f "$RUN_DIR/load.pid" ]; then
        local lp; lp=$(cat "$RUN_DIR/load.pid" 2>/dev/null || true)
        [ -n "${lp:-}" ] && kill "$lp" 2>/dev/null || true
    fi
    for i in 0 1 2 3; do
        local pf="$RUN_DIR/v$i.pid"
        [ -f "$pf" ] || continue
        local p; p=$(cat "$pf" 2>/dev/null || true)
        [ -n "${p:-}" ] || continue
        # Only kill if it is still our node process (defensive: match the data dir
        # in its argv before signalling, so a recycled PID is never touched).
        if kill -0 "$p" 2>/dev/null; then
            if tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null | grep -q "$RUN_DIR/v$i/data"; then
                kill "$p" 2>/dev/null || true
            fi
        fi
    done
    sleep 2
    for i in 0 1 2 3; do
        local pf="$RUN_DIR/v$i.pid"
        [ -f "$pf" ] || continue
        local p; p=$(cat "$pf" 2>/dev/null || true)
        [ -n "${p:-}" ] && kill -0 "$p" 2>/dev/null || continue
        if tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null | grep -q "$RUN_DIR/v$i/data"; then
            kill -9 "$p" 2>/dev/null || true
        fi
    done
}
trap teardown EXIT INT TERM

# ---------------------------------------------------------------------------
# RPC helpers
# ---------------------------------------------------------------------------
node_head() { # $1 = rpc port -> committed head (or -1 on failure)
    curl -s -m 3 -X POST -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "http://127.0.0.1:$1" \
        | python3 -c 'import sys,json
try:
    print(int(json.load(sys.stdin)["result"],16))
except Exception:
    print(-1)' 2>/dev/null | tail -n1
}

# Head from any currently-live node (prefers V0, falls back through the survivors).
live_head() {
    local i h
    for i in 0 1 2 3; do
        [ -f "$RUN_DIR/v$i.down" ] && continue
        h=$(node_head "${RPC_PORTS[$i]}")
        [ "$h" -ge 0 ] && { echo "$h"; return 0; }
    done
    echo -1
}

# ---------------------------------------------------------------------------
# Node lifecycle
# ---------------------------------------------------------------------------
start_node() { # $1 = index
    local i=$1 peers
    case "$i" in
        0) peers="$ADDR_V1" ;;                 # V0 dials V1
        1) peers="$ADDR_V0" ;;                 # V1 dials V0
        *) peers="$ADDR_V0,$ADDR_V1" ;;        # leaves dial both seeds
    esac
    log "start node v$i  rpc=${RPC_PORTS[$i]} p2p=${P2P_PORTS[$i]} metrics=${METRICS_PORTS[$i]}"
    "$TORUS_NODE_BIN" \
        --genesis "$RUN_DIR/genesis.json" \
        --data-dir "$RUN_DIR/v$i/data" \
        --validator-key "${KEYS[$i]}" \
        --p2p-listen "/ip4/127.0.0.1/udp/${P2P_PORTS[$i]}/quic-v1" \
        --p2p-private-addrs \
        --p2p-peers "$peers" \
        --rpc-addr "127.0.0.1:${RPC_PORTS[$i]}" \
        --metrics-addr "127.0.0.1:${METRICS_PORTS[$i]}" \
        --archive \
        --log-level info \
        >>"$RUN_DIR/v$i/node.log" 2>&1 &
    echo $! >"$RUN_DIR/v$i.pid"
    rm -f "$RUN_DIR/v$i.down"
}

# Update the load loop's live-RPC set to only currently-up nodes.
refresh_load_rpcs() {
    local i list=""
    for i in 0 1 2 3; do
        [ -f "$RUN_DIR/v$i.down" ] && continue
        list+="${list:+,}http://127.0.0.1:${RPC_PORTS[$i]}"
    done
    printf '%s\n' "$list" >"$RUN_DIR/load_rpcs"
}

wait_chain_up() { # block until head > $1 within ~120s, else fail
    local target=$1 h
    for _ in $(seq 1 60); do
        h=$(live_head)
        [ "$h" -gt "$target" ] && { log "chain up: head=$h (> $target)"; return 0; }
        sleep 2
    done
    log "FATAL: chain never climbed past $target"
    return 1
}

# ---------------------------------------------------------------------------
# Background load loop — re-fires <55s bench bursts until STOP_LOAD appears.
# Reads the live-RPC set from $RUN_DIR/load_rpcs each burst so it stops hammering
# a killed node during the fault window. Runs as a direct child (recorded PID).
# ---------------------------------------------------------------------------
load_loop() {
    local burst_no=0 rpcs
    while [ ! -f "$RUN_DIR/STOP_LOAD" ]; do
        burst_no=$((burst_no + 1))
        rpcs=$(cat "$RUN_DIR/load_rpcs" 2>/dev/null)
        [ -n "$rpcs" ] || { sleep 2; continue; }
        "$TORUS_BENCH_BIN" consensus \
            --rpc-urls "$rpcs" \
            --batch-size "$BS" \
            --senders "$SENDERS" \
            --duration "$BURST" \
            --rate "$RATE" \
            --pre-sign "$((BURST * RATE))" \
            --sign-mode "$SIGN" \
            --markets "$MARKETS" \
            >>"$RUN_DIR/bench-burst-$burst_no.txt" 2>&1 || true
        [ -f "$RUN_DIR/STOP_LOAD" ] && break
        sleep 1
    done
}

# Height sampler (foreground, invoked per-phase for a fixed duration). Records
# timestamp, phase label, and every survivor's head so liveness is provable later.
sample_heights() { # $1 = seconds  $2 = phase-label
    local secs=$1 label=$2 end i h line
    end=$((SECONDS + secs))
    while [ $SECONDS -lt $end ]; do
        line="$(date +%s.%N)	$label"
        for i in 0 1 2 3; do
            if [ -f "$RUN_DIR/v$i.down" ]; then
                h="down"
            else
                h=$(node_head "${RPC_PORTS[$i]}")
            fi
            line+="	v$i=$h"
        done
        printf '%s\n' "$line" >>"$HEIGHTS_TSV"
        sleep 3
    done
}

# ===========================================================================
# RUN
# ===========================================================================
log "############################################################"
log "# T1.2 FORK-DETECT n=4/f=1  SMOKE=$SMOKE  RUN_DIR=$RUN_DIR"
log "# node=$TORUS_NODE_BIN"
log "# phaseA=${PHASE_A_SECS}s fault=${FAULT_SECS}s post=${POST_SECS}s kill=v$KILL_IDX"
log "# bench: bs=$BS rate=$RATE senders=$SENDERS markets=$MARKETS burst=${BURST}s"
log "############################################################"

# ---- launch all 4 validators ----
for i in 0 1 2 3; do start_node "$i"; done
if ! wait_chain_up 2; then
    log "RESULT: FAIL — devnet never produced blocks"
    for i in 0 1 2 3; do tail -40 "$RUN_DIR/v$i/node.log" >"$RUN_DIR/v$i.tail.log" 2>/dev/null || true; done
    echo "1 t12 FAILED (no-start)" >"$RUN_DIR/.done"
    exit 1
fi

# ---- start sustained load ----
refresh_load_rpcs
load_loop &
echo $! >"$RUN_DIR/load.pid"
log "load loop started (pid $(cat "$RUN_DIR/load.pid"))"

# ---- PHASE A: all up under load ----
log "=== PHASE A: 4-of-4 under load for ${PHASE_A_SECS}s ==="
A_START=$(live_head)
sample_heights "$PHASE_A_SECS" "A-allup"
A_END=$(live_head)
log "phase A: head $A_START -> $A_END (climbed $((A_END - A_START)))"
if [ "$A_END" -le "$A_START" ]; then
    log "RESULT: FAIL — chain did not progress under load in phase A"
    echo "1 t12 FAILED (no-progress-A)" >"$RUN_DIR/.done"
    exit 1
fi

# ---- PHASE B: f=1 fault ----
log "=== PHASE B: SIGKILL v$KILL_IDX (f=1), stay live 3-of-4 for ${FAULT_SECS}s ==="
KP=$(cat "$RUN_DIR/v$KILL_IDX.pid" 2>/dev/null || true)
if [ -n "${KP:-}" ] && kill -0 "$KP" 2>/dev/null \
   && tr '\0' ' ' <"/proc/$KP/cmdline" 2>/dev/null | grep -q "$RUN_DIR/v$KILL_IDX/data"; then
    kill -9 "$KP" 2>/dev/null || true
    touch "$RUN_DIR/v$KILL_IDX.down"
    log "killed v$KILL_IDX (pid $KP)"
else
    log "RESULT: FAIL — could not identify v$KILL_IDX process to kill"
    echo "1 t12 FAILED (kill-target)" >"$RUN_DIR/.done"
    exit 1
fi
refresh_load_rpcs                          # steer load away from the dead node
FAULT_START=$(live_head)
sample_heights "$FAULT_SECS" "B-fault"
FAULT_END=$(live_head)
FAULT_CLIMB=$((FAULT_END - FAULT_START))
log "fault window: survivor head $FAULT_START -> $FAULT_END (climbed $FAULT_CLIMB)"
# 3-of-4 liveness assertion: the surviving quorum MUST keep committing.
LIVENESS_MIN=${LIVENESS_MIN:-3}
if [ "$FAULT_CLIMB" -lt "$LIVENESS_MIN" ]; then
    log "RESULT: FAIL — chain stalled during f=1 fault (climbed $FAULT_CLIMB < $LIVENESS_MIN)"
    echo "1 t12 FAILED (fault-liveness)" >"$RUN_DIR/.done"
    exit 1
fi
log "liveness OK: 3-of-4 quorum kept committing during the fault"

# ---- restart the killed validator on the SAME data dir ----
log "=== restart v$KILL_IDX (same data dir), wait for catch-up ==="
start_node "$KILL_IDX"
refresh_load_rpcs
# Catch-up: wait until the restarted node's head reaches within CATCHUP_LAG of the
# survivor head (it must actively re-sync, not just replay its stale committed tip).
CATCHUP_LAG=${CATCHUP_LAG:-3}
caught=0
for _ in $(seq 1 90); do            # up to ~180s
    sv=$(live_head)
    rn=$(node_head "${RPC_PORTS[$KILL_IDX]}")
    if [ "$rn" -ge 0 ] && [ "$sv" -ge 0 ] && [ "$((sv - rn))" -le "$CATCHUP_LAG" ] && [ "$rn" -gt "$FAULT_END" ]; then
        rm -f "$RUN_DIR/v$KILL_IDX.down"
        caught=1
        log "v$KILL_IDX caught up: head=$rn (survivor=$sv)"
        break
    fi
    sleep 2
done
if [ "$caught" -ne 1 ]; then
    log "RESULT: FAIL — v$KILL_IDX did not rejoin/catch up after restart"
    echo "1 t12 FAILED (rejoin)" >"$RUN_DIR/.done"
    exit 1
fi
rm -f "$RUN_DIR/v$KILL_IDX.down"

# ---- PHASE C: post-restart under load ----
log "=== PHASE C: 4-of-4 again under load for ${POST_SECS}s ==="
sample_heights "$POST_SECS" "C-postrestart"

# ---- stop load (bench idle) so the checker reads a settled chain ----
log "stopping load"
touch "$RUN_DIR/STOP_LOAD"
if [ -f "$RUN_DIR/load.pid" ]; then kill "$(cat "$RUN_DIR/load.pid")" 2>/dev/null || true; fi
sleep 5                                     # let in-flight commits settle

# ---- collect logs before the fork check ----
for i in 0 1 2 3; do cp "$RUN_DIR/v$i/node.log" "$RUN_DIR/log-v$i.txt" 2>/dev/null || true; done

# ===========================================================================
# FORK CHECK — the whole point. All 4 up; cross-check every committed height.
# ===========================================================================
log "=== FORK CHECK: cross-validating committed hashes across all 4 nodes ==="
MIN_HEIGHTS=${MIN_HEIGHTS:-10}
python3 "$DEVNET_DIR/t12-fork-check.py" \
    --rpc-urls "$RPC_URLS" \
    --from 1 \
    --min-heights "$MIN_HEIGHTS" \
    --out "$RUN_DIR/fork-report.txt"
CHECK_RC=$?

# ---- liveness / lag stats for the summary ----
FINAL_HEADS=""
for i in 0 1 2 3; do FINAL_HEADS+="v$i=$(node_head "${RPC_PORTS[$i]}") "; done

log "############################################################"
log "# T1.2 SUMMARY"
log "#   phase A climb:        $((A_END - A_START)) blocks"
log "#   fault-window climb:   $FAULT_CLIMB blocks (3-of-4, min=$LIVENESS_MIN)"
log "#   final heads:          $FINAL_HEADS"
log "#   fork-check exit:      $CHECK_RC (0=PASS)"
log "#   report:               $RUN_DIR/fork-report.txt"
log "############################################################"

if [ "$CHECK_RC" -eq 0 ]; then
    log "RESULT: PASS — no fork; all nodes agree on every committed height"
    echo "0 t12 PASS faultclimb=$FAULT_CLIMB heads=[$FINAL_HEADS]" >"$RUN_DIR/.done"
    exit 0
else
    log "RESULT: FAIL — fork check returned $CHECK_RC (see $RUN_DIR/fork-report.txt)"
    echo "$CHECK_RC t12 FAILED (fork-check)" >"$RUN_DIR/.done"
    exit "$CHECK_RC"
fi
