# 8r-checks.sh: campaign checks of ozarchy-8r (bench queue 8r, "8o rerun with the RH4 GC worker", owner s131, archy s68):
# ONE binary perf/rh4-gc-worker a0181b8b. A = no retention; B = TORUS_RETENTION_BLOCKS=1000 (DA body GC on the
# torus-da-gc worker, the branch default); C = TORUS_RETENTION_BLOCKS=1000 + TORUS_DA_GC_INLINE=1 (the old inline GC, as
# 8o's B). All via the arm env (EXTRA_ENV). = ozarchy-8o-tools/8o-checks.sh renamed o8 -> r8, retchk / gcchk / slowchk
# take B|C as 8o's B, plus r8modechk (GC mode line + TORUS_DA_GC_INLINE environ per arm; worker metrics -> <cell>/r8-worker.txt).
# Called as `fn <label> <arm>` in a subshell with R set. 8o's header follows:
# (8o) 8o-checks.sh: campaign checks (make-driver.py spec "checks") of ozarchy-8o (bench queue 8o, "8h rerun: does retention
# cost native blocks/s?", owner s131, archy s68): one binary, main 32648b44 (RH4 + slow-pass logging, retention off by
# default). A = no retention; B = TORUS_RETENTION_BLOCKS=1000 in the arm's env (EXTRA_ENV; start-node.sh ->
# --retention-blocks=1000; DA body GC window 1128).
# Both arms otherwise the code defaults with nothing set (PC 8, F on, trust cache on, re-send 250; 524288 push threshold,
# re-send 250 and custody 0 from env.sh). Called as `fn <label> <arm>` in a subshell with R set.
# Copy of ozarchy-p8h-tools/p8h-checks.sh renamed r8* (cells 300 s, warm-up w1 300 s, smoke 180 s so the smoke reaches
# the first deleting GC pass at ~load + 90 s), plus r8swapchk (8m's k8swapchk) and r8slowchk (new: RH4 slow-pass lines):
#   r8retchk: B = every node's cmdline (r8 sampler record, q9-sampler.py) has --retention-blocks=1000 and its log
#              'DA body GC on: committed bodies deleted' with window=1128, TORUS_RETENTION_BLOCKS=1000 on 3 environs;
#              A = no --retention-blocks, no 'DA body GC on' line, TORUS_RETENTION_BLOCKS on no environ.
#   r8gcchk:  B: torus_da_bodies_gc_deleted(_total) > 0 on every node (metrics-after); A: absent or 0.
#   r8rejchk: order margin rejects (funnel-val<i>.csv): WARN above 2%, FAIL above 20% (the load ran out of margin).
#   r8pushchk, r8loadchk, r8clchk, r8flagchk, r8custchk, r8diskchk: as p8g (see below).
#   r8pushchk: threshold 524288 (env.sh) on 3 node environs, no clamp warning, >= R8_MIN_MANIFEST manifests and every
#   r8loadchk: standard load (bench-queue.md "Standard load", s127): dur 300 s, warm-up w1 300 s, smoke 180 s.
#   r8clchk:   Classic: every node logged 'coalesced state checkpoints ON', none the deferred-book-save warning.
#   r8flagchk: F on (1 'epoch fast path ON' line per node, every boundary block pipelined=true, >= 10), PE off
#   r8custchk: TORUS_SHARD_CUSTODY=0 on 3 environs, custody seconds sum < 0.05 s per node.
#   r8diskchk: fresh launch (CLEAN=1), node data sizes to <cell>/r8-disk.txt, /home >= 30 GB free after the cell.
R8_TC_RE='exec trust-cache ENABLED'
R8_CELL_WANT="markets=300 rate=76000 senders=5000 block_cap='400' band=5 cross=0.5 cancel=0.05 open_order_budget='900' retry_busy='1' max_in_flight='4' in_flight_tail='commit' cloid_share='0'"
R8_BENCH_WANT="--senders 5000,--markets 300,--batch-size 400,--cross-fraction 0.5,--cancel-fraction 0.05,--band 5,--rate-total 76000,--open-order-budget 900,--retry-busy,--max-in-flight 4,--in-flight-tail commit,--cloid-share 0"
R8_ORACLE_RE='oracle feed ON: price=30000 walk_bp=0 shock_bp=0 shock_round=0 interval=2000ms markets=300 '
R8_DEFER_RE='TORUS_BOOK_ROWS selects a deferred book save'
R8_CKPT_RE='coalesced state checkpoints ON'
R8_DB_RE='TORUS_CKPT_DEFER_BOOKS: book CF in the deferred part'
R8_F_RE='epoch fast path ON'
R8_BOUNDARY_RE='executing finalized block height=[0-9]*00 '
R8_PE_METRIC=torus_exec_thread_spawns_margin_prepare
R8_MIN_BOUNDARY=10
R8_MIN_FREE_GB=30
R8_MIN_MANIFEST=20
R8_CLAMP_RE='TORUS_HASH_ONLY_PUSH_THRESHOLD above fleet direct-msg floor'
R8_PREWARM_RE='pre-warm pull of MISSING bodies'

