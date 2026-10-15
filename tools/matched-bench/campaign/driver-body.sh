# ---- driver body (tools/matched-bench/campaign/driver-body.sh, copied in by make-driver.py; do not edit the copy) ----
# Everything below reads only the GENERATED CONFIG above. Modes:
#   DRIVER launch [smoke]   preflight, then start "DRIVER run [smoke]" as unit bench-$CAMPAIGN[-smoke] through
#                           detach.sh (the unit holds the build lock $HOME/projects/wt/.cargo-heavy.lock for its
#                           whole run); log $RUN[/smoke]/campaign.log; done marker $RUN[/smoke]/campaign.done
#   DRIVER dry [smoke]      preflight only (paths, staged md5s, tools), prints the cell list; starts nothing
#   DRIVER show [smoke]     the plan as tables (arms, node env, harness env, env checks, cells)
#   DRIVER run [smoke]      the campaign itself; refuses to run outside a bench-*.service unit
# smoke = the one SMOKE_CELL under $P-smoke in $RUN/smoke, with every check: run it (and read its checks) before
# launching the campaign. Library mode for tests: DRIVER_LIB_ONLY=1 source DRIVER (functions + config, no main).
# Test-only overrides: DRIVER_PROC (a fake /proc), DRIVER_DETACH (a fake detach.sh).
set -u
SELF=$(readlink -f "${BASH_SOURCE[0]}")
PROC=${DRIVER_PROC:-/proc}
HARNESS=$TOOLS/run-cell.sh
DETACH=${DRIVER_DETACH:-$TOOLS/campaign/detach.sh}
UNIT_NAME=$CAMPAIGN
MIN_FREE_MB=${MIN_FREE_MB:-20000}
QUIET_WAIT_S=${QUIET_WAIT_S:-3600}

log() { echo "[$(date +%T)] $*"; }

apply_smoke() { # the smoke run: SMOKE_CELL only, own prefix and run dir, no rerun pass
    RUN=$RUN/smoke; P=$P-smoke; UNIT_NAME=$CAMPAIGN-smoke; CELLS=("$SMOKE_CELL"); SMOKE=1
}

env_file() { # NAME -> the per-cell record that shows it: TORUS_* in node-env-all.txt, any other env in node-preload.txt
    case $1 in TORUS_*) echo node-env-all.txt ;; *) echo node-preload.txt ;; esac
}

absent_names() { # arm: every name in ENV_NAMES the arm's node env (base + arm) does not set
    local arm=$1 kv k set=" "
    for kv in ${AXE[$arm]}; do set+="${kv%%=*} "; done
    for k in $ENV_NAMES; do [[ "$set" == *" $k "* ]] || echo "$k"; done
}

step_summary_md5() { # arm: 0 = the step skips summary.json torus_node_md5 (run-cell.sh hashes the staged file,
    # a wrapper script on wrapper arms); the running-exe md5 records are checked on every arm
    [ "${BWRAP[${ABIN[$1]}]}" = 1 ] && echo 0 || echo 1
}

show_plan() { # [smoke]: the plan as tables (make-driver tests compare these with earlier drivers)
    local a kv k
    [ "${1:-}" = smoke ] && apply_smoke
    echo "PATH RUN $RUN"; echo "PATH P $P"; echo "PATH MARKETS $MARKETS"; echo "PATH RATE $RATE"
    echo "PATH TOOLS $TOOLS"; echo "PATH WT_H $WT_H"; echo "PATH DEVNET $DEVNET"; echo "PATH BASELINE $BASELINE"
    for a in "${ARMS[@]}"; do
        echo "ARM $a ${ABIN[$a]} ${BMD5[${ABIN[$a]}]} wrapper=${BWRAP[${ABIN[$a]}]} xe=${AXE[$a]}"
        [ -n "${AHENV[$a]}" ] || echo "HENV $a"
        for kv in ${AHENV[$a]}; do echo "HENV $a $kv"; done
        for kv in ${AXE[$a]}; do echo "PRESENT $a $kv $(env_file "${kv%%=*}")"; done
        for k in $(absent_names "$a"); do echo "ABSENT $a $k $(env_file "$k")"; done
    done
    for c in "${CELLS[@]}"; do IFS=: read -r t a d k _ <<< "$c"; echo "CELL $t $a $d $k"; done
}

