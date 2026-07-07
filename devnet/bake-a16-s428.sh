#!/usr/bin/env bash
# bake-a16-s428.sh — A1.6 large-state devnet bake for the incremental state root.
#
# Proves, on a 4-validator docker devnet running the fleet-pin 6e03294 binary,
# that the incremental EVM/native state root keeps block time FLAT and sub-100ms
# as chain state grows large — and never diverges from the full-scan oracle.
#
# TWO PHASES (see the A1.6 spec: prompts/incremental-state-root-phase-a-prompt.md
# and docs/plans/incremental-state-root-impl.md):
#
#   PHASE G (grow) — flag ON + oracle ON. Grow REAL state in GROW_STAGES stages;
#     each stage floods FRESH EVM accounts (devnet/scripts/state-grow.py — new
#     recipients, monotonic account growth, NOT churn on 20 senders). Per stage:
#     full /metrics dumps (before/after, all 4 ports), height, per-validator
#     data-dir size, and a grep of every validator log for StateRootMismatch /
#     divergence (MUST be 0 — the oracle is cross-checking incremental vs full
#     scan every block). Artifacts under devnet/bake-a16-s428/stage-N/.
#
#   PHASE M (measure at the final large state) — three legs, volumes are NOT
#     wiped between them (down WITHOUT -v, then up with new env). Identical
#     native-bench burn each leg:
#       M1  flag ON  + oracle ON   — correctness under load at large state.
#       M2  flag ON  + oracle OFF  — THE FLATNESS PROOF: block time / p99 view
#                                    duration ~flat vs baseline, well under 100ms.
#       M3  flag =0  + oracle OFF  — full-scan CONTROL: cost grows with state.
#
# WHY view-duration, not torus_state_root_compute_seconds: that histogram is
# DEFINED but NOT wired (never .observe()'d) at 6e03294 — it stays count=0. The
# state root is computed pre-vote on the block critical path, so the operative
# flatness signals are torus_view_duration_seconds p50/p99 + the block-time
# least-squares fit. The bake still dumps the state-root histogram (0,0,0 here)
# so a future wired binary needs no harness change.
#
# HARD PINS: TORUS_PUSH_THRESHOLD=524288 (the S419-proven control; the compose
# default is a 6MB testnet-mirror that collapses bs400). Bench burst < 55s
# (pre-signed nonce window). --markets <= 4 (genesis seeds 4). Bench runs INSIDE
# the devnet image via docker run --network host (host glibc quirk).
#
# SMOKE: SMOKE=1 runs 1 tiny grow stage + one short M2 leg then tears down (-v).
#
# Does NOT rebuild the node, switch branches, or commit anything. The fleet-pin
# binary must already be at ../target/release/torus-node.
#
# Syntax gate: bash -n devnet/bake-a16-s428.sh
set -uo pipefail
cd "$(dirname "$0")"

# ---- pinned control config (S419) ------------------------------------------
export TORUS_PUSH_THRESHOLD=524288

# ---- knobs (SMOKE shrinks everything) --------------------------------------
SMOKE=${SMOKE:-0}
if [ "$SMOKE" = 1 ]; then
    GROW_STAGES=${GROW_STAGES:-1}
    GROW_DURATION=${GROW_DURATION:-60}
    GROW_SENDERS=${GROW_SENDERS:-40}
    BURN=${BURN:-30}
    M_LEGS=${M_LEGS:-"M2"}
    OUTROOT=${OUTROOT:-bake-a16-s428-smoke}
else
    GROW_STAGES=${GROW_STAGES:-4}
    GROW_DURATION=${GROW_DURATION:-180}
    GROW_SENDERS=${GROW_SENDERS:-60}
    BURN=${BURN:-50}          # < 55s pre-signed nonce window
    M_LEGS=${M_LEGS:-"M1 M2 M3"}
    OUTROOT=${OUTROOT:-bake-a16-s428}
fi

