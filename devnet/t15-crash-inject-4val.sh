#!/usr/bin/env bash
# t15-crash-inject-4val.sh — T1.5/6 crash-injection crash-safety devnet harness (think-dev).
#
# WHY THIS EXISTS  (checklist item 2d)
#   think-dev carries the T1.5/6 native-execution crash-safety work: block execution
#   runs AFTER consensus commit (consensus-then-execute), so a validator killed
#   between the commit and the native post-commit flush / applied-marker write
#   (app.rs execute_committed_block: native flush ~:574, applied marker ~:625) must,
#   on restart, EITHER replay to state IDENTICAL to its peers (replay_committed
#   app.rs:1327) OR fail-stop LOUDLY (exec_failed latch -> torus-node main.rs:627-634
#   process::exit(70)) — it must NEVER silently diverge and keep finalizing over
#   frozen/forked native state. The unit tests exercise the latch; consensus needs
#   BEHAVIORAL proof on a real 4-validator devnet under native-order load.
#
# WHAT IT DOES  (native binaries, NO docker, NO cargo — release binaries are provided)
#   Warmup   — 4 validators up under NATIVE-order load so native state grows per block.
#   Cycle x N (env CYCLES, default 3):
#     KILL   — SIGKILL validator KILL_IDX at a worst-case moment:
#                KILL_MODE=random (default) — kill at a seeded random instant under load.
#                KILL_MODE=tight           — arm a log watcher on the target and kill the
#                                            instant a commit line appears, to bias the
#                                            hit toward the commit->applied window.
#     LIVE   — ASSERT 3-of-4 liveness (survivor head keeps climbing) during the fault.
#     BACK   — restart the SAME data dir and classify the outcome:
#                RECOVER   — process stays up and catches up to the survivor head. PASS.
#                FAIL-STOP — process exits 70 (exec_failed latch). PASS-VARIANT (correct).
#                            Restarted up to FAILSTOP_MAX times; a repeated instant 70 =
#                            CRASH LOOP => FAIL (logs captured).
#                STUCK     — alive but never catches up within RECOVER_SECS => FAIL.
#   Quiesce  — stop load, wait until ALL 4 nodes settle to the SAME, STABLE height.
#   DIFF     — t15-state-diff.py cross-checks the NATIVE state (order books, open
#              interest, balances, positions, markets, staking, treasury) across all 4.
#              ANY mismatch => loud FAIL with per-node values. (Header-hash agreement
#              is ALSO cross-checked via t12-fork-check.py as a secondary signal — it
#              can miss native divergence because the block hash is agreed BEFORE
#              native execution, which is the whole reason the native diff exists.)
#
#   PASS (exit 0) only if every cycle passed AND the native-state diff PASSes.
#
# ISOLATION / SAFETY (identical posture to t12)
#   Everything under RUN_DIR (default devnet/t15-run-<ts>): own genesis copy, own data
#   dirs, own port block chosen to avoid the live seed (8545/30333/9090), the docker
#   devnet (8645+/9091+) AND the t12 harness (18645/31433/19091):
#       RPC 28645-28648   P2P 32433-32436   METRICS 29091-29094
#   Teardown / kills touch ONLY the PIDs this script recorded, and only after matching
#   the RUN_DIR data-dir in the process's argv — never pkill/pgrep by name, so the
#   production seed node is never at risk. A false PASS is worse than a crash: the
#   diff FAILS on any unreachable node, any unequal-height snapshot, and any mismatch.
#
#   WHAT THIS HARNESS CANNOT PROVE (honest limits):
#     * It cannot DETERMINISTICALLY land the kill inside the commit->applied window.
#       'tight' mode only biases toward it (kill on a commit log line); truly
#       deterministic window-hitting needs a code-level fault point (out of scope).
#     * Native replay-nonce state (CF_NATIVE_NONCES) has no read RPC, so it is diffed
#       only indirectly via balances/books/OI (see t15-state-diff.py COVERAGE GAP).
#     * 'random' timing is seeded for the bash kill delay, but bench order content
#       uses its own RNG, so runs are not bit-for-bit reproducible.
#
# Static gates (do NOT compile Rust): bash -n devnet/t15-crash-inject-4val.sh
#                                     python3 -m py_compile devnet/t15-state-diff.py
set -uo pipefail
cd "$(dirname "$0")"                      # -> <worktree>/devnet
DEVNET_DIR="$(pwd)"
WT_ROOT="$(cd .. && pwd)"

