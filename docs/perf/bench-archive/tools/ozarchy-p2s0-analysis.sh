#!/usr/bin/env bash
# ozarchy-p2s0-analysis.sh: item 6 Phase 2 step 0 profile numbers from the ozarchy-p2s0-campaign.sh cells.
# Run after ozarchy-p2s0.done exists, on a quiet host (perf script + llvm-addr2line take a few CPU-minutes).
# Output: ozarchy-p2s0-analysis.txt (all sections) + per-cell p2s0-*.json, perf*.folded, *.script.gz, *.tids.json.
# Idempotent: existing folded/script files are reused.
#   perf cells: crab-r2 (walk 0), main-r2, crab-w10 (walk 10 + feed drain; load window + drain window)
#   per perf window: fold (perf script | scan.py | fold.py --period), buckets2.py, glue.py, incl.py, p2s0.py window,
#                    inlfn.py (inline-expanded: cancel/modify scan, flush_all, diff_stop_rows, sums)
#   crab-w10: p2s0.py timeline load + drain (rows 77/78), logwin.py on the drain window, drain.json feed_live
#   all cells: cells.py, threads.py, p2s0.py threads, R rebuild (row 7)
set -u
R=/home/oz/bench-results-matched
T=$R/ozarchy-14236fa-tools
T2=$R/ozarchy-p2s0-tools
S2=$R/ozarchy-step2-tools
P=ozarchy-p2s0-300m
ELF_C=$R/ozarchy-mif2-stage/crab/release/torus-node
ELF_M=$R/ozarchy-mif-stage/main/release/torus-node
OUT=$R/ozarchy-p2s0-analysis.txt
[ -z "$(pgrep -x torus-node)" ] || { echo "torus-node is running: a campaign is live, analyse later"; exit 1; }
[ -e "$R/ozarchy-p2s0.done" ] || echo "WARNING: $R/ozarchy-p2s0.done missing (campaign not finished?)"
cd "$R" || exit 1
exec > >(tee "$OUT") 2>&1
echo "# ozarchy-p2s0 analysis $(date '+%F %T')   N=$(grep -m1 -oE 'N=[0-9]+' "$R/ozarchy-p2s0.log" 2>/dev/null)"
perf buildid-cache --add "$ELF_C" 2>/dev/null; perf buildid-cache --add "$ELF_M" 2>/dev/null

pfold() { # cell_dir data prefix: perf script (with time) -> prefix.folded, prefix.exec.script.gz, prefix.tids.json
    local d=$1 data=$2 pre=$3
    [ -s "$d/$data" ] || { echo "  no $d/$data"; return 1; }
    [ -s "$d/$pre.folded" ] && [ -s "$d/$pre.tids.json" ] && return 0
    perf script -F comm,tid,time,period,ip,sym --no-inline -i "$d/$data" 2>"$d/$pre.script.err" \
        | python3 "$T2/scan.py" "$d/$pre" | python3 "$T/fold.py" --period > "$d/$pre.folded"
    awk '{n=$NF; t+=n; if (index($0,";")==0) u+=n} END {printf "  %s: %d stacks, %.2f%% of period with no symbolised frame\n", FILENAME, NR, (t?100*u/t:0)}' "$d/$pre.folded"
}

inlscript() { # cell_dir label: exec-thread script with symoff for inl.py (unique basename: inl.py caches by basename)
    local d=$1 s=$1/$2.inl.script.gz
    [ -s "$s" ] || perf script -F comm,tid,time,period,ip,sym,symoff --no-inline --comms torus-execution -i "$d/perf.data" 2>/dev/null | gzip -1 > "$s"
    echo "$s"
}

drain_dir() { # cell_dir: <cell>/drain/ with the original file names, so buckets2.py / glue.py read the drain window
    local d=$1
    mkdir -p "$d/drain"
    ln -sf ../perf.drain.folded "$d/drain/perf.folded"
    ln -sf ../prof-drain-before.ts "$d/drain/prof-before.ts"; ln -sf ../prof-drain-after.ts "$d/drain/prof-after.ts"
    ln -sf ../prof-stat-drain-before.txt "$d/drain/prof-stat-before.txt"; ln -sf ../prof-stat-drain-after.txt "$d/drain/prof-stat-after.txt"
    ln -sf ../prof-metrics-drain-before.txt "$d/drain/prof-metrics-before.txt"; ln -sf ../prof-metrics-drain-after.txt "$d/drain/prof-metrics-after.txt"
}

echo; echo "## 1. Cells (cells.py) and crab/main ratios"
LABS=""; for g in crab-warm crab-r1 main-r1 crab-r2 main-r2 crab-w10; do [ -f "$R/$P-$g/summary.json" ] && LABS="$LABS $P-$g"; done
# shellcheck disable=SC2086
python3 "$T/cells.py" $LABS
python3 - "$R" "$P" <<'EOF'
import json, sys
R, P = sys.argv[1], sys.argv[2]
def m(g):
    try: return json.load(open(f"{R}/{P}-{g}/summary.json"))["headline"]["matched_s_avg"]
    except (OSError, KeyError, ValueError): return None
