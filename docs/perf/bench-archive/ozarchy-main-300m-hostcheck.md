# ozarchy main 300-market host check (2026-10-06 00:43)

Dir: `ozarchy-main-300m-hostcheck-c1`. Recorded in the docs: **no**. (`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section 18 mentions the three voided runs that
motivated it, not this cell.)

## Purpose

From the header of `ozarchy-main-300m-hostcheck-campaign.sh`: "Host check: MAIN only (92a02ed, staged node 31a95c65, TORUS_NATIVE_TRIE_MAINTENANCE=0 via EXTRA_ENV), 300 markets, cap 400,
rate 76,000, RETRY_BUSY=1, 120 s, no oracle feed, no perf. 2 cells. ... Question: do validators die on main at 300 markets on this host? Harness/tools from the 4acdc59 worktree; bench 4acdc59."
It ran at 00:43-00:47, between the second voided run (val2 died at 00:37) and the third (starts 00:50) of the step 2 campaign (`ozarchy-4acdc59-300m-void-runs.md`), to see whether nodes also die on main.

## Setup

* Main `92a02ed6fd72d9c5b3f444bf855c9a2f93cf3ede` (worktree `/home/oz/projects/wt/main`, `dirty_files` 0), node md5 `31a95c654761f582a5c124b4146d6524`, `TORUS_NATIVE_TRIE_MAINTENANCE=0`;
  bench-throughput md5 `1de55dedde6ae64fc4162380cd361204` (from the 4acdc59 build); genesis 300 markets, md5 `b83180e1458c06b5b0cceb89d80cfa7a`.
* Cell: 300 markets, cap 400, rate 76,000, 5,000 senders, `RETRY_BUSY=1`, 120 s, no oracle feed, no perf. A watcher polled `kill -0` on the 3 node pids every 2 s; a death aborts the cell.
* Driver `ozarchy-main-300m-hostcheck-campaign.sh` (launched by `ozarchy-main-300m-hostcheck-launch.sh`, log `ozarchy-main-300m-hostcheck.campaign.log`). The script lists two cells (`-c1`, `-c2`);
  only `-c1` ran. The marker `ozarchy-main-300m-hostcheck.campaign.done` reads `exit=aborted-by-coordinator 00:48:01`.

## Result

```
SUMMARY ozarchy-main-300m-hostcheck-c1: matched/s avg=98205.6 first120=97286.9 best60=135933.8 placed/s=128125.4 blk/s=1.2 txs/blk=190.5 timeouts=20.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[53.3, 55.4, 42.0] drained=True bench_rc=0
VALIDITY ozarchy-main-300m-hostcheck-c1: ACCEPT liveness=PASS reasons=[]
```

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-main-300m-hostcheck-c1` | OK | 2026-10-06 00:47 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 98,205.6 | 97,286.9 | 135,933.8 | 128,125.4 | 1.2 | 190.5 | 595.73 | 7.18 | AGREE | PASS | ACCEPT |

`node-life.txt`: "all 3 node pids alive at every 2 s check until the harness stop began"; stop window 2026-10-06 00:47:42.608 to 00:47:42.611 (local, CEST).
`node-environ-trie.txt`: all 3 pids `exe_md5=31a95c65 TORUS_NATIVE_TRIE_MAINTENANCE=0`. Drained after 46 s (`drained=1`), heights after drain 891 890 891, state digest done in 56 s wall.
So: no validator died on main in this one cell; the other cell was not run.

Harness lines for val0 (verbatim from `ozarchy-main-300m-hostcheck-c1.campaign.log`):

```
PHASE val0: block_ms=595.86 wall/committed(load)=807.9 (incl_drain=310.0) evm=0.0(0.0%) verify=28.76(4.8%) replay_guard=5.07(0.9%) load_books=0.05(0.0%) engine=492.14(82.6%) save_books=21.66(3.6%) flush=179.85(off-chain) body_persist=0.1(0.0%) end_resident=0.0(0.0%) end_resident_wait=0.0(0.0%) residual_untimed=48.07(8.1%)
ORDER_AGE val0 (ms p50/p90/p99): admit=2374.5/10464.2/34456.8 commit=6652.0/44714.1/78199.4 exec=48227.3/81139.0/154788.9 durable=48370.5/81189.7/154847.6 fills_visible=48281.6/81158.2/154811.1
CHAIN val0: chain_ms=595.73 (p50=482.4 p95=1695.0) gap_to_100ms=495.73 pipelined_ms=178.1 handoff_wait_ms=0.24 worker=True empty_block_ms=0.13 | fills/blk=68588.7 engine_ms/1k_fills=7.18 | LOAD native_blk/s=1.23 empty_blk/s=0.008 commit_ms avg/p50/p95=567.5/457.6/1821.7 | save_books=21.66(drain=20.89 write=47.97) | identity covers_e=True le_block=True chain-e_phases=48.05
ENGINE val0: total=492.14 = phase1_actions=53.74 margin=97.61 match=165.61 settle=166.12(passA=34.07 passB=83.83 cache_flush=46.38) tail=0.05 untimed=9.01
```

## Files

Common to all 1 dirs (46 files): `agreement.jsonl`, `analysis-val0.txt`, `analysis-val1.txt`, `analysis-val2.txt`, `bench.log`, `buckets.csv`, `cpu-ticks.txt`, `cpu.csv`, `digest-accounts.txt`, `digest-val0.out`, `digest-val1.out`, `digest-val2.out`, `drain-samples.jsonl`, `drain.json`, `funnel-val0.csv`, `funnel-val1.csv`, `funnel-val2.csv`, `log-summary.json`, `metrics-after-val0.txt`, `metrics-after-val1.txt`, `metrics-after-val2.txt`, `metrics-before-val0.txt`, `metrics-before-val1.txt`, `metrics-before-val2.txt`, `node-environ-trie.txt`, `node-life.txt`, `phase-val0.csv`, `phase-val1.csv`, `phase-val2.csv`, `run.log`, `sampler-diagnostics.jsonl`, `sampler.csv`, `sampler.log`, `schedstat.json`, `schedstat.raw`, `state-digest-val0.txt`, `state-digest-val1.txt`, `state-digest-val2.txt`, `summary.json`, `tasks.txt`, `val0.log.excerpt`, `val0.log.gz`, `val1.log.excerpt`, `val1.log.gz`, `val2.log.excerpt`, `val2.log.gz`
Files of 1 MiB or more (all dirs): `ozarchy-main-300m-hostcheck-c1/val1.log.gz` 6.4 MiB; `ozarchy-main-300m-hostcheck-c1/val0.log.gz` 6.4 MiB; `ozarchy-main-300m-hostcheck-c1/val2.log.gz` 6.4 MiB; `ozarchy-main-300m-hostcheck-c1/buckets.csv` 4.9 MiB; `ozarchy-main-300m-hostcheck-c1/tasks.txt` 1.4 MiB

Top-level: `ozarchy-main-300m-hostcheck-c1.campaign.log` (8 KiB), `ozarchy-main-300m-hostcheck-campaign.sh` (7 KiB), `ozarchy-main-300m-hostcheck-launch.sh` (268 B), `ozarchy-main-300m-hostcheck.campaign.done` (37 B), `ozarchy-main-300m-hostcheck.campaign.log` (322 B).
