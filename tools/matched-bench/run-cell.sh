#!/usr/bin/env bash
# run-cell.sh — reusable ONE-CELL runner for the matched/s devnet campaign.
#
#   run-cell.sh <worktree> <label> [MARKETS=10] [DUR=120] [RATE=76000] [EXTRA_ENV='K=V ...']
#
# BUILDS NOTHING. It stages the binary that already sits in
# $TARGET_DIR/release/torus-node (default $HOME/.cargo-target-matched) into
# <worktree>/target/release/torus-node (devnet/wsl/env.sh hardcodes that path),
# proves the copy with md5sums, generates a MARKETS-market 3-val genesis, launches
# the bare-metal 3-validator devnet with the RE-PROOF5 record-cell env (+EXTRA_ENV
# overrides), waits for health, samples all 3 nodes' /metrics at 1 Hz, runs the
# record-cell bench-throughput command, drains, checks 3-validator agreement,
# stops the devnet and writes  $RESULTS_ROOT/<label>/summary.json.
#
# All throughput numbers come from NODE Prometheus counters
# (torus_orders_matched_total etc.) — never from bench-side math, never placed/s.
#
# Optional env overrides:
#   TARGET_DIR   cargo target dir holding torus-node + bench-throughput
#                (default $HOME/.cargo-target-matched)
#   EXPECT_NODE_MD5=H  EXPECT_BENCH_MD5=H  md5 (or its first 8+ hex chars) the
#                build recorded for this cell's torus-node / bench-throughput.
#                Step 1 refuses a binary whose md5 differs and prints both
#                (s17: a reflink-seeded target dir kept a stale bench). Unset
#                (default) = only logged, as before.
#   RESULTS_ROOT (default $HOME/bench-results-matched)
#   DATA_ROOT    devnet data root (default $HOME/torus-wsl-devnet)
#   SENDERS      bench --senders (default 5000)   CONC  --concurrency (256)
#   BATCH        --batch-size (400)               SUBMIT --submit-batch (1)
#   MPS=K        bench --markets-per-sender K (LOCALITY shape: each sender gets
#                a fixed set of K markets instead of drawing uniformly over all
#                of them). Unset (default) = flag omitted = today's uniform
#                shape. Recorded as cell.markets_per_sender in summary.json.
#   BAND / CROSS_FRACTION / CANCEL_FRACTION  econ shape (defaults 5 / 0.5 /
#                0.05).
#   OPEN_ORDER_BUDGET=N  bench --open-order-budget N: each sender sends a
#                cancel-all before its estimated open orders would pass N, so
#                it stays under the chain's per-user open-order limit (1000+,
#                all markets). Unset (default) = flag omitted.
#   RETRY_BUSY=1 bench --retry-busy: a sender resends an action the node shed
#                as busy instead of drawing a new one, so the admitted mix keeps
#                the cancel fraction (without it, overloaded small-cap cells
#                admit almost only cancel-alls and placement starves). Unset
#                (default) = flag omitted.
#   MAX_IN_FLIGHT=N  bench --max-in-flight N: each sender keeps at most N
#                signed actions in flight (released when seen committed, refused
#                by the RPC, or past nonce + 70 s); the shared block-body tail
#                reads val2 (--in-flight-watch-rpc), never val0, which takes all
#                ingress under BENCH_RPCS=0. With OPEN_ORDER_BUDGET the estimate
#                counts orders since the last COMMITTED cancel-all. Also
#                exports TORUS_RPC_MAX_RESPONSE_MB=64 to the nodes: loaded
#                torus_getBlockBody replies pass jsonrpsee's 10 MiB default
#                (~68 KB per 400-order batch) and the tail would miss them
#                (EXTRA_ENV can override; summary cell.rpc_max_response_mb).
#                Positive integer; unset (default) = flags omitted. Recorded as
#                cell.max_in_flight in summary.json.
#   IN_FLIGHT_TAIL=head|commit  bench --in-flight-tail: how the MAX_IN_FLIGHT
#                block tail finds new bodies. head = eth_blockNumber, then each
#                body up to it (the bench default; every campaign before the
#                knob). commit = probe torus_getBlockBody for the next height
#                directly: the body is on disk at commit, while eth_blockNumber
#                waits for the applied marker, which item 6 Phase 3 writes only
#                at checkpoints (slots then free in ~100-block bursts and the
#                senders stall). Needs MAX_IN_FLIGHT and a bench built with the
#                flag. Unset (default) = flag omitted = head. Recorded as
#                cell.in_flight_tail in summary.json.
#   ANTISPAM=1   turn the node anti-spam limits ON (devnet/wsl/env.sh has them
#                OFF): TORUS_INGRESS_MIN_COLLATERAL=1, TORUS_ADDR_RATE_LIMIT=1,
#                TORUS_RPC_IP_WEIGHT_PER_MIN=1200. Exported before EXTRA_ENV,
#                so EXTRA_ENV can override one knob (e.g. TORUS_ADDR_RATE_EXEMPT=
#                0x..,0x.. for the bench senders, or TORUS_RPC_IP_EXEMPT= to
#                limit loopback too). Unset (default) = limits off.
#   SPAM_CANCEL_KEYS=K SPAM_CANCEL_RATE=R [SPAM_CANCEL_FUNDED=1]
#                bench --spam-cancel-keys K --spam-cancel-rate R
#                [--spam-cancel-funded]: K extra keys send CancelAllOrders at
#                R actions/s in aggregate alongside the load, never retrying a
#                refused one; bench.log reports their sent/accepted/rejected
#                apart from the load ("Spam cancel-all ..."). FUNDED=1 uses the
#                top K funded genesis accounts (never a load sender), else
#                fresh unfunded keys. Unset (default) = flags omitted.
#   CANCEL_BY_ID_FRACTION=F MODIFY_FRACTION=G  (item 6 Phase 2 step 0.2, the
#                P2-1b cell) bench --cancel-by-id-fraction F / --modify-fraction
#                G: per fire, a CancelOrder / ModifyOrder of one of the sender's
#                own resting orders (ids from torus_getOpenOrders on the
#                in-flight watch node) instead of the drawn action; bench.log
#                "Cancel-by-id ..." -> summary.json ingest.cancel_by_id. Needs a
#                bench built with the flags. Unset (default) = flags omitted.
#   CLOID_SHARE=P  bench --cloid-share P (0..100): P% of the placed orders carry
#                a client order id (s114 cloid), unique per sender and nonzero;
#                bench.log prints "Cloid share: ..." and "Cloids (load-gen
#                generated): ...". Needs a bench built with the flag. Unset
#                (default) = flag omitted = no cloids.
#   BLOCK_CAP=N  block-cap-raise sweep bundle: exports the COHERENT set of
#                proposer-local selection caps for an N-action native block
#                (TORUS_NATIVE_TOTAL_BLOCK_CAP=N plus the companion caps that
#                would otherwise bind first — see block_cap_bundle below).
#                Applied after RECORD_ENV and before EXTRA_ENV, so EXTRA_ENV
#                can still override any single knob. DEFAULT 400 since s76
#                (200 from r3 to s75); the COMPILED node defaults are the
#                same bundle (BLOCK_CAP=400 exports exactly the compiled
#                values). BLOCK_CAP=200 = the r3-s75 default; BLOCK_CAP=100
#                = the pre-r4 cap-100 control.
#   BENCH_RPCS=all  bench ingress over all three validators (the bench pins
#                sender i to url i % 3). DEFAULT since s76 (unset = all).
#                BENCH_RPCS=0 = val0 only, the ingress every pre-s76 cell used.
#                Recorded via cell.bench_cmd.
#   OVERWRITE=1  allow reusing an existing non-empty results dir
#   HEALTH_TIMEOUT (240 s)
#   DRAIN_TIMEOUT  default 180 + 2*MARKETS s (300 markets => 780 s). A 300-market
#                  cell does NOT drain inside 180 s — r6-base-300m-r1 ended with
#                  drained=0 and ~20.8k nonce-expired evictions/node, which then
#                  poisoned the state digest.
#   DIGEST_PAR   per-node parallel RPCs during the state digest (default 8)
#   RPC_TIMEOUT  per-RPC curl timeout in the agreement step (default 60 s)
#   CRASH_KILL_AT_S=N  CRASH GATE (bl3). N seconds after the bench starts, SIGKILL
#                one devnet validator and restart it from the same data dir
#                (tools/matched-bench/crash-kill.sh). Unset = off. Must sit
#                INSIDE the load window (>= 10 s in, >= 30 s of load left):
#                killing during the drain hangs the agreement probe. 40-60 on a
#                120 s cell. The cell then also reports a crash verdict —
#                summary.json `.crash` / `.headline.crash_gate` — which is what
#                gates flipping TORUS_EXEC_PIPELINE on by default: the restarted
#                node must replay from its durable applied-height marker, rewind
#                no more than 2 blocks beyond the exec queue it already had, come
#                back with the flush worker attached, and still AGREE with the
#                two survivors.
#   CRASH_KILL_AT_S=a,b,c...  MULTI-CRASH (s75): kill+restart the same node once
#                per offset, sequentially (e.g. "60,180,300,420,540" with
#                DUR=660: five rejoins in one cell). Every offset must pass the
#                single-kill window rule above, offsets must strictly increase
#                and sit >= 90 s apart. Kill 1 writes exactly the single-kill
#                files (crash-kill.json, crash.json, crash-restart-tail.log);
#                kill k>=2 writes crash-kill-<k>.json, crash-<k>.json and
#                crash-restart-tail-<k>.log, each log tail cut at the NEXT
#                kill's byte offset (so crash.json then covers kill 1's window
#                only; the union of all tails is still everything after kill 1).
#                summary.json gains .crash_kills (one entry per kill) and the
#                headline crash_gate FAILs if ANY kill fails. A failed kill
#                stops the sequence and fails the cell (as for a single kill);
#                if a kill+restart overran so that < 60 s would separate the
#                restart from the next kill, the remaining kills are SKIPPED
#                (logged, crash-kill.skipped). Per-kill chain freeze:
#                tools/matched-bench/crash-freeze.py <results-dir>.
#   KILL_NODE    which validator the crash gate kills: val1 (default) or val2.
#                NEVER val0 (it serves the bench RPC and every headline number),
#                and never anything outside this devnet — see the guard in
#                crash-kill.sh.
#   HOTSTUFF_CPUS=a/b/c  pin each node's "hotstuff-algo" thread (the consensus
#                thread, named by the node binary) to CPU list a / b / c (one
#                CPU or a range per node, taskset syntax, e.g. 2/6/10 or
#                2-3/6-7/10-11). Applied with `taskset -pc` right after the
#                node pids are known; a node whose binary does not name the
#                thread is logged as a WARNING and the cell continues unpinned.
#                If the process also runs under a cpuset, the chosen CPU must
#                lie inside that cpuset (not enforced here). Logged in the
#                "node env" line next to the TORUS_* vars.
#   (schedstat)  For every node the harness snapshots
#                /proc/<pid>/task/<tid>/schedstat (on_cpu_ns runqueue_wait_ns
#                timeslices) of the main thread and of the threads named
#                hotstuff-algo / torus-execution / torus-flush-worker at
#                metrics-before, bench end and metrics-after, into
#                $OUT/schedstat.json (summarize.py -> sched_by_node). A
#                thread the binary does not have is recorded as null. The same
#                snapshots record each node's process utime / stime from
#                /proc/<pid>/stat into $OUT/procstat.raw (summarize.py ->
#                proc_cpu_by_node: user / sys ms per native block and per 1k
#                fills, item 6 Phase 2 step 0.2).
#   ORACLE_FEED=1  run an oracle price feed during the cell (s87), so the stack
#                is benched with fresh MARK prices like a real chain. Unset/0
#                (default) = no feed, exactly the pre-s87 cell. When 1: after the
#                idle probe, `bench-throughput oracle-feed` signs submissions for
#                all MARKETS markets with the 3 devnet validator keys
#                ($MAINREPO/devnet/wsl/bench-validator-keys.json) every
#                ORACLE_INTERVAL_MS (default 2000) at ORACLE_PRICE (default
#                30000 = the bench econ mid, 20 x --target-margin 1500). The
#                load starts only once torus_getMarkPrice on val0 shows a usable
#                mark for EVERY market whose fresh aggregate was written after
#                the feed started (all 3 validators reported); 90 s timeout,
#                else the cell FAILS. The feed is SIGSTOPped for the drain and
#                the state digest (its submissions are native actions: they
#                would keep the drain's quiet counters and the digest's funnel
#                snapshot moving forever; a mark stays usable 60 s past its last
#                fresh aggregate), then SIGTERMed after the digest. Artifacts:
#                oracle-feed.log, oracle-feed-stats.json, oracle-marks-*.json,
#                summary.json .oracle_feed (sent/accepted/rejected/stale marks
#                at bench end).
#   ORACLE_WALK_BP=N  with ORACLE_FEED=1 (item 6): every feed round moves each
#                market's price N bp around ORACLE_PRICE (`oracle-feed
#                --walk-bp N`: deterministic, mean-reverting, bounded at ±8N
#                bp), so the marks change while the cell runs. 0 (default) =
#                the fixed price and the flag is not passed (older
#                bench-throughput binaries keep working); N > 0 needs a binary
#                whose oracle-feed has --walk-bp.
#   ORACLE_FEED_DRAIN=1  with ORACLE_FEED=1 (else FATAL): keep the feed RUNNING
#                through the drain, so oracle-only blocks are executed with live
#                marks, as on a real chain whose feed never stops. 0 (default) =
#                the feed is SIGSTOPped before the drain, as above. When 1 the
#                drain is `health.py drain --feed-live`: done once the order
#                counters (placed/matched/resting) are quiet, every native
#                mempool holds at most the feed's own footprint (2 rounds x 3
#                validators x ceil(MARKETS/256) chunks), no flush/trade-writer
#                work, exec lag (torus_exec_queue_depth) <= 2 on every sample,
#                for QUIET_S with commits on every node. Per sample and node:
#                exec lag and mean exec ms per native block
#                (torus_exec_chain_seconds) -> drain-feed-live.tsv; summary in
#                drain.json .feed_live and run.log. Then the feed is SIGSTOPped
#                and the default quiet drain re-run (feed-stop-settle/) before
#                the after-snapshots and the state digest; that settle also
#                waits for a height (FEED_STOP_SETTLE_MAX_S below).
#   ORACLE_FEED_DRAIN_MAX_LAG=N  with ORACLE_FEED_DRAIN=1 (else FATAL unless 2):
#                the feed-live drain's exec-lag bound (health.py --max-lag),
#                default 2. Only for cells whose timings are not read: a
#                TORUS_LIQ_VALUE_SUM=1 node walks every balance and position
#                each native block (~0.34 s at 300 markets), so the lag stays
#                ~60 while the feed runs and the default bound never holds.
#                The paused settle (feed-stop-settle/) keeps the idle bound.
#   FEED_STOP_SETTLE_MAX_S=N  with ORACLE_FEED_DRAIN=1 (else FATAL unless 600):
#                the max seconds of the paused-feed settle, default 600 (s64; a
#                fixed 60 s before). At the pause `health.py drain --to-height
#                auto` records the target = the highest committed height across
#                the nodes (torus_block_height + torus_exec_queue_depth) and
#                waits until every node has executed it (torus_block_height >=
#                target) AND the default quiet criterion holds. On a slow chain
#                (value-sum cells: 0.82 blocks/s, exec ~60 blocks behind at the
#                pause) 60 s ended mid catch-up and the digest read the nodes at
#                different executed heights (perf doc section 72.4: a false
#                REJECT on 5 mark fields). Target not reached within N s: rc 3,
#                a WARNING in run.log naming the target and each node's executed
#                height (feed-stop-settle/drain.json), and the digest
#                quiescence check decides, as before.
#   LIQ_THIN=N   liquidation stress (row 76): after the genesis is generated, set
#                native `available` = LIQ_THIN_AVAIL (default 1000000.0 TRS, the
#                genesis' decimal-string units; at most 8 decimals) on the
#                balances noted "bulk-test 60" .. "bulk-test 60+N-1", i.e. load
#                senders 0..N-1 (--sender-offset 60). N <= SENDERS; exactly N rows
#                must match or the cell FAILS. 0 (default) = genesis untouched.
#                Recorded as cell.liq_thin / cell.liq_thin_avail.
#   ORACLE_SHOCK_BP=S  ORACLE_SHOCK_ROUND=R  with ORACLE_FEED=1 (else FATAL):
#                `oracle-feed --shock-bp S --shock-round R`: from feed round R on
#                (rounds count from 1, one per ORACLE_INTERVAL_MS) odd markets
#                are priced +S bp and even markets -S bp off the walk. Under the
#                uniform shape (MPS unset) a sender buys a market iff (local
#                sender index + market id) is even, so EVEN-index senders lose on
#                every position. S in 0..9999; 0 (default) = flags omitted. The
#                feed logs the wall-clock ms of round R (oracle-feed.log), which
#                liq_stress.py maps to a block height. Recorded in .oracle_feed.
#                With LIQ_THIN>0 or S>0 the liquidator vault
#                (LIQUIDATOR_VAULT) and both ADL escrows (ADL_ESCROW_LONG /
#                ADL_ESCROW_SHORT) join the state digest: their balances and
#                their position in every market (digest-position-accounts.txt,
#                torus_getPosition; +3*MARKETS RPCs per node), each node's
#                torus_getLiquidatorVault is saved as vault-val<i>.json, and
#                torus_liquidations_triggered_total / torus_liquidator_vault_deficit
#                and the liquidation telemetry (step timer _sum/_count, per-class /
#                scanned / acted counters, pending / deferred gauges) are sampled
#                at 1 Hz; otherwise the cell is unchanged. Analysis:
#                tools/matched-bench/liq_stress.py <results-dir>. Either knob
#                needs a summarizer with --liq-thin/--liq-thin-avail and a
#                digest-node.sh whose --capabilities lists position-accounts:
#                pre-flight refuses the cell when the selected ones (see
#                TOOLS_FROM_WORKTREE; an older worktree's) lack them.
#   EVM_LOAD_RATE=R  EVM load (item 7): R EIP-1559 transfers/s in aggregate (a
#                number > 0) from tools/tx-flood (`torus-tx-flood --rate R
#                --fanout pinned`), started right after the bench and stopped
#                (SIGTERM) when the bench exits; --duration-secs DUR bounds it
#                too. Senders: the genesis-funded "bench sender 20" ..
#                "bench sender 20+S-1" accounts (`bench-throughput gen-accounts
#                --offset 20 --secret-keys`; the load senders start at 60);
#                the k-th sender sends only to val k % 3, like a wallet. After
#                the agreement step evm_load.py (next to this script) counts the
#                executed EVM txs of every height on val0 from the EVM load's
#                start to the head. Artifacts: evm-load.log, evm-load-stats.json
#                (sent / accepted / refused by reason), evm-load-scan.json
#                (landed, EVM blocks, blocks off the genesis N cadence),
#                summary.json .evm_load (also each node's torus_exec_evm_seconds
#                delta). A cell that did not get its EVM load (.evm_load.problems)
#                is REJECTed in .validity. Needs $TARGET_DIR/release/torus-tx-flood with --rate
#                (TX_FLOOD=<path> overrides). Unset or 0 (default) = off: no
#                tx-flood, no extra file or log line.
#   EVM_LOAD_SENDERS=S  with EVM_LOAD_RATE > 0 (else FATAL): senders 1..40
#                (default 40).
#   EXTRA_ENV='EVM_BLOCK_INTERVAL=N'  genesis EVM cadence N of this cell (bench
#                Q12): EVM txs only at heights with height % N == 0. A genesis
#                field (like TIMEOUT_BASE_MS), so it is lifted out of EXTRA_ENV
#                and handed to gen-3val-genesis.sh, which sets .evm.block_interval
#                in this cell's genesis only (never in the reused
#                testnet/genesis-weighted-full.json). N in 2..=100 (governance's
#                range), else FATAL at preflight. Unset = the field stays absent
#                (node default 11; genesis byte-identical). Either way the cell
#                refuses a genesis whose N is not the one it asked for (a
#                hand-patched weighted-full genesis), logs N, uses it for the
#                EVM load's cadence check and records it as
#                summary.json .genesis.evm_block_interval. An ambient
#                EVM_BLOCK_INTERVAL is ignored: only EXTRA_ENV sets it.
#   BULK_COUNT=N  genesis-funded bulk senders "bulk-test 60".."bulk-test
#                60+N-1" (testnet/gen-weighted-genesis.sh; default 100000 =
#                every cell before bench queue 8f). Handed to
#                gen-3val-genesis.sh, which refuses to reuse a weighted-full
#                genesis built with another N (rebuild with FORCE=1; 1M accounts
#                take minutes and ~290 MB, each jq pass ~3-7 s). SENDERS <= N.
#                Off the default the bench gets --funded-bulk-end 60+N (the
#                funded spam keys sit at the top of that range). Logged in the
#                cell= line; summary.json .genesis.bulk_count.
#   GENESIS_CACHE_DIR=<dir>  reuse the genesis when none of its inputs changed
#                (the campaign driver sets <run_dir>/genesis-cache; unset or
#                empty = build every cell, as before). genesis_cache.py keys
#                every input: the generator scripts and their data (incl.
#                testnet/genesis-weighted-full.json + params sidecar and the
#                base genesis), this script, MARKETS, BULK_COUNT, the
#                TIMEOUT_BASE_MS / EVM_BLOCK_INTERVAL of EXTRA_ENV, LIQ_THIN,
#                LIQ_THIN_AVAIL, the ambient STATE_HASH_ACTIVATION / BASE /
#                BULK_OFFSET / NATIVE_AVAIL / EVM_WEI, jq's version. A cached
#                entry (<key>.genesis, its md5 re-checked on the copy) replaces
#                the generator, weighted_full_check and the jq checks (their
#                values come from the entry): "genesis reused (key K, md5 M)";
#                else "genesis built (key K, md5 M)" and the entry is stored.
#                run.log has the build's genesis lines either way, in their form
#                (the "generating" line, the generator's output: its summary
#                kept in the entry, "reusing existing ..." / "wrote G" for this
#                cell; genesis_cache.py replay).
#                A cached md5 mismatch rebuilds. FORCE=1: only an entry a
#                FORCE=1 cell built is reused, so the first FORCE=1 cell of a
#                campaign rebuilds as before and later ones reuse its result.
#                Data dirs stay fresh per cell (launch-3val.sh CLEAN=1).
#   DEFER_REPORT_SCANS=1  the campaign driver's end pass: the report-only
#                node-log scans (log-scans.sh REPORT_SCANS: the ingest
#                eviction counts and val<i>.log.excerpt) are left to
#                cell-endpass.sh, which the driver runs once after the
#                campaign's last cell (deferred-scans.json marks the cell
#                until then). Validity scans stay in-line. Default 0.
#   PRELOAD=N    bench queue 8f: after node ready (and fresh oracle marks)
#                and BEFORE the metrics-before snapshot, the samplers and the
#                load, `bench-throughput preload` places N non-crossing resting
#                orders over markets 1..MARKETS from funded owners in their own
#                index range (never a load sender, so the load's cancel-alls
#                never touch them), PRELOAD_DISTANCE ticks or more outside the
#                load's mid, at most PRELOAD_PER_OWNER per owner, then waits
#                until every node's torus_orders_resting_total rose by N.
#                Artifacts: preload.log, preload.json (the bench report),
#                preload-cell.json; summary.json .preload (seconds, counts,
#                resting, owners; boot_to_ready_s apart from the pre-load's
#                own seconds). Target missed (or rc != 0): .preload.ok=false,
#                validity REJECT, and the cell FAILS before the load. 0 / unset
#                (default) = off: no step, file or flag changes.
#   PRELOAD_PER_OWNER=P (default 900, <= 1000 = the per-user open-order limit)
#   PRELOAD_OWNER_OFFSET=I  first owner index (default 60 + SENDERS, just past
#                the load); owners I..I+ceil(N/P) must be genesis-funded (<= 60
#                + BULK_COUNT) and below the funded spam keys.
#   PRELOAD_DISTANCE=D (default 1000 ticks; > BAND)  PRELOAD_LEVELS=L (default
#                100 price levels per side)  PRELOAD_RATE=R (default 50
#                actions/s)  PRELOAD_TIMEOUT=S (default 900 s to reach N).
#   TOOLS_FROM_WORKTREE  1 (default) scores the cell with <worktree>/tools/
#                matched-bench/summarize.py, i.e. the CANDIDATE's own summarizer,
#                whichever copy of run-cell.sh was invoked. 0 keeps the old
#                behaviour (this script's own directory). TOOLS_DIR=<dir> pins it
#                explicitly. RUN_CELL_PRINT_PATHS=1 prints the resolution and
#                exits without touching the devnet.
set -uo pipefail