r8_plainlog() { # cell_dir node -> the node log without ANSI colour codes
    zcat "$1/$2.log.gz" 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g'
}

r8_metric() { # file metric_name -> value (first unlabelled sample) or empty
    awk -v k="$2" '$1 == k { print $2; exit }' "$1" 2>/dev/null
}

r8loadchk() {
    local label=$1 arm=$2 o=$R/$1 s cl bl w dur
    s=$o/summary.json
    [ -s "$o/run.log" ] || { echo "CHECK FAIL $label: no run.log"; return 1; }
    [ -s "$s" ] || { echo "CHECK FAIL $label: no summary.json"; return 1; }
    cl=$(grep -m1 -E '^\[[0-9:]+\] cell=' "$o/run.log")
    [ -n "$cl" ] || { echo "CHECK FAIL $label: no cell= line in run.log"; return 1; }
    case "$cl" in *" cell=$label "*) ;; *) echo "CHECK FAIL $label: cell= line names another cell: $cl"; return 1 ;; esac
    for w in $R8_CELL_WANT; do
        case " $cl " in *" $w "*) ;; *) echo "CHECK FAIL $label: load: cell= line lacks $w"; return 1 ;; esac
    done
    dur=$(echo "$cl" | grep -o ' dur=[0-9]*s ' | tr -d ' ')
    case "$label" in
        *-smoke*) [ "$dur" = dur=180s ] ;;
        *-w1) [ "$dur" = dur=300s ] ;;
        *) [ "$dur" = dur=300s ] ;;
    esac || { echo "CHECK FAIL $label: load: $dur (want 300s, warm-up 300s, smoke 180s)"; return 1; }
    bl=$(grep -m1 -E '^\[[0-9:]+\] bench: .* consensus ' "$o/run.log")
    [ -n "$bl" ] || { echo "CHECK FAIL $label: no bench: command line in run.log"; return 1; }
    local IFS=,
    for w in $R8_BENCH_WANT; do
        case "$bl " in *" $w "*) ;; *) echo "CHECK FAIL $label: load: bench command lacks '$w'"; return 1 ;; esac
    done
    IFS=$' \t\n'
    case "$bl" in *tail-depth*) echo "CHECK FAIL $label: load: bench command has a tail depth flag"; return 1 ;; esac
    grep -q -- "$R8_ORACLE_RE" "$o/run.log" || { echo "CHECK FAIL $label: load: no '$R8_ORACLE_RE' line"; return 1; }
    grep -q -E 'oracle feed pid [0-9]+;' "$o/run.log" || { echo "CHECK FAIL $label: load: no oracle feed pid line"; return 1; }
    [ "$(jq -r '(.oracle_feed.oracle_feed == 1) and (.oracle_feed.rc == 0) and (.oracle_feed.interval_ms == 2000) and (.oracle_feed.walk_bp == 0)' "$s")" = true ] \
        || { echo "CHECK FAIL $label: load: summary .oracle_feed $(jq -c '.oracle_feed | {oracle_feed, rc, price, interval_ms, walk_bp}' "$s")"; return 1; }
    [ "$(jq -r '.evm_load == null' "$s")" = true ] || { echo "CHECK FAIL $label: .evm_load not null (native only)"; return 1; }
    echo "CHECK OK $label r8loadchk: arm $arm standard load ($R8_CELL_WANT $dur; bench flags match; oracle ON 30000/2000ms/walk 0 rc 0 accepted $(jq -r '.oracle_feed.accepted' "$s")/$(jq -r '.oracle_feed.sent' "$s"); no EVM load)"
}