# ---- host state ----
buildhits() { local n p; for n in rust-analyzer cargo rustc; do for p in $(pgrep -x "$n"); do printf '%s:%s ' "$n" "$p"; done; done; }
benchhits() { local p; for p in $(pgrep -x torus-node); do printf 'torus-node:%s ' "$p"; done
              for p in $(pgrep -f '^(\S*/)?bench-throughput( |$)'); do printf 'bench:%s ' "$p"; done; }
quiet() { # no build, no node or bench left over, 1-min load < MAX_LOAD; waits up to QUIET_WAIT_S
    local t=0 l1
    while :; do
        read -r l1 _ < /proc/loadavg
        if [ -z "$(buildhits)$(benchhits)" ] && awk -v l="$l1" -v m="$MAX_LOAD" 'BEGIN{exit !(l<m)}'; then return 0; fi
        [ $((t % 300)) = 0 ] && [ $t -gt 0 ] && log "waiting for a quiet host ${t}s (load $l1; $(buildhits)$(benchhits))"
        t=$((t+10)); [ $t -ge "$QUIET_WAIT_S" ] && { log "host not quiet after ${QUIET_WAIT_S}s (load $l1; $(buildhits)$(benchhits))"; return 1; }
        sleep 10
    done
}

# ---- per-cell records ----
node_pids() { cat "$DEVNET/run/pids" 2>/dev/null; }

envrec_now() { # o: per node pid (DEVNET/run/pids): exe md5 = md5 of PROC/PID/exe, the RUNNING exe (a wrapper script
    # has exec'd the real node by then; never hash the staged file), the TORUS_* environ, the other recorded environ
    local o=$1 p
    for p in $(node_pids); do
        echo "pid=$p exe_md5=$(md5sum < "$PROC/$p/exe" 2>/dev/null | cut -c1-32)"
    done > "$o/node-environ-trie.txt"
    for p in $(node_pids); do
        echo "pid=$p $(tr '\0' '\n' < "$PROC/$p/environ" 2>/dev/null | grep -E '^TORUS_' | sort | tr '\n' ' ')"
    done > "$o/node-env-all.txt"
    for p in $(node_pids); do
        echo "pid=$p $(tr '\0' '\n' < "$PROC/$p/environ" 2>/dev/null | grep -E "^($PROC_REC_RE)=" | sort | tr '\n' ' ')"
    done > "$o/node-preload.txt"
}

envrec() { # o: wait until the bench runs (run.log "] bench: "), then record (envrec_now)
    local o=$1 t=0
    until [ -f "$o/run.log" ] && grep -q '\] bench: ' "$o/run.log"; do sleep 1; t=$((t+1)); [ $t -gt 900 ] && return 1; done
    envrec_now "$o"
}

# ---- per-cell checks (each prints CHECK OK / CHECK FAIL lines; rc 1 = fail) ----
md5chk() { # o arm: every node runs the arm's node (exe md5 of the running process = the arm's staged md5)
    local o=$1 arm=$2 want=${BMD5[${ABIN[$2]}]} f=$1/node-environ-trie.txt n m
    # a record is the full md5 (this driver) or its first 8+ hex digits (older drivers' records)
    n=$(grep -c '^pid=' "$f" 2>/dev/null)
    m=$(awk -v w="$want" '{for (i = 1; i <= NF; i++) if ($i ~ /^exe_md5=[0-9a-f]{8,32}$/) {v = substr($i, 9); if (v == substr(w, 1, length(v))) c++}} END {print c + 0}' "$f" 2>/dev/null)
    [ "${n:-0}" = "$NODES" ] && [ "${m:-0}" = "$NODES" ] \
        || { echo "CHECK FAIL $(basename "$o"): node exe md5 ${want:0:8} on ${m:-0} of ${n:-0} node lines (want $NODES; $f)"; return 1; }
    echo "CHECK OK $(basename "$o") node exe md5 ${want:0:8} on $NODES nodes (arm $arm)"
}