usage() { sed -n '2,124p' "$0"; exit 2; }
[ $# -ge 2 ] || usage

WT=$(cd "$1" && pwd) || { echo "FATAL: worktree '$1' not found" >&2; exit 2; }
LABEL=$2
MARKETS=${3:-10}
DUR=${4:-120}
RATE=${5:-76000}
EXTRA_ENV=${6:-}

SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
MAINREPO=$(cd "$SELF_DIR/../.." && pwd)
# A CANDIDATE is scored by ITS OWN summarizer. This script is invoked from the
# integration repo but handed a candidate worktree, so resolving the scoring
# scripts next to the SCRIPT scores a harness candidate with the head's
# summarize.py -- which is how a summarize.py fix can read as a no-op.
# TOOLS_FROM_WORKTREE=0 forces the old behaviour (score every cell with the
# integration repo's tools); an explicit TOOLS_DIR= wins over both.
TOOLS_FROM_WORKTREE=${TOOLS_FROM_WORKTREE:-1}
if [ -n "${TOOLS_DIR:-}" ]; then
    TOOLS_DIR=$(cd "$TOOLS_DIR" && pwd) || { echo "FATAL: TOOLS_DIR not found" >&2; exit 2; }
    TOOLS_FROM_WORKTREE=explicit
elif [ "$TOOLS_FROM_WORKTREE" = 1 ] && [ -f "$WT/tools/matched-bench/summarize.py" ]; then
    TOOLS_DIR="$WT/tools/matched-bench"
else
    [ "$TOOLS_FROM_WORKTREE" = 1 ] && TOOLS_FROM_WORKTREE=0
    TOOLS_DIR="$SELF_DIR"
fi
if [ -n "${RUN_CELL_PRINT_PATHS:-}" ]; then
    printf 'WT=%s\nSELF_DIR=%s\nTOOLS_DIR=%s\nTOOLS_FROM_WORKTREE=%s\nMAINREPO=%s\n' \
        "$WT" "$SELF_DIR" "$TOOLS_DIR" "$TOOLS_FROM_WORKTREE" "$MAINREPO"
    exit 0
fi
# A cell runs in its own transient user service, never as a descendant of an
# interactive or agent shell: see campaign/detach.sh (2026-10-06, three
# 300-market runs lost a process to an outside SIGKILL). BENCH_ALLOW_UNDETACHED=1
# overrides, for a deliberate one-off.
BENCH_UNIT_RE='/bench-[^/]*\.service$'
if [ "${BENCH_ALLOW_UNDETACHED:-0}" != 1 ] && ! grep -qE "$BENCH_UNIT_RE" /proc/self/cgroup; then
    echo "FATAL: not running in a bench-*.service unit; start the cell or campaign with" \
        "tools/matched-bench/campaign/detach.sh NAME LOGFILE CMD... (override: BENCH_ALLOW_UNDETACHED=1)" >&2
    exit 2
fi
TARGET_DIR=${TARGET_DIR:-$HOME/.cargo-target-matched}
EXPECT_NODE_MD5=${EXPECT_NODE_MD5:-}
EXPECT_BENCH_MD5=${EXPECT_BENCH_MD5:-}
RESULTS_ROOT=${RESULTS_ROOT:-$HOME/bench-results-matched}
export DATA_ROOT=${DATA_ROOT:-$HOME/torus-wsl-devnet}
SENDERS=${SENDERS:-5000}
CONC=${CONC:-256}
BATCH=${BATCH:-400}
SUBMIT=${SUBMIT:-1}
MPS=${MPS:-}
BAND=${BAND:-5}
CROSS_FRACTION=${CROSS_FRACTION:-0.5}
CANCEL_FRACTION=${CANCEL_FRACTION:-0.05}
OPEN_ORDER_BUDGET=${OPEN_ORDER_BUDGET:-}
RETRY_BUSY=${RETRY_BUSY:-}
MAX_IN_FLIGHT=${MAX_IN_FLIGHT:-}
IN_FLIGHT_TAIL=${IN_FLIGHT_TAIL:-}
ANTISPAM=${ANTISPAM:-}
SPAM_CANCEL_KEYS=${SPAM_CANCEL_KEYS:-}
SPAM_CANCEL_RATE=${SPAM_CANCEL_RATE:-}
SPAM_CANCEL_FUNDED=${SPAM_CANCEL_FUNDED:-}
CANCEL_BY_ID_FRACTION=${CANCEL_BY_ID_FRACTION:-}
MODIFY_FRACTION=${MODIFY_FRACTION:-}
CLOID_SHARE=${CLOID_SHARE:-}
BULK_COUNT=${BULK_COUNT:-100000}
GENESIS_CACHE_DIR=${GENESIS_CACHE_DIR:-}
DEFER_REPORT_SCANS=${DEFER_REPORT_SCANS:-0}
PRELOAD=${PRELOAD:-0}
PRELOAD_PER_OWNER=${PRELOAD_PER_OWNER:-900}
PRELOAD_OWNER_OFFSET=${PRELOAD_OWNER_OFFSET:-}
PRELOAD_DISTANCE=${PRELOAD_DISTANCE:-1000}
PRELOAD_LEVELS=${PRELOAD_LEVELS:-100}
PRELOAD_RATE=${PRELOAD_RATE:-50}
PRELOAD_TIMEOUT=${PRELOAD_TIMEOUT:-900}
# The harness kills a pre-load still running PRELOAD_KILL_GRACE s past its own
# --timeout-s (a hang, never a slow but bounded run): rc 124, the cell fails.
PRELOAD_KILL_GRACE=60
HEALTH_TIMEOUT=${HEALTH_TIMEOUT:-240}
# Drain scales with the market count: the mempool backlog at 300 markets needs
# far longer than 180 s to execute, and a cell that stops draining early is
# digested mid-flight (see step 8).
DRAIN_TIMEOUT=${DRAIN_TIMEOUT:-$(( 180 + 2 * MARKETS ))}
DIGEST_PAR=${DIGEST_PAR:-8}
RPC_TIMEOUT=${RPC_TIMEOUT:-60}
CRASH_KILL_AT_S=${CRASH_KILL_AT_S:-}
KILL_NODE=${KILL_NODE:-val1}
HOTSTUFF_CPUS=${HOTSTUFF_CPUS:-}
SCHED_THREADS="hotstuff-algo torus-execution torus-flush-worker"
ORACLE_FEED=${ORACLE_FEED:-0}
ORACLE_PRICE=${ORACLE_PRICE:-30000}
ORACLE_INTERVAL_MS=${ORACLE_INTERVAL_MS:-2000}
ORACLE_WALK_BP=${ORACLE_WALK_BP:-0}
ORACLE_FEED_DRAIN=${ORACLE_FEED_DRAIN:-0}
ORACLE_FEED_DRAIN_MAX_LAG=${ORACLE_FEED_DRAIN_MAX_LAG:-2}
FEED_STOP_SETTLE_MAX_S=${FEED_STOP_SETTLE_MAX_S:-600}
LIQ_THIN=${LIQ_THIN:-0}
LIQ_THIN_AVAIL=${LIQ_THIN_AVAIL:-1000000.0}
ORACLE_SHOCK_BP=${ORACLE_SHOCK_BP:-0}
ORACLE_SHOCK_ROUND=${ORACLE_SHOCK_ROUND:-0}
EVM_LOAD_RATE=${EVM_LOAD_RATE:-0}
EVM_LOAD_SENDERS=${EVM_LOAD_SENDERS:-}
# EVM load senders: "bench sender 20..59" in testnet/genesis-weighted-base.json
# (1M TRS of EVM balance each), below the load senders (--sender-offset 60) and
# the funded spam keys (the top of the bulk range).
EVM_LOAD_OFFSET=20
EVM_LOAD_MAX_SENDERS=40
# torus_core::liquidation::LIQUIDATOR_VAULT ("torus-liquidator-vlt").
LIQUIDATOR_VAULT=0x746f7275732d6c697175696461746f722d766c74
# torus_core::liquidation::ADL_ESCROW_LONG / ADL_ESCROW_SHORT
# ("torus-adl-escrow-lng" / "torus-adl-escrow-sht"): the two fixed escrows that
# take bankrupt positions in the ADL path (one per side; is_adl_escrow names
# exactly these two). test_harness.py pins all three against the Rust source.
ADL_ESCROW_LONG=0x746f7275732d61646c2d657363726f772d6c6e67
ADL_ESCROW_SHORT=0x746f7275732d61646c2d657363726f772d736874
ORACLE_KEYS="$MAINREPO/devnet/wsl/bench-validator-keys.json"
ORACLE_FRESH_TIMEOUT=90
OUT="$RESULTS_ROOT/$LABEL"

SRC_NODE="$TARGET_DIR/release/torus-node"
BENCH="$TARGET_DIR/release/bench-throughput"
TX_FLOOD=${TX_FLOOD:-$TARGET_DIR/release/torus-tx-flood}
DST_NODE="$WT/target/release/torus-node"
GENESIS="$WT/devnet/wsl/genesis-3val.json"
# torus_types::DEFAULT_EVM_BLOCK_INTERVAL (crates/torus-types/src/lib.rs): the
# N of a genesis without .evm.block_interval.
DEFAULT_EVM_N=11
WSL="$WT/devnet/wsl"
RUN_DIR="$DATA_ROOT/run"

# RE-PROOF5 record cell (R190) node env, with TORUS_BOOK_ROWS bumped 2 -> 3
# after the r2 merge (level-hash-seq-chunked: mode 3 chunked level digest,
# consensus-visible, fleet-uniform, fresh genesis — all three hold here since
# every cell is CLEAN=1 with one env for all 3 nodes). Since plan 9.13 the
# node-code defaults equal this list except TORUS_BOOK_ROWS (the node-code
# default stays Classic until the book mode is in genesis, so the =3 here is
# required) and TORUS_COMMIT_LAG_BACKOFF_CAP (genesis-sourced, 0); the other
# exports keep OLDER binaries equivalent. EXTRA_ENV entries are applied AFTER these, so a cell
# can override any of them (e.g. EXTRA_ENV='TORUS_BOOK_ROWS=2' for a mode-2
# control, or 'TORUS_PARALLEL_SETTLE=0').
# TORUS_PARALLEL_BUCKET_HASH: 4 -> 8 after the r5 root-and-save-workers sweep
# (+10.8 % n=3 vs n=3, agreement clean); 8 is ALSO the compiled default now
# (DEFAULT_BUCKET_HASH_THREADS), the explicit export keeps env-only cells on
# OLDER binaries equivalent. EXTRA_ENV='TORUS_PARALLEL_BUCKET_HASH=4' = the
# pre-r5 control; '=1' = serial. Leave TORUS_SAVE_BOOKS_WORKERS at the host
# default (4 is a -34 % cliff, r5).
# TORUS_ROCKSDB_PIPELINED_WRITE=1: s46 n=2 at cap 25, view_ms 132.6-132.9 ->
# 119.2-121.5 (-9 %), db.write avg 7 -> 5.8 ms, matched/s +7-13 %, AGREE. A
# RocksDB write-scheduling option (WAL and memtable writers overlap), no
# durability change. Control = EXTRA_ENV='TORUS_ROCKSDB_PIPELINED_WRITE=0'.
RECORD_ENV=(
    TORUS_ROCKSDB_PIPELINED_WRITE=1
    TORUS_BOOK_ROWS=3
    TORUS_RESIDENT_BOOKS=1
    TORUS_NATIVE_ROOT_CACHE=1
    TORUS_PARALLEL_SETTLE=1
    TORUS_PARALLEL_BUCKET_HASH=8
    TORUS_BUCKET_MEMBER_CACHE_MB=256
    TORUS_COMMIT_LAG_BACKOFF_CAP=8
)

# block-cap-raise sweep bundle (r2 candidate block-cap-raise-sweep). All four
# knobs are PROPOSER-LOCAL selection policy / node-local cache sizing
# (validate_block rejects on none of them), so they cannot fork; run-cell.sh
# exports the same values to all 3 nodes anyway. Companion caps, and why they
# must move with the action cap (P3 cap-sweep verdict, mem 74c34440):
#   TORUS_NATIVE_ORDERS_PER_BLOCK_CAP  default 50_000 binds FIRST at bs400
#       (125 actions) => max(50_000, N*BATCH*1.25): N=100 reproduces the 50_000
#       default exactly (clean control cell), never binding above it.
#   TORUS_VERIFIED_SENDER_CACHE_CAP    exec trust-cache must bridge the 64-block
#       exec queue: 64*N*2.5 (16_384 floor). The r2 binary derives this default
#       itself; exporting it keeps env-only cells on OLDER binaries equivalent.
#   TORUS_NATIVE_BLOCK_BYTES_CAP       6 MB default; N*BATCH*~70 B touches it at
#       N=200 => max(6 MB, N*BATCH*150 B) CAPPED at 12 MB — MAX_BLOCK_DATA_MSG_SIZE
#       (block-data sync codec) is 16 MB, a body above it could never be synced.
# NOT touched: TORUS_HASH_ONLY_PUSH_THRESHOLD — env.sh sets it to the 512 KB
# code default since s128 (section 86: +4.3% matched/s, -5.9% CPU per fill vs
# the old 8 MB pin at the direct-push floor), so most pre-proposal body sets go
# out as a HASH manifest and are PULLED (the S387-hardened path); sets up to
# 512 KB are still pushed whole. The dissemination counters below (manifest
# pushes / body-fetch exhaustion / sync fallbacks) record which path a cell took;
# EXTRA_ENV='TORUS_DIRECT_PUSH_BODY_BYTES=4194304' reproduces the pre-r4 clamp.
block_cap_bundle() { # $1=N -> prints K=V lines
    local n=$1 orders bytes cache
    orders=$(( n * BATCH * 5 / 4 )); [ "$orders" -lt 50000 ] && orders=50000
    cache=$(( 64 * n * 5 / 2 )); [ "$cache" -lt 16384 ] && cache=16384
    bytes=$(( n * BATCH * 150 )); [ "$bytes" -lt 6000000 ] && bytes=6000000; [ "$bytes" -gt 12000000 ] && bytes=12000000
    printf 'TORUS_NATIVE_TOTAL_BLOCK_CAP=%s\nTORUS_NATIVE_ORDERS_PER_BLOCK_CAP=%s\nTORUS_VERIFIED_SENDER_CACHE_CAP=%s\nTORUS_NATIVE_BLOCK_BYTES_CAP=%s\n' \
        "$n" "$orders" "$cache" "$bytes"
}
# r3 merge: the harness defaults to the block-cap-raise-sweep winner (cap 200).
# r4: the compiled node defaults are now that same bundle (rate_limit.rs
# NATIVE_TOTAL_BLOCK_CAP 200 / ORDERS_PER_BLOCK 100_000 / BLOCK_BYTES 12 MB /
# derived trust-cache 32_000 — unit-pinned against this function's N=200 output
# in rate_limit.rs `compiled_defaults_reproduce_the_r3_cap200_bundle`), so the
# BLOCK_CAP=200 export is a no-op on r4+ binaries and keeps OLDER binaries
# equivalent; BLOCK_CAP=100 = the pre-r4 control cell.
# s76: harness and compiled defaults move to cap 400 (NATIVE_TOTAL_BLOCK_CAP
# 400 / ORDERS_PER_BLOCK 200_000 / BLOCK_BYTES 12 MB / trust-cache 64_000,
# pinned in `compiled_defaults_reproduce_the_cap400_bundle`); BLOCK_CAP=200 =
# the r4 default.
BLOCK_CAP=${BLOCK_CAP:-400}
if [ -n "$BLOCK_CAP" ]; then
    [[ "$BLOCK_CAP" =~ ^[0-9]+$ ]] && [ "$BLOCK_CAP" -ge 1 ] || { echo "FATAL: BLOCK_CAP must be a positive integer" >&2; exit 2; }
    mapfile -t BLOCK_CAP_ENV < <(block_cap_bundle "$BLOCK_CAP")
else
    BLOCK_CAP_ENV=()
fi

RPCS=(http://127.0.0.1:8645 http://127.0.0.1:8646 http://127.0.0.1:8647)
# s76: which validators take bench ingress (see BENCH_RPCS in the usage).
bench_rpc_urls() {
    case "${BENCH_RPCS:-}" in
        0) printf '%s\n' "${RPCS[0]}" ;;
        ""|all) (IFS=,; printf '%s\n' "${RPCS[*]}") ;;
        *) echo "FATAL: BENCH_RPCS must be unset, all or 0 (got '$BENCH_RPCS')" >&2; return 2 ;;
    esac
}
BENCH_RPC_URLS=$(bench_rpc_urls) || exit 2
METS=(9161 9162 9163)

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log"; }
die() { log "FATAL: $*"; finish_fail; exit 1; }