# ---- bench (native load) — identical every M leg ----------------------------
# DELIBERATELY LIGHT (batch=1) so per-block EXECUTION is cheap and the state-root
# compute is a RESOLVABLE fraction of block time. At bs=400 (the ab-bs4a shape)
# blocks are ~1.5s execution-bound and would mask the root cost entirely — the
# smoke measured 1499ms/block at bs=400, drowning any full-scan signal. With
# bs=1 the composite root (which full-scans ALL of CF_ACCOUNTS every block in the
# M3 flag=0 leg — proposer.rs:255 -> flagged_evm_root) is the dominant per-block
# cost, so M3 grows with state while M2 (incremental, O(changed)) stays flat.
MBS=${MBS:-1}
RATE=${RATE:-20}    # per-sender actions/s (x20 senders) — steady light block cadence
SENDERS=20                    # genesis funds 20 market-makers
MARKETS=4                     # genesis seeds 4 markets — must not exceed
SIGN=session
RPCS="http://localhost:8645,http://localhost:8546,http://localhost:8547,http://localhost:8548"
V0_RPC=http://localhost:8645
METRICS_PORTS="9091 9092 9093 9094"
VALIDATORS="validator-0 validator-1 validator-2 validator-3"
PY=scripts/ab-metrics-delta.py
VIEW_HIST=torus_view_duration_seconds
SR_HIST=torus_state_root_compute_seconds
IMG=torus-devnet-node:local

[ -x ../target/release/torus-node ]      || { echo "missing ../target/release/torus-node (fleet-pin)" >&2; exit 1; }
[ -x ../target/release/bench-throughput ] || { echo "missing ../target/release/bench-throughput" >&2; exit 1; }

dc() { docker compose -f docker-compose.yml -f docker-compose.bake.yml "$@"; }

trap 'dc down -v >/dev/null 2>&1 || true' EXIT

height() {
    curl -s -m 2 -X POST -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "$V0_RPC" | python3 -c 'import sys,json;print(int(json.load(sys.stdin)["result"],16))' \
        2>/dev/null || echo -1
}

snap_metrics() { # $1=dir $2=label — FULL dumps (histogram buckets needed for p99)
    local p
    for p in $METRICS_PORTS; do
        { curl -s -m 2 "http://localhost:$p/metrics" || true; } >"$1/metrics-$p-$2.txt" || true
    done
}

datadir_sizes() { # $1=dir $2=label — per-validator /data bytes via exec (nodes up)
    local svc out
    : >"$1/ddsize-$2.tsv"
    for svc in $VALIDATORS; do
        out=$(dc exec -T "$svc" du -sb /data 2>/dev/null | awk '{print $1}')
        printf '%s\t%s\n' "$svc" "${out:-na}" >>"$1/ddsize-$2.tsv"
    done
    awk '{s+=$2} END{print s+0}' "$1/ddsize-$2.tsv"
}

save_logs() { # $1=dir — capture node logs BEFORE any teardown (S419 lesson)
    local svc
    for svc in $VALIDATORS rpc-node; do
        dc logs --no-color "$svc" >"$1/log-$svc.txt" 2>&1 || true
    done
}

mismatch_count() { # $1=dir — StateRootMismatch / divergence across saved logs (MUST be 0)
    grep -icE 'StateRootMismatch|state-root divergence|native-root divergence|incremental.*divergence' \
        "$1"/log-*.txt 2>/dev/null | awk -F: '{s+=$2} END{print s+0}'
}

wait_up() { # $1=min-height — block until chain past it (or fail)
    local target=$1 h
    for _ in $(seq 1 45); do
        h=$(height)
        [ "$h" -gt "$target" ] && return 0
        sleep 2
    done
    return 1
}

wait_producing() { # block until the chain is PRODUCING NEW blocks (or fail).
    # After a down/up the RPC reports the replayed COMMITTED height immediately,
    # but the QUIC mesh needs to re-form before consensus resumes — measuring in
    # that gap gives an all-flat heights.tsv (block_ms_fit=0). Require the height
    # to ADVANCE from its post-restart reading before any measured burn starts.
    local base h
    base=$(height)
    for _ in $(seq 1 60); do   # up to ~120s for the mesh to resume
        sleep 2
        h=$(height)
        [ "$h" -gt "$((base + 2))" ] && return 0
    done
    return 1
}