envchk() { # o arm: the arm's node env (base + arm) on every node, every other name of ENV_NAMES on none.
    # TORUS_* from node-env-all.txt, other env (MALLOC_CONF, ...) from node-preload.txt (env_file)
    local o=$1 arm=$2 kv k f n rc=0 lb
    lb=$(basename "$o")
    for f in node-env-all.txt node-preload.txt; do
        n=$(grep -c '^pid=' "$o/$f" 2>/dev/null)
        [ "${n:-0}" = "$NODES" ] || { echo "CHECK FAIL $lb: $f has ${n:-0} node lines (want $NODES)"; rc=1; }
    done
    [ $rc = 0 ] || return 1
    for kv in ${AXE[$arm]}; do
        f=$(env_file "${kv%%=*}"); n=$(grep -F -c " $kv " "$o/$f")
        [ "$n" = "$NODES" ] || { echo "CHECK FAIL $lb: $kv on $n of $NODES node lines in $f (arm $arm)"; rc=1; }
    done
    for k in $(absent_names "$arm"); do
        f=$(env_file "$k"); n=$(grep -c " $k=" "$o/$f")
        [ "$n" = 0 ] || { echo "CHECK FAIL $lb: $k set on $n node lines in $f (arm $arm does not set it)"; rc=1; }
    done
    [ $rc = 0 ] && echo "CHECK OK $lb node env on $NODES nodes (arm $arm: ${AENV[$arm]:-no arm env}; unset: $(absent_names "$arm" | tr '\n' ' '))"
    return $rc
}

marketchk() { # o: the cell ran MARKETS markets (summary.json cell.markets)
    local o=$1 m
    m=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["cell"]["markets"])' "$o/summary.json" 2>/dev/null)
    [ "$m" = "$MARKETS" ] || { echo "CHECK FAIL $(basename "$o"): summary cell.markets=${m:-none} (want $MARKETS)"; return 1; }
    echo "CHECK OK $(basename "$o") markets=$m"
}

expectchk() { # o arm: the spec's expectations for every arm (EXP_ARM '*') and for this arm: grep -E -c in a cell file
    local o=$1 arm=$2 i n rc=0
    for i in "${!EXP_ARM[@]}"; do
        [ "${EXP_ARM[$i]}" = '*' ] || [ "${EXP_ARM[$i]}" = "$arm" ] || continue
        n=$(grep -E -c -- "${EXP_RE[$i]}" "$o/${EXP_FILE[$i]}" 2>/dev/null); n=${n:-0}
        if [ "$n" -lt "${EXP_MIN[$i]}" ] || { [ -n "${EXP_MAX[$i]}" ] && [ "$n" -gt "${EXP_MAX[$i]}" ]; }; then
            echo "CHECK FAIL $(basename "$o"): ${EXP_FILE[$i]} has $n lines matching '${EXP_RE[$i]}' (want ${EXP_MIN[$i]}..${EXP_MAX[$i]:-any}; arm $arm)"; rc=1
        else
            echo "CHECK OK $(basename "$o") ${EXP_FILE[$i]}: $n lines matching '${EXP_RE[$i]}' (arm $arm)"
        fi
    done
    return $rc
}

cellchecks() { # o arm: every standard check (all run, so one cell shows every failure)
    local rc=0
    md5chk "$1" "$2" || rc=1
    envchk "$1" "$2" || rc=1
    marketchk "$1" || rc=1
    expectchk "$1" "$2" || rc=1
    return $rc
}