# ---------------------------------------------------------------------------
# Config (all overridable via env)
# ---------------------------------------------------------------------------
TORUS_NODE_BIN=${TORUS_NODE_BIN:-"$WT_ROOT/target/release/torus-node"}
TORUS_BENCH_BIN=${TORUS_BENCH_BIN:-"$WT_ROOT/target/release/bench-throughput"}
GENESIS_SRC=${GENESIS_SRC:-"$DEVNET_DIR/genesis.json"}   # 4-validator devnet genesis (read-only, copied)
TS=$(date +%Y%m%d-%H%M%S)
RUN_DIR=${RUN_DIR:-"$DEVNET_DIR/t15-run-$TS"}

# Crash behavior.
KILL_MODE=${KILL_MODE:-random}           # random | tight
CYCLES=${CYCLES:-3}                       # crash/restart cycles within one chain run
KILL_IDX=${KILL_IDX:-3}                   # validator to kill (leaf; 0..3). V3 is a pure leaf.
SEED=${SEED:-1337}                        # seeds the bash random kill delay (KILL_MODE=random)
FAILSTOP_MAX=${FAILSTOP_MAX:-2}           # consecutive exit-70 restarts tolerated before "crash loop"
# Commit log line the 'tight' watcher arms on (the START of native execution for a
# committed block — killing here biases the hit toward the flush/marker window).
COMMIT_MARK=${COMMIT_MARK:-"execution pipeline: executing finalized block"}

# Phase durations (seconds). SMOKE shrinks everything for a quick shakeout.
SMOKE=${SMOKE:-0}
if [ "$SMOKE" = 1 ]; then
    WARMUP_SECS=${WARMUP_SECS:-40}
    FAULT_SECS=${FAULT_SECS:-25}
    RECOVER_SECS=${RECOVER_SECS:-90}
    QUIESCE_SECS=${QUIESCE_SECS:-90}
else
    WARMUP_SECS=${WARMUP_SECS:-120}       # build native state before the first crash
    FAULT_SECS=${FAULT_SECS:-60}          # time with the target down (3-of-4)
    RECOVER_SECS=${RECOVER_SECS:-180}     # max time to catch up / fail-stop after restart
    QUIESCE_SECS=${QUIESCE_SECS:-150}     # max time to settle all nodes to one stable height
fi

# Load shape (bench 'consensus' — NATIVE PlaceOrder/PlaceOrderBatch so native state
# changes every block). Pre-signed nonce window is <55s, so each burst is capped and
# the load loop re-fires bursts to sustain pressure (same as t12).
BURST=${BURST:-45}
BS=${BS:-100}                            # orders per action
RATE=${RATE:-3}                          # actions/s per sender
SENDERS=${SENDERS:-20}                   # genesis native_balances funds 20 makers
MARKETS=${MARKETS:-4}                    # genesis seeds 4 markets — do not exceed
SIGN=${SIGN:-session}

# Ports (isolated from live 8545/30333/9090, docker 8645+/9091+, and t12 18645+/31433+/19091+).
RPC_PORTS=(28645 28646 28647 28648)
P2P_PORTS=(32433 32434 32435 32436)
METRICS_PORTS=(29091 29092 29093 29094)
KEYS=(
    0100000000000000000000000000000000000000000000000000000000000000
    0200000000000000000000000000000000000000000000000000000000000000
    0300000000000000000000000000000000000000000000000000000000000000
    0400000000000000000000000000000000000000000000000000000000000000
)
# Deterministic libp2p peer IDs for keys 01/02 (same keys as t12/docker-compose). Every
# node dials V0+V1; a restarted leaf rejoins by dialing them.
PID_V0=12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X
PID_V1=12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3
ADDR_V0="/ip4/127.0.0.1/udp/${P2P_PORTS[0]}/quic-v1/p2p/$PID_V0"
ADDR_V1="/ip4/127.0.0.1/udp/${P2P_PORTS[1]}/quic-v1/p2p/$PID_V1"