r8clchk() {
    local label=$1 arm=$2 o=$R/$1 v d c
    for v in val0 val1 val2; do
        [ -s "$o/$v.log.gz" ] || { echo "CHECK FAIL $label: no $v.log.gz"; return 1; }
        d=$(zcat "$o/$v.log.gz" | grep -c -- "$R8_DEFER_RE"); c=$(zcat "$o/$v.log.gz" | grep -c -- "$R8_CKPT_RE")
        [ "$c" -ge 1 ] && [ "$d" = 0 ] || { echo "CHECK FAIL $label: $v Classic lines: ckpt_on=$c (want >=1) deferred=$d (want 0)"; return 1; }
    done
    echo "CHECK OK $label r8clchk: arm $arm Classic on val0-2 (coalesced checkpoints ON)"
}

r8flagchk() {
    local label=$1 arm=$2 o=$R/$1 v n f pt pf pe tc rs info=""
    case "$arm" in A|B|C) ;; *) echo "CHECK FAIL $label: r8flagchk: unknown arm $arm"; return 1 ;; esac
    arm=PCF # both arms run the code defaults PC + F: the F-on branch applies to both
    [ -s "$o/node-env-all.txt" ] || { echo "CHECK FAIL $label: no node-env-all.txt"; return 1; }
    rs=$(grep -c -E "(^| )TORUS_HEADER_RESEND_MS=250( |$)" "$o/node-env-all.txt")
    [ "$rs" = 3 ] || { echo "CHECK FAIL $label: TORUS_HEADER_RESEND_MS=250 on $rs node environs (want 3, env.sh)"; return 1; }
    for v in val0 val1 val2; do
        [ -s "$o/$v.log.gz" ] || { echo "CHECK FAIL $label: no $v.log.gz"; return 1; }
        [ -s "$o/metrics-after-$v.txt" ] || { echo "CHECK FAIL $label: no metrics-after-$v.txt"; return 1; }
        n=$(r8_plainlog "$o" "$v" | grep -c -- "$R8_DB_RE")
        f=$(r8_plainlog "$o" "$v" | grep -c -- "$R8_F_RE")
        pt=$(r8_plainlog "$o" "$v" | grep -E -- "$R8_BOUNDARY_RE" | grep -c 'pipelined=true')
        pf=$(r8_plainlog "$o" "$v" | grep -E -- "$R8_BOUNDARY_RE" | grep -c 'pipelined=false')
        pe=$(r8_metric "$o/metrics-after-$v.txt" "$R8_PE_METRIC")
        tc=$(r8_plainlog "$o" "$v" | grep -c -- "$R8_TC_RE")
        [ "$tc" = 1 ] || { echo "CHECK FAIL $label: $v '$R8_TC_RE' count=$tc (want 1: trust cache on)"; return 1; }
        [ "$n" = 0 ] || { echo "CHECK FAIL $label: $v has $n TORUS_CKPT_DEFER_BOOKS lines (want 0: DB absent)"; return 1; }
        if [ "$arm" = PCF ]; then
            [ "$f" = 1 ] || { echo "CHECK FAIL $label: $v 'epoch fast path ON' count=$f (want 1: F on)"; return 1; }
            [ "$pf" = 0 ] && [ "$pt" -ge "$R8_MIN_BOUNDARY" ] \
                || { echo "CHECK FAIL $label: $v boundary blocks pipelined=true $pt (want >= $R8_MIN_BOUNDARY) pipelined=false $pf (want 0: F on)"; return 1; }
        else
            [ "$f" = 0 ] || { echo "CHECK FAIL $label: $v 'epoch fast path ON' count=$f (want 0: F off)"; return 1; }
            [ "$pt" = 0 ] && [ "$pf" -ge "$R8_MIN_BOUNDARY" ] \
                || { echo "CHECK FAIL $label: $v boundary blocks pipelined=true $pt (want 0: F off) pipelined=false $pf (want >= $R8_MIN_BOUNDARY)"; return 1; }
        fi
        [ -n "$pe" ] || { echo "CHECK FAIL $label: $v no $R8_PE_METRIC in metrics-after"; return 1; }
        awk -v x="$pe" 'BEGIN { exit !(x == 0) }' || { echo "CHECK FAIL $label: $v $R8_PE_METRIC=$pe (want 0: PE off)"; return 1; }
        info="$info $v:db=$n,fON=$f,trust_cache=$tc,boundary_pipelined=$pt/$((pt + pf)),pe_spawns=$pe"
    done
    echo "CHECK OK $label r8flagchk: arm $2 (F on) took-effect, re-send 250 on 3 environs;$info"
}