# ---- one cell ----
cell() { # tag arm dur kind killat. return 0 = go on, 1 = stop the campaign (a started cell sets CELL_FAIL)
    local tg=$1 arm=$2 dur=$3 b label rcf unit EP t=0 rc nt=0 kv
    local -a E
    b=${ABIN[$arm]:-}
    [ -n "$b" ] || { log "ABORT: unknown arm $arm"; return 1; }
    label=$P-$tg; rcf=$R/$label.cell.rc; unit=bench-$label
    quiet || return 1
    [ ! -e "$rcf" ] && [ ! -e "$R/$label" ] || { log "ABORT: $R/$label or its .cell.rc exists"; return 1; }
    t=$(buildhits); log "HOST before $label: builds ${t:-none}; load $(cut -d' ' -f1-3 /proc/loadavg)"; t=0
    log "START $label arm=$arm bin=$b node_md5=${BMD5[$b]:0:8} wrapper=${BWRAP[$b]} env=${AENV[$arm]:-none} dur=$dur markets=$MARKETS rate=$RATE mem_avail_kb=$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)"
    mkdir -p "$R/$label"; STARTED_LABEL=$label
    envrec "$R/$label" & EP=$!
    E=(TARGET_DIR="${BDIR[$b]}" TOOLS_DIR="$TOOLS" DATA_ROOT="$DEVNET" RESULTS_ROOT="$R")
    for kv in ${AHENV[$arm]}; do E+=("$kv"); done
    [ "${BWRAP[$b]}" = 1 ] || E+=(EXPECT_NODE_MD5="${BMD5[$b]}")
    [ -z "${BBMD5[$b]}" ] || E+=(EXPECT_BENCH_MD5="${BBMD5[$b]}")
    "$DETACH" "$label" "$R/$label.campaign.log" bash -c 'rcf=$1; shift; env "$@"; echo "rc=$?" > "$rcf"' cellwrap "$rcf" \
        "${E[@]}" "$HARNESS" "$WT_H" "$label" "$MARKETS" "$dur" "$RATE" "${AXE[$arm]}" \
        || { kill "$EP" 2>/dev/null; CELL_FAIL=abort-detach; return 1; }
    until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done
    sleep 1
    rc=$(sed -n 's/^rc=//p' "$rcf" 2>/dev/null); rc=${rc:-unit-ended-without-rc}
    while [ -n "$(pgrep -x torus-node)" ] && [ $nt -lt 60 ]; do sleep 1; nt=$((nt+1)); done
    kill "$EP" 2>/dev/null; wait "$EP" 2>/dev/null
    log "END $label rc=$rc $(tail -1 "$R/$label.campaign.log" 2>/dev/null)"
    [ -z "$(pgrep -x torus-node)" ] || { log "ABORT: torus-node left running after $label"; CELL_FAIL=abort-node-left; return 1; }
    cellchecks "$R/$label" "$arm" || { CELL_FAIL=check; return 1; }
    [ "$rc" = 0 ] || log "CELL FAIL $label: run-cell rc=$rc -> recorded (cells.tsv row + rerun), continuing"
    return 0
}

run_spec() { # SPEC [SUFFIX]: one cell, then its write-up step (cells.tsv row, CELL-FLAG + rerun when bad), in sequence.
    # Returns cell()'s code: 0 = go on, 1 = stop. A started cell that cell() failed is passed as bad:<CELL_FAIL>.
    local spec=$1 sfx=${2:-} tg arm dur kind killat label crc xr=
    IFS=: read -r tg arm dur kind killat <<< "$spec"
    tg=$tg$sfx; label=$P-$tg
    STARTED_LABEL= CELL_FAIL=
    cell "$tg" "$arm" "$dur" "$kind" "$killat"; crc=$?
    if [ "$STARTED_LABEL" = "$label" ]; then
        [ "$crc" = 0 ] || { xr=${CELL_FAIL:-check}; log "$label failed the campaign (cell rc $crc): step flags it bad:$xr"; }
        CELL_STEP_SUMMARY_MD5=$(step_summary_md5 "$arm") \
            cell_step "$RUN" "$R/$label" "$label" "$tg" "$arm" "${tg%%-*}" "${BMD5[${ABIN[$arm]}]}" "$spec" "$xr"
    else
        log "$label did not start: no cells.tsv row"
    fi
    return $crc
}