# Labeled + bare RPC URL lists.
RPC_URLS=""
BARE_RPCS=""
for i in 0 1 2 3; do
    RPC_URLS+="${RPC_URLS:+,}v$i=http://127.0.0.1:${RPC_PORTS[$i]}"
    BARE_RPCS+="${BARE_RPCS:+,}http://127.0.0.1:${RPC_PORTS[$i]}"
done

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------
[ -x "$TORUS_NODE_BIN" ]  || { echo "FATAL: TORUS_NODE_BIN not executable: $TORUS_NODE_BIN" >&2; exit 1; }
[ -x "$TORUS_BENCH_BIN" ] || { echo "FATAL: TORUS_BENCH_BIN not executable: $TORUS_BENCH_BIN" >&2; exit 1; }
[ -r "$GENESIS_SRC" ]     || { echo "FATAL: genesis not readable: $GENESIS_SRC" >&2; exit 1; }
[ -r "$DEVNET_DIR/t15-state-diff.py" ] || { echo "FATAL: t15-state-diff.py missing" >&2; exit 1; }
command -v python3 >/dev/null || { echo "FATAL: python3 required" >&2; exit 1; }
command -v curl    >/dev/null || { echo "FATAL: curl required" >&2; exit 1; }
case "$KILL_IDX"  in 0|1|2|3) : ;; *) echo "FATAL: KILL_IDX must be 0..3" >&2; exit 1 ;; esac
case "$KILL_MODE" in random|tight) : ;; *) echo "FATAL: KILL_MODE must be random|tight" >&2; exit 1 ;; esac
[ "$CYCLES" -ge 1 ] 2>/dev/null || { echo "FATAL: CYCLES must be >= 1" >&2; exit 1; }

mkdir -p "$RUN_DIR"
cp "$GENESIS_SRC" "$RUN_DIR/genesis.json"   # copy, never edit the tracked file
for i in 0 1 2 3; do mkdir -p "$RUN_DIR/v$i/data"; done
: >"$RUN_DIR/load_rpcs"
printf '%s\n' "$BARE_RPCS" >"$RUN_DIR/load_rpcs"
EVENTS_LOG="$RUN_DIR/events.log"; : >"$EVENTS_LOG"
CYCLES_LOG="$RUN_DIR/cycles.log"; : >"$CYCLES_LOG"
RANDOM=$SEED                                 # seed the bash RNG for KILL_MODE=random

# Logs go to STDERR (+ events.log), never stdout — several helpers below are called
# via $(...) command substitution, and a log line on stdout would pollute the
# captured value (e.g. turn a numeric head into multiline text).
log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$EVENTS_LOG" >&2; }

# ---------------------------------------------------------------------------
# Teardown — kill ONLY recorded PIDs, and only after matching the RUN_DIR data
# dir in the process argv. Never pkill/pgrep by name.
# ---------------------------------------------------------------------------
teardown() {
    log "teardown: stopping load + nodes (recorded PIDs only)"
    touch "$RUN_DIR/STOP_LOAD" 2>/dev/null || true
    if [ -f "$RUN_DIR/load.pid" ]; then
        local lp; lp=$(cat "$RUN_DIR/load.pid" 2>/dev/null || true)
        [ -n "${lp:-}" ] && kill "$lp" 2>/dev/null || true
    fi
    for i in 0 1 2 3; do
        local pf="$RUN_DIR/v$i.pid" p
        [ -f "$pf" ] || continue
        p=$(cat "$pf" 2>/dev/null || true)
        [ -n "${p:-}" ] || continue
        if kill -0 "$p" 2>/dev/null \
           && tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null | grep -q "$RUN_DIR/v$i/data"; then
            kill "$p" 2>/dev/null || true
        fi
    done
    sleep 2
    for i in 0 1 2 3; do
        local pf="$RUN_DIR/v$i.pid" p
        [ -f "$pf" ] || continue
        p=$(cat "$pf" 2>/dev/null || true)
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
    print(-1)' 2>/dev/null || echo -1
}