for a, b in (("crab-r1", "main-r1"), ("crab-r2", "main-r2"), ("crab-w10", "main-r2"), ("crab-w10", "crab-r2")):
    x, y = m(a), m(b)
    print(f"  {a}/{b} matched/s: " + (f"{x/y:.3f}x ({x:.0f} / {y:.0f})" if x and y else "n/a"))
print("  (r2 and w10 cells run perf on val0: 4-12% matched/s cost; compare r1 for the clean ratio)")
EOF

echo; echo "## 2. Thread CPU (threads.py, ms CPU per 1k val0 fills) and thread ids per minute"
# shellcheck disable=SC2086
python3 "$T/threads.py" $LABS
for l in $LABS; do python3 "$T2/p2s0.py" threads "$R/$l"; done

echo; echo "## 3. R rebuild at process start (row 7)"
for l in $LABS; do python3 "$T2/p2s0.py" rebuild "$R/$l"; done

for g in crab-r2 main-r2 crab-w10; do
    d=$R/$P-$g
    case $g in main*) elf=$ELF_M;; *) elf=$ELF_C;; esac
    echo; echo "## 4. Load window profile: $P-$g (perf.data, 35 s after bench start, 45 s)"
    pfold "$d" perf.data perf || continue
    python3 "$T/buckets2.py" "$d" > "$d/p2s0-buckets2.json" && cat "$d/p2s0-buckets2.json"
    echo "  -- glue.py (execute_batch_phases glue by first frame below, ms/1k)"; python3 "$T/glue.py" "$d"
    echo "  -- incl.py torus-execution (top 40)"; python3 "$T/incl.py" "$d/perf.folded" torus-execution 40
    python3 "$T2/p2s0.py" window "$d" load
    python3 "$T2/inlfn.py" "$d" "$elf" "$(inlscript "$d" "$P-$g")"
done

d=$R/$P-crab-w10
if [ -s "$d/perf.drain.data" ]; then
    echo; echo "## 5. crab-w10 drain window (perf.drain.data from the 'bench exited rc=' line)"
    pfold "$d" perf.drain.data perf.drain
    drain_dir "$d"
    python3 "$T/buckets2.py" "$d/drain" > "$d/p2s0-buckets2-drain.json" 2>&1 && cat "$d/p2s0-buckets2-drain.json" \
        || echo "  buckets2.py on the drain window failed (no fills in the window?): $(tail -1 "$d/p2s0-buckets2-drain.json")"
    echo "  -- incl.py torus-execution (top 40), drain window"; python3 "$T/incl.py" "$d/perf.drain.folded" torus-execution 40
    python3 "$T2/p2s0.py" window "$d" drain
    echo; echo "## 6. crab-w10 per-block timelines (rows 77/78)"
    python3 "$T2/p2s0.py" timeline "$d" drain
    python3 "$T2/p2s0.py" timeline "$d" load
    t0=$(cat "$d/prof-drain-before.ts"); t1=$(cat "$d/prof-drain-after.ts")
    echo "  -- logwin.py on the drain window (val0, consecutive native blocks)"
    python3 "$S2/logwin.py" "$d/val0.log.gz" "$t0" "$t1" --csv "$d/p2s0-logwin-drain.csv" || echo "  logwin.py: no consecutive native pairs"
    echo "  -- harness feed-live drain (drain.json .feed_live, oracle-only proxy = chain ms per native block in the quiet window)"
    grep -E 'feed-live drain:|drained=' "$d/run.log"
    python3 -c 'import json,sys; print(" ", json.dumps(json.load(open(sys.argv[1])).get("feed_live")))' "$d/drain.json"
else
    echo; echo "## 5/6. crab-w10: no perf.drain.data"
fi

echo; echo "## 7. Re-value with moving prices: crab walk 0 (crab-r2) vs walk 10 (crab-w10), load windows"
[ -f "$R/$P-crab-r2/p2s0-load.json" ] && [ -f "$R/$P-crab-w10/p2s0-load.json" ] \
    && python3 "$T2/p2s0.py" compare "$R/$P-crab-r2" "$R/$P-crab-w10"
echo; echo "## 8. crab vs main, load windows (crab-r2 vs main-r2)"
[ -f "$R/$P-crab-r2/p2s0-load.json" ] && [ -f "$R/$P-main-r2/p2s0-load.json" ] \
    && python3 "$T2/p2s0.py" compare "$R/$P-main-r2" "$R/$P-crab-r2"
echo; echo "# done $(date '+%F %T') -> $OUT"
