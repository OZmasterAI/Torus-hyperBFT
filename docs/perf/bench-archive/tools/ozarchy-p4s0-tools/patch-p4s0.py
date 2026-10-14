import sys
TEMPLATE = "/home/oz/bench-results-matched/ozarchy-p4s0-tools/ozarchy-p4s0-campaign.sh"
DEST = TEMPLATE

CONFIG = r'''# p4s0 (Phase 4 step 0 re-profile on the d3cba5e2 head, owner change of plan 18c): one arm, ORACLE_FEED=1, the
# shipped checkpoint default (TORUS_CHECKPOINT_INTERVAL_MS unset = 15 s), commit tail, CLOID_SHARE=0, 300 markets uniform.
unset TORUS_MODEL_PROFILE CRASH_KILL_AT_S TORUS_ROCKSDB_WAL_COMPRESSION TORUS_ROCKSDB_APPEND_CF_CODEC TORUS_TRADE_HISTORY TORUS_ROCKSDB_MAX_TOTAL_WAL_MB TORUS_P3_PROTO TORUS_P3_PROTO_INTERVAL STATE_HASH_ACTIVATION KILL_SIG KILL_WAIT LD_PRELOAD MALLOC_CONF P3RSS_HEAP_PREFIX TORUS_CHECKPOINT_INTERVAL_MS CLOID_SHARE
unset TORUS_CHECKPOINT_MAX_BYTES TORUS_EXEC_PIPELINE TORUS_STATE_HASH_FAILSTOP TORUS_SHUTDOWN_TIMEOUT_SECS
CAMP=P4S0
BUDGET=900
FD_STOP=16384
R=/home/oz/bench-results-matched
REPO=/home/oz/projects/torus-economy/Torus-hyperBFT
DEVNET=/home/oz/torus-wsl-devnet
KNOB=TORUS_CHECKPOINT_INTERVAL_MS=15000   # name only (knobchk): no arm sets it; the 15 s role default is the shipped state
ENABLE_RE='item 6 Phase 3: coalesced state checkpoints ON'
PIPE_RE=${PIPE_RE:-bl2 exec pipeline ENABLED}
SHA_HARNESS=d3cba5e2837dea109acb4077f625b0991e0e1ac3
WT_H=/home/oz/projects/wt/g3s0-d3cba5e2
HARNESS=$WT_H/tools/matched-bench/run-cell.sh
DETACH=$WT_H/tools/matched-bench/campaign/detach.sh
TOOLS=$R/ozarchy-p4s0-tools/matched-bench
T=$R/ozarchy-14236fa-tools
T3=$R/ozarchy-p3s0r-tools
SIDE=$R/ozarchy-p4s0-tools/perf-sidecar.sh
PYCHK=$R/ozarchy-acc-codec-check.py
LSM=$R/ozarchy-acc3-tools/ozarchy-acc3-lsm.py
RSTPY=$R/ozarchy-acc3-tools/ozarchy-acc3-restart.py
POST=$R/ozarchy-p3s2g-tools/ozarchy-p3c1-post.py
ST=${ST:-$R/ozarchy-g3s0-stage}
MD5_BUILD=${MD5_BUILD:-$R/ozarchy-g3s0-build/md5s.txt}
JEMALLOC=/usr/lib/libjemalloc.so.2
MK=300
METS=(9161 9162 9163)
CRASHCELL=0
# binary: key m = d3cba5e2 node (jemalloc compiled in) + bench built at d3cba5e2; md5s from the build record
declare -A BSHA BWT BMD5 BBMD5
BSHA[m]=d3cba5e2837dea109acb4077f625b0991e0e1ac3; BWT[m]=$WT_H
BMD5[m]=$(awk '$1=="M" && $2=="d3cba5e2" {print $3}' "$MD5_BUILD" 2>/dev/null | tail -1)
BBMD5[m]=$(awk '$1=="bench-M" && $2=="d3cba5e2" {print $3}' "$MD5_BUILD" 2>/dev/null | tail -1)
# EXTRA_ENV knobs: the same empty-knob list as ozarchy-p3cloidec (kept for comparability with that step-0 reference)
KNOBS="TORUS_ROCKSDB_PIPELINED_WRITE= TORUS_RESIDENT_BOOKS= TORUS_NATIVE_ROOT_CACHE= TORUS_PARALLEL_SETTLE= TORUS_PARALLEL_BUCKET_HASH= TORUS_BUCKET_MEMBER_CACHE_MB= TORUS_BOOK_ROWS="
# arms: ABIN binary key, ATAIL commit, AKNOB 0 (no checkpoint knob: shipped default), APROF none|perf (perf = sidecar on val1), ACLOID 0, AORC 1 (oracle on)
declare -A ABIN ATAIL AKNOB APROF KNOBX WALCAP ACLOID AORC
arm() { ABIN[$1]=$2; ATAIL[$1]=$3; AKNOB[$1]=$4; APROF[$1]=$5; ACLOID[$1]=$6; AORC[$1]=$7; }
arm m m commit 0 none 0 1
arm mperf m commit 0 perf 0 1
DD=$DEVNET
KNOBX[tw]=""; WALCAP[tw]=2147483648
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
RUN=$R/ozarchy-p4s0-run; P=ozarchy-p4s0-300m
# tag:arm:dur:kind:killat. Order: warm-up, profile cells p1/p2 (perf on val1), clean SAT cells u1/u2, one 600 s soak k1.
CELLS=( "w0:m:60:none:"
        "p1-mperf:mperf:120:none:" "p2-mperf:mperf:120:none:"
        "u1-m:m:120:none:" "u2-m:m:120:none:"
        "k1-m:m:600:none:" )
# SMOKE=1: one 60 s cell without perf and one 120 s cell with the perf sidecar (checks the sidecar and both arms' checks)
if [ "${SMOKE:-0}" = 1 ]; then P=ozarchy-p4s0-smoke-300m; RUN=$RUN/smoke; CELLS=("s0-m:m:60:none:" "s0-mperf:mperf:120:none:"); fi
ANALYST=$R/ozarchy-p4s0-tools/analyst
export CELL_STEP_ANALYSIS_CMD="python3 -I $ANALYST/a-cells.py {out} {prefix} {tag}:{arm}"
export CELL_STEP_ANALYSIS_METRIC=ms
export CELL_STEP_TIMEOUT=${CELL_STEP_TIMEOUT:-900} CELL_STEP_ANALYSIS_TIMEOUT=${CELL_STEP_ANALYSIS_TIMEOUT:-600}
'''