live_head() { # head from any currently-up node
    local i h
    for i in 0 1 2 3; do
        [ -f "$RUN_DIR/v$i.down" ] && continue
        h=$(node_head "${RPC_PORTS[$i]}")
        [ "$h" -ge 0 ] && { echo "$h"; return 0; }
    done
    echo -1
}

# Guarded kill of a recorded node PID — matches the RUN_DIR data dir in argv first.
guarded_kill() { # $1 = index, $2 = signal (default 9) -> 0 if a kill was issued
    local i=$1 sig=${2:-9} p
    p=$(cat "$RUN_DIR/v$i.pid" 2>/dev/null || true)
    [ -n "${p:-}" ] || return 1
    kill -0 "$p" 2>/dev/null || return 1
    tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null | grep -q "$RUN_DIR/v$i/data" || return 1
    kill "-$sig" "$p" 2>/dev/null || true
    return 0
}

# Reap the exit code of a recorded node PID if it has exited; echoes code or "alive".
node_exit_code() { # $1 = index
    local i=$1 p
    p=$(cat "$RUN_DIR/v$i.pid" 2>/dev/null || true)
    [ -n "${p:-}" ] || { echo "noPID"; return; }
    if kill -0 "$p" 2>/dev/null; then echo "alive"; return; fi
    # Process gone. `wait` only works for direct children; use it, fall back to "gone".
    local code
    if wait "$p" 2>/dev/null; then code=$?; else code=$?; fi
    echo "${code:-gone}"
}

# ---------------------------------------------------------------------------
# Node lifecycle
# ---------------------------------------------------------------------------
start_node() { # $1 = index
    local i=$1 peers
    case "$i" in
        0) peers="$ADDR_V1" ;;
        1) peers="$ADDR_V0" ;;
        *) peers="$ADDR_V0,$ADDR_V1" ;;
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

# Background load loop — re-fires <55s NATIVE-order bursts until STOP_LOAD appears.
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

# ---------------------------------------------------------------------------
# Kill strategies
# ---------------------------------------------------------------------------
# random: kill the target at a seeded random instant within FAULT-onset window.
kill_random() { # $1 = index
    local i=$1 delay=$(( SEED % 7 + (RANDOM % 8) ))   # ~0..14s, seeded-repeatable delay
    log "cycle: KILL_MODE=random — waiting ${delay}s under load, then SIGKILL v$i"
    sleep "$delay"
    guarded_kill "$i" 9 && log "killed v$i (random)" || { log "FATAL: could not kill v$i"; return 1; }
    return 0
}

# tight: arm a watcher on the target's log; SIGKILL the instant a commit line appears,
# to bias the hit toward the commit->applied window. Bounded by a timeout so it always
# kills (falls back to an unconditional kill) rather than hanging.
kill_tight() { # $1 = index
    local i=$1 logf="$RUN_DIR/v$i/node.log" waited=0
    log "cycle: KILL_MODE=tight — arming commit-line watcher on v$i ('$COMMIT_MARK')"
    # Tail only NEW lines; grep -m1 exits on the first commit line after arming.
    if timeout 30 stdbuf -oL tail -Fn0 "$logf" 2>/dev/null | grep -m1 -F "$COMMIT_MARK" >/dev/null; then
        guarded_kill "$i" 9 && { log "killed v$i (tight — on commit line)"; return 0; }
    fi
    # Timed out waiting for a commit line, or the guarded kill missed: kill unconditionally.
    log "tight watcher did not fire cleanly — issuing unconditional guarded kill on v$i"
    guarded_kill "$i" 9 && { log "killed v$i (tight — fallback)"; return 0; }
    log "FATAL: could not kill v$i"; return 1
}

# Assert 3-of-4 liveness while the target is down.
assert_liveness() { # returns 0 if survivors climbed >= LIVENESS_MIN
    local start end climb
    start=$(live_head)
    sleep "$FAULT_SECS"
    end=$(live_head)
    climb=$((end - start))
    log "fault window: survivor head $start -> $end (climbed $climb)"
    local mn=${LIVENESS_MIN:-3}
    [ "$climb" -ge "$mn" ] && { log "liveness OK (3-of-4 kept committing)"; echo "$end"; return 0; }
    log "FAIL: chain stalled during fault (climbed $climb < $mn)"
    echo "$end"; return 1
}

