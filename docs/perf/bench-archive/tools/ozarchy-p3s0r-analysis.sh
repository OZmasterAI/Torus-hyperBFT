#!/usr/bin/env bash
# ozarchy-p3s0r-analysis.sh: after ozarchy-p3s0r-driver.done. Output ozarchy-p3s0r-analysis.txt.
set -u
R=/home/oz/bench-results-matched; T=$R/ozarchy-14236fa-tools; T2=$R/ozarchy-p2s0-tools; T3=$R/ozarchy-p3s0r-tools
P=ozarchy-p3s0r-300m; ELF=$R/ozarchy-p3s0r-stage/m/release/torus-node
[ -z "$(pgrep -x torus-node)" ] || { echo "torus-node running"; exit 1; }
cd "$R" || exit 1
exec > >(tee "$R/ozarchy-p3s0r-analysis.txt") 2>&1
echo "# ozarchy-p3s0r analysis $(date '+%F %T')"
perf buildid-cache --add "$ELF" 2>/dev/null
echo; echo "## 1. cell tables (cells3.py)"; python3 "$T3/cells3.py"
echo; echo "## 2. per-thread CPU per 1k val0 fills (threads.py, tasks.txt, [t_bench0, t_drain])"
for g in m-r1 m-r2 m-r3 m-r4 m-p1 m-p2 k-r1 k-r2; do python3 "$T/threads.py" "$P-$g"; done
for g in c-r1 c-r2 c-r3 c-r4; do python3 "$T/threads.py" "ozarchy-p3s1-300m-$g"; done
for g in m-p1 m-p2; do
    d=$R/$P-$g
    echo; echo "## 3. perf $g"
    if [ ! -s "$d/perf.folded" ]; then
        perf script -F comm,tid,time,period,ip,sym --no-inline -i "$d/perf.data" 2>"$d/perf.script.err" \
            | python3 "$T2/scan.py" "$d/perf" | python3 "$T/fold.py" --period > "$d/perf.folded"
    fi
    awk '{n=$NF; t+=n; if (index($0,";")==0) u+=n} END {printf "%d stacks, %.2f%% unsymbolised\n", NR, (t?100*u/t:0)}' "$d/perf.folded"
    python3 "$T3/p3q.py" "$d" | tee "$d/p3q.json"
    python3 "$T/buckets2.py" "$d" > "$d/buckets2.json"; cat "$d/buckets2.json"
    echo "### glue"; python3 "$T/glue.py" "$d"
    echo "### flush worker inclusive top 40"; python3 "$T/incl.py" "$d/perf.folded" torus-flush-wor 40
    echo "### rocksdb threads inclusive top 25"; python3 "$T/incl.py" "$d/perf.folded" rocksdb 25
    echo "### trade writer inclusive top 20"; python3 "$T/incl.py" "$d/perf.folded" torus-trade-wri 20
    echo "### end_resident inclusive top 20"; python3 "$T/incl.py" "$d/perf.folded" torus-end-resid 20
    echo "### exec: within flush_all"; python3 "$T/incl.py" "$d/perf.folded" torus-execution 30 flush_all
    echo "### exec: within run_liquidations_with"; python3 "$T/incl.py" "$d/perf.folded" torus-execution 40 run_liquidations_with
    echo "### exec: within delta_sums"; python3 "$T/incl.py" "$d/perf.folded" torus-execution 30 delta_sums
    echo "### gossip inclusive top 20"; python3 "$T/incl.py" "$d/perf.folded" torus-gossip 20
    echo "### ingress inclusive top 20"; python3 "$T/incl.py" "$d/perf.folded" torus-ingress 20
    echo "### hotstuff inclusive top 20"; python3 "$T/incl.py" "$d/perf.folded" hotstuff 20
done