r8custchk() {
    local label=$1 arm=$2 o=$R/$1 f v n s c info=""
    f=$o/node-env-all.txt
    [ -s "$f" ] || { echo "CHECK FAIL $label: no node-env-all.txt"; return 1; }
    n=$(grep -c -E "(^| )TORUS_SHARD_CUSTODY=0( |$)" "$f")
    [ "$n" = 3 ] || { echo "CHECK FAIL $label: TORUS_SHARD_CUSTODY=0 on $n node environs (want 3, arm $arm)"; return 1; }
    for v in val0 val1 val2; do
        [ -s "$o/metrics-after-$v.txt" ] || { echo "CHECK FAIL $label: no metrics-after-$v.txt"; return 1; }
        s=$(r8_metric "$o/metrics-after-$v.txt" torus_validate_block_custody_seconds_sum)
        c=$(r8_metric "$o/metrics-after-$v.txt" torus_validate_block_custody_seconds_count)
        [ -n "$s" ] && [ -n "$c" ] || { echo "CHECK FAIL $label: $v no torus_validate_block_custody_seconds sum/count"; return 1; }
        awk -v s="$s" 'BEGIN { exit !(s < 0.05) }' || { echo "CHECK FAIL $label: $v custody seconds sum $s (want < 0.05 s: custody OFF)"; return 1; }
        info="$info $v:custody_sum=${s}s/count=$c"
    done
    echo "CHECK OK $label r8custchk: arm $arm TORUS_SHARD_CUSTODY=0 on 3 nodes;$info"
}

r8diskchk() {
    local label=$1 arm=$2 o=$R/$1 v free
    grep -q 'CLEAN=1' "$o/run.log" || { echo "CHECK FAIL $label: run.log has no CLEAN=1 wipe line (data dirs not fresh)"; return 1; }
    : > "$o/r8-disk.txt"
    for v in val0 val1 val2; do
        if [ -d "/home/oz/torus-wsl-devnet/data/$v" ]; then
            echo "$v data_bytes=$(du -sb "/home/oz/torus-wsl-devnet/data/$v" | cut -f1)" >> "$o/r8-disk.txt"
        else
            echo "$v no data dir" >> "$o/r8-disk.txt"
        fi
    done
    free=$(df --output=avail -BG /home | tail -1 | tr -dc 0-9)
    echo "free_gb_after=$free" >> "$o/r8-disk.txt"
    [ "$free" -ge "$R8_MIN_FREE_GB" ] || { echo "CHECK FAIL $label: /home has ${free} GB free (< $R8_MIN_FREE_GB GB)"; return 1; }
    echo "CHECK OK $label r8diskchk: arm $arm fresh launch (CLEAN=1); $(tr '\n' ';' < "$o/r8-disk.txt")"
}

r8pushchk() {
    local label=$1 arm=$2 o=$R/$1 s want n v m b x cl pw pwb tm=0 tpw=0 info=""
    case "$arm" in A|B|C) want=524288 ;; *) echo "CHECK FAIL $label: r8pushchk: unknown arm $arm"; return 1 ;; esac
    s=$o/summary.json
    [ -s "$o/node-env-all.txt" ] || { echo "CHECK FAIL $label: no node-env-all.txt"; return 1; }
    n=$(grep -c -E "(^| )TORUS_HASH_ONLY_PUSH_THRESHOLD=$want( |$)" "$o/node-env-all.txt")
    [ "$n" = 3 ] || { echo "CHECK FAIL $label: TORUS_HASH_ONLY_PUSH_THRESHOLD=$want on $n node environs (want 3, arm $arm)"; return 1; }
    : > "$o/r8-push.txt"
    for v in val0 val1 val2; do
        [ -s "$o/$v.log.gz" ] || { echo "CHECK FAIL $label: no $v.log.gz"; return 1; }
        m=$(jq -r ".dissemination.$v.manifest // empty" "$s"); b=$(jq -r ".dissemination.$v.body_push // empty" "$s")
        x=$(jq -r ".dissemination.$v.body_push_max_bytes // empty" "$s")
        [ -n "$m" ] && [ -n "$b" ] && [ -n "$x" ] || { echo "CHECK FAIL $label: $v no summary .dissemination counts"; return 1; }
        cl=$(r8_plainlog "$o" "$v" | grep -c -- "$R8_CLAMP_RE")
        read -r pw pwb < <(r8_plainlog "$o" "$v" | grep -- "$R8_PREWARM_RE" | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^count=/) { sub(/^count=/, "", $i); s += $i } n++ } END { print n + 0, s + 0 }')
        echo "$v manifest=$m body_push=$b body_push_max_bytes=$x clamp_warn=$cl prewarm_lines=$pw prewarm_bodies=$pwb" >> "$o/r8-push.txt"
        [ "$cl" = 0 ] || { echo "CHECK FAIL $label: $v logged $cl threshold clamp warnings (want 0)"; return 1; }
        if [ "$want" = 524288 ]; then
            [ "$m" -ge "$R8_MIN_MANIFEST" ] || { echo "CHECK FAIL $label: $v manifest pushes $m (want >= $R8_MIN_MANIFEST: threshold 524288)"; return 1; }
            [ "$x" -le 524288 ] || { echo "CHECK FAIL $label: $v full-body push of $x bytes > 524288 (threshold not in effect)"; return 1; }
        else
            [ "$b" -ge "$R8_MIN_MANIFEST" ] || { echo "CHECK FAIL $label: $v full-body pushes $b (want >= $R8_MIN_MANIFEST)"; return 1; }
            [ $((m * 100)) -le $((m + b)) ] || { echo "CHECK FAIL $label: $v manifests $m of $((m + b)) pushes (want <= 1%: threshold 8000000)"; return 1; }
            [ "$x" -gt 524288 ] || { echo "CHECK FAIL $label: $v max full-body push $x bytes <= 524288 (want > 524288: threshold 8000000)"; return 1; }
        fi
        tm=$((tm + m)); tpw=$((tpw + pw))
        info="$info $v:manifest=$m,body=$b,max_bytes=$x,prewarm=$pw/$pwb"
    done
    if [ "$want" = 524288 ]; then
        [ "$tpw" -ge 1 ] || { echo "CHECK FAIL $label: no node logged a pre-warm pull ($tm manifests; want >= 1 pull)"; return 1; }
    fi
    echo "CHECK OK $label r8pushchk: arm $arm threshold $want on 3 nodes, no clamp;$info"
}