# Restart the target and classify: RECOVER | FAILSTOP | STUCK | CRASHLOOP.
# Echoes the classification; returns 0 for RECOVER/FAILSTOP (accepted), 1 otherwise.
recover_or_failstop() { # $1 = index, $2 = fault_end_head
    local i=$1 fault_end=$2 attempts=0 lag=${CATCHUP_LAG:-3}
    while :; do
        attempts=$((attempts + 1))
        start_node "$i"
        refresh_load_rpcs
        local waited=0 code rn sv
        while [ "$waited" -lt "$RECOVER_SECS" ]; do
            code=$(node_exit_code "$i")
            if [ "$code" != "alive" ] && [ "$code" != "noPID" ]; then
                # Process exited on its own during recovery.
                if [ "$code" = "70" ]; then
                    log "v$i exited 70 (fail-stop latch) on restart attempt $attempts"
                    touch "$RUN_DIR/v$i.down"
                    if [ "$attempts" -ge "$FAILSTOP_MAX" ]; then
                        log "FAIL: v$i hit exit-70 $attempts times — CRASH LOOP"
                        echo "CRASHLOOP"; return 1
                    fi
                    break   # retry restart (same data dir) to see if it recovers
                fi
                log "FAIL: v$i exited with unexpected code $code on restart"
                echo "EXIT_$code"; return 1
            fi
            rn=$(node_head "${RPC_PORTS[$i]}")
            sv=$(live_head)
            if [ "$rn" -ge 0 ] && [ "$sv" -ge 0 ] \
               && [ "$((sv - rn))" -le "$lag" ] && [ "$rn" -gt "$fault_end" ]; then
                rm -f "$RUN_DIR/v$i.down"
                log "v$i RECOVERED: head=$rn (survivor=$sv) after $attempts restart(s)"
                echo "RECOVER"; return 0
            fi
            sleep 2; waited=$((waited + 2))
        done
        # RECOVER_SECS elapsed without catch-up and without a clean exit.
        code=$(node_exit_code "$i")
        if [ "$code" = "70" ]; then
            log "v$i is in steady fail-stop (exit 70) — accepted PASS-variant"
            echo "FAILSTOP"; return 0
        fi
        log "FAIL: v$i neither caught up nor failed-stopped within ${RECOVER_SECS}s (STUCK)"
        echo "STUCK"; return 1
    done
}

# ===========================================================================
# RUN
# ===========================================================================
log "############################################################"
log "# T1.5/6 CRASH-INJECT n=4  SMOKE=$SMOKE  RUN_DIR=$RUN_DIR"
log "# node=$TORUS_NODE_BIN"
log "# mode=$KILL_MODE cycles=$CYCLES kill=v$KILL_IDX seed=$SEED"
log "# warmup=${WARMUP_SECS}s fault=${FAULT_SECS}s recover=${RECOVER_SECS}s quiesce=${QUIESCE_SECS}s"
log "# bench(native): bs=$BS rate=$RATE senders=$SENDERS markets=$MARKETS burst=${BURST}s"
log "############################################################"

# ---- launch all 4 validators ----
for i in 0 1 2 3; do start_node "$i"; done
if ! wait_chain_up 2; then
    log "RESULT: FAIL — devnet never produced blocks"
    for i in 0 1 2 3; do tail -40 "$RUN_DIR/v$i/node.log" >"$RUN_DIR/v$i.tail.log" 2>/dev/null || true; done
    echo "1 t15 FAILED (no-start)" >"$RUN_DIR/.done"
    exit 1
fi

# ---- start sustained NATIVE-order load ----
refresh_load_rpcs
load_loop &
echo $! >"$RUN_DIR/load.pid"
log "load loop started (pid $(cat "$RUN_DIR/load.pid"))"

# ---- warmup so native state (books/positions/balances) accumulates ----
log "=== WARMUP: 4-of-4 under native load for ${WARMUP_SECS}s ==="
W_START=$(live_head)
sleep "$WARMUP_SECS"
W_END=$(live_head)
log "warmup: head $W_START -> $W_END (climbed $((W_END - W_START)))"
if [ "$W_END" -le "$W_START" ]; then
    log "RESULT: FAIL — chain did not progress under load during warmup"
    echo "1 t15 FAILED (no-progress-warmup)" >"$RUN_DIR/.done"
    exit 1