# ---- thread helpers (HOTSTUFF_CPUS pinning + schedstat snapshots) -----------
find_tid() { # $1=pid $2=thread comm -> first matching tid on stdout, rc 1 if none
    local t
    for t in /proc/"$1"/task/*/; do
        [ "$(cat "$t/comm" 2>/dev/null)" = "$2" ] && { basename "$t"; return 0; }
    done
    return 1
}
# schedstat_snapshot <phase> <pid>...: appends one
# "phase node thread tid on_cpu_ns wait_ns slices" line per (node, thread) to
# $OUT/schedstat.raw. thread "main" = the pid itself; a thread the binary does
# not name gets tid=- and no counters (=> null in schedstat.json).
schedstat_snapshot() {
    local phase=$1 i=0 p t tid st f
    shift
    for p in "$@"; do
        for t in main $SCHED_THREADS; do
            if [ "$t" = main ]; then tid=$p; else tid=$(find_tid "$p" "$t") || tid=""; fi
            if [ -n "$tid" ] && read -r st < /proc/"$p"/task/"$tid"/schedstat 2>/dev/null; then
                printf '%s val%s %s %s %s\n' "$phase" "$i" "$t" "$tid" "$st"
            else
                printf '%s val%s %s -\n' "$phase" "$i" "$t"
            fi
        done >> "$OUT/schedstat.raw"
        # Item 6 Phase 2 step 0.2: process utime / stime (clock ticks, fields
        # 14 / 15 of /proc/<pid>/stat) -> summarize.py proc_cpu_by_node.
        if read -r st < /proc/"$p"/stat 2>/dev/null; then
            read -ra f <<< "${st##*) }"
            printf '%s val%s %s %s %s\n' "$phase" "$i" "$p" "${f[11]}" "${f[12]}" >> "$OUT/procstat.raw"
        fi
        i=$((i+1))
    done
}
# schedstat_json <raw> <out.json>:
# {"val0": {"main": {"tid": N, "before": [on_cpu_ns, wait_ns, slices], "bench_end": [...], "after": [...]},
#           "hotstuff-algo": null | {...}, ...}, ...}
schedstat_json() {
    python3 - "$1" "$2" <<'PYJ'
import json, sys
out = {}
for line in open(sys.argv[1]):
    f = line.split()
    if len(f) < 4:
        continue
    phase, node, thread, tid = f[:4]
    d = out.setdefault(node, {})
    if tid == "-" or len(f) < 7:
        d.setdefault(thread, None)
        continue
    e = d.get(thread) or {"tid": int(tid)}
    e[phase] = [int(x) for x in f[4:7]]
    d[thread] = e
json.dump(out, open(sys.argv[2], "w"), indent=1)
PYJ
}

# ---------------------------------------------------------------- pre-flight
[ -x "$SRC_NODE" ] || { echo "FATAL: missing $SRC_NODE" >&2; exit 1; }
[ -x "$BENCH" ]    || { echo "FATAL: missing $BENCH" >&2; exit 1; }
[ -x "$WSL/launch-3val.sh" ] || { echo "FATAL: $WSL/launch-3val.sh missing" >&2; exit 1; }
for t in jq curl python3 md5sum awk; do command -v $t >/dev/null || { echo "FATAL: need $t" >&2; exit 1; }; done
[[ "$MARKETS" =~ ^[0-9]+$ && "$DUR" =~ ^[0-9]+$ && "$RATE" =~ ^[0-9]+$ ]] || usage
[ -z "$MPS" ] || [[ "$MPS" =~ ^[0-9]+$ ]] || { echo "FATAL: MPS must be an integer" >&2; exit 2; }
[[ "$BAND" =~ ^[1-9][0-9]*$ ]] || { echo "FATAL: BAND must be a positive integer" >&2; exit 2; }
[ -z "$OPEN_ORDER_BUDGET" ] || [[ "$OPEN_ORDER_BUDGET" =~ ^[0-9]+$ ]] || { echo "FATAL: OPEN_ORDER_BUDGET must be an integer" >&2; exit 2; }
[ -z "$RETRY_BUSY" ] || [ "$RETRY_BUSY" = 1 ] || { echo "FATAL: RETRY_BUSY must be 1 or unset" >&2; exit 2; }
[ -z "$MAX_IN_FLIGHT" ] || [[ "$MAX_IN_FLIGHT" =~ ^[1-9][0-9]*$ ]] || { echo "FATAL: MAX_IN_FLIGHT must be a positive integer" >&2; exit 2; }
[ -z "$IN_FLIGHT_TAIL" ] || [ "$IN_FLIGHT_TAIL" = head ] || [ "$IN_FLIGHT_TAIL" = commit ] || { echo "FATAL: IN_FLIGHT_TAIL must be head, commit or unset" >&2; exit 2; }
[ -z "$IN_FLIGHT_TAIL" ] || [ -n "$MAX_IN_FLIGHT" ] || { echo "FATAL: IN_FLIGHT_TAIL needs MAX_IN_FLIGHT" >&2; exit 2; }
[ -z "$ANTISPAM" ] || [ "$ANTISPAM" = 1 ] || { echo "FATAL: ANTISPAM must be 1 or unset" >&2; exit 2; }
[ -z "$SPAM_CANCEL_KEYS" ] || [[ "$SPAM_CANCEL_KEYS" =~ ^[0-9]+$ ]] || { echo "FATAL: SPAM_CANCEL_KEYS must be an integer" >&2; exit 2; }
[ -z "$SPAM_CANCEL_RATE" ] || [[ "$SPAM_CANCEL_RATE" =~ ^[0-9]+(\.[0-9]+)?$ ]] || { echo "FATAL: SPAM_CANCEL_RATE must be a number (actions/s)" >&2; exit 2; }
[ -z "$SPAM_CANCEL_FUNDED" ] || [ "$SPAM_CANCEL_FUNDED" = 1 ] || { echo "FATAL: SPAM_CANCEL_FUNDED must be 1 or unset" >&2; exit 2; }
[ -z "$SPAM_CANCEL_KEYS" ] || [ -n "$SPAM_CANCEL_RATE" ] || { echo "FATAL: SPAM_CANCEL_KEYS needs SPAM_CANCEL_RATE" >&2; exit 2; }
[ -z "$CLOID_SHARE" ] || { [[ "$CLOID_SHARE" =~ ^[0-9]+$ ]] && [ "$CLOID_SHARE" -le 100 ]; } || { echo "FATAL: CLOID_SHARE must be an integer 0..100" >&2; exit 2; }
for f in "$CANCEL_BY_ID_FRACTION" "$MODIFY_FRACTION"; do [ -z "$f" ] || [[ "$f" =~ ^(0(\.[0-9]+)?|1(\.0+)?)$ ]] || { echo "FATAL: CANCEL_BY_ID_FRACTION / MODIFY_FRACTION '$f' must be in [0,1]" >&2; exit 2; }; done
for f in "$CROSS_FRACTION" "$CANCEL_FRACTION"; do
    [[ "$f" =~ ^(0(\.[0-9]+)?|1(\.0+)?)$ ]] || { echo "FATAL: fraction '$f' must be in [0,1]" >&2; exit 2; }
done
[ -x "$TOOLS_DIR/digest-node.sh" ] || { echo "FATAL: $TOOLS_DIR/digest-node.sh missing" >&2; exit 1; }
# The post-cell node-log scans (in-line, or report-only ones deferred to the campaign end pass) and the genesis
# cache are this script's own helpers (SELF_DIR, like evm_load.py / preload.py), never a candidate's copy.
case "$DEFER_REPORT_SCANS" in 0|1) ;; *) echo "FATAL: DEFER_REPORT_SCANS must be 0 or 1 (got '$DEFER_REPORT_SCANS')" >&2; exit 2 ;; esac
# EXTRA_ENV is exported mid-run: there a harness variable would overwrite this script's own (DEFER_REPORT_SCANS
# before the report scans read it; GENESIS_CACHE_DIR reaches only the nodes). Set them in run-cell.sh's env.
for kv in $EXTRA_ENV; do case $kv in DEFER_REPORT_SCANS=*|GENESIS_CACHE_DIR=*) echo "FATAL: ${kv%%=*} in EXTRA_ENV (the node env): a run-cell.sh variable, set it in run-cell.sh's own env (the campaign driver sets it from its preflight)" >&2; exit 2 ;; esac; done
# shellcheck source=log-scans.sh
. "$SELF_DIR/log-scans.sh" 2>/dev/null || { echo "FATAL: $SELF_DIR/log-scans.sh missing (copy it beside run-cell.sh)" >&2; exit 1; }
[ -z "$GENESIS_CACHE_DIR" ] || [ -f "$SELF_DIR/genesis_cache.py" ] \
    || { echo "FATAL: GENESIS_CACHE_DIR is set but $SELF_DIR/genesis_cache.py is missing (copy it beside run-cell.sh)" >&2; exit 1; }
# BULK_COUNT: the genesis' funded bulk senders; the load's must be among them.
[[ "$BULK_COUNT" =~ ^[1-9][0-9]{0,8}$ ]] || { echo "FATAL: BULK_COUNT must be a positive integer (got '$BULK_COUNT')" >&2; exit 2; }
{ [[ "$SENDERS" =~ ^[0-9]+$ ]] && [ "$SENDERS" -le "$BULK_COUNT" ]; } \
    || { echo "FATAL: SENDERS=$SENDERS exceeds BULK_COUNT=$BULK_COUNT (the load senders 60..60+SENDERS must be genesis-funded)" >&2; exit 2; }
# EVM_BLOCK_INTERVAL (genesis EVM cadence N) comes from EXTRA_ENV only; checked
# here so a bad N fails before anything is staged. Same range as
# gen-3val-genesis.sh (torus_types::EVM_BLOCK_INTERVAL_RANGE = 2..=100).
GEN_EVM_BLOCK_INTERVAL=""; GEN_EVM_BLOCK_INTERVAL_SET=0
for kv in $EXTRA_ENV; do case $kv in EVM_BLOCK_INTERVAL=*) GEN_EVM_BLOCK_INTERVAL=${kv#*=}; GEN_EVM_BLOCK_INTERVAL_SET=1 ;; esac; done
if [ "$GEN_EVM_BLOCK_INTERVAL_SET" = 1 ]; then
    [[ "$GEN_EVM_BLOCK_INTERVAL" =~ ^[1-9][0-9]{0,2}$ ]] && [ "$GEN_EVM_BLOCK_INTERVAL" -ge 2 ] && [ "$GEN_EVM_BLOCK_INTERVAL" -le 100 ] \
        || { echo "FATAL: EVM_BLOCK_INTERVAL must be an integer in 2..=100 (torus_types::EVM_BLOCK_INTERVAL_RANGE) (got '$GEN_EVM_BLOCK_INTERVAL')" >&2; exit 2; }
fi
for v in EXPECT_NODE_MD5 EXPECT_BENCH_MD5; do
    [[ -z "${!v}" || "${!v}" =~ ^[0-9a-f]{8,32}$ ]] || { echo "FATAL: $v must be 8-32 lowercase hex chars of an md5 (got '${!v}')" >&2; exit 2; }
done
[[ "$ORACLE_WALK_BP" =~ ^[0-9]+$ ]] && [ "$ORACLE_WALK_BP" -lt 1250 ] \
    || { echo "FATAL: ORACLE_WALK_BP must be an integer in 0..1249 (got '$ORACLE_WALK_BP')" >&2; exit 2; }
[[ "$LIQ_THIN" =~ ^[0-9]{1,9}$ ]] || { echo "FATAL: LIQ_THIN must be a non-negative integer (got '$LIQ_THIN')" >&2; exit 2; }
LIQ_THIN=$((10#$LIQ_THIN))
if [ "$LIQ_THIN" -gt 0 ]; then
    [ "$LIQ_THIN" -le "$SENDERS" ] 2>/dev/null \
        || { echo "FATAL: LIQ_THIN=$LIQ_THIN must not exceed SENDERS=$SENDERS (the thin senders are load senders 0..LIQ_THIN-1)" >&2; exit 2; }
    [[ "$LIQ_THIN_AVAIL" =~ ^[0-9]+(\.[0-9]{1,8})?$ ]] && awk -v a="$LIQ_THIN_AVAIL" 'BEGIN{exit !(a>0)}' \
        || { echo "FATAL: LIQ_THIN_AVAIL must be a positive TRS amount with at most 8 decimals (got '$LIQ_THIN_AVAIL')" >&2; exit 2; }
fi
[[ "$ORACLE_SHOCK_BP" =~ ^[0-9]{1,4}$ ]] || { echo "FATAL: ORACLE_SHOCK_BP must be an integer in 0..9999 (got '$ORACLE_SHOCK_BP')" >&2; exit 2; }
[[ "$ORACLE_SHOCK_ROUND" =~ ^[0-9]{1,9}$ ]] || { echo "FATAL: ORACLE_SHOCK_ROUND must be a non-negative integer (got '$ORACLE_SHOCK_ROUND')" >&2; exit 2; }
ORACLE_SHOCK_BP=$((10#$ORACLE_SHOCK_BP)); ORACLE_SHOCK_ROUND=$((10#$ORACLE_SHOCK_ROUND))
[ "$ORACLE_SHOCK_ROUND" = 0 ] || [ "$ORACLE_SHOCK_BP" -gt 0 ] \
    || { echo "FATAL: ORACLE_SHOCK_ROUND needs ORACLE_SHOCK_BP > 0" >&2; exit 2; }
case "$ORACLE_FEED_DRAIN" in
    0) ;;
    1) [ "$ORACLE_FEED" = 1 ] || { echo "FATAL: ORACLE_FEED_DRAIN=1 needs ORACLE_FEED=1" >&2; exit 2; } ;;
    *) echo "FATAL: ORACLE_FEED_DRAIN must be 0 or 1 (got '$ORACLE_FEED_DRAIN')" >&2; exit 2 ;;
esac
[[ "$ORACLE_FEED_DRAIN_MAX_LAG" =~ ^[0-9]{1,6}$ ]] || { echo "FATAL: ORACLE_FEED_DRAIN_MAX_LAG must be a non-negative integer (got '$ORACLE_FEED_DRAIN_MAX_LAG')" >&2; exit 2; }
ORACLE_FEED_DRAIN_MAX_LAG=$((10#$ORACLE_FEED_DRAIN_MAX_LAG))
[ "$ORACLE_FEED_DRAIN_MAX_LAG" = 2 ] || [ "$ORACLE_FEED_DRAIN" = 1 ] \
    || { echo "FATAL: ORACLE_FEED_DRAIN_MAX_LAG=$ORACLE_FEED_DRAIN_MAX_LAG needs ORACLE_FEED_DRAIN=1" >&2; exit 2; }
[[ "$FEED_STOP_SETTLE_MAX_S" =~ ^[0-9]{1,6}$ ]] && [ "$((10#$FEED_STOP_SETTLE_MAX_S))" -gt 0 ] \
    || { echo "FATAL: FEED_STOP_SETTLE_MAX_S must be a positive integer number of seconds (got '$FEED_STOP_SETTLE_MAX_S')" >&2; exit 2; }
FEED_STOP_SETTLE_MAX_S=$((10#$FEED_STOP_SETTLE_MAX_S))
[ "$FEED_STOP_SETTLE_MAX_S" = 600 ] || [ "$ORACLE_FEED_DRAIN" = 1 ] \
    || { echo "FATAL: FEED_STOP_SETTLE_MAX_S=$FEED_STOP_SETTLE_MAX_S needs ORACLE_FEED_DRAIN=1" >&2; exit 2; }
case "$ORACLE_FEED" in
    0) [ "$ORACLE_WALK_BP" = 0 ] || { echo "FATAL: ORACLE_WALK_BP=$ORACLE_WALK_BP needs ORACLE_FEED=1" >&2; exit 2; }
       [ "$ORACLE_SHOCK_BP" = 0 ] || { echo "FATAL: ORACLE_SHOCK_BP=$ORACLE_SHOCK_BP needs ORACLE_FEED=1" >&2; exit 2; } ;;
    1)
        [[ "$ORACLE_PRICE" =~ ^[0-9]+(\.[0-9]+)?$ ]] && awk -v p="$ORACLE_PRICE" 'BEGIN{exit !(p>0)}' \
            || { echo "FATAL: ORACLE_PRICE must be a positive number (got '$ORACLE_PRICE')" >&2; exit 2; }
        # a fresh aggregate needs all 3 validators inside one 10 s window
        [[ "$ORACLE_INTERVAL_MS" =~ ^[1-9][0-9]*$ ]] && [ "$ORACLE_INTERVAL_MS" -lt 10000 ] \
            || { echo "FATAL: ORACLE_INTERVAL_MS must be an integer in 1..9999 (got '$ORACLE_INTERVAL_MS')" >&2; exit 2; }
        [ -s "$ORACLE_KEYS" ] || { echo "FATAL: ORACLE_FEED=1 needs the validator key file $ORACLE_KEYS" >&2; exit 1; }
        "$BENCH" oracle-feed --help >/dev/null 2>&1 || { echo "FATAL: ORACLE_FEED=1 but $BENCH has no oracle-feed subcommand" >&2; exit 1; }
        [ "$ORACLE_WALK_BP" = 0 ] || "$BENCH" oracle-feed --help 2>/dev/null | grep -q -- --walk-bp \
            || { echo "FATAL: ORACLE_WALK_BP=$ORACLE_WALK_BP but $BENCH oracle-feed has no --walk-bp" >&2; exit 1; }
        [ "$ORACLE_SHOCK_BP" = 0 ] || "$BENCH" oracle-feed --help 2>/dev/null | grep -q -- --shock-bp \
            || { echo "FATAL: ORACLE_SHOCK_BP=$ORACLE_SHOCK_BP but $BENCH oracle-feed has no --shock-bp" >&2; exit 1; }
        ;;
    *) echo "FATAL: ORACLE_FEED must be unset, 0 or 1 (got '$ORACLE_FEED')" >&2; exit 2 ;;
esac

# ---- EVM load (EVM_LOAD_RATE) pre-flight. Every EVM-load step below sits in an
# `if evm_load_on` block: off (the default), the cell runs exactly as before.
[[ "$EVM_LOAD_RATE" =~ ^[0-9]+(\.[0-9]+)?$ ]] \
    || { echo "FATAL: EVM_LOAD_RATE must be a non-negative number of tx/s (got '$EVM_LOAD_RATE')" >&2; exit 2; }
evm_load_on() { awk -v r="$EVM_LOAD_RATE" 'BEGIN{exit !(r>0)}'; }
if evm_load_on; then
    EVM_LOAD_SENDERS=${EVM_LOAD_SENDERS:-$EVM_LOAD_MAX_SENDERS}
    [[ "$EVM_LOAD_SENDERS" =~ ^[1-9][0-9]?$ ]] && [ "$EVM_LOAD_SENDERS" -le "$EVM_LOAD_MAX_SENDERS" ] \
        || { echo "FATAL: EVM_LOAD_SENDERS must be an integer in 1..$EVM_LOAD_MAX_SENDERS (got '$EVM_LOAD_SENDERS')" >&2; exit 2; }
    [ -x "$TX_FLOOD" ] || { echo "FATAL: EVM_LOAD_RATE=$EVM_LOAD_RATE needs $TX_FLOOD (cargo build --release -p torus-tx-flood, or TX_FLOOD=<path>)" >&2; exit 1; }
    TX_FLOOD_HELP=$("$TX_FLOOD" --help 2>/dev/null) || TX_FLOOD_HELP=""
    for f in --rate --fanout --keys-file --chain-id --duration-secs --stats-file; do
        case "$TX_FLOOD_HELP" in *"$f"*) ;; *) echo "FATAL: EVM_LOAD_RATE=$EVM_LOAD_RATE but $TX_FLOOD has no $f (rebuild torus-tx-flood)" >&2; exit 1 ;; esac
    done
    [ -f "$SELF_DIR/evm_load.py" ] || { echo "FATAL: $SELF_DIR/evm_load.py missing" >&2; exit 1; }
elif [ -n "$EVM_LOAD_SENDERS" ]; then
    echo "FATAL: EVM_LOAD_SENDERS=$EVM_LOAD_SENDERS needs EVM_LOAD_RATE > 0" >&2; exit 2
fi

# ---- pre-load (PRELOAD > 0, bench queue 8f) pre-flight. Every pre-load step
# below sits in an `if preload_on` block: off (the default), the cell runs
# exactly as before.
[[ "$PRELOAD" =~ ^[0-9]{1,9}$ ]] || { echo "FATAL: PRELOAD must be a non-negative integer number of orders (got '$PRELOAD')" >&2; exit 2; }
PRELOAD=$((10#$PRELOAD))
preload_on() { [ "$PRELOAD" -gt 0 ]; }
# preload_cmd: PRELOAD_CMD = the bench pre-load for this cell (same econ mid,
# --target-margin 1500, and band as the load; owners past the load's senders).
preload_cmd() {
    PRELOAD_CMD=("$BENCH" preload --rpc-urls "$(IFS=,; printf '%s' "${RPCS[*]}")" \
        --metrics-urls "http://127.0.0.1:${METS[0]}/metrics,http://127.0.0.1:${METS[1]}/metrics,http://127.0.0.1:${METS[2]}/metrics" \
        --target "$PRELOAD" --markets "$MARKETS" --per-owner "$PRELOAD_PER_OWNER" --owner-offset "$PRELOAD_OWNER_OFFSET" \
        --sender-offset 60 --senders "$SENDERS" --funded-bulk-end "$((60 + BULK_COUNT))" \
        --target-margin 1500 --band "$BAND" --distance "$PRELOAD_DISTANCE" --levels "$PRELOAD_LEVELS" \
        --rate "$PRELOAD_RATE" --timeout-s "$PRELOAD_TIMEOUT" --out "$OUT/preload.json")
}
PRELOAD_OWNERS=""
if preload_on; then
    [[ "$PRELOAD_PER_OWNER" =~ ^[1-9][0-9]{0,3}$ ]] && [ "$PRELOAD_PER_OWNER" -le 1000 ] \
        || { echo "FATAL: PRELOAD_PER_OWNER must be an integer in 1..1000 (the per-user open-order limit) (got '$PRELOAD_PER_OWNER')" >&2; exit 2; }
    for v in PRELOAD_DISTANCE PRELOAD_LEVELS PRELOAD_TIMEOUT; do
        [[ "${!v}" =~ ^[1-9][0-9]{0,8}$ ]] || { echo "FATAL: $v must be a positive integer (got '${!v}')" >&2; exit 2; }
    done
    PRELOAD_OWNER_OFFSET=${PRELOAD_OWNER_OFFSET:-$((60 + SENDERS))}
    [[ "$PRELOAD_OWNER_OFFSET" =~ ^[0-9]{1,9}$ ]] || { echo "FATAL: PRELOAD_OWNER_OFFSET must be a non-negative integer (got '$PRELOAD_OWNER_OFFSET')" >&2; exit 2; }
    { [[ "$PRELOAD_RATE" =~ ^[0-9]+(\.[0-9]+)?$ ]] && awk -v r="$PRELOAD_RATE" 'BEGIN{exit !(r>0)}'; } \
        || { echo "FATAL: PRELOAD_RATE must be a positive number of actions/s (got '$PRELOAD_RATE')" >&2; exit 2; }
    [ -f "$SELF_DIR/preload.py" ] || { echo "FATAL: PRELOAD=$PRELOAD needs $SELF_DIR/preload.py" >&2; exit 1; }
    "$BENCH" preload --help 2>/dev/null | grep -q -- --per-owner \
        || { echo "FATAL: PRELOAD=$PRELOAD but $BENCH has no preload subcommand (rebuild bench-throughput)" >&2; exit 1; }
    PRELOAD_SPAM=0; [ "$SPAM_CANCEL_FUNDED" = 1 ] && PRELOAD_SPAM=${SPAM_CANCEL_KEYS:-0}
    PRELOAD_OWNERS=$(python3 "$SELF_DIR/preload.py" owners --target "$PRELOAD" --per-owner "$PRELOAD_PER_OWNER" \
        --owner-offset "$PRELOAD_OWNER_OFFSET" --bulk-count "$BULK_COUNT" --spam-funded-keys "$PRELOAD_SPAM" 2>&1) \
        || { echo "FATAL: PRELOAD=$PRELOAD: $PRELOAD_OWNERS" >&2; exit 2; }
    preload_cmd
    # The bench validates the whole plan (funded range, load overlap, band, batch caps).
    PRELOAD_DRY=$("${PRELOAD_CMD[@]}" --dry-run 2>&1 >/dev/null) \
        || { echo "FATAL: PRELOAD=$PRELOAD: bench preload --dry-run refused the plan: $PRELOAD_DRY" >&2; exit 2; }
fi

# ---- crash gate (bl3) pre-flight: validated HERE, before anything is launched,
# so a bad kill window can never be discovered mid-cell.
KILL_IDX=""
if [ -n "$CRASH_KILL_AT_S" ]; then
    [ -x "$TOOLS_DIR/crash-kill.sh" ] || { echo "FATAL: $TOOLS_DIR/crash-kill.sh missing" >&2; exit 1; }
    # shellcheck source=/dev/null
    CRASH_KILL_LIB=1 source "$TOOLS_DIR/crash-kill.sh"
    case "$KILL_NODE" in
        val1) KILL_IDX=1 ;;
        val2) KILL_IDX=2 ;;
        *) echo "FATAL: KILL_NODE must be val1 or val2 (never val0 — it serves the bench RPC and every headline number)" >&2; exit 2 ;;
    esac
    crash_kill_list_ok "$CRASH_KILL_AT_S" "$DUR" || {
        echo "FATAL: CRASH_KILL_AT_S=$CRASH_KILL_AT_S is not a valid kill list for a ${DUR}s cell (each offset >= 10 and <= $((DUR-30)); killing during the drain hangs the agreement probe; a list must strictly increase with >= ${CRASH_KILL_MIN_GAP_S}s between kills)" >&2; exit 2; }
    IFS=, read -r -a CRASH_AT <<< "$CRASH_KILL_AT_S"
fi

# ---- liquidation stress (row 76) helpers
# liq_stress_on: either knob set -> the vault digest/RPC and liquidation sampling.
liq_stress_on() {
    [ "$LIQ_THIN" -gt 0 ] || [ "$ORACLE_SHOCK_BP" -gt 0 ]
}
# summarizer_has_liq_opts: the selected summarize.py accepts --liq-thin and
# --liq-thin-avail (asked through its own argparse --help). An older one (the
# worktree's own under TOOLS_FROM_WORKTREE=1, e.g. main 46bb3a40 or the
# Phase 4 build) exits on them as unrecognized arguments, after the run.
summarizer_has_liq_opts() {
    local h
    h=$(python3 "$TOOLS_DIR/summarize.py" --help 2>/dev/null) || return 1
    case "$h" in *--liq-thin-avail*) return 0 ;; esac
    return 1
}
# digest_has_position_accounts: the selected digest-node.sh digests the
# position accounts it is handed as 7th argument (`--capabilities` lists
# `position-accounts`). An older one ignores that argument silently, so the
# cell would report agreement without a single protocol position checked; asked
# for capabilities it only prints its usage and exits 2.
digest_has_position_accounts() {
    local c
    c=$("$TOOLS_DIR/digest-node.sh" --capabilities 2>/dev/null) || return 1
    case $'\n'"$c"$'\n' in *$'\n'position-accounts$'\n'*) return 0 ;; esac
    return 1
}
# Pre-flight: a liquidation cell passes the options, and refuses here (not after
# the run) when the summarizer lacks them or the digest script would not cover
# the protocol accounts' positions; any other cell passes none and asks neither
# script anything, as before.
SUMMARIZE_LIQ_ARGS=()
DIGEST_HAS_POS=0
if liq_stress_on; then
    summarizer_has_liq_opts || {
        echo "FATAL: liquidation stress (LIQ_THIN=$LIQ_THIN ORACLE_SHOCK_BP=$ORACLE_SHOCK_BP) needs a summarizer" \
            "that accepts --liq-thin/--liq-thin-avail, and $TOOLS_DIR/summarize.py does not" \
            "(TOOLS_FROM_WORKTREE=$TOOLS_FROM_WORKTREE; score with this harness's own tools:" \
            "TOOLS_FROM_WORKTREE=0, or point TOOLS_DIR= at a newer tools/matched-bench)" >&2
        exit 1
    }
    digest_has_position_accounts || {
        echo "FATAL: liquidation stress (LIQ_THIN=$LIQ_THIN ORACLE_SHOCK_BP=$ORACLE_SHOCK_BP) needs a digest script" \
            "that covers the protocol accounts' positions (digest-node.sh --capabilities lists position-accounts)," \
            "and $TOOLS_DIR/digest-node.sh does not: it would ignore them and report agreement unchecked" \
            "(TOOLS_FROM_WORKTREE=$TOOLS_FROM_WORKTREE; score with this harness's own tools:" \
            "TOOLS_FROM_WORKTREE=0, or point TOOLS_DIR= at a newer tools/matched-bench)" >&2
        exit 1
    }
    DIGEST_HAS_POS=1
    SUMMARIZE_LIQ_ARGS=(--liq-thin "$LIQ_THIN" --liq-thin-avail "$LIQ_THIN_AVAIL")
fi
# liq_thin_patch <genesis> <n> <available>: native `available` = <available> on
# the balances noted "bulk-test 60" .. "bulk-test 60+n-1" (load senders
# 0..n-1). Prints the rows changed. rc 1, genesis untouched, unless exactly n
# distinct notes match.
liq_thin_patch() {
    local g=$1 n=$2 avail=$3 tmp got
    tmp="$g.liq-thin.tmp"
    got=$(jq --argjson n "$n" '[.native_balances[] | (.note // "") | select(test("^bulk-test [0-9]+$"))
        | ltrimstr("bulk-test ") | tonumber | select(. >= 60 and . < 60 + $n)]
        | if length == (unique | length) then length else -1 end' "$g") || return 1
    if [ "$got" != "$n" ]; then
        echo "liq_thin_patch: $g has $got (wanted $n distinct) of the notes bulk-test 60..$((60 + n - 1))" >&2
        return 1
    fi
    if ! jq --argjson n "$n" --arg av "$avail" '.native_balances |= map(
            if ((.note // "") | test("^bulk-test [0-9]+$")) then
                (if (.note | ltrimstr("bulk-test ") | tonumber) as $i | $i >= 60 and $i < 60 + $n
                 then .available = $av else . end)
            else . end)' "$g" > "$tmp" || ! mv -f "$tmp" "$g"; then
        rm -f -- "${tmp:?}"
        return 1
    fi
    echo "$got"
}

# record_genesis_evm_n <summary.json> <n>: .genesis.evm_block_interval = n (the
# genesis EVM cadence N; summarize.py, possibly the candidate's older copy,
# knows nothing of it). rc 1, file untouched, on failure.
record_genesis_evm_n() {
    local p=$1 n=$2 tmp
    tmp="$p.evm-n.tmp"
    if ! jq --argjson n "$n" '.genesis.evm_block_interval = $n' "$p" > "$tmp" || ! mv -f "$tmp" "$p"; then
        rm -f -- "${tmp:?}"
        return 1
    fi
}

# record_genesis_bulk_count <summary.json> <n>: .genesis.bulk_count = n (the
# BULK_COUNT this cell's genesis was built with). rc 1, file untouched, on failure.
record_genesis_bulk_count() {
    local p=$1 n=$2 tmp
    tmp="$p.bulk-count.tmp"
    if ! jq --argjson n "$n" '.genesis.bulk_count = $n' "$p" > "$tmp" || ! mv -f "$tmp" "$p"; then
        rm -f -- "${tmp:?}"
        return 1
    fi
}

# preload_record <rc> <seconds>: preload-cell.json (what run-cell.sh knows of
# the pre-load) + preload.py summary -> summary.json .preload (REJECT folded in
# when the target was missed). Prints preload.py's one-line verdict; rc 0 iff ok.
preload_record() {
    jq -n --argjson target "$PRELOAD" --argjson seconds "$2" --argjson rc "$1" \
        --argjson boot "$((T_HEALTHY - T_LAUNCH))" --argjson markets "$MARKETS" --arg cmd "${PRELOAD_CMD[*]}" \
        '{target: $target, seconds: $seconds, rc: $rc, boot_to_ready_s: $boot, markets: $markets, cmd: $cmd}' \
        > "$OUT/preload-cell.json" || return 1
    python3 "$SELF_DIR/preload.py" summary --dir "$OUT" | jq -e -c '., if .ok then empty else error("not ok") end' 2>/dev/null
}

# weighted_full_check <file> <senders>: gen-3val-genesis.sh REUSES an existing
# testnet/genesis-weighted-full.json (gitignored, never checked). Prints why and
# returns 1 unless it parses, every native address is 0x + 40 hex, and at least
# <senders> rows carry a "bulk-test N" note (the load senders). s17: a test run
# with a stub bench left one with 61 balances and the address 'oracle-feed'.
weighted_full_check() {
    local f=$1 n=$2 why
    [ -e "$f" ] || return 0
    why=$(jq -r --argjson n "$n" '
        [.native_balances[]?.address | select(type != "string" or (test("^0x[0-9a-f]{40}$") | not))] as $bad
        | ([.native_balances[]? | (.note // "") | select(test("^bulk-test [0-9]+$"))] | length) as $bulk
        | if ($bad | length) > 0 then "\($bad | length) native address(es) not 0x + 40 hex, e.g. \($bad[0:3] | tojson)"
          elif $bulk < $n then "\($bulk) bulk-test rows < SENDERS=\($n)"
          else empty end' "$f" 2>&1) || why="jq cannot check it: ${why:0:200}"
    [ -z "$why" ] && return 0
    echo "$f is stale: $why. gen-3val-genesis.sh would reuse it as-is; move it away (it is rebuilt) or set FORCE=1"
    return 1
}

if pgrep -f "bench-throughput consensus" >/dev/null; then echo "FATAL: a bench is already running" >&2; exit 1; fi
# by process name: a builder waiting on the campaign's build lock (flock ... cargo build)
# or a shell that only mentions "cargo build" is not building (s1156)
if pgrep -x cargo >/dev/null || pgrep -x rustc >/dev/null; then echo "FATAL: a cargo build is running — never bench while building" >&2; exit 1; fi
# A stray feed (from a SIGKILLed oracle cell) would sign into THIS devnet too.
if pgrep -f "bench-throughput oracle-feed" >/dev/null; then echo "FATAL: an oracle feed is already running" >&2; exit 1; fi
# Same for a stray EVM load (by process name: a driver's `env TX_FLOOD=...` argv must not match).
if evm_load_on; then
    if pgrep -x torus-tx-flood >/dev/null; then echo "FATAL: a torus-tx-flood is already running" >&2; exit 1; fi
fi
if [ -f "$RUN_DIR/pids" ] && xargs -a "$RUN_DIR/pids" -r -I{} kill -0 {} 2>/dev/null; then
    echo "FATAL: a devnet is already running ($RUN_DIR/pids) — run $WSL/stop-3val.sh first" >&2; exit 1
fi
# The live testnet validator (8555/9090/30333) is never touched: we only use
# 8645-8647 / 9161-9163 / 30401-30403 and never write into ~/.cargo-target.
if [ -d "$OUT" ] && [ -n "$(ls -A "$OUT" 2>/dev/null)" ] && [ "${OVERWRITE:-0}" != 1 ]; then
    echo "FATAL: $OUT exists and is non-empty (pick a new label or OVERWRITE=1)" >&2; exit 1
fi
mkdir -p "$OUT"
: > "$OUT/run.log"

SAMPLER_PID=""; CPU_PID=""; BENCH_PID=""; CRASH_PID=""; ORACLE_PID=""; ORACLE_RC=""
# alive <pid>: running or stopped, NOT an unreaped zombie (kill -0 succeeds on those).
alive() { local s; s=$(ps -o stat= -p "$1" 2>/dev/null) && [ -n "$s" ] && [ "${s#Z}" = "$s" ]; }
# Idempotent; safe on every exit path. TERM first, CONT so a feed paused for the
# drain receives it, 10 s grace, then KILL. The feed writes its stats on TERM.
stop_oracle_feed() {
    [ -n "$ORACLE_PID" ] || return 0
    local pid=$ORACLE_PID _t
    ORACLE_PID=""
    kill -TERM "$pid" 2>/dev/null; kill -CONT "$pid" 2>/dev/null
    for _t in $(seq 1 50); do alive "$pid" || break; sleep 0.2; done
    if alive "$pid"; then
        kill -KILL "$pid" 2>/dev/null
        log "WARNING: oracle feed pid $pid ignored SIGTERM for 10 s — SIGKILLed (stats may be missing)"
    fi
    wait "$pid" 2>/dev/null; ORACLE_RC=$?
    log "oracle feed pid $pid stopped rc=$ORACLE_RC"
}
# ---- EVM load (EVM_LOAD_RATE > 0): tx-flood runs beside the bench.
EVM_PID=""; EVM_RC=""; EVM_H0=""; EVM_H1=""; T_EVM0=""; EVM_KEYS=""; EVM_CHAIN_ID=""; EVM_N=""
# evm_load_start: tx-flood in the background, from val0's committed height EVM_H0.
evm_load_start() {
    local h
    h=$(scrape_one "${METS[0]}" | mval torus_block_height); EVM_H0=${h%.*}
    EVM_LOAD_CMD=("$TX_FLOOD" --rpc-urls "$(IFS=,; printf '%s' "${RPCS[*]}")" --fanout pinned \
        --keys-file "$EVM_KEYS" --chain-id "$EVM_CHAIN_ID" --rate "$EVM_LOAD_RATE" \
        --duration-secs "$DUR" --stats-file "$OUT/evm-load-stats.json")
    log "EVM load: ${EVM_LOAD_CMD[*]} (val0 committed height $EVM_H0)"
    T_EVM0=$(date +%s)
    "${EVM_LOAD_CMD[@]}" > "$OUT/evm-load.log" 2>&1 & EVM_PID=$!
}
# evm_load_stop: idempotent, safe on every exit path. TERM (tx-flood stops
# sending and writes its stats), 20 s grace (tx-flood drains for up to 15 s: 12 s send timeout + 3 s), then KILL; EVM_H1 = val0's
# committed height when it stopped.
evm_load_stop() {
    [ -n "$EVM_PID" ] || return 0
    local pid=$EVM_PID _t h
    EVM_PID=""
    kill -TERM "$pid" 2>/dev/null
    for _t in $(seq 1 100); do alive "$pid" || break; sleep 0.2; done
    if alive "$pid"; then
        kill -KILL "$pid" 2>/dev/null
        log "WARNING: tx-flood pid $pid ignored SIGTERM for 20 s — SIGKILLed (stats may be missing)"
    fi
    wait "$pid" 2>/dev/null; EVM_RC=$?
    h=$(scrape_one "${METS[0]}" | mval torus_block_height); EVM_H1=${h%.*}
    [ -z "$EVM_KEYS" ] || rm -f -- "${EVM_KEYS:?}"
    log "EVM load: tx-flood pid $pid stopped rc=$EVM_RC after $(( $(date +%s) - T_EVM0 ))s (val0 committed height $EVM_H1)"
}
stop_sampler() {
    [ -n "$SAMPLER_PID" ] || return 0
    local pid="$SAMPLER_PID" rc=0
    kill "$pid" 2>/dev/null || true
    # The Python sampler cancels, terminates and reaps every in-flight curl
    # before exiting. Reap it too, including during an interrupted cell.
    wait "$pid" || rc=$?
    SAMPLER_PID=""
    return "$rc"
}
finish_fail() {
    stop_sampler || true
    [ -n "$CPU_PID" ] && kill "$CPU_PID" 2>/dev/null
    [ -n "$BENCH_PID" ] && kill "$BENCH_PID" 2>/dev/null
    [ -n "$CRASH_PID" ] && kill "$CRASH_PID" 2>/dev/null
    stop_oracle_feed
    evm_load_stop
    "$WSL/stop-3val.sh" >>"$OUT/run.log" 2>&1 || true
    python3 - "$OUT" <<'PY' 2>/dev/null || true
import json,sys,os,time
p=os.path.join(sys.argv[1],'summary.json')
d={}
if os.path.exists(p):
    try:
        d=json.load(open(p))
    except (OSError, ValueError):
        d={}
d['status']='FAILED'; d['failed_at']=time.strftime('%Y-%m-%dT%H:%M:%S')
json.dump(d,open(p,'w'),indent=1)
PY
}
trap 'log "interrupted"; finish_fail; exit 130' INT TERM
# Exit paths that bypass finish_fail (plain `exit`) must not leave a feed behind.
if [ "$ORACLE_FEED" = 1 ]; then trap 'stop_oracle_feed' EXIT; fi
# ... nor a tx-flood (stop_oracle_feed is a no-op without a feed).
if evm_load_on; then trap 'stop_oracle_feed; evm_load_stop' EXIT; fi

log "cell=$LABEL worktree=$WT markets=$MARKETS dur=${DUR}s rate=$RATE senders=$SENDERS block_cap='${BLOCK_CAP:-unset}' mps='${MPS:-unset}' band=$BAND cross=$CROSS_FRACTION cancel=$CANCEL_FRACTION open_order_budget='${OPEN_ORDER_BUDGET:-unset}' retry_busy='${RETRY_BUSY:-unset}' max_in_flight='${MAX_IN_FLIGHT:-unset}' in_flight_tail='${IN_FLIGHT_TAIL:-unset}' antispam='${ANTISPAM:-unset}' spam_cancel_keys='${SPAM_CANCEL_KEYS:-unset}' spam_cancel_rate='${SPAM_CANCEL_RATE:-unset}' spam_cancel_funded='${SPAM_CANCEL_FUNDED:-unset}' cancel_by_id='${CANCEL_BY_ID_FRACTION:-unset}' modify='${MODIFY_FRACTION:-unset}' cloid_share='${CLOID_SHARE:-unset}' bulk_count=$BULK_COUNT preload='$(preload_on && echo "$PRELOAD" || echo off)' extra_env='$EXTRA_ENV'"
log "drain_timeout=${DRAIN_TIMEOUT}s digest_par=$DIGEST_PAR rpc_timeout=${RPC_TIMEOUT}s"
[ -n "$BLOCK_CAP" ] && log "block-cap bundle (BLOCK_CAP=$BLOCK_CAP, BATCH=$BATCH): ${BLOCK_CAP_ENV[*]}"
if liq_stress_on; then
    log "liquidation stress ON: liq_thin=$LIQ_THIN liq_thin_avail=$LIQ_THIN_AVAIL shock_bp=$ORACLE_SHOCK_BP shock_round=$ORACLE_SHOCK_ROUND vault=$LIQUIDATOR_VAULT adl_escrows=$ADL_ESCROW_LONG,$ADL_ESCROW_SHORT (balances + positions in the digest; vault-val<i>.json; liquidation metrics sampled)"
    [ -z "$MPS" ] || log "WARNING: MPS=$MPS: the locality shape sets sides by owner rank, not (sender + market) parity, so the shock's 'even-index senders lose on every position' does not hold"
fi
[ "$ORACLE_FEED" = 1 ] && log "oracle feed ON: price=$ORACLE_PRICE walk_bp=$ORACLE_WALK_BP shock_bp=$ORACLE_SHOCK_BP shock_round=$ORACLE_SHOCK_ROUND interval=${ORACLE_INTERVAL_MS}ms markets=$MARKETS keys=$ORACLE_KEYS fresh_timeout=${ORACLE_FRESH_TIMEOUT}s"
if evm_load_on; then
    log "EVM load ON: rate=$EVM_LOAD_RATE tx/s senders=$EVM_LOAD_SENDERS (bench senders $EVM_LOAD_OFFSET..$((EVM_LOAD_OFFSET + EVM_LOAD_SENDERS - 1)), the k-th sender -> val k % 3 only) tx_flood=$TX_FLOOD md5=$(md5sum "$TX_FLOOD" | cut -d' ' -f1)"
    if awk -v r="$EVM_LOAD_RATE" -v s="$EVM_LOAD_SENDERS" 'BEGIN{exit !(r / s > 2)}'; then
        log "WARNING: EVM_LOAD_RATE / EVM_LOAD_SENDERS > 2 tx/s per sender: the node's per-sender limits (16 pooled, 50 per 10 s at the RPC, nonce <= executed + 64) will refuse some txs"
    fi
fi
WT_COMMIT=$(git -C "$WT" rev-parse HEAD)
WT_DIRTY=$(git -C "$WT" status --porcelain --untracked-files=no | wc -l)
log "worktree commit=$WT_COMMIT dirty_files=$WT_DIRTY"

# ---------------------------------------------------------------- 1. stage binary
mkdir -p "$(dirname "$DST_NODE")"
cp -f "$SRC_NODE" "$DST_NODE" || die "copy failed"
MD5_SRC=$(md5sum "$SRC_NODE" | cut -d' ' -f1)
MD5_DST=$(md5sum "$DST_NODE" | cut -d' ' -f1)
MD5_BENCH=$(md5sum "$BENCH" | cut -d' ' -f1)
log "torus-node md5 src=$MD5_SRC ($SRC_NODE, mtime $(date -r "$SRC_NODE" +%FT%T))${EXPECT_NODE_MD5:+ expected $EXPECT_NODE_MD5}"
log "torus-node md5 dst=$MD5_DST ($DST_NODE)"
[ "$MD5_SRC" = "$MD5_DST" ] || die "md5 mismatch after copy (stale-binary trap)"
log "bench-throughput md5=$MD5_BENCH ($BENCH, mtime $(date -r "$BENCH" +%FT%T))${EXPECT_BENCH_MD5:+ expected $EXPECT_BENCH_MD5}"
[ "${MD5_SRC:0:${#EXPECT_NODE_MD5}}" = "$EXPECT_NODE_MD5" ] \
    || die "torus-node md5 $MD5_SRC != EXPECT_NODE_MD5 $EXPECT_NODE_MD5 ($SRC_NODE is not the binary built for this cell: stale-binary trap)"
[ "${MD5_BENCH:0:${#EXPECT_BENCH_MD5}}" = "$EXPECT_BENCH_MD5" ] \
    || die "bench-throughput md5 $MD5_BENCH != EXPECT_BENCH_MD5 $EXPECT_BENCH_MD5 ($BENCH is not the binary built for this cell: stale-binary trap)"

# ---------------------------------------------------------------- 2. genesis
# s68: TIMEOUT_BASE_MS is a genesis parameter, not a node env var, and genesis is
# generated before EXTRA_ENV is exported, so lift it out of EXTRA_ENV here.
# Absent => the generator's default.
GEN_TIMEOUT_BASE_MS=""
for kv in $EXTRA_ENV; do case $kv in TIMEOUT_BASE_MS=*) GEN_TIMEOUT_BASE_MS=${kv#*=} ;; esac; done
# GENESIS_CACHE_DIR: the key of every genesis input (genesis_cache.py); a cached
# entry for it replaces the generator and the checks below (header: GENESIS_CACHE_DIR).
genesis_key() { # key | store ARGS...: genesis_cache.py with this cell's key inputs
    local cmd=$1; shift
    python3 -I "$SELF_DIR/genesis_cache.py" "$cmd" --root "$MAINREPO" --run-cell "$SELF_DIR/run-cell.sh" \
        --param MARKETS="$MARKETS" --param BULK_COUNT="$BULK_COUNT" --param TIMEOUT_BASE_MS="$GEN_TIMEOUT_BASE_MS" \
        --param EVM_BLOCK_INTERVAL="$GEN_EVM_BLOCK_INTERVAL" --param LIQ_THIN="$LIQ_THIN" --param LIQ_THIN_AVAIL="$LIQ_THIN_AVAIL" "$@"
}
genesis_key_store() {
    genesis_key store --dir "$GENESIS_CACHE_DIR" --genesis "$GENESIS" --md5 "$GEN_MD5" $GEN_FORCED \
        --meta label="$LABEL" --meta markets="$GEN_MARKETS" --meta native_balances="$GEN_ACCTS" --meta bulk_count="$GEN_BULK" \
        --meta timeout_base_ms="$GEN_TBMS" --meta evm_block_interval="$GEN_EVM_N" --meta liq_thin_rows="${LIQ_THIN_ROWS:-}" \
        --gen-out <(tail -c +$((GEN_LOG_FROM + 1)) "$OUT/run.log" | head -c $((GEN_LOG_TO - GEN_LOG_FROM))) \
        2>>"$OUT/run.log"
}
GEN_REUSED=0; GEN_KEY=""; GEN_FORCED=""
[ "${FORCE:-0}" = 1 ] && GEN_FORCED=--forced
if [ -n "$GENESIS_CACHE_DIR" ]; then
    GEN_KEY=$(genesis_key key 2>>"$OUT/run.log") || die "genesis key failed (genesis_cache.py, see run.log)"
    GEN_META=$(python3 -I "$SELF_DIR/genesis_cache.py" fetch --dir "$GENESIS_CACHE_DIR" --key "$GEN_KEY" --dest "$GENESIS" \
        --root "$MAINREPO" $GEN_FORCED 2>"$OUT/genesis-cache.err")
    case $? in
        0) GEN_REUSED=1 ;;
        1) log "genesis cache: $(cat "$OUT/genesis-cache.err") -> building" ;;
        *) log "WARNING: genesis cache: $(cat "$OUT/genesis-cache.err") -> rebuilding (the entry is overwritten)" ;;
    esac
    rm -f -- "${OUT:?}/genesis-cache.err"
fi
if [ "$GEN_REUSED" = 1 ]; then
    # The entry holds the values the build path reads with jq below (its md5 was re-checked on the copy).
    # (the one field that may be empty comes last: read collapses adjacent tabs)
    IFS=$'\t' read -r GEN_MD5 GEN_MARKETS GEN_ACCTS GEN_BULK GEN_TBMS GEN_EVM_N GEN_BUILT LIQ_THIN_ROWS < <(jq -r \
        '[.md5, .markets, .native_balances, .bulk_count, .timeout_base_ms, .evm_block_interval, .built_at, (.liq_thin_rows // "")] | @tsv' <<<"$GEN_META")
    # run.log gets the build path's lines in their form (checks and spec expectations read them, e.g. the
    # generator's "  state_hash_activation = off"): this line, then the generator's output as it prints it for
    # this cell (genesis_cache.py replay: its summary from the entry), then the reuse line (test_genesis_reuse_lines.py).
    log "generating $MARKETS-market 3-val genesis -> $GENESIS"
    jq -r '.replay[]' <<<"$GEN_META" >> "$OUT/run.log" || die "genesis cache entry $GEN_KEY: no generator lines to replay"
    log "genesis reused (key $GEN_KEY, md5 $GEN_MD5) from $GENESIS_CACHE_DIR (built $GEN_BUILT${GEN_FORCED:+ under FORCE=1}): generator, weighted-full check and jq passes skipped, the generator lines above replayed from the entry -> $GENESIS"
    log "genesis timeout_base_ms=$GEN_TBMS"
else
    log "generating $MARKETS-market 3-val genesis -> $GENESIS"
    if [ "${FORCE:-0}" != 1 ]; then
        WHY=$(weighted_full_check "$MAINREPO/testnet/genesis-weighted-full.json" "$SENDERS") || die "$WHY"
    fi
    # The generator's run.log bytes (GEN_LOG_FROM..GEN_LOG_TO) go into the cache entry (genesis_key_store).
    GEN_LOG_FROM=$(stat -c %s -- "$OUT/run.log")
    TIMEOUT_BASE_MS=$GEN_TIMEOUT_BASE_MS EVM_BLOCK_INTERVAL=$GEN_EVM_BLOCK_INTERVAL MARKETS=$MARKETS BULK_COUNT=$BULK_COUNT OUT="$GENESIS" BENCH_BIN="$BENCH" "$MAINREPO/devnet/wsl/gen-3val-genesis.sh" >>"$OUT/run.log" 2>&1 \
        || die "genesis generation failed (see run.log)"
    GEN_LOG_TO=$(stat -c %s -- "$OUT/run.log")
    GEN_TBMS=$(jq '.consensus.timeout_base_ms' "$GENESIS")
    log "genesis timeout_base_ms=$GEN_TBMS"
    # The genesis EVM cadence N must be the cell's: EVM_BLOCK_INTERVAL, else the
    # node default. A mismatch = a hand-patched weighted-full genesis carried over.
    GEN_EVM_N=$(jq -r ".evm.block_interval // $DEFAULT_EVM_N" "$GENESIS")
fi
WANT_EVM_N=${GEN_EVM_BLOCK_INTERVAL:-$DEFAULT_EVM_N}
[ "$GEN_EVM_N" = "$WANT_EVM_N" ] \
    || die "genesis evm.block_interval=$GEN_EVM_N, wanted $WANT_EVM_N (EVM_BLOCK_INTERVAL='$GEN_EVM_BLOCK_INTERVAL'): gen-3val-genesis.sh copies .evm from $MAINREPO/testnet/genesis-weighted-full.json; remove a hand patch there (set N per cell with EXTRA_ENV EVM_BLOCK_INTERVAL=N)"
if [ -n "$GEN_EVM_BLOCK_INTERVAL" ]; then
    log "genesis evm.block_interval=$GEN_EVM_N (EVM_BLOCK_INTERVAL=$GEN_EVM_BLOCK_INTERVAL)"
else
    log "genesis evm.block_interval=$GEN_EVM_N (absent: node default)"
fi
if [ "$LIQ_THIN" -gt 0 ]; then
    if [ "$GEN_REUSED" != 1 ]; then
        LIQ_THIN_ROWS=$(liq_thin_patch "$GENESIS" "$LIQ_THIN" "$LIQ_THIN_AVAIL" 2>>"$OUT/run.log") \
            || die "LIQ_THIN=$LIQ_THIN genesis patch failed (see run.log)"
    fi
    log "LIQ_THIN=$LIQ_THIN: native available=$LIQ_THIN_AVAIL on $LIQ_THIN_ROWS genesis rows 'bulk-test 60'..'bulk-test $((60 + LIQ_THIN - 1))' (load senders 0..$((LIQ_THIN - 1)); under a shock the EVEN ones lose on every position)"
fi
if [ "$GEN_REUSED" != 1 ]; then
    # One jq pass (each parses the whole genesis: ~3 s at BULK_COUNT=1M).
    read -r GEN_MARKETS GEN_ACCTS GEN_BULK < <(jq -r '[(.markets|length), (.native_balances|length),
        ([.native_balances[] | (.note // "") | select(test("^bulk-test [0-9]+$"))] | length)] | @tsv' "$GENESIS")
    GEN_MD5=$(md5sum "$GENESIS" | cut -d' ' -f1)
fi
[ "$GEN_MARKETS" = "$MARKETS" ] || die "genesis has $GEN_MARKETS markets, wanted $MARKETS"
[ "$GEN_BULK" = "$BULK_COUNT" ] || die "genesis has $GEN_BULK bulk-test senders, wanted BULK_COUNT=$BULK_COUNT"
log "genesis markets=$GEN_MARKETS native_balances=$GEN_ACCTS bulk_count=$GEN_BULK md5=$GEN_MD5"
if [ "$GEN_REUSED" != 1 ]; then
    if [ -n "$GENESIS_CACHE_DIR" ]; then
        # Keyed again by the store, after the build: a first (or FORCE=1) build writes the weighted-full genesis.
        if GEN_SKEY=$(genesis_key_store); then
            GEN_KEY=$GEN_SKEY
            log "genesis built (key $GEN_KEY, md5 $GEN_MD5${GEN_FORCED:+, FORCE=1}); cached in $GENESIS_CACHE_DIR"
        else
            log "WARNING: genesis built (md5 $GEN_MD5) but not cached (genesis_cache.py store failed, see run.log): the next cell builds again"
        fi
    else
        log "genesis built (no GENESIS_CACHE_DIR: not cached)"
    fi
fi
if evm_load_on; then
    # Sender keys (deterministic bench keys) live beside the devnet's pids and
    # logs, never in the results dir; evm_load_stop deletes them.
    mkdir -p "$RUN_DIR"
    EVM_KEYS="$RUN_DIR/evm-load-keys.txt"
    "$BENCH" gen-accounts --offset "$EVM_LOAD_OFFSET" --count "$EVM_LOAD_SENDERS" --secret-keys > "$EVM_KEYS" 2>>"$OUT/run.log" \
        || die "EVM load: bench-throughput gen-accounts --secret-keys failed (see run.log)"
    EVM_FUNDED=$(python3 "$SELF_DIR/evm_load.py" funded --genesis "$GENESIS" --keys "$EVM_KEYS" 2>&1) || die "EVM load: $EVM_FUNDED"
    EVM_CHAIN_ID=$(jq -r '.evm.chain_id // empty' "$GENESIS")
    EVM_N=$GEN_EVM_N
    [[ "$EVM_CHAIN_ID" =~ ^[0-9]+$ ]] || die "EVM load: $GENESIS has no evm.chain_id"
    log "EVM load: $EVM_FUNDED; evm.chain_id=$EVM_CHAIN_ID evm.block_interval=$EVM_N"
fi

# ---------------------------------------------------------------- 3. launch
# start from a clean TORUS_* env: only the record env + EXTRA_ENV reach the nodes
for v in $(env | grep -oE '^TORUS_[A-Za-z0-9_]+'); do unset "$v"; done
for kv in "${RECORD_ENV[@]}"; do export "$kv"; done
for kv in "${BLOCK_CAP_ENV[@]}"; do export "$kv"; done
if [ "$ANTISPAM" = 1 ]; then
    export TORUS_INGRESS_MIN_COLLATERAL=1 TORUS_ADDR_RATE_LIMIT=1 TORUS_RPC_IP_WEIGHT_PER_MIN=1200
fi
if [ -n "$MAX_IN_FLIGHT" ]; then
    export TORUS_RPC_MAX_RESPONSE_MB=64
    log "MAX_IN_FLIGHT=$MAX_IN_FLIGHT: TORUS_RPC_MAX_RESPONSE_MB=64 on the nodes (loaded torus_getBlockBody replies pass the 10 MiB default)"
fi
for kv in $EXTRA_ENV; do export "$kv"; done
# HOTSTUFF_CPUS is a HARNESS knob (taskset, not read by the node) but is logged
# alongside the node env so a pinned cell is recognisable from its summary.
# Any TORUS_* var in EXTRA_ENV (e.g. TORUS_ROCKSDB_STATS=2) is already covered.
[ -n "$HOTSTUFF_CPUS" ] && export HOTSTUFF_CPUS
NODE_ENV_JSON=$(env | grep -E '^(TORUS_|HOTSTUFF_CPUS=)' | sort | python3 -c 'import sys,json;print(json.dumps(dict(l.rstrip("\n").split("=",1) for l in sys.stdin)))')
log "node env: $NODE_ENV_JSON"
T_LAUNCH=$(date +%s)
CLEAN=1 "$WSL/launch-3val.sh" >>"$OUT/run.log" 2>&1 || die "launch-3val.sh failed"
sleep 2
mapfile -t PIDS < "$RUN_DIR/pids"
log "node pids: ${PIDS[*]}"
# verify the env actually reached each node process (fleet-uniform)
ENV_VERIFY=$(for p in "${PIDS[@]}"; do tr '\0' '\n' < /proc/$p/environ 2>/dev/null | grep -E '^TORUS_' | sort | md5sum | cut -c1-8; done | sort -u | tr '\n' ' ')
log "per-node TORUS_* env digest(s): $ENV_VERIFY (must be a single value)"

# ---------------------------------------------------------------- 4. health
scrape_one() { curl -fsS -m 3 "http://127.0.0.1:$1/metrics"; }
mval() { awk -v m="$1" '$1==m {print $2; f=1} END{if(!f) print 0}'; }

log "waiting for health (all 3 committing, peers>=2, timeout ${HEALTH_TIMEOUT}s)"
ok=0; t0=$(date +%s)
declare -a C0=(0 0 0)
while [ $(( $(date +%s) - t0 )) -lt "$HEALTH_TIMEOUT" ]; do
    good=0
    for i in 0 1 2; do
        M=$(scrape_one "${METS[$i]}") || true
        c=$(printf '%s' "$M" | mval torus_blocks_committed_total)
        pc=$(printf '%s' "$M" | mval torus_peers_connected)
        c=${c%.*}; pc=${pc%.*}
        if [ "${c:-0}" -ge $(( ${C0[$i]} + 5 )) ] && [ "${pc:-0}" -ge 2 ]; then good=$((good+1)); fi
        [ "${C0[$i]}" = 0 ] && C0[$i]=${c:-0}
    done
    for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null || die "node pid $p died during startup"; done
    [ "$good" = 3 ] && { ok=1; break; }
    sleep 2
done
[ "$ok" = 1 ] || die "devnet not healthy after ${HEALTH_TIMEOUT}s"
T_HEALTHY=$(date +%s)
log "healthy after $((T_HEALTHY - T_LAUNCH))s"
# ---- HOTSTUFF_CPUS: pin each node's consensus thread -----------------------
if [ -n "$HOTSTUFF_CPUS" ]; then
    IFS=/ read -r -a HS_CPU <<< "$HOTSTUFF_CPUS"
    [ "${#HS_CPU[@]}" = 3 ] || die "HOTSTUFF_CPUS must be a/b/c (one CPU list per node), got '$HOTSTUFF_CPUS'"
    command -v taskset >/dev/null || die "HOTSTUFF_CPUS set but taskset not found"
    for i in 0 1 2; do
        p=${PIDS[$i]}
        tid=""
        for _try in $(seq 1 20); do tid=$(find_tid "$p" hotstuff-algo) && break; sleep 0.5; done
        if [ -n "$tid" ]; then
            if taskset -pc "${HS_CPU[$i]}" "$tid" >>"$OUT/run.log" 2>&1; then
                log "val$i pid=$p hotstuff-algo tid=$tid Cpus_allowed_list=$(awk '/^Cpus_allowed_list/{print $2}' /proc/$p/task/$tid/status) (HOTSTUFF_CPUS='$HOTSTUFF_CPUS')"
            else
                log "WARNING: val$i pid=$p taskset -pc ${HS_CPU[$i]} $tid FAILED — hotstuff-algo left unpinned"
            fi
        else
            log "WARNING: val$i pid=$p has no thread named hotstuff-algo (binary predates the naming, or not spawned within 10 s) — left unpinned (HOTSTUFF_CPUS='$HOTSTUFF_CPUS')"
        fi
    done
fi
# idle probe (10 s) for idle blk/s; startup view-timeout wobble can make the
# first probe read low, so re-probe up to 3x until the mesh settles (>=10 blk/s).
for _try in 1 2 3; do
    ic0=$(scrape_one "${METS[0]}" | mval torus_blocks_committed_total); sleep 10
    ic1=$(scrape_one "${METS[0]}" | mval torus_blocks_committed_total)
    IDLE_BLKS=$(awk -v a="$ic0" -v b="$ic1" 'BEGIN{printf "%.2f",(b-a)/10}')
    log "idle blk/s (val0, 10s, probe $_try) = $IDLE_BLKS"
    awk -v v="$IDLE_BLKS" 'BEGIN{exit !(v>=10)}' && break
done

# ---------------------------------------------------------------- 4b. oracle feed (ORACLE_FEED=1)
# oracle_marks <since-height>: one JSON object on stdout, from val0's
# torus_getMarkPrice for markets 1..MARKETS (whole pass capped at 30 s).
#   usable = markPrice > 0: the RPC reports 0 unless the mark is usable, i.e.
#            <= 60 s past its last FRESH aggregate (crates/torus-rpc torus.rs
#            get_mark_price -> usable_oracle_price);
#   fresh  = usable AND that aggregate was written in a block > since-height
#            (the RPC's `timestamp` field carries the aggregate's BLOCK number):
#            a fresh aggregate needs all 3 validators inside one 10 s window, so
#            this proves the feed's submissions from every validator landed.
oracle_marks() {
    python3 - "${RPCS[0]}" "$MARKETS" "$1" <<'PYO'
import json, sys, time, urllib.request
url, n, since = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
deadline = time.monotonic() + 30
# urllib/socket failures are OSError; bad JSON is ValueError; a missing or
# malformed result is KeyError/TypeError. Each counts as an error, never a mark.
RPC_ERRORS = (OSError, ValueError, KeyError, TypeError)
def call(method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(url, body, {"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.load(r)["result"]
out = {"since_height": since, "markets": n, "checked": 0, "usable": 0, "fresh": 0, "errors": 0,
       "head": None, "oldest_usable_agg_block": None, "max_agg_age_blocks": None, "not_fresh_ids": []}
try:
    out["head"] = int(call("eth_blockNumber", []), 16)
except RPC_ERRORS:
    out["errors"] += 1
for mid in range(1, n + 1):
    if time.monotonic() > deadline:
        break
    out["checked"] += 1
    try:
        r = call("torus_getMarkPrice", [hex(mid)])
        usable, blk = float(r["markPrice"]) > 0, int(r["timestamp"])
    except RPC_ERRORS:
        out["errors"] += 1
        usable, blk = False, 0
    if usable:
        out["usable"] += 1
        o = out["oldest_usable_agg_block"]
        out["oldest_usable_agg_block"] = blk if o is None else min(o, blk)
    if usable and blk > since:
        out["fresh"] += 1
    elif len(out["not_fresh_ids"]) < 20:
        out["not_fresh_ids"].append(mid)
if out["head"] is not None and out["oldest_usable_agg_block"] is not None:
    out["max_agg_age_blocks"] = out["head"] - out["oldest_usable_agg_block"]
sys.stdout.write(json.dumps(out) + "\n")
PYO
}
ORACLE_FRESH_S=""; ORACLE_MEND=""
if [ "$ORACLE_FEED" = 1 ]; then
    ORACLE_M=$(oracle_marks 0)
    printf '%s\n' "$ORACLE_M" > "$OUT/oracle-marks-before.json"
    ORACLE_H0=$(jq -r '.head // empty' <<<"$ORACLE_M")
    [[ "$ORACLE_H0" =~ ^[0-9]+$ ]] || die "oracle feed: cannot read val0's head (eth_blockNumber): $ORACLE_M"
    log "oracle marks before the feed: usable=$(jq .usable <<<"$ORACLE_M")/$MARKETS head=$ORACLE_H0"
    ORACLE_CMD=("$BENCH" oracle-feed --rpc-urls "$(IFS=,; printf '%s' "${RPCS[*]}")" --validator-keys "$ORACLE_KEYS" \
        --markets "$MARKETS" --price "$ORACLE_PRICE" --interval-ms "$ORACLE_INTERVAL_MS" \
        --stats-file "$OUT/oracle-feed-stats.json")
    [ "$ORACLE_WALK_BP" = 0 ] || ORACLE_CMD+=(--walk-bp "$ORACLE_WALK_BP")
    [ "${ORACLE_SHOCK_BP:-0}" = 0 ] || ORACLE_CMD+=(--shock-bp "$ORACLE_SHOCK_BP" --shock-round "$ORACLE_SHOCK_ROUND")
    log "oracle feed: ${ORACLE_CMD[*]}"
    T_ORACLE0=$(date +%s)
    "${ORACLE_CMD[@]}" > "$OUT/oracle-feed.log" 2>&1 & ORACLE_PID=$!
    log "oracle feed pid $ORACLE_PID; waiting for fresh marks on all $MARKETS markets (aggregate block > $ORACLE_H0, timeout ${ORACLE_FRESH_TIMEOUT}s)"
    while [ $(( $(date +%s) - T_ORACLE0 )) -lt "$ORACLE_FRESH_TIMEOUT" ]; do
        sleep 3
        alive "$ORACLE_PID" || die "oracle feed exited before the marks were fresh — oracle-feed.log tail: $(tail -c 600 "$OUT/oracle-feed.log" | tr '\n' ' ')"
        ORACLE_M=$(oracle_marks "$ORACLE_H0")
        [ "$(jq -r .fresh <<<"$ORACLE_M")" = "$MARKETS" ] && { ORACLE_FRESH_S=$(( $(date +%s) - T_ORACLE0 )); break; }
    done
    printf '%s\n' "$ORACLE_M" > "$OUT/oracle-marks-fresh.json"
    [ -n "$ORACLE_FRESH_S" ] || die "oracle marks NOT fresh on all $MARKETS markets after ${ORACLE_FRESH_TIMEOUT}s: $ORACLE_M (see oracle-feed.log)"
    log "oracle marks fresh on all $MARKETS markets after ${ORACLE_FRESH_S}s: $ORACLE_M"
fi

# ---------------------------------------------------------------- 4c. pre-load (PRELOAD > 0)
# After ready (and fresh marks: the placement band is around the mark), BEFORE
# the metrics-before snapshot, the samplers and the load: nothing below counts
# the pre-load's blocks as load. Its seconds are reported apart from boot ->
# ready (.preload.seconds vs .preload.boot_to_ready_s).
PRELOAD_S=""; PRELOAD_RC=""
if preload_on; then
    log "pre-load: PRELOAD=$PRELOAD resting orders over $MARKETS markets, owners (count first end) $PRELOAD_OWNERS: ${PRELOAD_CMD[*]}"
    T_PRELOAD0=$(date +%s)
    timeout --kill-after=10 "$((PRELOAD_TIMEOUT + PRELOAD_KILL_GRACE))" "${PRELOAD_CMD[@]}" > "$OUT/preload.log" 2>&1; PRELOAD_RC=$?
    PRELOAD_S=$(( $(date +%s) - T_PRELOAD0 ))
    for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null || die "node pid $p died during the pre-load"; done
    if PRELOAD_V=$(preload_record "$PRELOAD_RC" "$PRELOAD_S"); then
        log "pre-load done rc=$PRELOAD_RC in ${PRELOAD_S}s (boot -> ready $((T_HEALTHY - T_LAUNCH))s, apart): $PRELOAD_V"
    else
        die "pre-load missed PRELOAD=$PRELOAD (rc=$PRELOAD_RC after ${PRELOAD_S}s): $(jq -c '.preload | {resting, reached, problems}' "$OUT/summary.json" 2>/dev/null || echo 'no record') (preload.log, preload.json; summary.json validity REJECT)"
    fi
fi

# ---------------------------------------------------------------- 5. samplers
# Wide CSV: independent per-node 1 Hz attempts (bounded requests, no catch-up),
# timestamped at response completion; per-node CSVs retain the exact
# layouts of the RE-PROOF5 scrape.sh / scrape-phase.sh so win60.awk / phase60.awk
# run unchanged.
FUNNEL_COLS="torus_native_actions_processed_total torus_orders_placed_accepted_total torus_orders_matched_total torus_orders_resting_total torus_orders_rejected_margin_total torus_orders_rejected_open_limit_total torus_orders_rejected_book_total torus_orders_rejected_cancelled_total torus_orders_rejected_other_total torus_orders_self_trade_cancels_total torus_orders_cancelled_partial_fill_total torus_blocks_committed_total torus_block_height torus_exec_queue_depth"
# Keep the positional phase60.awk layout; detailed write metrics belong in WIDE_COLS.
PHASE_COLS="torus_blocks_committed_total torus_block_height torus_orders_placed_accepted_total torus_orders_matched_total torus_orders_resting_total torus_exec_resting_orders torus_exec_load_books_seconds_sum torus_exec_load_books_seconds_count torus_exec_root_seconds_sum torus_exec_root_seconds_count torus_exec_state_write_seconds_sum torus_exec_state_write_seconds_count torus_exec_evm_resync_seconds_sum torus_exec_evm_resync_seconds_count torus_exec_flush_seconds_sum torus_exec_flush_seconds_count torus_exec_root_dirty_buckets_sum torus_exec_root_dirty_buckets_count torus_exec_queue_depth torus_exec_root_bucket_scans_total torus_member_cache_hits_total torus_member_cache_misses_total torus_member_cache_evictions_total torus_member_cache_resident_buckets"
WIDE_COLS="torus_blocks_committed_total torus_block_height torus_native_actions_processed_total torus_orders_placed_accepted_total torus_orders_matched_total torus_orders_resting_total torus_exec_resting_orders torus_orders_rejected_margin_total torus_orders_rejected_open_limit_total torus_orders_rejected_book_total torus_orders_rejected_cancelled_total torus_orders_rejected_other_total torus_exec_queue_depth torus_mempool_native_size torus_exec_block_seconds_sum torus_exec_block_seconds_count torus_exec_engine_seconds_count torus_exec_verify_seconds_count torus_exec_flush_seconds_count torus_exec_evm_seconds_sum torus_exec_verify_seconds_sum torus_exec_replay_guard_seconds_sum torus_exec_load_books_seconds_sum torus_exec_engine_seconds_sum torus_exec_phase_margin_seconds_sum torus_exec_phase_match_seconds_sum torus_exec_phase_settle_seconds_sum torus_exec_phase1_actions_seconds_sum torus_exec_phase1_actions_seconds_count torus_exec_settle_pass_a_seconds_sum torus_exec_settle_pass_b_seconds_sum torus_exec_cache_flush_seconds_sum torus_exec_post_engine_tail_seconds_sum torus_exec_engine_untimed_seconds_sum torus_exec_engine_untimed_seconds_count torus_exec_save_books_seconds_sum torus_exec_flush_seconds_sum torus_exec_root_seconds_sum torus_exec_state_write_seconds_sum torus_exec_state_write_build_seconds_sum torus_exec_state_write_build_seconds_count torus_exec_state_write_db_seconds_sum torus_exec_state_write_db_seconds_count torus_exec_state_write_batch_bytes_sum torus_exec_state_write_batch_bytes_count torus_exec_evm_resync_seconds_sum torus_exec_body_persist_seconds_sum torus_exec_root_dirty_buckets_sum torus_exec_root_dirty_buckets_count torus_exec_root_bucket_scans_total torus_member_cache_evictions_total torus_commit_interval_seconds_sum torus_commit_interval_seconds_count torus_exec_chain_seconds_sum torus_exec_chain_seconds_count torus_exec_handoff_wait_seconds_sum torus_exec_handoff_wait_seconds_count torus_flush_worker_seconds_sum torus_flush_worker_seconds_count torus_flush_worker_depth torus_exec_save_books_seconds_count torus_exec_save_books_drain_seconds_sum torus_exec_save_books_drain_seconds_count torus_exec_save_books_write_seconds_sum torus_exec_save_books_write_seconds_count torus_exec_native_blocks_total torus_consensus_timeout_total_total torus_consensus_view torus_block_transactions_count_sum torus_block_transactions_count_count torus_native_gossip_published_actions_total torus_native_gossip_dropped_full_total torus_rocksdb_memtable_bytes torus_rocksdb_l0_files torus_rocksdb_pending_compaction_bytes torus_db_size_bytes torus_exec_body_persist_write_seconds_sum torus_exec_body_persist_write_seconds_count torus_commit_persist_seconds_sum torus_commit_persist_seconds_count torus_commit_body_encode_seconds_sum torus_commit_persist_write_seconds_sum torus_trade_writer_queued_batches torus_rocksdb_memtable_bytes_all torus_rocksdb_immutable_memtables_all torus_rocksdb_l0_files_max torus_rocksdb_pending_compaction_bytes_all torus_rocksdb_delayed_write_rate torus_rocksdb_write_stopped torus_rocksdb_running_compactions torus_rocksdb_running_flushes torus_rocksdb_stall_micros torus_rocksdb_write_self torus_rocksdb_write_other torus_rocksdb_bytes_written torus_rocksdb_wal_bytes torus_rocksdb_flush_write_bytes torus_rocksdb_compact_read_bytes torus_rocksdb_compact_write_bytes torus_rocksdb_compaction_cpu_micros torus_rocksdb_db_write_count torus_rocksdb_db_write_sum_micros torus_rocksdb_db_write_p99_micros torus_rocksdb_db_write_max_micros torus_rocksdb_write_stall_count torus_rocksdb_write_stall_sum_micros torus_rocksdb_write_stall_p99_micros torus_rocksdb_write_stall_max_micros torus_rocksdb_flush_count torus_rocksdb_flush_sum_micros torus_rocksdb_compaction_count torus_rocksdb_compaction_sum_micros torus_view_duration_seconds_sum torus_view_duration_seconds_count torus_view_propose_delay_seconds_sum torus_view_propose_delay_seconds_count torus_view_propose_build_seconds_sum torus_view_propose_build_seconds_count torus_view_propose_finalize_seconds_sum torus_view_propose_finalize_seconds_count torus_view_qc_collect_seconds_sum torus_view_qc_collect_seconds_count torus_view_proposal_arrival_seconds_sum torus_view_proposal_arrival_seconds_count torus_view_insert_persist_seconds_sum torus_view_insert_persist_seconds_count torus_view_vote_delay_seconds_sum torus_view_vote_delay_seconds_count torus_block_build_seconds_sum torus_block_build_seconds_count torus_validate_block_seconds_sum torus_validate_block_seconds_count torus_validate_block_decode_seconds_sum torus_validate_block_decode_seconds_count torus_validate_block_da_reconstruct_seconds_sum torus_validate_block_da_reconstruct_seconds_count torus_validate_block_attest_seconds_sum torus_validate_block_attest_seconds_count torus_validate_block_custody_seconds_sum torus_validate_block_custody_seconds_count torus_on_committed_block_seconds_sum torus_on_committed_block_seconds_count torus_mempool_remove_committed_seconds_sum torus_mempool_remove_committed_seconds_count torus_block_build_parent_seconds_sum torus_block_build_parent_seconds_count torus_block_build_select_seconds_sum torus_block_build_select_seconds_count torus_block_build_mirror_seconds_sum torus_block_build_mirror_seconds_count torus_block_build_attest_seconds_sum torus_block_build_attest_seconds_count torus_block_build_encode_seconds_sum torus_block_build_encode_seconds_count torus_view_entry_slack_seconds_sum torus_view_entry_slack_seconds_count torus_view_timeout_after_seconds_sum torus_view_timeout_after_seconds_count torus_view_vote_gather_seconds_sum torus_view_vote_gather_seconds_count torus_view_qc_to_advance_seconds_sum torus_view_qc_to_advance_seconds_count torus_view_entered_past_deadline_total torus_view_timeout_no_proposal_total torus_view_timeout_no_vote_total torus_view_timeout_after_vote_total torus_view_timeout_leader_total torus_view_proposals_uncertified_total torus_exec_end_resident_seconds_sum torus_exec_end_resident_seconds_count torus_exec_end_resident_rows_seconds_sum torus_exec_end_resident_positions_seconds_sum torus_exec_end_resident_users_seconds_sum torus_exec_end_resident_wait_seconds_sum torus_exec_end_resident_wait_seconds_count torus_exec_margin_configs_seconds_sum torus_exec_margin_configs_seconds_count torus_exec_action_status_seconds_sum torus_exec_action_status_seconds_count torus_sell_cuts_pool_zero_t0_total torus_sell_cuts_pool_zero_t1_2_total torus_sell_cuts_pool_zero_t3_5_total torus_sell_cuts_pool_zero_t6_10_total torus_sell_cuts_pool_zero_t11_30_total torus_sell_cuts_pool_zero_t31p_total torus_sell_cuts_pool_partial_t0_total torus_sell_cuts_pool_partial_t1_2_total torus_sell_cuts_pool_partial_t3_5_total torus_sell_cuts_pool_partial_t6_10_total torus_sell_cuts_pool_partial_t11_30_total torus_sell_cuts_pool_partial_t31p_total torus_sell_cuts_nonpool_zero_t0_total torus_sell_cuts_nonpool_zero_t1_2_total torus_sell_cuts_nonpool_zero_t3_5_total torus_sell_cuts_nonpool_zero_t6_10_total torus_sell_cuts_nonpool_zero_t11_30_total torus_sell_cuts_nonpool_zero_t31p_total torus_sell_cuts_nonpool_partial_t0_total torus_sell_cuts_nonpool_partial_t1_2_total torus_sell_cuts_nonpool_partial_t3_5_total torus_sell_cuts_nonpool_partial_t6_10_total torus_sell_cuts_nonpool_partial_t11_30_total torus_sell_cuts_nonpool_partial_t31p_total torus_maker_margin_cancels_total torus_reduce_only_cuts_total torus_sell_top_ups_full_total torus_sell_top_ups_partial_total torus_sell_top_ups_none_total torus_exec_cancel_all_total torus_exec_cancel_all_books_visited_total torus_exec_cancel_all_books_hit_total torus_exec_by_id_actions_total torus_exec_by_id_books_probed_total torus_exec_thread_spawns_match torus_exec_thread_spawns_settle torus_exec_thread_spawns_save_books torus_exec_thread_spawns_margin_prepare torus_exec_thread_spawns_open_orders torus_exec_thread_spawns_end_resident torus_exec_thread_spawns_flush_digest torus_exec_thread_spawns_root_buckets torus_exec_thread_spawns_load_books torus_exec_oracle_only_block_seconds_sum torus_exec_oracle_only_block_seconds_count torus_exec_cancel_all_index_entries torus_exec_cancel_all_index_traders torus_exec_checkpoint_settle_wait_seconds_sum torus_exec_checkpoint_settle_wait_seconds_count torus_exec_checkpoint_upkeep_seconds_sum torus_exec_checkpoint_upkeep_seconds_count torus_exec_checkpoint_wait_seconds_sum torus_exec_checkpoint_wait_seconds_count torus_exec_checkpoint_deferred_join_seconds_sum torus_exec_checkpoint_deferred_join_seconds_count"
# section 71.8 option A (ozarchy-hsdb, upstreamed s64): the consensus DB gauges (torus_rocksdb_consensus_*, the 29
# RocksdbStatMetrics::NAMES) and its per-CF families (db="consensus", 60 s cadence) as CSV-safe
# <family>__<cf>__consensus alias columns (sample_metrics.py); a build without the consensus DB writes nan in them.
WIDE_COLS="$WIDE_COLS torus_rocksdb_consensus_memtable_bytes_all torus_rocksdb_consensus_immutable_memtables_all torus_rocksdb_consensus_l0_files_max torus_rocksdb_consensus_pending_compaction_bytes_all torus_rocksdb_consensus_delayed_write_rate torus_rocksdb_consensus_write_stopped torus_rocksdb_consensus_running_compactions torus_rocksdb_consensus_running_flushes torus_rocksdb_consensus_stall_micros torus_rocksdb_consensus_write_self torus_rocksdb_consensus_write_other torus_rocksdb_consensus_bytes_written torus_rocksdb_consensus_wal_bytes torus_rocksdb_consensus_flush_write_bytes torus_rocksdb_consensus_compact_read_bytes torus_rocksdb_consensus_compact_write_bytes torus_rocksdb_consensus_compaction_cpu_micros torus_rocksdb_consensus_db_write_count torus_rocksdb_consensus_db_write_sum_micros torus_rocksdb_consensus_db_write_p99_micros torus_rocksdb_consensus_db_write_max_micros torus_rocksdb_consensus_write_stall_count torus_rocksdb_consensus_write_stall_sum_micros torus_rocksdb_consensus_write_stall_p99_micros torus_rocksdb_consensus_write_stall_max_micros torus_rocksdb_consensus_flush_count torus_rocksdb_consensus_flush_sum_micros torus_rocksdb_consensus_compaction_count torus_rocksdb_consensus_compaction_sum_micros torus_rocksdb_l0_files__cf_consensus_meta__consensus torus_rocksdb_l0_files__cf_native_pending__consensus torus_rocksdb_l0_files__cf_native_shards__consensus torus_rocksdb_memtable_bytes__cf_consensus_meta__consensus torus_rocksdb_memtable_bytes__cf_native_pending__consensus torus_rocksdb_memtable_bytes__cf_native_shards__consensus torus_rocksdb_pending_compaction_bytes__cf_consensus_meta__consensus torus_rocksdb_pending_compaction_bytes__cf_native_pending__consensus torus_rocksdb_pending_compaction_bytes__cf_native_shards__consensus"

# bl1 exec-chain-sub-100-attribution: histogram BUCKET series in LONG format
# (ts,node,metric,le,count), read out of the SAME scrape as the wide row above
# (no extra HTTP). summarize.py turns these into commit-cadence p50/p95 and
# chain / hand-off percentiles; a cell run by an older harness simply has no
# buckets.csv and gets None for every percentile.
# s77: end-to-end order latency, age (now - nonce) at each stage.
BUCKET_METRICS="torus_commit_interval_seconds_bucket torus_exec_chain_seconds_bucket torus_exec_handoff_wait_seconds_bucket torus_flush_worker_seconds_bucket torus_order_age_admit_seconds_bucket torus_order_age_commit_seconds_bucket torus_order_age_exec_seconds_bucket torus_order_age_durable_seconds_bucket torus_order_age_fills_visible_seconds_bucket torus_exec_end_resident_wait_seconds_bucket torus_exec_oracle_only_block_seconds_bucket torus_exec_checkpoint_settle_wait_seconds_bucket torus_exec_checkpoint_upkeep_seconds_bucket torus_exec_checkpoint_deferred_join_seconds_bucket"
# Liquidation stress: per-sample liquidation counter and vault deficit (default
# cells keep today's columns).
if liq_stress_on; then
    WIDE_COLS="$WIDE_COLS torus_liquidations_triggered_total torus_liquidator_vault_deficit"
    # feat/liq-telemetry: step timer, per-class / scanned / acted counters,
    # pending and deferred gauges (zeros on an older binary; liq_stress.py
    # checks metrics-after-*.txt for the timer).
    WIDE_COLS="$WIDE_COLS torus_liquidation_step_seconds_sum torus_liquidation_step_seconds_count torus_liquidations_stage1_total torus_liquidations_backstop_total torus_liquidations_adl_total torus_liquidation_scanned_total torus_liquidation_acted_total torus_liquidation_pending torus_liquidation_deferred"
    # liq_stress.py: the post-engine tail per-block divisor, and the ADL-budget
    # gauges (docs/plans/adl-budget.md; zeros until a node exports them).
    WIDE_COLS="$WIDE_COLS torus_exec_post_engine_tail_seconds_count torus_liquidation_adl_queue torus_liquidation_adl_queue_deficit"
fi
WIDE_COLS="$WIDE_COLS scrape_valid"
echo "ts,node,$(echo $WIDE_COLS | tr ' ' ',')" > "$OUT/sampler.csv"
echo "ts,node,metric,le,count" > "$OUT/buckets.csv"
for i in 0 1 2; do
    echo "ts,actions_processed,placed_accepted,matched,resting,rej_margin,rej_book,rej_cancelled,rej_other,self_trade_cancels,cancelled_partial_fill,blocks_committed,block_height,exec_queue_depth" > "$OUT/funnel-val$i.csv"
    echo "ts,committed,height,placed,matched,resting,exec_resting,lb_s,lb_c,root_s,root_c,sw_s,sw_c,evm_s,evm_c,fl_s,fl_c,db_s,db_c,execq,bscan,mc_hit,mc_miss,mc_evict,mc_resident" > "$OUT/phase-val$i.csv"
done
cpusampler() {
    echo "ts,load1,load5,ncpu,pid,comm,pcpu,rss_kb" > "$OUT/cpu.csv"
    while true; do
        ts=$(date +%s); read -r l1 l5 _ < /proc/loadavg
        ps -o pid=,comm=,pcpu=,rss= -p "$(tr '\n' ',' < "$RUN_DIR/pids" | sed 's/,$//')${BENCH_PID:+,$BENCH_PID}${EVM_PID:+,$EVM_PID}" 2>/dev/null \
            | awk -v ts="$ts" -v l1="$l1" -v l5="$l5" -v n="$(nproc)" '{printf "%s,%s,%s,%s,%s,%s,%s,%s\n",ts,l1,l5,n,$1,$2,$3,$4}' >> "$OUT/cpu.csv"
        sleep 10
    done
}
for i in 0 1 2; do scrape_one "${METS[$i]}" > "$OUT/metrics-before-val$i.txt"; done
: > "$OUT/schedstat.raw"; : > "$OUT/procstat.raw"; schedstat_snapshot before "${PIDS[@]}"
# This collector belongs to the runner; scoring scripts still come from
# TOOLS_DIR. Keeping it beside run-cell.sh also supports older node worktrees.
python3 "$SELF_DIR/sample_metrics.py" --out "$OUT" \
    --wide "$WIDE_COLS" --funnel "$FUNNEL_COLS" --phase "$PHASE_COLS" --buckets "$BUCKET_METRICS" \
    "http://127.0.0.1:${METS[0]}/metrics" "http://127.0.0.1:${METS[1]}/metrics" "http://127.0.0.1:${METS[2]}/metrics" \
    >"$OUT/sampler.log" 2>&1 & SAMPLER_PID=$!
sleep 3
kill -0 "$SAMPLER_PID" 2>/dev/null || die "metrics sampler exited (see sampler.log)"

# ---------------------------------------------------------------- 6. bench
BENCH_CMD=("$BENCH" consensus --rpc-urls "$BENCH_RPC_URLS" --econ --senders "$SENDERS" --sender-offset 60 \
    --markets "$MARKETS" --batch-size "$BATCH" --submit-batch "$SUBMIT" --format bin --concurrency "$CONC" \
    --duration "$DUR" --target-margin 1500 --cross-fraction "$CROSS_FRACTION" --cancel-fraction "$CANCEL_FRACTION" --band "$BAND" --rate-total "$RATE")
# LOCALITY shape (unset = flag omitted = uniform draw over 1..=MARKETS, i.e. the
# shape every campaign cell so far used). Needs a bench-throughput built at or
# after cand/r6-harness-300m-digest-and-parity.
[ -n "$MPS" ] && BENCH_CMD+=(--markets-per-sender "$MPS")
[ -n "$OPEN_ORDER_BUDGET" ] && BENCH_CMD+=(--open-order-budget "$OPEN_ORDER_BUDGET")
[ "$RETRY_BUSY" = 1 ] && BENCH_CMD+=(--retry-busy)
[ -n "$MAX_IN_FLIGHT" ] && BENCH_CMD+=(--max-in-flight "$MAX_IN_FLIGHT" --in-flight-watch-rpc "${RPCS[2]}")
[ -n "$IN_FLIGHT_TAIL" ] && BENCH_CMD+=(--in-flight-tail "$IN_FLIGHT_TAIL")
[ -n "$SPAM_CANCEL_KEYS" ] && BENCH_CMD+=(--spam-cancel-keys "$SPAM_CANCEL_KEYS")
[ -n "$SPAM_CANCEL_RATE" ] && BENCH_CMD+=(--spam-cancel-rate "$SPAM_CANCEL_RATE")
[ "$SPAM_CANCEL_FUNDED" = 1 ] && BENCH_CMD+=(--spam-cancel-funded)
[ -n "$CANCEL_BY_ID_FRACTION" ] && BENCH_CMD+=(--cancel-by-id-fraction "$CANCEL_BY_ID_FRACTION")
[ -n "$MODIFY_FRACTION" ] && BENCH_CMD+=(--modify-fraction "$MODIFY_FRACTION")
[ -n "$CLOID_SHARE" ] && BENCH_CMD+=(--cloid-share "$CLOID_SHARE")
[ "$BULK_COUNT" = 100000 ] || BENCH_CMD+=(--funded-bulk-end "$((60 + BULK_COUNT))")
log "bench: ${BENCH_CMD[*]}"
T_BENCH0=$(date +%s)
"${BENCH_CMD[@]}" > "$OUT/bench.log" 2>&1 & BENCH_PID=$!
if evm_load_on; then evm_load_start; fi
cpusampler & CPU_PID=$!
# bl3 crash gate: SIGKILL + restart one validator CRASH_KILL_AT_S into the load
# window. Off unless CRASH_KILL_AT_S is set; the target guard lives in
# crash-kill.sh and refuses anything but this devnet's val1/val2 — and runs on
# EVERY kill (one crash-kill.sh invocation per offset, on the current pid).
# s75 multi-crash: kills run in order, each offset measured from this sub-shell's start. A
# failed kill ends the sequence (the record check below then fails the cell); a
# kill that would land < CRASH_KILL_MIN_UP_S after the previous restart ends it
# too, as a logged SKIP (crash-kill.skipped = first skipped k).
crash_sequence() {
    local t0 k at rec now due prev_up="" since
    t0=$(date +%s.%N)
    for k in $(seq 1 "${#CRASH_AT[@]}"); do
        at=${CRASH_AT[$((k-1))]}
        rec=$(crash_seq_name crash-kill "$k" json)
        now=$(date +%s.%N)
        due=$(awk -v a="$t0" -v b="$at" -v n="$now" 'BEGIN{d=a+b; printf "%.3f", (d>n ? d : n)}')
        if [ -n "$prev_up" ]; then
            since=$(awk -v d="$due" -v u="$prev_up" 'BEGIN{printf "%.1f", d-u}')
            if awk -v s="$since" -v m="$CRASH_KILL_MIN_UP_S" 'BEGIN{exit !(s<m)}'; then
                echo "crash gate: WARNING SKIPPING kills $k..${#CRASH_AT[@]} of ${#CRASH_AT[@]}: kill $k (bench+${at}s) would land ${since}s after kill $((k-1))'s restart (< ${CRASH_KILL_MIN_UP_S}s) — the previous kill+restart overran"
                echo "$k" > "$OUT/crash-kill.skipped"
                return 0
            fi
        fi
        sleep "$(awk -v d="$due" -v n="$now" 'BEGIN{printf "%.3f", d-n}')"
        echo "crash gate: kill $k/${#CRASH_AT[@]} at bench+${at}s -> $rec"
        "$TOOLS_DIR/crash-kill.sh" "$WT" "$KILL_IDX" "$OUT" "$rec" && [ -s "$OUT/$rec" ] || {
            echo "crash gate: kill $k/${#CRASH_AT[@]} FAILED — no further kills"; return 1; }
        prev_up=$(jq -r '.restart_ts' "$OUT/$rec" 2>/dev/null) || prev_up=""
        [[ "$prev_up" =~ ^[0-9.]+$ ]] || prev_up=$(date +%s.%N)
    done
}
CRASH_RC=""
if [ -n "$CRASH_KILL_AT_S" ]; then
    log "crash gate ARMED: SIGKILL $KILL_NODE at bench+${CRASH_KILL_AT_S}s (${#CRASH_AT[@]} kill(s)), restart from the same data dir"
    crash_sequence >>"$OUT/run.log" 2>&1 &
    CRASH_PID=$!
fi
wait "$BENCH_PID"; BENCH_RC=$?
if [ -n "$CRASH_PID" ]; then
    # rc is informational: bash may already have reaped the sub-shell, and a
    # spurious 127 from `wait` must not throw away a good cell. The ARTIFACT is
    # the truth — crash-kill.sh writes it last, after the restart succeeded.
    wait "$CRASH_PID"; CRASH_RC=$?; CRASH_PID=""
    log "crash gate exited rc=$CRASH_RC"
fi
if [ -n "$CRASH_KILL_AT_S" ] && [ ! -s "$OUT/crash-kill.json" ]; then
    die "crash gate did not complete (no crash-kill.json) — the node was not killed+restarted, see run.log"
fi
# multi-crash: every planned kill needs its record, except a logged SKIP tail.
CRASH_DONE=0
if [ -n "$CRASH_KILL_AT_S" ]; then
    CRASH_SKIP_FROM=$(cat "$OUT/crash-kill.skipped" 2>/dev/null)
    for k in $(seq 1 "${#CRASH_AT[@]}"); do
        rec=$(crash_seq_name crash-kill "$k" json)
        if [ -s "$OUT/$rec" ]; then CRASH_DONE=$k; continue; fi
        [ -n "$CRASH_SKIP_FROM" ] && [ "$k" -ge "$CRASH_SKIP_FROM" ] && break
        die "crash gate kill $k/${#CRASH_AT[@]} did not complete (no $rec) — see run.log"
    done
    [ "$CRASH_DONE" = "${#CRASH_AT[@]}" ] || log "WARNING: crash gate ran only $CRASH_DONE of ${#CRASH_AT[@]} planned kills (kills $CRASH_SKIP_FROM.. SKIPPED, see run.log)"
fi
T_BENCH1=$(date +%s)
BENCH_PID=""
# a SIGKILLed+restarted node has a new pid: re-read so the snapshot follows the live process
mapfile -t PIDS_NOW < "$RUN_DIR/pids"; schedstat_snapshot bench_end "${PIDS_NOW[@]}"
log "bench exited rc=$BENCH_RC after $((T_BENCH1 - T_BENCH0))s"
# EVM load: the load window is over, stop the EVM senders too (before the drain).
if evm_load_on; then evm_load_stop; fi
# Oracle feed: record the marks the load ended with, then PAUSE the feed. Its
# SubmitOraclePrices are native actions: running, they bump
# torus_native_actions_processed_total and the native mempool gauge every
# interval, so health.py's 10 s quiet window and the digest's funnel snapshot
# could never hold. Paused, the marks stay usable for 60 s past the last fresh
# aggregate; it is resumed only to receive SIGTERM after the digest.
ORACLE_ALIVE_END=""
if [ "$ORACLE_FEED" = 1 ]; then
    if alive "$ORACLE_PID"; then ORACLE_ALIVE_END=1; else ORACLE_ALIVE_END=0; log "WARNING: oracle feed died during the load — see oracle-feed.log"; fi
    ORACLE_MEND=$(oracle_marks "$ORACLE_H0")
    printf '%s\n' "$ORACLE_MEND" > "$OUT/oracle-marks-bench-end.json"
    log "oracle marks at bench end: $ORACLE_MEND"
    if [ "$ORACLE_FEED_DRAIN" = 1 ]; then
        log "oracle feed kept RUNNING through the drain (ORACLE_FEED_DRAIN=1); paused before the digest"
    else
        kill -STOP "$ORACLE_PID" 2>/dev/null
        log "oracle feed paused (SIGSTOP) for drain + digest"
    fi
fi

# ---------------------------------------------------------------- 7. drain
# Counter quiescence alone also describes a wedged chain. Require complete
# scrapes, quiet native counters, no pending native/flush work, and commit
# progress on every validator during the quiet interval. Empty blocks may leave
# the execution queue at 1-2. health.py retains all drain observations.
QUIET_S=${QUIET_S:-10}
log "draining (quiet counters, no pending work, per-node commit progress for ${QUIET_S}s; timeout ${DRAIN_TIMEOUT}s)"
DRAINED=0
DRAIN_URLS=("http://127.0.0.1:${METS[0]}/metrics" "http://127.0.0.1:${METS[1]}/metrics" "http://127.0.0.1:${METS[2]}/metrics")
FEED_DRAIN_ARGS=()
if [ "$ORACLE_FEED_DRAIN" = 1 ]; then
    FEED_DRAIN_ARGS=(--feed-live --feed-mempool-max $(( 6 * ((MARKETS + 255) / 256) )) --max-lag "$ORACLE_FEED_DRAIN_MAX_LAG")
    log "feed-live drain: ${FEED_DRAIN_ARGS[*]} (order counters quiet, exec lag <= $ORACLE_FEED_DRAIN_MAX_LAG; the feed's actions may move)"
fi
if python3 "$TOOLS_DIR/health.py" drain --out "$OUT" --timeout "$DRAIN_TIMEOUT" --quiet "$QUIET_S" \
    --urls "${DRAIN_URLS[@]}" "${FEED_DRAIN_ARGS[@]}" \
    >>"$OUT/run.log" 2>&1; then DRAINED=1; fi
T_DRAIN=$(date +%s)
log "drained=$DRAINED after $((T_DRAIN - T_BENCH1))s (evidence: drain.json and drain-samples.jsonl)"
if [ "$ORACLE_FEED_DRAIN" = 1 ]; then
    log "feed-live drain: $(jq -r '.feed_live | "max_exec_lag=\(.max_exec_lag) quiet_window_native_block_exec_ms (oracle-only proxy) p50=\(.chain_ms.p50) p95=\(.chain_ms.p95) max=\(.chain_ms.max) native_blocks=\(.native_blocks) single_block_intervals=\(.single_block_intervals)/\(.native_intervals)"' "$OUT/drain.json" 2>/dev/null || echo MISSING) (drain-feed-live.tsv)"
    # The digest and the after-snapshots need quiet counters: pause the feed
    # now and prove quiescence with the default criterion before them. s64:
    # and wait until every node has executed the highest committed height at
    # the pause (--to-height auto), up to FEED_STOP_SETTLE_MAX_S: a slow chain
    # is still catching up long after a fixed 60 s (perf doc section 72.4).
    kill -STOP "$ORACLE_PID" 2>/dev/null
    mkdir -p "$OUT/feed-stop-settle"
    SETTLED=0
    python3 "$TOOLS_DIR/health.py" drain --out "$OUT/feed-stop-settle" --timeout "$FEED_STOP_SETTLE_MAX_S" --quiet "$QUIET_S" \
        --to-height auto --urls "${DRAIN_URLS[@]}" >>"$OUT/run.log" 2>&1
    SETTLE_RC=$?
    [ "$SETTLE_RC" = 0 ] && SETTLED=1
    SETTLE_JSON="$OUT/feed-stop-settle/drain.json"
    SETTLE_S=$(jq -r '.elapsed_s // "?" | if type == "number" then floor else . end' "$SETTLE_JSON" 2>/dev/null || echo '?')
    SETTLE_TARGET=$(jq -r '.target_height // "?"' "$SETTLE_JSON" 2>/dev/null || echo '?')
    case "$SETTLE_RC" in
        0) SETTLE_WHAT="every node executed target height $SETTLE_TARGET" ;;
        3) SETTLE_WHAT="WARNING: target height $SETTLE_TARGET NOT reached within FEED_STOP_SETTLE_MAX_S=$FEED_STOP_SETTLE_MAX_S s (exec heights $(jq -r '(.exec_heights // {}) | to_entries | map("\(.key)=\(.value // "?")") | join(" ")' "$SETTLE_JSON" 2>/dev/null || echo '?')); the digest quiescence check decides" ;;
        *) SETTLE_WHAT="WARNING: NOT quiet within FEED_STOP_SETTLE_MAX_S=$FEED_STOP_SETTLE_MAX_S s ($(jq -r '.reason // "?"' "$SETTLE_JSON" 2>/dev/null || echo '?')); the digest quiescence check decides" ;;
    esac
    log "oracle feed paused (SIGSTOP) for the digest; settle drained=$SETTLED rc=$SETTLE_RC after ${SETTLE_S}s: $SETTLE_WHAT (feed-stop-settle/drain.json)"
fi
sleep 2
stop_sampler || die "metrics sampler failed (see sampler.log)"
kill "$CPU_PID" 2>/dev/null; CPU_PID=""

# ---------------------------------------------------------------- 8. after-snapshots + agreement
for i in 0 1 2; do scrape_one "${METS[$i]}" > "$OUT/metrics-after-val$i.txt"; done
mapfile -t PIDS_NOW < "$RUN_DIR/pids"; schedstat_snapshot after "${PIDS_NOW[@]}"
schedstat_json "$OUT/schedstat.raw" "$OUT/schedstat.json" || log "WARNING: schedstat.json not written"
rpc() { # $1=url $2=method $3=params-json
    curl -s -m "$RPC_TIMEOUT" -H 'content-type: application/json' "$1" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":$3}"
}
# Agreement compare height: at or below EVERY node's served head. eth_getBlockByNumber answers null
# above eth_blockNumber = min(applied marker, committed), and item 6 Phase 3+ writes the applied
# marker only at checkpoints (15 s, ~160 blocks at 10 markets), so HMIN-5 from torus_block_height
# (committed) can sit above it: the hash read ERR and seven ozarchy-p4gate 10-market cells were
# DISAGREE with equal state digests. Older binaries (marker every block) serve >= HMIN-5: unchanged.
# A node whose head does not read stays out of the min (WARNING at the call): its block read then
# decides, so a real RPC failure still reads ERR and DISAGREE.
agree_cmp_height() { # BASE URL... -> AGREE_HCMP = min(BASE, served heads), AGREE_SERVED[i] = head or ERR
    local base=$1 u h; shift
    AGREE_HCMP=$base; AGREE_SERVED=()
    for u in "$@"; do
        h=$(rpc "$u" eth_blockNumber '[]' | jq -r '.result // empty' 2>/dev/null)
        if [[ "$h" =~ ^0x[0-9a-fA-F]{1,15}$ ]]; then
            h=$((h)); AGREE_SERVED+=("$h")
            [ "$h" -lt "$AGREE_HCMP" ] && AGREE_HCMP=$h
        else
            AGREE_SERVED+=(ERR)
        fi
    done
    return 0
}
declare -a HGT
for i in 0 1 2; do HGT[$i]=$(mval torus_block_height < "$OUT/metrics-after-val$i.txt"); HGT[$i]=${HGT[$i]%.*}; done
HMIN=$(printf '%s\n' "${HGT[@]}" | sort -n | head -1); HMAX=$(printf '%s\n' "${HGT[@]}" | sort -n | tail -1)
HCMP=$(( HMIN - 5 )); HHEX=$(printf '0x%x' "$HCMP")
log "heights after drain: ${HGT[*]} (spread $((HMAX-HMIN))); comparing block $HCMP on all nodes"

# --- state digest -----------------------------------------------------------
# The digest is the ONLY determinism check that sees executed native state (the
# header stateRoot is 0x0 on this branch). It is therefore only worth anything
# when all three nodes are digested over the SAME state:
#   * the per-market RPCs run DIGEST_PAR-way parallel inside each node
#     (digest-node.sh, order-stable), and
#   * the three nodes are digested CONCURRENTLY, in one window, and
#   * the funnel counters are snapshotted either side of that window: if they
#     moved, the digest is flagged NOT quiescent and summarize.py reports
#     DIGEST_UNVERIFIED instead of pretending the fleet forked.
# At 300 markets the old serial loop was ~4 min/node (~12 min end to end) —
# val0 and val2 were digested minutes apart (r6-base-300m-r1).
"$BENCH" gen-accounts --offset 60 --count 50 2>/dev/null | awk '{print $2}' > "$OUT/digest-accounts.txt"
# Liquidation stress: the protocol accounts (liquidator vault + both ADL
# escrows) join the digest twice: their balances (digest-accounts.txt) and
# their position in every market (digest-position-accounts.txt -> digest-node.sh
# 7th argument, torus_getPosition). Default cells keep exactly the 50
# load-sender accounts and no positions file: their digest stream (and so the
# digest value) is byte-identical to tools without escrow support.
DIG_POS=()
if liq_stress_on; then
    printf '%s\n' "$LIQUIDATOR_VAULT" "$ADL_ESCROW_LONG" "$ADL_ESCROW_SHORT" >> "$OUT/digest-accounts.txt"
    printf '%s\n' "$LIQUIDATOR_VAULT" "$ADL_ESCROW_LONG" "$ADL_ESCROW_SHORT" > "$OUT/digest-position-accounts.txt"
    DIG_POS=("$OUT/digest-position-accounts.txt")
    # Coverage is claimed only when pre-flight confirmed the digest script has it.
    if [ "${DIGEST_HAS_POS:-0}" = 1 ]; then
        log "liquidation stress: liquidator vault $LIQUIDATOR_VAULT and ADL escrows $ADL_ESCROW_LONG $ADL_ESCROW_SHORT added to the state digest (balances + positions in all $MARKETS markets)"
    else
        log "WARNING: liquidation stress: liquidator vault $LIQUIDATOR_VAULT and ADL escrows $ADL_ESCROW_LONG $ADL_ESCROW_SHORT added to the state digest balances; their positions are NOT confirmed covered ($TOOLS_DIR/digest-node.sh does not list position-accounts)"
    fi
fi
DIG_ACCTS=$(grep -c . "$OUT/digest-accounts.txt")
if [ ${#DIG_POS[@]} -gt 0 ]; then DIG_POS_ACCTS=$(grep -c . "${DIG_POS[0]}"); else DIG_POS_ACCTS=0; fi
funnel_counters() {
    # Missing/failed metrics are unavailable evidence, never fabricated zeros.
    python3 -c '
import math, re, sys
wanted = ("torus_orders_placed_accepted_total", "torus_orders_matched_total",
          "torus_native_actions_processed_total", "torus_orders_resting_total")
values = {}
for line in sys.stdin:
    fields = line.split()
    if fields and fields[0] in wanted:
        if fields[0] in values or len(fields) < 2:
            sys.exit(1)
        values[fields[0]] = fields[1]
if not all(name in values and
           re.fullmatch(r"[0-9]+([.][0-9]+)?([eE][+-]?[0-9]+)?", values[name]) and
           math.isfinite(float(values[name])) for name in wanted):
    sys.exit(1)
print(",".join(values[name] for name in wanted))'
}
funnel_snapshot() {
    local i metrics row snapshot=""
    for i in 0 1 2; do
        metrics=$(scrape_one "${METS[$i]}") || return 1
        row=$(printf '%s\n' "$metrics" | funnel_counters) || return 1
        [ -n "$row" ] || return 1
        snapshot+="${snapshot:+$'\n'}$row"
    done
    # Emit only a complete three-node snapshot, never a successful partial one.
    printf '%s\n' "$snapshot"
}
declare -a DHGT DIGSHA DIGSECS
Q_BEFORE_OK=0
if Q_BEFORE=$(funnel_snapshot); then Q_BEFORE_OK=1; fi
for i in 0 1 2; do
    h=$(scrape_one "${METS[$i]}" | mval torus_block_height); DHGT[$i]=${h%.*}
done
log "state digest: $MARKETS markets + $DIG_ACCTS accounts + $DIG_POS_ACCTS position accounts, 3 nodes CONCURRENTLY (par=$DIGEST_PAR/node, rpc timeout ${RPC_TIMEOUT}s), heights ${DHGT[*]}"
TDIG0=$(date +%s)
DIG_PIDS=()
for i in 0 1 2; do
    "$TOOLS_DIR/digest-node.sh" "${RPCS[$i]}" "$MARKETS" "$OUT/digest-accounts.txt" \
        "$OUT/state-digest-val$i.txt" "$DIGEST_PAR" "$RPC_TIMEOUT" ${DIG_POS[@]+"${DIG_POS[@]}"} \
        > "$OUT/digest-val$i.out" 2>>"$OUT/run.log" &
    DIG_PIDS+=($!)
done
# Only the digests: a bare `wait` would also wait on the (paused) oracle feed forever.
wait "${DIG_PIDS[@]}"
TDIG1=$(date +%s)
Q_AFTER_OK=0
if Q_AFTER=$(funnel_snapshot); then Q_AFTER_OK=1; fi
# Liquidation stress: each node's vault (cash, deficit, open positions) while
# the chain is still quiet; "RPC_ERR" if a node did not answer.
if liq_stress_on; then
    for i in 0 1 2; do
        rpc "${RPCS[$i]}" torus_getLiquidatorVault '[]' | jq -cS '.result // .error' > "$OUT/vault-val$i.json" 2>/dev/null
        [ -s "$OUT/vault-val$i.json" ] || echo '"RPC_ERR"' > "$OUT/vault-val$i.json"
    done
    log "liquidator vault: $(for i in 0 1 2; do printf 'val%s=%s ' "$i" "$(cat "$OUT/vault-val$i.json")"; done)"
fi
if [ "$Q_BEFORE_OK" = 1 ] && [ "$Q_AFTER_OK" = 1 ] && [ -n "$Q_BEFORE" ] && [ "$Q_BEFORE" = "$Q_AFTER" ]; then DIGEST_QUIESCENT=1; else DIGEST_QUIESCENT=0; fi
for i in 0 1 2; do
    sha=""; secs=""; read -r sha secs < "$OUT/digest-val$i.out" || true
    DIGSHA[$i]=${sha:-ERR}; DIGSECS[$i]=${secs:-0}
done
log "state digest done in $((TDIG1 - TDIG0))s wall (per node: ${DIGSECS[*]} s), quiescent=$DIGEST_QUIESCENT"
[ "$DIGEST_QUIESCENT" = 1 ] || log "WARNING: funnel counters moved or snapshot evidence was unavailable during the digest — digest is NOT a determinism proof for this cell"
if [ "$ORACLE_FEED" = 1 ]; then
    stop_oracle_feed
    log "oracle feed: oracle_feed=1 $(jq -c '{sent,accepted,rejected,last_error}' "$OUT/oracle-feed-stats.json" 2>/dev/null || echo 'stats=MISSING') stale_marks_at_bench_end=$(jq -r '.markets - .usable' <<<"$ORACLE_MEND" 2>/dev/null) fresh_after_s=$ORACLE_FRESH_S"
fi

agree_cmp_height "$HCMP" "${RPCS[@]}"
if [ "$AGREE_HCMP" != "$HCMP" ]; then
    log "agreement: block $HCMP is above a node's served (applied) head (served ${AGREE_SERVED[*]}); comparing block $AGREE_HCMP instead"
    HCMP=$AGREE_HCMP; HHEX=$(printf '0x%x' "$HCMP")
fi
case " ${AGREE_SERVED[*]} " in *" ERR "*) log "WARNING: agreement: eth_blockNumber unreadable on a node (served ${AGREE_SERVED[*]}); its block read at $HCMP decides" ;; esac
: > "$OUT/agreement.jsonl"
for i in 0 1 2; do
    blk=$(rpc "${RPCS[$i]}" eth_getBlockByNumber "[\"$HHEX\",false]")
    bh=$(printf '%s' "$blk" | jq -r '.result.hash // "ERR"'); sr=$(printf '%s' "$blk" | jq -r '.result.stateRoot // "ERR"')
    [ "$bh" = ERR ] && [ "${AGREE_SERVED[$i]}" != ERR ] && log "ERROR: agreement: val$i did not return block $HCMP although it serves up to ${AGREE_SERVED[$i]}: an RPC failure, not the applied-marker lag"
    dig=${DIGSHA[$i]}
    matched=$(mval torus_orders_matched_total < "$OUT/metrics-after-val$i.txt")
    placed=$(mval torus_orders_placed_accepted_total < "$OUT/metrics-after-val$i.txt")
    resting=$(mval torus_orders_resting_total < "$OUT/metrics-after-val$i.txt")
    actions=$(mval torus_native_actions_processed_total < "$OUT/metrics-after-val$i.txt")
    # r3 resident-books-stale-rebuild: full O(resting depth) reloads of the rank8
    # holder. Expect exactly 1 per process (the cold start); anything more is a
    # mid-run "resident books stale" stall — check val$i.log.excerpt for the reason.
    rebuilds=$(mval torus_exec_resident_rebuilds_total < "$OUT/metrics-after-val$i.txt")
    lg="$RUN_DIR/val$i.log"
    # s83/s131: count raw `panicked` lines plus fail-stop / FATAL / conflicting-blocks lines whose
    # tracing level is ERROR (the node emits every real fail-stop at ERROR); INFO/WARN lines only
    # mention it (e.g. "INFO ... state hash fail-stop ... on=false", "WARN ... fail-stop is off").
    # Level = first TRACE|DEBUG|INFO|WARN|ERROR word after ANSI codes are stripped (fmt + JSON
    # shapes). Same rule as crash-kill.sh crash_failstop_count.
    panics=$(sed -E 's/\x1b\[[0-9;]*m//g' "$lg" 2>/dev/null | awk '/panicked/ { n++; next } /FAIL-STOP|fail-stop|FATAL|conflicting blocks/ { if (match($0, /(^|[^A-Za-z])(TRACE|DEBUG|INFO|WARN|ERROR)([^A-Za-z]|$)/) && substr($0, RSTART, RLENGTH) ~ /ERROR/) n++ } END { print n + 0 }'); panics=${panics:-0}
    errors=$(grep -c ' ERROR ' "$lg" 2>/dev/null); errors=${errors:-0}
    printf '{"node":"val%s","height":%s,"cmp_height":%s,"block_hash":"%s","header_state_root":"%s","state_digest":"%s","digest_height":%s,"digest_seconds":%s,"matched":%s,"placed":%s,"resting":%s,"actions":%s,"resident_rebuilds":%s,"panic_or_failstop_lines":%s,"error_lines":%s}\n' \
        "$i" "${HGT[$i]}" "$HCMP" "$bh" "$sr" "$dig" "${DHGT[$i]}" "${DIGSECS[$i]}" "${matched%.*}" "${placed%.*}" "${resting%.*}" "${actions%.*}" "${rebuilds%.*}" "$panics" "$errors" >> "$OUT/agreement.jsonl"
done
cat "$OUT/agreement.jsonl" >> "$OUT/run.log"

# ingest accounting: actions the mempool evicted as nonce-expired (NONCE_WINDOW_MS=60s —
# a backlog older than 60 s is dropped silently; the bench's "submitted" therefore
# overstates what the chain executed).
# Report-only (summary.json .ingest, never a verdict): with DEFER_REPORT_SCANS=1 the
# campaign end pass scans each preserved log up to its length now (log-scans.sh).
# [report-scans:ingest]
BENCH_SUBMITTED=$(grep -oE 'Submitted \(load-gen accepted\): [0-9,]+' "$OUT/bench.log" | grep -oE '[0-9,]+$' | tr -d ,)
if [ "$DEFER_REPORT_SCANS" = 1 ]; then
    EVICTED=""
    INGEST_LOG_BYTES=$(file_bytes "$RUN_DIR/val0.log" "$RUN_DIR/val1.log" "$RUN_DIR/val2.log")
    log "ingest scan deferred to the campaign end pass (DEFER_REPORT_SCANS=1; node log bytes $INGEST_LOG_BYTES; bench submitted=${BENCH_SUBMITTED:-?} actions)"
else
    EVICTED=$(for i in 0 1 2; do ingest_evicted "$RUN_DIR/val$i.log"; done)
    log "$(ingest_line "$BENCH_SUBMITTED" "$EVICTED")"
fi
# [/report-scans:ingest]

# EVM load: what landed, read from val0 after the digest (read-only RPCs, so
# they cannot disturb it): executed EVM txs per height from the EVM load's start
# to the committed head, once val0's eth view (applied marker, written at
# checkpoints) reaches that head (at most 60 s).
if evm_load_on; then
    EVM_HEAD=$(scrape_one "${METS[0]}" | mval torus_block_height); EVM_HEAD=${EVM_HEAD%.*}
    EVM_SCAN=$(python3 "$SELF_DIR/evm_load.py" scan --rpc "${RPCS[0]}" --from "${EVM_H0:-0}" --to "${EVM_HEAD:-0}" \
        --interval "$EVM_N" --stats "$OUT/evm-load-stats.json" --wait-s 60 --out "$OUT/evm-load-scan.json" 2>&1) \
        || EVM_SCAN="WARNING: evm_load.py scan failed: $(tail -c 400 <<<"$EVM_SCAN")"
    log "EVM load: $EVM_SCAN"
    log "EVM load: tx-flood $(jq -c '{sent,accepted,rejected,throttled,missed,reject_reasons,stopped_by,elapsed_s}' "$OUT/evm-load-stats.json" 2>/dev/null || echo 'stats=MISSING')"
fi

# ---------------------------------------------------------------- 9. stop + collect logs
"$WSL/stop-3val.sh" >>"$OUT/run.log" 2>&1
# Body-dissemination / exec-pacing accounting per node (block-cap-raise sweep:
# a raised cap is REJECTED if bodies stop disseminating, whatever matched/s says).
#   manifest = pre-proposal HASH-ONLY manifest pushes (body set over the push floor)
#   body_push = pre-proposal FULL-BODY pushes (r4 log line; body set under the floor)
#   body_push_max_bytes = largest full-body push payload seen (vs the 8 MB floor)
#   exhausted = body fetch exhausted retries / no remaining targets -> sync fallback
#   sync_fallback = any "falling back to sync" (body or justify)
#   da_outbound_fail = native-da / block-data OUTBOUND FAILURE
#   starvation = header-first body starvation
#   pacing = exec-backlog pacing lines (selection caps scaled down / cancels-only)
DISSEM=$(python3 "$TOOLS_DIR/collect_logs.py" --logs "$RUN_DIR/val0.log" "$RUN_DIR/val1.log" "$RUN_DIR/val2.log" \
    --out "$OUT/log-summary.json") || { log "log collection failed"; exit 1; }
log "dissemination/pacing log counts: $DISSEM"
# The node logs are preserved here (val<i>.log.gz, always in-line: the next cell's
# launch overwrites RUN_DIR/val<i>.log); the excerpt is report-only (DEFER_REPORT_SCANS=1:
# the campaign end pass writes it from the .gz, deferred-scans.json marks the cell).
# [report-scans:save]
for i in 0 1 2; do
    [ "$DEFER_REPORT_SCANS" = 1 ] || log_excerpt "$RUN_DIR/val$i.log" > "$OUT/val$i.log.excerpt"
    gzip -c "$RUN_DIR/val$i.log" > "$OUT/val$i.log.gz"
done
[ "$DEFER_REPORT_SCANS" != 1 ] || write_deferred_marker "$OUT" "${BENCH_SUBMITTED:-}" "$INGEST_LOG_BYTES"
# [/report-scans:save]

# ---- crash gate: scan the killed node's log TAIL — everything it wrote AFTER
# the SIGKILL (crash-kill.sh restarted it with the log appended and recorded the
# byte offset) — and merge that into crash.json, which summarize.py turns into
# the PASS/FAIL verdict. Multi-crash: kill k's tail runs from ITS offset up to
# kill k+1's offset (the last one to EOF), into crash-<k>.json; a single kill
# is scanned exactly as before.
if [ -n "$CRASH_KILL_AT_S" ] && [ -s "$OUT/crash-kill.json" ]; then
    for k in $(seq 1 "$CRASH_DONE"); do
        REC="$OUT/$(crash_seq_name crash-kill "$k" json)"
        OFFB=$(jq -r '.pre_kill.log_bytes // 0' "$REC")
        TAILF="$OUT/$(crash_seq_name crash-restart-tail "$k" log)"
        if [ "$k" -lt "$CRASH_DONE" ]; then
            NEXTB=$(jq -r '.pre_kill.log_bytes // 0' "$OUT/$(crash_seq_name crash-kill $((k+1)) json)")
            tail -c "+$(( OFFB + 1 ))" "$RUN_DIR/val$KILL_IDX.log" | head -c "$(( NEXTB > OFFB ? NEXTB - OFFB : 0 ))"
        else
            tail -c "+$(( OFFB + 1 ))" "$RUN_DIR/val$KILL_IDX.log"
        fi | sed -E 's/\x1b\[[0-9;]*m//g' > "$TAILF"
        log "crash gate: scanning val$KILL_IDX log tail of kill $k from byte $OFFB ($(wc -l < "$TAILF") lines)"
        "$TOOLS_DIR/crash-kill.sh" scan "$TAILF" "$REC" "$OUT/$(crash_seq_name crash "$k" json)" "${CRASH_AT[$((k-1))]}" \
            | tee -a "$OUT/run.log" || log "WARNING: crash gate scan of kill $k failed"
    done
fi

# ---------------------------------------------------------------- 10. analysis
for i in 0 1 2; do
    { echo "== win60 val$i (all samples)"; awk -f "$TOOLS_DIR/win60.awk" "$OUT/funnel-val$i.csv";
      echo "== phase60 val$i"; awk -f "$TOOLS_DIR/phase60.awk" "$OUT/phase-val$i.csv"; } > "$OUT/analysis-val$i.txt" 2>&1
done

python3 "$TOOLS_DIR/summarize.py" \
    --out "$OUT" --label "$LABEL" --worktree "$WT" --commit "$WT_COMMIT" --dirty "$WT_DIRTY" \
    --markets "$MARKETS" --dur "$DUR" --rate "$RATE" --senders "$SENDERS" \
    --t-bench0 "$T_BENCH0" --t-bench1 "$T_BENCH1" --t-drain "$T_DRAIN" --drained "$DRAINED" \
    --bench-rc "$BENCH_RC" --idle-blks "$IDLE_BLKS" --md5-node "$MD5_SRC" --md5-bench "$MD5_BENCH" \
    --genesis-md5 "$GEN_MD5" --genesis-markets "$GEN_MARKETS" --genesis-accounts "$GEN_ACCTS" \
    --node-env "$NODE_ENV_JSON" --env-digests "$ENV_VERIFY" --extra-env "$EXTRA_ENV" \
    --bench-cmd "${BENCH_CMD[*]}" --pids "${PIDS[*]}" \
    --evicted "$EVICTED" --bench-submitted "${BENCH_SUBMITTED:-0}" \
    --block-cap "${BLOCK_CAP:-}" --dissem "$DISSEM" \
    --drain-timeout "$DRAIN_TIMEOUT" --markets-per-sender "${MPS:-}" \
    --max-in-flight "${MAX_IN_FLIGHT:-}" --open-order-budget "${OPEN_ORDER_BUDGET:-}" \
    --in-flight-tail "${IN_FLIGHT_TAIL:-}" \
    "${SUMMARIZE_LIQ_ARGS[@]}" \
    --digest-quiescent "$DIGEST_QUIESCENT" --digest-secs "${DIGSECS[*]}" --digest-heights "${DHGT[*]}" \
    | tee -a "$OUT/run.log"
rc=${PIPESTATUS[0]}
if [ "$rc" = 0 ]; then
    record_genesis_evm_n "$OUT/summary.json" "$GEN_EVM_N" || log "WARNING: summary.json .genesis.evm_block_interval not written"
    record_genesis_bulk_count "$OUT/summary.json" "$BULK_COUNT" || log "WARNING: summary.json .genesis.bulk_count not written"
fi
# summarize.py rewrote summary.json: put the pre-load record (and its verdict) back.
if preload_on && [ "$rc" = 0 ]; then
    python3 "$SELF_DIR/preload.py" summary --dir "$OUT" >/dev/null || log "WARNING: summary.json .preload not written"
fi
# summary.json .oracle_feed: feed stats + the marks the load ended with
# (stale_marks_at_bench_end = markets whose mark was NOT usable at bench end).
if [ "$ORACLE_FEED" = 1 ] && [ "$rc" = 0 ]; then
    python3 - "$OUT" "$ORACLE_PRICE" "$ORACLE_INTERVAL_MS" "$ORACLE_RC" "$ORACLE_FRESH_S" "$ORACLE_ALIVE_END" "$ORACLE_WALK_BP" \
        "$ORACLE_SHOCK_BP" "$ORACLE_SHOCK_ROUND" <<'PYS' \
        || log "WARNING: summary.json .oracle_feed not written"
import json, os, sys
out, price, interval, rc, fresh_s, alive_end, walk_bp, shock_bp, shock_round = sys.argv[1:10]
def load(name):
    try:
        with open(os.path.join(out, name)) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None
stats = load('oracle-feed-stats.json') or {}
marks = load('oracle-marks-bench-end.json') or {}
o = {'oracle_feed': 1, 'price': price, 'walk_bp': int(walk_bp), 'interval_ms': int(interval),
     'shock_bp': int(shock_bp), 'shock_round': int(shock_round),
     'rc': int(rc) if rc.lstrip('-').isdigit() else None,
     'stats_present': bool(stats),
     'sent': stats.get('sent'), 'accepted': stats.get('accepted'), 'rejected': stats.get('rejected'),
     'last_error': stats.get('last_error'), 'per_validator': stats.get('per_validator'),
     'fresh_after_s': int(fresh_s) if fresh_s.isdigit() else None,
     'alive_at_bench_end': alive_end == '1',
     'marks_at_bench_end': marks or None,
     'stale_marks_at_bench_end': (marks['markets'] - marks['usable']) if 'usable' in marks else None}
p = os.path.join(out, 'summary.json')
with open(p) as f:
    d = json.load(f)
d['oracle_feed'] = o
with open(p, 'w') as f:
    json.dump(d, f, indent=1)
PYS
fi
# summary.json .evm_load: tx-flood stats + the scan + per-node torus_exec_evm_seconds deltas.
if evm_load_on && [ "$rc" = 0 ]; then
    python3 "$SELF_DIR/evm_load.py" summary --dir "$OUT" --rate "$EVM_LOAD_RATE" --senders "$EVM_LOAD_SENDERS" --rc "$EVM_RC" \
        || log "WARNING: summary.json .evm_load not written"
fi
log "done -> $OUT/summary.json"
[ "$rc" = 0 ] || exit "$rc"
# EVM load: a cell that asked for EVM load and did not get it (tx-flood failed,
# stats or scan missing or incomplete, nothing accepted or landed, EVM txs off
# the cadence: .evm_load.problems) is rejected, whatever the native verdict:
# evm_load.py summary folded the problems into .validity (REJECT), .status and
# .headline.benchmark_accepted, so health.py below exits 2 as for any rejected
# cell. Without .evm_load at all nothing was folded: fail here.
if evm_load_on && ! jq -e '.evm_load.ok == true' "$OUT/summary.json" >/dev/null 2>&1; then
    log "EVM load: cell NOT accepted: $(jq -c '.evm_load.problems // ["no .evm_load in summary.json"]' "$OUT/summary.json" 2>/dev/null || echo '["summary.json unreadable"]')"
    jq -e '.evm_load' "$OUT/summary.json" >/dev/null 2>&1 || exit 1
fi
# A successful report write is not benchmark acceptance. Preserve artifacts and
# return nonzero for stalled, undrained, inconsistent or unverified cells.
python3 "$TOOLS_DIR/health.py" accept "$OUT/summary.json"
