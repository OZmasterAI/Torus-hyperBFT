# devnet fill ladder — branch A/B harness

Runs `bench-throughput --flow match` (and `rest`/`cross`/`churn`) against a
throwaway 4-validator devnet and reports **node-counter-sourced** placed/s and
matched/s, plus the per-phase exec timing that explains them.

Built during the p3 fill-ladder round (2026-07-19). Everything below is a trap
that already cost real measurement time — read before running.

## Quick start

```bash
# 1. one image per branch (VERIFIES the binary that lands inside the image)
git checkout perf/p3-throughput      && ./build-image.sh p3
git checkout perf/deep-book-storage  && ./build-image.sh deepbook

# 2. bench binary (identical across these branches -> one control load generator)
cargo build --release -p bench-throughput

# 3. legs (testnet-shaped caps: 6MB / 100 actions / 50k orders)
export TORUS_NATIVE_TOTAL_BLOCK_CAP=100 TORUS_NATIVE_ORDERS_PER_BLOCK_CAP=50000 \
       TORUS_NATIVE_BLOCK_BYTES_CAP=6000000
./run-leg.sh torus-devnet-node:ladder-p3       p3-match-t050 0.5 10 60 120
./run-leg.sh torus-devnet-node:ladder-deepbook db-match-t050 0.5 10 60 120

# 4. compare (refuses a verdict at n=1)
python3 aggregate.py
```

## Traps

**1. Stale binaries → a false null.** A shared cargo `target-dir` means cargo
never writes to `<repo>/target/`, but `devnet/Dockerfile` copies from there.
Naive image builds bake a stale binary — the *same* one for every branch — and
the A/B looks clean while measuring nothing. `build-image.sh` verifies the
in-image md5. Never `docker build` the devnet image by hand.

**2. `sudo` strips env.** Cap overrides must ride inline on `sudo env VAR=val
docker compose ...`. Exporting them silently no-ops and you measure devnet
defaults while believing otherwise. Verify against the node's startup log, which
prints `effective=` for the byte and order budgets. (The **action** cap is NOT
logged — `rate_limit.rs:native_total_block_cap()` has no `info!` — so it cannot
be self-verified. Shipped value is 100 on the p3/deep-book/topn branches, 1000 on
`cte-architecture`.)

**3. `PRESIGN=60` starves the bench.** The documented default runs senders dry
after ~20s while the harness keeps measuring an idle chain, diluting "sustained"
with idle time. Default here is 400.

**4. Markets never auto-create.** The RPC rejects unknown `market_id` (S432), so
`--markets 10` needs all ten in genesis — hence `genesis-10mkt.json`.

**5. Metrics live at `/metrics`.** Root 404s; the stock harness default omits the
path.

**6. n=1 is not a result.** Run-to-run spread on a contended box exceeded a
26% branch "gap" that vanished under repeats. Use >=3 reps per cell and
counterbalance branch order — throughput drifted downward across a session as the
box got more oversubscribed. `aggregate.py` refuses a verdict at n=1.

**7. This rig may be the bottleneck.** 4 validators + rpc-node + bench demanded
~18-23 cores on an 18-core box (load ~35). Check `cpu.csv` before believing any
plateau. The bench talks to **validator-0** (`:8645`, metrics `:9091`) — the
`rpc-node` service is NOT used by this harness and can be dropped to free ~3
cores.

## Reading the results

Per leg, under `results/<label>/`:

| file | contents |
|---|---|
| `summary.json` | sustained + peak placed/matched/executed, blk/s, conversion |
| `leg-config.json` | flow, taker, markets, senders, branch, commit — provenance |
| `sampler.csv` | 1 Hz counter series |
| `cpu.csv` | per-container CPU + load average (is the plateau real?) |
| `offered-vs-included.txt` | drop rate — the saturation tell |
| `post-metrics.txt` | rejects, resting depth, view vs height |
| `da-signals.txt` | body-fetch-exhausted / hash-only / Send-Queue-full counts |
| `counters-{before,after}.json` | raw counters **and `torus_exec_*` phase histograms** |

The phase histograms in `counters-*.json` are the production exec decomposition
(margin / match / settle, plus verify, flush, load_books, save_books). They are
measured *inside* the node, so unlike throughput they are immune to host
contention — prefer them when the rig is loaded.

`flushsplit.patch` further splits `flush` into batch-build / dirty-build /
trie-apply / rocksdb-write. Apply with `git apply --3way`; it is a diagnostic,
not for merge.