fi

# ---- crash/restart cycles ----
CYCLE_FAILS=0
declare -a CYCLE_RESULT
for c in $(seq 1 "$CYCLES"); do
    log "========================================================"
    log "=== CYCLE $c/$CYCLES: crash v$KILL_IDX ($KILL_MODE) ==="
    log "========================================================"

    if [ "$KILL_MODE" = tight ]; then kill_tight "$KILL_IDX"; else kill_random "$KILL_IDX"; fi
    if [ $? -ne 0 ]; then
        CYCLE_RESULT[$c]="KILL_FAILED"; CYCLE_FAILS=$((CYCLE_FAILS + 1))
        echo "cycle $c: KILL_FAILED" >>"$CYCLES_LOG"; continue
    fi
    touch "$RUN_DIR/v$KILL_IDX.down"
    refresh_load_rpcs

    FAULT_END=$(assert_liveness); LIVE_RC=$?
    if [ "$LIVE_RC" -ne 0 ]; then
        CYCLE_RESULT[$c]="LIVENESS_FAIL"; CYCLE_FAILS=$((CYCLE_FAILS + 1))
        echo "cycle $c: LIVENESS_FAIL faultend=$FAULT_END" >>"$CYCLES_LOG"
        # keep going: restart it so later cycles/diff still have 4 nodes if possible
        start_node "$KILL_IDX"; refresh_load_rpcs; sleep 10; rm -f "$RUN_DIR/v$KILL_IDX.down"
        continue
    fi

    OUTCOME=$(recover_or_failstop "$KILL_IDX" "$FAULT_END"); REC_RC=$?
    CYCLE_RESULT[$c]="$OUTCOME"
    echo "cycle $c: $OUTCOME faultend=$FAULT_END" >>"$CYCLES_LOG"
    if [ "$REC_RC" -ne 0 ]; then
        CYCLE_FAILS=$((CYCLE_FAILS + 1))
        for i in 0 1 2 3; do tail -80 "$RUN_DIR/v$i/node.log" >"$RUN_DIR/v$i.cycle$c.tail.log" 2>/dev/null || true; done
    fi
    log "cycle $c outcome: $OUTCOME"
    # brief settle before the next cycle
    sleep 5
done

# ---- stop load ----
log "stopping load"
touch "$RUN_DIR/STOP_LOAD"
if [ -f "$RUN_DIR/load.pid" ]; then kill "$(cat "$RUN_DIR/load.pid")" 2>/dev/null || true; fi
sleep 5

# ---- quiesce: settle ALL up nodes to the SAME, STABLE height ----
# The native diff is only valid at an equal, non-moving height (there is no
# "state as of height h" read — every torus_* read is of the latest committed
# state). Require QUIESCE_STABLE consecutive identical all-equal samples.
log "=== QUIESCE: waiting for all up nodes to reach one stable height ==="
QUIESCE_STABLE=${QUIESCE_STABLE:-3}
DOWN_OK=0                                  # count nodes that are in accepted fail-stop
for i in 0 1 2 3; do [ -f "$RUN_DIR/v$i.down" ] && DOWN_OK=$((DOWN_OK + 1)); done
stable=0; last_sig=""; waited=0; quiesced=0
while [ "$waited" -lt "$QUIESCE_SECS" ]; do
    sig=""; alleq=1; first=""
    for i in 0 1 2 3; do
        [ -f "$RUN_DIR/v$i.down" ] && continue
        h=$(node_head "${RPC_PORTS[$i]}")
        [ "$h" -ge 0 ] || { alleq=0; break; }
        if [ -z "$first" ]; then first=$h; elif [ "$h" != "$first" ]; then alleq=0; fi
        sig+="v$i=$h "
    done
    if [ "$alleq" = 1 ] && [ -n "$first" ] && [ "$sig" = "$last_sig" ]; then
        stable=$((stable + 1))
        [ "$stable" -ge "$QUIESCE_STABLE" ] && { quiesced=1; log "quiesced at stable height $first ($sig)"; break; }
    else
        stable=0
    fi
    last_sig="$sig"
    sleep 3; waited=$((waited + 3))