R8_SAMPLES=/home/oz/bench-results-matched/ozarchy-8r-run/r8-samples
R8_MAX_REJ_PCT=2
R8_FAIL_REJ_PCT=20
R8_GC_ON_RE='DA body GC on: committed bodies deleted'

r8_rows() { # label node -> sampler rows of that node (no header)
    awk -F'\t' -v n="$2" 'NR > 1 && $3 == n' "$R8_SAMPLES/$1.tsv" 2>/dev/null
}

r8retchk() {
    local label=$1 arm=$2 o=$R/$1 c v n g w info=""
    c=$R8_SAMPLES/$label.cmdline
    [ -s "$c" ] || { echo "CHECK FAIL $label: no sampler cmdline record $c"; return 1; }
    [ -s "$o/node-env-all.txt" ] || { echo "CHECK FAIL $label: no node-env-all.txt"; return 1; }
    for v in val0 val1 val2; do
        [ -s "$o/$v.log.gz" ] || { echo "CHECK FAIL $label: no $v.log.gz"; return 1; }
        n=$(grep -E "^[0-9.]+ $v pid=[0-9]+ " "$c" | grep -c -- '--retention-blocks=1000')
        g=$(r8_plainlog "$o" "$v" | grep -c -- "$R8_GC_ON_RE")
        w=$(r8_plainlog "$o" "$v" | grep -- "$R8_GC_ON_RE" | grep -c 'window=1128')
        grep -q -E "^[0-9.]+ $v pid=[0-9]+ .*torus-node" "$c" || { echo "CHECK FAIL $label: no $v torus-node cmdline in $c"; return 1; }
        case "$arm" in
            B|C) [ "$n" -ge 1 ] && [ "$g" = 1 ] && [ "$w" = 1 ] || { echo "CHECK FAIL $label: $v retention: cmdline --retention-blocks=1000 lines $n (want >= 1), 'DA body GC on' $g (want 1), window=1128 $w (want 1)"; return 1; } ;;
            A) [ "$n" = 0 ] && ! grep -E "^[0-9.]+ $v " "$c" | grep -q -- '--retention-blocks' && [ "$g" = 0 ] || { echo "CHECK FAIL $label: $v arm A has retention: cmdline lines $n, 'DA body GC on' $g (want 0, 0)"; return 1; } ;;
            *) echo "CHECK FAIL $label: r8retchk: unknown arm $arm"; return 1 ;;
        esac
        info="$info $v:retention_cmdline=$n,gc_on=$g,window1128=$w"
    done
    n=$(grep -c -E "(^| )TORUS_RETENTION_BLOCKS=1000( |$)" "$o/node-env-all.txt")
    case "$arm" in
        B|C) [ "$n" = 3 ] || { echo "CHECK FAIL $label: TORUS_RETENTION_BLOCKS=1000 on $n node environs (want 3)"; return 1; } ;;
        A) [ "$(grep -c 'TORUS_RETENTION_BLOCKS' "$o/node-env-all.txt")" = 0 ] || { echo "CHECK FAIL $label: TORUS_RETENTION_BLOCKS on an arm A environ"; return 1; } ;;
    esac
    echo "CHECK OK $label r8retchk: arm $arm retention as specified (environ TORUS_RETENTION_BLOCKS=1000 on $n);$info"
}