# ---- preflight ----
PF() { echo "PREFLIGHT FAIL: $*"; exit 1; }

wrapper_target() { # FILE: the absolute path a wrapper script execs (its last `exec PATH` line); fails on a binary
    local t
    grep -qI . "$1" 2>/dev/null || return 1
    t=$(sed -n -E 's/^[[:space:]]*exec[[:space:]]+"?([^" ]+)"?.*/\1/p' "$1" | tail -1)
    [[ "$t" == /* ]] && [ -x "$t" ] || return 1
    echo "$t"
}

preflight_static() { # paths, tools, staged md5s (a wrapper's md5 = the md5 of the node it execs)
    local f b node m tgt
    for f in "$HARNESS" "$DETACH" "$TOOLS/cell-step.sh" "$TOOLS/cell_step.py"; do [ -f "$f" ] || PF "$f missing"; done
    [ -d "$WT_H" ] || PF "harness worktree $WT_H missing"
    for b in "${!BDIR[@]}"; do
        node=${BDIR[$b]}/release/torus-node
        [ -x "$node" ] || PF "binary $b: $node missing or not executable"
        if [ "${BWRAP[$b]}" = 1 ]; then
            tgt=$(wrapper_target "$node") || PF "binary $b: $node is declared a wrapper but has no 'exec /abs/path' line to an executable"
            m=$(md5sum < "$tgt" | cut -c1-32)
            [ "$m" = "${BMD5[$b]}" ] || PF "binary $b: wrapper $node execs $tgt, md5 $m != staged ${BMD5[$b]}"
            grep -q -- '--no-summary-md5' "$TOOLS/cell_step.py" || PF "$TOOLS/cell_step.py lacks --no-summary-md5 (wrapper arm $b): copy current tools"
        else
            m=$(md5sum < "$node" | cut -c1-32)
            [ "$m" = "${BMD5[$b]}" ] || PF "binary $b: $node md5 $m != staged ${BMD5[$b]}"
        fi
        if [ -n "${BBMD5[$b]}" ]; then
            m=$(md5sum < "${BDIR[$b]}/release/bench-throughput" 2>/dev/null | cut -c1-32)
            [ "$m" = "${BBMD5[$b]}" ] || PF "binary $b: bench-throughput md5 ${m:-missing} != staged ${BBMD5[$b]}"
        fi
    done
    for f in $ANALYSIS_CMD; do case $f in */*.py|*/*.sh) [ -f "$f" ] || PF "analysis script $f missing";; esac; done
    return 0
}

preflight_host() { # run mode only: inside a bench unit, nothing else running, no old attempt
    local c g self other
    self=$(grep -oE 'bench-[^/]*\.service' /proc/self/cgroup | tail -1)
    [ -n "$self" ] || PF "not inside a bench-*.service unit: start it with: $SELF launch${SMOKE:+ smoke} (detach.sh holds the build lock)"
    other=$(systemctl --user list-units 'bench-*.service' --state=active --no-legend --plain 2>/dev/null | awk '{print $1}' | grep -vxF "$self")
    [ -z "$other" ] || PF "other bench units active: $(echo $other)"
    [ -z "$(buildhits)" ] || PF "build running: $(buildhits)"
    [ -z "$(benchhits)" ] || PF "node or bench running: $(benchhits)"
    for f in cells.tsv rerun-queue.tsv; do [ ! -e "$RUN/$f" ] || PF "$RUN/$f exists (old attempt: move it away first)"; done
    for c in "${CELLS[@]}"; do g=${c%%:*}
        for x in "$P-$g" "$P-$g-rerun"; do [ ! -e "$R/$x" ] && [ ! -e "$R/$x.cell.rc" ] || PF "$R/$x exists (old attempt)"; done
    done
    [ "$(df --output=avail -BM "$R" | tail -1 | tr -dc 0-9)" -ge "$MIN_FREE_MB" ] || PF "< $MIN_FREE_MB MB free under $R"
}

scrub_env() { # the units get this process's exported env (detach.sh): drop everything but the basics, so no
    # TORUS_*, MALLOC_CONF, LD_PRELOAD or run-cell.sh knob from the launching shell reaches a cell
    local v
    for v in $(compgen -e); do
        case $v in PATH|HOME|USER|LOGNAME|SHELL|LANG|LC_*|TZ|TMPDIR|XDG_RUNTIME_DIR|DBUS_SESSION_BUS_ADDRESS) ;; *) unset "$v" ;; esac
    done
    export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
}

[ "${DRIVER_LIB_ONLY:-0}" = 1 ] && return 0

# ---- main ----
MODE=${1:-}; SM=${2:-}
case "$MODE:$SM" in
    launch:|launch:smoke|dry:|dry:smoke|run:|run:smoke|show:|show:smoke) ;;
    *) echo "usage: $0 launch|dry|show|run [smoke]   (see the header)"; exit 2 ;;
esac
if [ "$MODE" = show ]; then show_plan "$SM"; exit 0; fi
[ "$SM" = smoke ] && apply_smoke
TAGS=$(for c in "${CELLS[@]}"; do printf '%s ' "${c%%:*}"; done)

case $MODE in
dry)
    preflight_static
    echo "PREFLIGHT OK (dry: host state not checked) CAMPAIGN=$UNIT_NAME RUN=$RUN P=$P arms=${ARMS[*]} baseline=$BASELINE markets=$MARKETS rate=$RATE cells=${#CELLS[@]}: $TAGS"
    exit 0 ;;
launch)
    preflight_static
    grep -qE '/bench-[^/]*\.service$' /proc/self/cgroup && PF "launch from outside a bench unit (this shell is inside one)"
    [ ! -e "$RUN/campaign.done" ] || PF "$RUN/campaign.done exists (old attempt: move it away first)"
    mkdir -p "$RUN" || PF "cannot create $RUN"
    scrub_env
    exec "$DETACH" "$UNIT_NAME" "$RUN/campaign.log" "$SELF" run $SM ;;
esac

# run
mkdir -p "$RUN" || exit 1
trap 'rc=$?; echo "exit=$rc $(date +%FT%T)" > "$RUN/campaign.done"' EXIT
scrub_env
preflight_static
preflight_host
export CELL_STEP_LOG=$RUN/campaign.log
export CELL_STEP_TIMEOUT=${CELL_STEP_TIMEOUT:-900} CELL_STEP_ANALYSIS_TIMEOUT=${CELL_STEP_ANALYSIS_TIMEOUT:-600}
[ -z "$ANALYSIS_CMD" ] || export CELL_STEP_ANALYSIS_CMD=$ANALYSIS_CMD
[ -z "$ANALYSIS_METRIC" ] || export CELL_STEP_ANALYSIS_METRIC=$ANALYSIS_METRIC
# shellcheck source=../cell-step.sh
source "$TOOLS/cell-step.sh" || PF "cannot source $TOOLS/cell-step.sh"
log "PREFLIGHT OK CAMPAIGN=$UNIT_NAME driver=$SELF RUN=$RUN P=$P arms=${ARMS[*]} baseline=$BASELINE markets=$MARKETS rate=$RATE cells=${#CELLS[@]}: $TAGS"
for c in "${CELLS[@]}"; do
    run_spec "$c" || { log "CAMPAIGN STOP at ${c%%:*} (queued reruns not run: $RUN/rerun-queue.tsv)"; exit 1; }
done
log "MAIN CELLS END"
if [ "${SMOKE:-0}" != 1 ]; then
    cell_step_rerun_pass "$RUN" run_spec || { log "CAMPAIGN STOP in the rerun pass"; exit 1; }
fi
log "CAMPAIGN END"
[ -s "$RUN/cells.tsv" ] && awk -F'\t' 'NR > 1 {n++; if ($8 != "ok") b++} END {printf "cells.tsv: %d rows, %d bad\n", n, b}' "$RUN/cells.tsv"
exit 0