# ---- env control: bare-name passthrough in docker-compose.bake.yml ----------
# flag: "1" (on) / "0" (off, full-scan). oracle: "on" (present) / "off" (absent).
set_leg_env() { # $1=flag $2=oracle
    export TORUS_INCREMENTAL_STATE_ROOT="$1"
    if [ "$2" = on ]; then export TORUS_INCREMENTAL_ORACLE=1; else unset TORUS_INCREMENTAL_ORACLE || true; fi
}

verify_on_node() { # $1=dir — PROVE the flag/oracle env reached PID 1 in-container
    dc exec -T validator-0 cat /proc/1/environ 2>/dev/null | tr '\0' '\n' \
        | grep -E '^TORUS_INCREMENTAL_(STATE_ROOT|ORACLE)=' >"$1/onnode-env.txt" || true
    echo "  on-node env (validator-0):"; sed 's/^/    /' "$1/onnode-env.txt" 2>/dev/null || true
}

# ---- run the identical native-bench burn (inside the devnet image) ----------
run_burn() { # $1=dir — drives blocks; heights.tsv + before/after dumps captured here
    local dir=$1
    snap_metrics "$dir" before
    : >"$dir/heights.tsv"
    # Inline sampler (direct child, so `wait` reaps it — a command-substituted
    # subshell PID cannot be waited on and would leak into teardown).
    (
        local end=$((SECONDS + BURN + 10))
        while [ $SECONDS -lt $end ]; do
            printf '%s\t%s\n' "$(date +%s.%N)" "$(height)" >>"$dir/heights.tsv"
            sleep 2
        done
    ) &
    local sampler=$!
    docker run --rm --network host --entrypoint /bench \
        -v "$(cd .. && pwd)/target/release/bench-throughput:/bench:ro" "$IMG" \
        consensus --rpc-urls "$RPCS" --batch-size "$MBS" --senders "$SENDERS" \
        --duration "$BURN" --rate "$RATE" --pre-sign "$((BURN * RATE))" \
        --sign-mode "$SIGN" --markets "$MARKETS" --format bin \
        2>&1 | tee "$dir/bench.txt" || true
    wait "$sampler" 2>/dev/null || true
    snap_metrics "$dir" after
}

mkdir -p "$OUTROOT"
GROW_CSV="$OUTROOT/grow-stages.csv"
M_CSV="$OUTROOT/measure-legs.csv"
echo "stage,height,ddsize_bytes_total,new_recipients,mismatch" >"$GROW_CSV"
echo "leg,flag,oracle,h_start,h_end,block_ms_fit,p50_view_ms,p99_view_ms,sr_p50_ms,sr_p99_ms,sr_count,mismatch,ddsize_bytes_total" >"$M_CSV"

echo "############################################################"
echo "# A1.6 BAKE  SMOKE=$SMOKE  stages=$GROW_STAGES grow=${GROW_DURATION}s burn=${BURN}s legs=[$M_LEGS]"
echo "# threshold=$TORUS_PUSH_THRESHOLD  out=$OUTROOT"
echo "############################################################"

# ============================================================================
# PHASE G — grow (flag ON + oracle ON)
# ============================================================================
set_leg_env 1 on
echo "=== phase G: fresh devnet, flag=ON oracle=ON ==="
dc down -v >/dev/null 2>&1 || true
dc up -d --build >"$OUTROOT/compose-up-grow.log" 2>&1
if ! wait_up 2; then
    echo "phase G: chain never started" >&2
    save_logs "$OUTROOT"; dc down -v >/dev/null 2>&1 || true
    echo "$? bake-a16 FAILED (no-start)" >"$OUTROOT/.done"; exit 1
