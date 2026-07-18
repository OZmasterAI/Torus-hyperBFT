# p3 testnet throughput measurement (counter-scrape)

Reproduces the **p3 devnet mission** measurement on the **live testnet**, using
the same `scrape.py` parser and the same node counters, so numbers are directly
comparable. **Read-only**: it never starts/stops/wipes a node or touches systemd —
it only curls the local node's RPC + `/metrics` and (optionally) fires a bench.

## Why not just `bench-our.sh`?
`bench-*.sh` print the *bench tool's own* orders/s — the number the p3 mission
proved was misleading (most orders **rest**, few **match**). This harness reads
the chain's own counters instead:

- **placed/s**  = Δ`torus_orders_placed`   (orders that rested on a book)
- **matched/s** = Δ`torus_orders_matched`  (orders that actually crossed/filled)
- **executed/s**= Δ`torus_native_actions_processed`

and reports both **sustained** (Δ over the whole window) and **peak** (best
`SUBWIN`-second sliding window) — the two accounting bases behind "267k placed/s
peak vs ~37k sustained".

> placed/matched/executed are **consensus-deterministic → chain-global**. In a
> multi-box run, **one** measuring node already captures the aggregate. **Do NOT
> sum across boxes.**

## Prereqs
- The testnet node is **already running** on this box (p3 binary, anchored genesis).
- `--flow cross` needs the funded senders to exist in genesis (anchored `f2fa4b28`
  funds senders **0..59**). `--markets 10` is valid (genesis registers 10 markets).
- `curl`, `python3`. No sudo, no docker.

## Per-node config
| node | `RPC` | `METRICS` | sender range |
|------|-------|-----------|--------------|
| c18  | `http://127.0.0.1:8555` (default) | `http://127.0.0.1:9090` (default) | `SENDER_OFFSET=0`  |
| seed | `http://127.0.0.1:8545` | (seed's metrics-addr) | `SENDER_OFFSET=20` |
| val2 | `http://127.0.0.1:8545` | (val2's metrics-addr) | `SENDER_OFFSET=40` |

## A) Single-box smoke test (one validator loads itself + measures)
```bash
cd <repo>
RPC=http://127.0.0.1:8555 METRICS=http://127.0.0.1:9090 \
  ./testnet/p3-testnet/measure-leg.sh p3-c18-cross-smoke ./testnet/p3-testnet/results cross
```

## B) Coordinated run (real distributed measurement)
1. **Coordinator** (any one node) measures only, no local load:
   ```bash
   RUN_BENCH=0 DURATION=180 \
     ./testnet/p3-testnet/measure-leg.sh p3-round1-cross ./testnet/p3-testnet/results cross
   ```
2. **Each load box** fires its disjoint sender range at the SAME time (offsets
   0/20/40 so nonces never collide), e.g. on the box owning senders 20..39:
   ```bash
   RPC=http://127.0.0.1:8545 SENDER_OFFSET=20 DURATION=180 RUN_BENCH=1 \
     ./testnet/p3-testnet/measure-leg.sh load-20 /tmp/ignore cross
   ```
   (or just run `bench-throughput consensus … --sender-offset 20 --flow cross
   --markets 10` directly — the counters are read by the coordinator).
3. Read the coordinator's `results/p3-round1-cross/summary.json`.

## Knobs (env; defaults match the p3 proof legs)
`DURATION=120 SUBWIN=60 MARKETS=10 SENDERS=20 SENDER_OFFSET=0 BATCH=100 RATE=60`
`SUBMIT_BATCH=15 PRESIGN=60 SIGN_MODE=session FORMAT=bin FLOW=cross`

## Output (per leg, under `<results-dir>/<label>/`)
- `summary.json` — sustained + peak placed/matched/executed, blk/s, conversion %
- `sampler.csv` — 1 Hz counter time series (for your own peak/curve analysis)
- `counters-before.json` / `counters-after.json` — parsed snapshots
- `counters-*-raw.txt` — raw `/metrics` dumps
- `bench.log` — bench stdout/stderr (only in `RUN_BENCH=1` mode)

## Sanity: matching vs resting
- `--flow rest`  → ~98% rest, matched/s tiny (baseline shape).
- `--flow cross` → most orders cross, matched/s is real (the fill-path number).
- `matched_per_executed_order_pct` in the summary is the conversion; on clean
  books cross-flow should be far above the ~2.4% rest-flow floor.