r8gcchk() {
    local label=$1 arm=$2 o=$R/$1 v d s c info=""
    for v in val0 val1 val2; do
        [ -s "$o/metrics-after-$v.txt" ] || { echo "CHECK FAIL $label: no metrics-after-$v.txt"; return 1; }
        d=$(r8_metric "$o/metrics-after-$v.txt" torus_da_bodies_gc_deleted_total)
        [ -n "$d" ] || d=$(r8_metric "$o/metrics-after-$v.txt" torus_da_bodies_gc_deleted)
        s=$(r8_metric "$o/metrics-after-$v.txt" torus_da_gc_seconds_sum)
        c=$(r8_metric "$o/metrics-after-$v.txt" torus_da_gc_seconds_count)
        case "$arm" in
            B|C) [ -n "$d" ] && awk -v x="$d" 'BEGIN { exit !(x > 0) }' || { echo "CHECK FAIL $label: $v torus_da_bodies_gc_deleted='$d' (want > 0, arm $arm)"; return 1; } ;;
            A) [ -z "$d" ] || awk -v x="$d" 'BEGIN { exit !(x == 0) }' || { echo "CHECK FAIL $label: $v torus_da_bodies_gc_deleted=$d on arm A (want absent or 0)"; return 1; } ;;
            *) echo "CHECK FAIL $label: r8gcchk: unknown arm $arm"; return 1 ;;
        esac
        info="$info $v:gc_deleted=${d:-absent},gc_s_sum=${s:-absent},gc_count=${c:-absent}"
    done
    echo "CHECK OK $label r8gcchk: arm $arm;$info"
}

r8rejchk() {
    local label=$1 arm=$2 o=$R/$1 v f r p pct info="" worst=0
    for v in val0 val1 val2; do
        f=$o/funnel-$v.csv
        [ -s "$f" ] || { echo "CHECK FAIL $label: no funnel-$v.csv"; return 1; }
        read -r r p < <(awk -F, 'NR == 2 { r0 = $6; p0 = $3 } NR > 1 { r1 = $6; p1 = $3 } END { printf "%d %d\n", r1 - r0, p1 - p0 }' "$f")
        pct=$(awk -v r="$r" -v p="$p" 'BEGIN { if (r + p > 0) printf "%.3f", 100 * r / (r + p); else print "0" }')
        info="$info $v:rej_margin=$r,placed_accepted=$p,pct=$pct"
        awk -v x="$pct" -v w="$worst" 'BEGIN { exit !(x > w) }' && worst=$pct
    done
    awk -v x="$worst" -v m="$R8_FAIL_REJ_PCT" 'BEGIN { exit !(x <= m) }' || { echo "CHECK FAIL $label: margin rejects ${worst}% of orders (want <= $R8_FAIL_REJ_PCT%: the load collapsed, out of margin);$info"; return 1; }
    awk -v x="$worst" -v m="$R8_MAX_REJ_PCT" 'BEGIN { exit !(x <= m) }' || { echo "CHECK WARN $label: r8rejchk: margin rejects ${worst}% of orders (> $R8_MAX_REJ_PCT%: the load ran short of margin; recorded, the campaign goes on);$info"; return 0; }
    echo "CHECK OK $label r8rejchk: arm $arm margin rejects <= ${worst}%;$info"
}

# ---- added for 8o ----
R8_SWAP0_KB=$(awk 'NR > 1 { s += $4 } END { print s + 0 }' /proc/swaps)
R8_SWAP_MAX_GROWTH_KB=524288
R8_SLOW_RE='DA body GC: slow pass'
R8_SLOWCFG_RE='DA body GC: a pass at or over slow_ms logs one warn line'