SAMPLERS = r'''    SP=
    envchk "$R/$label" & EP=$!
    preloadrec "$R/$label" & LP=$!
    "$T/cpu-sampler.sh" "$R/$label" 2>/dev/null & CP=$!
    "$T3/io-sampler.sh" "$R/$label" 2>/dev/null & IP=$!
    dusamp "$R/$label" & DP=$!
    ckptsamp "$R/$label" & KP=$!
    memsamp "$R/$label" & MP=$!
    python3 "$T/task-sampler.py" "$R/$label" & TP=$!
    fdsampler "$R/$label" & FP=$!
    if [ "${APROF[$arm]}" = perf ]; then "$SIDE" "$R/$label" 2>/dev/null & SP=$!; fi
    E=(TARGET_DIR="$td" TOOLS_DIR="$TOOLS" BLOCK_CAP=400 OVERWRITE=1 RETRY_BUSY=1 MAX_IN_FLIGHT="$mif" OPEN_ORDER_BUDGET="$bud"
       ORACLE_FEED="${AORC[$arm]}" ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0 DATA_ROOT="$DD" IN_FLIGHT_TAIL="$tail"
       CLOID_SHARE="${ACLOID[$arm]}")
    [ "$kind" = none ] || E+=(CRASH_KILL_AT_S="$killat" KILL_SIG="$sig" KILL_WAIT=75)
'''