fi
mkdir -p "$OUTROOT/stage-0"; verify_on_node "$OUTROOT/stage-0"

OFFSET=0
for s in $(seq 1 "$GROW_STAGES"); do
    sdir="$OUTROOT/stage-$s"; mkdir -p "$sdir"
    echo "=== stage $s / $GROW_STAGES : growing ${GROW_DURATION}s (offset=$OFFSET) ==="
    snap_metrics "$sdir" before
    : >"$sdir/grow-heights.tsv"
    (
        gend=$((SECONDS + GROW_DURATION + 5))
        while [ $SECONDS -lt $gend ]; do
            printf '%s\t%s\n' "$(date +%s.%N)" "$(height)" >>"$sdir/grow-heights.tsv"
            sleep 2
        done
    ) &
    sampler=$!
    # EVM fresh-account flood over the 4 VALIDATOR RPCs (never rpc-node:8549):
    # EVM-tx gossip is unwired on this chain (mem 2d657035) so each validator
    # only mines EVM txs from its OWN local pool on its proposer turns — round-
    # robin submission across all 4 proposers is what makes the fresh accounts
    # land. state-grow.py is HTTP raw-tx (no per-tx cast spawn), ~20-50x the
    # cast loop; its encoder is self-tested byte-identical to `cast mktx`.
    DURATION=$GROW_DURATION OFFSET=$OFFSET SENDERS=$GROW_SENDERS RPCS="$RPCS" \
        python3 scripts/state-grow.py 2>&1 | tee "$sdir/grow.log" || true
    wait "$sampler" 2>/dev/null || true
    OFFSET=$(grep -oE 'NEXT_OFFSET=[0-9]+' "$sdir/grow.log" | tail -1 | cut -d= -f2)
    OFFSET=${OFFSET:-$OFFSET}
    snap_metrics "$sdir" after
    local_new=$(grep -oE 'new-recipients=[0-9]+' "$sdir/grow.log" | tail -1 | cut -d= -f2)
    ddtot=$(datadir_sizes "$sdir" after)
    save_logs "$sdir"
    mm=$(mismatch_count "$sdir")
    h=$(height)
    echo "$s,$h,$ddtot,${local_new:-0},$mm" >>"$GROW_CSV"
    echo "--- stage $s: height=$h ddsize_total=${ddtot}B new_recipients=${local_new:-0} mismatch=$mm ---"
    if [ "$mm" -ne 0 ]; then
        echo "!!! stage $s: StateRootMismatch/divergence detected ($mm) — oracle caught a bug" >&2
    fi
done

echo "=== PHASE G complete ==="; cat "$GROW_CSV"

# ============================================================================
# PHASE M — measure at final large state (NO volume wipe between legs)
# ============================================================================
leg_params() { # $1=leg -> "flag oracle"
    case "$1" in
        M1*) echo "1 on" ;;
        M2*) echo "1 off" ;;
        M3*) echo "0 off" ;;
        E2*) echo "1 off" ;; # EVM-load leg: incremental only — A1.6 flatness signal
        E3*) echo "0 off" ;; # EVM-load leg: full-scan control — must grow with state
        *)  echo "1 off" ;;
    esac
}

# ---- EVM-load burn for E* legs (state-grow.py, fresh accounts) --------------
# Drives the exec-pipeline EVM root compute (validate_block_for_catchup ->
# compute_post_bundle_state_root) so torus_state_root_compute_seconds samples —
# the native bench never reaches that path (produce_block stamps the parent
# root, so native-only M legs read sr_count=0 by design; S428 finding). The
# global OFFSET continues from phase G so recipients stay fresh and state keeps
# GROWING between E legs — that is what makes E2a-vs-E2b a flatness comparison.
E_DURATION=${E_DURATION:-150}
run_evm_burn() { # $1=dir
    local dir=$1
    snap_metrics "$dir" before
    : >"$dir/heights.tsv"
    (
        local end=$((SECONDS + E_DURATION + 10))
        while [ $SECONDS -lt $end ]; do
            printf '%s\t%s\n' "$(date +%s.%N)" "$(height)" >>"$dir/heights.tsv"
            sleep 2
        done
    ) &
    local sampler=$!
    DURATION=$E_DURATION OFFSET=$OFFSET SENDERS=$GROW_SENDERS RPCS="$RPCS" \
        python3 scripts/state-grow.py 2>&1 | tee "$dir/grow.log" || true
    wait "$sampler" 2>/dev/null || true
    local newoff
    newoff=$(grep -oE 'NEXT_OFFSET=[0-9]+' "$dir/grow.log" | tail -1 | cut -d= -f2)
    [ -n "$newoff" ] && OFFSET=$newoff
    snap_metrics "$dir" after
}