r8swapchk() { # swap used now vs at preflight (driver sources this file once): FAIL (campaign stops) above +512 MB
    local label=$1 arm=$2 o=$R/$1 now
    now=$(awk 'NR > 1 { s += $4 } END { print s + 0 }' /proc/swaps)
    echo "swap_used_kb_preflight=$R8_SWAP0_KB swap_used_kb_now=$now" > "$o/r8-swap.txt"
    [ "$now" -le $((R8_SWAP0_KB + R8_SWAP_MAX_GROWTH_KB)) ] \
        || { echo "CHECK FAIL $label: swap used ${now} kB grew > 512 MB over the preflight ${R8_SWAP0_KB} kB (stop on swap growth)"; return 1; }
    echo "CHECK OK $label r8swapchk: arm $arm swap used ${now} kB (preflight ${R8_SWAP0_KB} kB, limit +512 MB)"
}

r8slowchk() { # RH4 slow-pass logging (32648b44): B = every node logged the slow_ms=50 config line once, its
    # 'DA body GC: slow pass' WARN lines (copied to <cell>/r8-slowpass.txt) equal torus_da_gc_slow_passes(_total) and
    # torus_da_gc_last_pass_max_seconds is exported; A = no config line, no slow-pass line, counter absent or 0.
    local label=$1 arm=$2 o=$R/$1 v cfg c50 n k info=""
    : > "$o/r8-slowpass.txt"
    for v in val0 val1 val2; do
        [ -s "$o/$v.log.gz" ] || { echo "CHECK FAIL $label: no $v.log.gz"; return 1; }
        [ -s "$o/metrics-after-$v.txt" ] || { echo "CHECK FAIL $label: no metrics-after-$v.txt"; return 1; }
        cfg=$(r8_plainlog "$o" "$v" | grep -c -- "$R8_SLOWCFG_RE")
        c50=$(r8_plainlog "$o" "$v" | grep -- "$R8_SLOWCFG_RE" | grep -c 'slow_ms=50')
        r8_plainlog "$o" "$v" | grep -- "$R8_SLOW_RE" | sed "s/^/$v /" >> "$o/r8-slowpass.txt"
        n=$(grep -c "^$v " "$o/r8-slowpass.txt")
        k=$(r8_metric "$o/metrics-after-$v.txt" torus_da_gc_slow_passes_total)
        [ -n "$k" ] || k=$(r8_metric "$o/metrics-after-$v.txt" torus_da_gc_slow_passes)
        case "$arm" in
            B|C) [ "$cfg" = 1 ] && [ "$c50" = 1 ] || { echo "CHECK FAIL $label: $v slow-pass config line $cfg, with slow_ms=50 $c50 (want 1, 1)"; return 1; }
               [ -n "$k" ] && awk -v a="$k" -v b="$n" 'BEGIN { exit !(a == b) }' || { echo "CHECK FAIL $label: $v slow-pass WARN lines $n != torus_da_gc_slow_passes '$k'"; return 1; }
               grep -q '^torus_da_gc_last_pass_max_seconds ' "$o/metrics-after-$v.txt" || { echo "CHECK FAIL $label: $v no torus_da_gc_last_pass_max_seconds"; return 1; } ;;
            A) [ "$cfg" = 0 ] && [ "$n" = 0 ] && { [ -z "$k" ] || awk -v a="$k" 'BEGIN { exit !(a == 0) }'; } || { echo "CHECK FAIL $label: $v arm A: slow-pass config $cfg, WARN lines $n, counter '${k:-absent}' (want 0, 0, absent/0)"; return 1; } ;;
            *) echo "CHECK FAIL $label: r8slowchk: unknown arm $arm"; return 1 ;;
        esac
        info="$info $v:slow_lines=$n,slow_counter=${k:-absent}"
    done
    echo "CHECK OK $label r8slowchk: arm $arm;$info"
}

# ---- added for 8r ----
R8_WORKER_RE='passes run on the torus-da-gc worker (TORUS_DA_GC_INLINE)'
R8_INLINE_RE='passes run on the consensus thread (TORUS_DA_GC_INLINE)'

r8_held() { # file reason -> torus_da_gc_held_passes(_total){reason=...} or empty
    awk -v r="reason=\"$2\"" '$1 ~ /^torus_da_gc_held_passes(_total)?\{/ && index($1, r) { print $2; exit }' "$1" 2>/dev/null
}

