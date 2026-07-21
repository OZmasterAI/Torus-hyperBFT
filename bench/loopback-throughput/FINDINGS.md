# Torus-hyperBFT — isolated multi-validator throughput findings + reproduction harness

Rebuilt `perf/re-proof5` (bd80ba8) on a **fresh, WAN-free 3-validator loopback devnet** (all nodes on one host,
localhost transport) to isolate the **execution/commit ceiling** from network/DA effects. All numbers below are
**node-sourced** (Prometheus counter deltas over a fixed mid-load window — never the bench RPC summary, which lies
under load). Load = `bench-throughput consensus --econ` (margin-managed, sustained), `--sign-mode eip712`, `--format bin`.

## Headline (node-sourced, loopback)

| metric | value | config |
|---|---|---|
| matched FILLS/s sustained | **90,598** | 64 markets, cap 500, batch 50 |
| matched FILLS/s burst (1s) | **119,694** | " |
| placed orders/s sustained | 107,219 | " |
| placed orders/s burst | 224,251 | 64 mkt, cap 400, batch 400 |
| aggregate orders/s sustained (act×batch) | 230,000 | 64 mkt, cap 400, batch 400, cross 0.1 |
| aggregate orders/s **burst** | **842,800** | " (2,107 act/s × 400) |

**Honest caveat up front:** this is **loopback**, no WAN. It removes the network/DA-dissemination dimension, so
these are *not* comparable to WAN fleet numbers — the point is the opposite: with the network removed, the chain
sustains ~90k fills/s, which **localizes the WAN wall to network/DA, not execution.**

## Where per-block commit time goes (node-sourced, working block ≈ 54 ms, cap 500)

| phase (`torus_exec_*_seconds`) | ms/block | note |
|---|---|---|
| engine (margin 5.1 + match 3.7 + settle 11.7 + gov/fee) | 22.7 | settle dominates |
| verify (eip712 ecrecover) | 13.1 | verified-sender-cache already amortizes repeat senders |
| flush = state_write 10.0 + **root (trie) 5.3** | 15.7 | **trie already optimal** |
| save_books | 1.5 | |

**Key finding: the state-root/trie is only ~5.3 ms/block** — already optimized by `TORUS_INCREMENTAL_STATE_ROOT`
+ `TORUS_PARALLEL_BUCKET_HASH` + `TORUS_NATIVE_ROOT_CACHE`. It is **not** the wall. The wall is **exec-thread
serial commit** (verify + settle + state_write), and block time balloons 35 ms → 0.8–1.5 s under load while the
matching engine itself bursts ~120k fills/s. The large **sustained↔burst gap is the inline-commit cost.**

## Levers that moved the needle (each measured, fills/s sustained)

| lever | effect |
|---|---|
| per-market sharding, markets 1→10 | 8,130 → 28,836 (**3.5×**); also crushed book-contention (2.4M→105k rejects) |
| block-cap sweep | peaks at `TORUS_NATIVE_TOTAL_BLOCK_CAP≈500`; too-small = per-block overhead, too-big = block-rate collapse |
| market count 10→64 | 56,803 → 90,598 (**1.6×**), rejects −98.5% |
| **markets 64→128** | **REGRESSES** (22.8k) — one matching thread per market oversubscribes a 64-core box; **optimum = markets ≈ cores** |
| accelerators (confirmed engaged via /proc/environ) | `TORUS_PARALLEL_SETTLE=1` + `PARALLEL_SETTLE_MIN_FILLS` + `RESIDENT_BOOKS=1` + `PARALLEL_BUCKET_HASH=n` + `NATIVE_ROOT_CACHE=1` |
| session sign-mode | ~flat vs eip712 (verified-sender-cache already cheap for repeat senders) |

## Recommendation — the one lever that isn't a knob

To lift **sustained** toward the ~120k matching burst (and beyond), the target is **off-thread commit
pipelining**: execution is already off the consensus thread, but within the exec thread the flush
(`root` + `state_write`, ~15.7 ms/block) is inline and serial. Moving it to a background worker (immediate flat-KV
batch so exec advances to N+1; trie-root + trie-node write on a second thread in a later batch) is ~1.4× on the
serial path. **Caveat:** the native state root is consensus-validated (`validate_block` gate), so this requires the
root check to become lagging/async — a real protocol change (~3–5 days), touching `incremental.rs` (commit),
`backend.rs` (native flush), `native_trie.rs` (`apply_native_dirty`), `state_root.rs` (routing), `app.rs` (flush
caller) + the validate_block root gate + differential root tests. It is **not** an env-var flip.

## Reproduction

`run-bench2.sh` (node-sourced sampler: actions + placed + fills + blocks, sustained best-window + burst peak) and a
multi-market genesis generator are included alongside this doc. Cluster = 3 loopback validators, isolated ports,
`--p2p-private-addrs`, full-multiaddr peer list. Metric truth: `torus_native_actions_processed_total`,
`torus_orders_matched_total`, `torus_orders_placed_accepted_total`, `torus_blocks_committed_total`;
per-phase `torus_exec_*_seconds` `_sum`/`_count`.