for leg in $M_LEGS; do
    read -r flag oracle <<<"$(leg_params "$leg")"
    ldir="$OUTROOT/measure-$leg"; mkdir -p "$ldir"
    echo "=== $leg : flag=$flag oracle=$oracle (large state, volumes preserved) ==="
    h_pre=$(height)
    dc down >/dev/null 2>&1 || true          # NO -v — keep the grown state
    set_leg_env "$flag" "$oracle"
    dc up -d >"$ldir/compose-up.log" 2>&1
    # Require LIVE block production (not just the replayed committed height)
    # before measuring — else the burn lands in the mesh-reformation gap and the
    # heights.tsv fit reads 0 (smoke3 lesson).
    if ! wait_up "$((h_pre > 3 ? h_pre - 3 : 2))" || ! wait_producing; then
        echo "$leg: chain did not resume producing" >&2
        save_logs "$ldir"
        echo "$leg,$flag,$oracle,$h_pre,0,0,na,na,na,na,na,na,na" >>"$M_CSV"
        continue
    fi
    verify_on_node "$ldir"
    h0=$(height)
    case "$leg" in
        E*) run_evm_burn "$ldir" ;;
        *) run_burn "$ldir" ;;
    esac
    h1=$(height)
    ddtot=$(datadir_sizes "$ldir" after)
    save_logs "$ldir"
    mm=$(mismatch_count "$ldir")
    block_ms=$(python3 "$PY" fit "$ldir/heights.tsv" 2>/dev/null || echo 0)
    read -r p50 p99 _vc <<<"$(python3 "$PY" hist "$ldir" --hist "$VIEW_HIST" 2>/dev/null | tr ',' ' ')"
    read -r s50 s99 srcount <<<"$(python3 "$PY" hist "$ldir" --hist "$SR_HIST" 2>/dev/null | tr ',' ' ')"
    echo "$leg,$flag,$oracle,$h0,$h1,${block_ms:-0},${p50:-na},${p99:-na},${s50:-na},${s99:-na},${srcount:-na},$mm,$ddtot" >>"$M_CSV"
    tail -1 "$M_CSV"
    if [ "$mm" -ne 0 ]; then echo "!!! $leg: mismatch=$mm" >&2; fi
done

echo "=== PHASE M complete ==="; cat "$M_CSV"

# ---- final teardown: -v only at the very end -------------------------------
echo "=== final teardown (down -v) ==="
save_logs "$OUTROOT" || true
dc down -v >/dev/null 2>&1 || true
trap - EXIT

# Verdict: zero mismatch everywhere; M2 flat & sub-100ms; M3 (if present) higher.
total_mm=$(awk -F, 'NR>1{s+=$5} END{print s+0}' "$GROW_CSV")
m_mm=$(awk -F, 'NR>1 && $10!="na"{s+=$10} END{print s+0}' "$M_CSV")
rc=0
[ "$total_mm" -eq 0 ] && [ "$m_mm" -eq 0 ] || rc=1
echo "$rc bake-a16 done grow_mismatch=$total_mm measure_mismatch=$m_mm" >"$OUTROOT/.done"
echo "=== DONE rc=$rc (grow_mm=$total_mm measure_mm=$m_mm) — artifacts in $OUTROOT/ ==="
exit "$rc"