CHECKS = r'''    # the perf sidecar (if any) ends by itself; collect it before the checks read its files
    [ -n "$SP" ] && { wait "$SP" 2>/dev/null; SP=; }
    if [ -s "$R/$label/node-environ-trie.txt" ]; then
        [ "$(grep -c "exe_md5=$nm" "$R/$label/node-environ-trie.txt")" = 3 ] || { echo "CHECK FAIL $label: node exe md5 != $nm"; return 1; }
        envok "$label" tw || return 1
        knobok "$label" tw "$R/$label/node-env-all.txt" 3 || return 1
        knobchk "$label" "$arm" "$R/$label/node-env-all.txt" 3 || return 1
        oraclechk "$label" "$arm" || return 1
        enablechk_m "$label" "$arm" || return 1
        ckptchk_m "$label" "$arm" || return 1
        statechk "$label" "$arm" || return 1
        fschk "$label" "$arm" || return 1
        tailchk "$label" "$tail" || return 1
        heapchk "$label" "$arm" || return 1
        cloidchk "$label" "$arm" || return 1
        n=$(grep -c ' TORUS_SHUTDOWN_TIMEOUT_SECS=' "$R/$label/node-env-all.txt"); [ "$n" = 0 ] || { echo "CHECK FAIL $label: TORUS_SHUTDOWN_TIMEOUT_SECS set on $n nodes (want the default 60)"; return 1; }
        echo "CHECK OK $label TORUS_SHUTDOWN_TIMEOUT_SECS unset on 3 nodes (node default 60 s)"
    else
        echo "CHECK FAIL $label: no node-environ-trie.txt"; return 1
    fi
'''

PREFLIGHT = r'''# checks added for p4s0: the d3cba5e2 arm, the oracle / checkpoint helpers used by the per-cell checks
oraclechk() { # label arm: the oracle feed runs on the ORACLE_FEED=1 arms (run.log "oracle feed pid" line), on no others
    local label=$1 arm=$2 n
    n=$(grep -c -E 'oracle feed pid [0-9]+;' "$R/$label/run.log" 2>/dev/null); n=${n:-0}
    if [ "${AORC[$arm]}" = 1 ]; then
        [ "$n" -ge 1 ] || { echo "CHECK FAIL $label: no oracle feed line in run.log (arm $arm, ORACLE_FEED=1)"; return 1; }
    else
        [ "$n" = 0 ] || { echo "CHECK FAIL $label: oracle feed line on ORACLE_FEED=0 arm $arm"; return 1; }
    fi
    echo "CHECK OK $label oracle (arm $arm: ORACLE_FEED=${AORC[$arm]}; feed lines $n)"
}
enablechk_m() { # label arm: the Phase 3 enable line on each validator (checkpoints are on by default on the m binary); prints val0's line (policy = the 15 s default)
    local label=$1 arm=$2 o=$R/$1 v n tot=""
    for v in 0 1 2; do
        n=$(zcat "$o/val$v.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -c -E "$ENABLE_RE")
        [ "$n" -ge 1 ] || { echo "CHECK FAIL $label: val$v has no enable line /$ENABLE_RE/ (arm $arm)"; return 1; }
        tot="$tot val$v=$n"
    done
    echo "CHECK OK $label checkpoint enable line (arm $arm:$tot): $(zcat "$o/val0.log.gz" | sed 's/\x1b\[[0-9;]*m//g' | grep -m1 -E "$ENABLE_RE" | cut -c1-260)"
}
ckptchk_m() { # label arm: torus_exec_checkpoints delta >= 1 on each node (the 15 s default checkpoints run inside every SAT window)
    local label=$1 arm=$2 o=$R/$1 v b a d tot=""
    for v in 0 1 2; do
        b=$(ckval "$o/metrics-before-val$v.txt"); a=$(ckval "$o/metrics-after-val$v.txt")
        [ "$b" = absent ] && b=0
        { [ "$a" = absent ] || [ -z "$a" ]; } && { echo "CHECK FAIL $label: val$v torus_exec_checkpoints absent after the cell (arm $arm)"; return 1; }
        d=$(( a - b ))
        [ "$d" -ge 1 ] || { echo "CHECK FAIL $label: val$v torus_exec_checkpoints delta=$d (arm $arm)"; return 1; }
        tot="$tot val$v=$d"
    done
    echo "CHECK OK $label checkpoints delta (arm $arm:$tot)"
}
# preflight (binary, harness and tools copy)
for b in m; do
    [[ "${BMD5[$b]}" =~ ^[0-9a-f]{32}$ ]] || PF "no 'M d3cba5e2' node md5 line in $MD5_BUILD"
    [[ "${BBMD5[$b]}" =~ ^[0-9a-f]{32}$ ]] || PF "no 'bench-M d3cba5e2' bench md5 line in $MD5_BUILD"
    { [ "$(git -C "${BWT[$b]}" rev-parse HEAD)" = "${BSHA[$b]}" ] && [ -z "$(git -C "${BWT[$b]}" status --porcelain)" ]; } || PF "${BWT[$b]} not clean at ${BSHA[$b]}"
    [ "$(md5sum < "$ST/$b-run/release/torus-node" | cut -c1-32)" = "${BMD5[$b]}" ] || PF "$ST/$b-run torus-node md5 is not ${BMD5[$b]}"
    [ "$(md5sum < "$ST/$b-run/release/bench-throughput" | cut -c1-32)" = "${BBMD5[$b]}" ] || PF "$ST/$b-run bench md5 is not ${BBMD5[$b]}"
    n=$(nm "$ST/$b-run/release/torus-node" 2>/dev/null | grep -c '_rjem_'); [ "$n" -gt 0 ] || PF "$b node has no _rjem_ symbols (jemalloc compiled in expected)"
    echo "[$(date +%T)] $b node ${BMD5[$b]:0:8} _rjem_ symbols: $n; bench ${BBMD5[$b]:0:8}"
done
[ "$(git -C "$WT_H" rev-parse HEAD)" = "$SHA_HARNESS" ] || PF "harness worktree is not at $SHA_HARNESS"
grep -q 'CLOID_SHARE' "$HARNESS" || PF "$HARNESS does not know CLOID_SHARE"
grep -q 'IN_FLIGHT_TAIL' "$HARNESS" || PF "$HARNESS does not know IN_FLIGHT_TAIL"
grep -q 'cloid-share' "$TOOLS/summarize.py" || PF "harness summarize.py lacks ingest.cloids"
grep -q 'checkpoint_deferred_join' "$TOOLS/summarize.py" || PF "harness summarize.py lacks checkpoint_deferred_join"
[ "$(diff -rq -x __pycache__ "$WT_H/tools/matched-bench" "$TOOLS" | wc -l)" = 0 ] || PF "tools copy differs from $WT_H"
[ -f "$POST" ] || PF "$POST missing"
command -v jq > /dev/null || PF "jq missing"
{ [ "$(git -C "$WT_H" rev-parse HEAD)" = "$SHA_HARNESS" ] && [ -z "$(git -C "$WT_H" status --porcelain)" ]; } || PF "$WT_H not clean at $SHA_HARNESS"
[ "$(stat -f -c %T "$DEVNET")" = btrfs ] || PF "$DEVNET is not on btrfs (shared NVMe)"
[ "$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)" -ge 40000000 ] || PF "MemAvailable < 40 GB"
for f in "$PYCHK" "$LSM" "$RSTPY" "$TOOLS/crash-freeze.py" "$HARNESS" "$DETACH" "$T/task-sampler.py" "$T/positions.py" "$SIDE"; do [ -f "$f" ] || PF "$f missing"; done
for f in "$HARNESS" "$DETACH" "$TOOLS/crash-kill.sh" "$T/cpu-sampler.sh" "$T3/io-sampler.sh" "$SIDE"; do [ -x "$f" ] || PF "$f not executable"; done
'''