done
if [ "$quiesced" -ne 1 ]; then
    log "RESULT: FAIL — nodes never settled to one stable height (cannot run a valid native diff)"
    echo "1 t15 FAILED (no-quiesce)" >"$RUN_DIR/.done"
    exit 1
fi

# ---- collect logs before the diff ----
for i in 0 1 2 3; do cp "$RUN_DIR/v$i/node.log" "$RUN_DIR/log-v$i.txt" 2>/dev/null || true; done

# Build the labeled RPC list of only the currently-up nodes (a steady fail-stopped
# node is excluded — its exit-70 is already an accepted PASS-variant, and it holds no
# live RPC to diff). Need >= 2 up nodes to cross-check.
UP_RPCS=""; UP_COUNT=0
for i in 0 1 2 3; do
    [ -f "$RUN_DIR/v$i.down" ] && continue
    UP_RPCS+="${UP_RPCS:+,}v$i=http://127.0.0.1:${RPC_PORTS[$i]}"
    UP_COUNT=$((UP_COUNT + 1))
done
if [ "$UP_COUNT" -lt 2 ]; then
    log "RESULT: FAIL — fewer than 2 live nodes to cross-check native state"
    echo "1 t15 FAILED (too-few-up)" >"$RUN_DIR/.done"
    exit 1
fi

# ===========================================================================
# NATIVE STATE DIFF — the whole point. Cross-check native state across up nodes.
# ===========================================================================
log "=== NATIVE STATE DIFF across $UP_COUNT up node(s): $UP_RPCS ==="
python3 "$DEVNET_DIR/t15-state-diff.py" \
    --rpc-urls "$UP_RPCS" \
    --genesis "$RUN_DIR/genesis.json" \
    --out "$RUN_DIR/state-diff-report.txt"
DIFF_RC=$?

# ---- secondary signal: committed block-hash agreement (reuse t12 checker) ----
FORK_RC="skipped"
if [ -r "$DEVNET_DIR/t12-fork-check.py" ]; then
    log "=== SECONDARY: committed block-hash cross-check (t12-fork-check.py) ==="
    python3 "$DEVNET_DIR/t12-fork-check.py" \
        --rpc-urls "$UP_RPCS" --from 1 --min-heights 10 \
        --out "$RUN_DIR/fork-report.txt"
    FORK_RC=$?
fi

# ---- summary ----
FINAL_HEADS=""
for i in 0 1 2 3; do
    if [ -f "$RUN_DIR/v$i.down" ]; then FINAL_HEADS+="v$i=down(failstop) "
    else FINAL_HEADS+="v$i=$(node_head "${RPC_PORTS[$i]}") "; fi
done
log "############################################################"
log "# T1.5/6 SUMMARY  (mode=$KILL_MODE cycles=$CYCLES kill=v$KILL_IDX)"
for c in $(seq 1 "$CYCLES"); do log "#   cycle $c: ${CYCLE_RESULT[$c]:-?}"; done
log "#   cycle failures:    $CYCLE_FAILS"
log "#   final heads:       $FINAL_HEADS"
log "#   native-diff exit:  $DIFF_RC (0=identical, 2=DIVERGED, 3=inconclusive)"
log "#   fork-check exit:   $FORK_RC (0=agree; secondary, cannot see native divergence)"
log "#   diff report:       $RUN_DIR/state-diff-report.txt"
log "############################################################"

# ---- verdict: PASS only if every cycle passed AND native state is identical ----
if [ "$CYCLE_FAILS" -eq 0 ] && [ "$DIFF_RC" -eq 0 ]; then
    log "RESULT: PASS — every crash cycle recovered-or-failstopped; native state identical across nodes"
    echo "0 t15 PASS cycles=$CYCLES fails=0 diff=0 heads=[$FINAL_HEADS]" >"$RUN_DIR/.done"
    exit 0
fi
log "RESULT: FAIL — cycle_fails=$CYCLE_FAILS native_diff=$DIFF_RC (see $RUN_DIR/state-diff-report.txt)"
echo "1 t15 FAILED cycle_fails=$CYCLE_FAILS diff=$DIFF_RC" >"$RUN_DIR/.done"
exit 1