r8modechk() { # RH4 GC mode per arm: B = every node logged the GC config line with 'passes run on the torus-da-gc
    # worker' and inline=false, TORUS_DA_GC_INLINE on no environ; C = the line with 'on the consensus thread' and
    # inline=true, TORUS_DA_GC_INLINE=1 on 3 environs; A = no config line, TORUS_DA_GC_INLINE on no environ.
    # B and C: torus_da_gc_handoff_seconds_count > 0 per node. Worker metrics (metrics-after) -> <cell>/r8-worker.txt.
    local label=$1 arm=$2 o=$R/$1 v wk il fw fin n m hc hs qd lag coal kept pins ref ff info=""
    [ -s "$o/node-env-all.txt" ] || { echo "CHECK FAIL $label: no node-env-all.txt"; return 1; }
    : > "$o/r8-worker.txt"
    for v in val0 val1 val2; do
        [ -s "$o/$v.log.gz" ] || { echo "CHECK FAIL $label: no $v.log.gz"; return 1; }
        m=$o/metrics-after-$v.txt
        [ -s "$m" ] || { echo "CHECK FAIL $label: no metrics-after-$v.txt"; return 1; }
        wk=$(r8_plainlog "$o" "$v" | grep -c -F -- "$R8_WORKER_RE")
        il=$(r8_plainlog "$o" "$v" | grep -c -F -- "$R8_INLINE_RE")
        fw=$(r8_plainlog "$o" "$v" | grep -F -- "$R8_WORKER_RE" | grep -c 'inline=false')
        fin=$(r8_plainlog "$o" "$v" | grep -F -- "$R8_INLINE_RE" | grep -c 'inline=true')
        hc=$(r8_metric "$m" torus_da_gc_handoff_seconds_count); hs=$(r8_metric "$m" torus_da_gc_handoff_seconds_sum)
        qd=$(r8_metric "$m" torus_da_gc_queue_depth); lag=$(r8_metric "$m" torus_da_gc_lag_heights)
        coal=$(r8_metric "$m" torus_da_gc_coalesced_heights_total); kept=$(r8_metric "$m" torus_da_gc_fence_kept_total)
        pins=$(r8_metric "$m" torus_da_gc_fence_pins); ref=$(r8_metric "$m" torus_da_gc_fence_refused_pins_total)
        ff=$(r8_held "$m" fence_full)
        echo "$v mode_worker_lines=$wk mode_inline_lines=$il handoff_count=${hc:-NA} handoff_sum_s=${hs:-NA} queue_depth=${qd:-NA} lag_heights=${lag:-NA} coalesced=${coal:-NA} fence_kept=${kept:-NA} fence_pins=${pins:-NA} fence_refused=${ref:-NA} held_fence_full=${ff:-NA}" >> "$o/r8-worker.txt"
        case "$arm" in
            B) [ "$wk" = 1 ] && [ "$fw" = 1 ] && [ "$il" = 0 ] || { echo "CHECK FAIL $label: $v GC mode: worker line $wk (inline=false $fw), inline line $il (want 1, 1, 0)"; return 1; } ;;
            C) [ "$il" = 1 ] && [ "$fin" = 1 ] && [ "$wk" = 0 ] || { echo "CHECK FAIL $label: $v GC mode: inline line $il (inline=true $fin), worker line $wk (want 1, 1, 0)"; return 1; } ;;
            A) [ "$wk" = 0 ] && [ "$il" = 0 ] || { echo "CHECK FAIL $label: $v arm A logged a GC mode line (worker $wk, inline $il; want 0, 0)"; return 1; } ;;
            *) echo "CHECK FAIL $label: r8modechk: unknown arm $arm"; return 1 ;;
        esac
        case "$arm" in
            B|C) [ -n "$hc" ] && awk -v x="$hc" 'BEGIN { exit !(x > 0) }' || { echo "CHECK FAIL $label: $v torus_da_gc_handoff_seconds_count='${hc:-absent}' (want > 0, arm $arm)"; return 1; } ;;
        esac
        info="$info $v:worker=$wk,inline=$il,handoffs=${hc:-NA},handoff_s=${hs:-NA},lag=${lag:-NA},coalesced=${coal:-NA},refused=${ref:-NA},fence_full=${ff:-NA}"
    done
    n=$(grep -c -E "(^| )TORUS_DA_GC_INLINE=" "$o/node-env-all.txt")
    case "$arm" in
        C) [ "$(grep -c -E "(^| )TORUS_DA_GC_INLINE=1( |$)" "$o/node-env-all.txt")" = 3 ] && [ "$n" = 3 ] || { echo "CHECK FAIL $label: TORUS_DA_GC_INLINE=1 not on exactly 3 node environs ($n with the name)"; return 1; } ;;
        *) [ "$n" = 0 ] || { echo "CHECK FAIL $label: TORUS_DA_GC_INLINE on $n arm $arm environs (want 0)"; return 1; } ;;
    esac
    echo "CHECK OK $label r8modechk: arm $arm GC mode as specified (TORUS_DA_GC_INLINE environs $n);$info"
}