BLOCKS = {0: CONFIG, 1: SAMPLERS, 2: CHECKS, 3: PREFLIGHT}
lines = open(TEMPLATE).read().split("\n")
out = []
idx = 0
inblk = False
for line in lines:
    s = line.lstrip()
    if s.startswith("# >>> CAMPAIGN-SPECIFIC"):
        assert not inblk and idx < 4, "unexpected >>> marker"
        out.append(line)
        out.extend(BLOCKS[idx].rstrip("\n").split("\n"))
        inblk = True
        continue
    if s.startswith("# <<< CAMPAIGN-SPECIFIC"):
        assert inblk, "<<< without >>>"
        out.append(line)
        inblk = False
        idx += 1
        continue
    if inblk:
        continue
    out.append(line)
assert idx == 4 and not inblk, "expected 4 marker pairs, got %d" % idx
text = "\n".join(out)
old = "s2=${BSHA[s2]:0:8}/${BMD5[s2]:0:8} bench ${BBMD5[s2]:0:8} m="
assert text.count(old) == 1, "PREFLIGHT OK echo line not found exactly once"
text = text.replace(old, "m=")
assert text.count(" knob(s2)=$KNOB") == 1
text = text.replace(" knob(s2)=$KNOB", "")
open(DEST, "w").write(text)
print("patched", DEST, len(out), "lines")
