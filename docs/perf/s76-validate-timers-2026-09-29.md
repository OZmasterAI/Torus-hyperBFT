# s76 item 6 step 1: next-leader validate timers — 2026-09-29

Branch `perf/s76-validate-timers`: `1067cc5` (timers) + `af00202` (single-flush
fix). Metrics and one trace line only; no behaviour change.

## What was added

- `torus_validate_block_da_{flush,multiget,decode,wait}_seconds` and
  `torus_validate_block_da_flush_bodies_total`: split of the compact reconstruct
  (`reconstruct_native_actions_hot_core`). The flush is timed inside
  `Mempool::get_native_da_batch_timed`, which does the path's single flush.
- `torus_validate_block_attest_lookup_seconds`: proposer lookup, split from
  digest + ed25519 verify.
- `TORUS_BODY_FETCH_TRACE=1`: `body_fetch_diag insert` line in `try_insert_body`
  with validate end, tree insert end and commit-feed end (`*_mono_us`).

`1067cc5` alone flushed twice per compact validate (an extra untimed RocksDB
write, 14-25 ms/call). Its cells (`s76-vt-*`) are not valid for timing;
`af00202` fixes it.

## Cells

All cap 200, 10 markets, rate 76,000, 300 s, s75 unpin env
(`TORUS_BODY_FETCH_TRACE=1 TIMEOUT_BASE_MS=1200 TORUS_BODY_SERVE_THREAD=1
TORUS_DEFER_PARENT_FEED=0`), s75 `bench-throughput` binary. Campaign
`~/bench-results-matched/s76-vt2-20260929`, alternating, 11:00-11:50.

| cell | binary | matched/s | accepted / agree / liveness |
| --- | --- | --- | --- |
| main-r1 | e12bdac (= main b9b2fb4 code) | 61,944 | yes / AGREE / PASS |
| timers-r1 | af00202 | 65,854 | yes / AGREE / PASS |
| timers-r2 | af00202 | 64,760 | yes / AGREE / PASS |
| main-r2 | e12bdac | 71,134 | yes / AGREE / PASS |

No measurable cost: validate, reconstruct, attest, commit-persist write, block
build and node CPU per 1M matched are equal between arms (within cell spread).
Daytime host disk `w_await` was 17-22 ms in both arms vs 9-11 ms in the s75
night cells, which is why both arms sit below the s75 74.0k mean. Compare only
arms from the same campaign.

## Where the next leader's parent validate goes (timers cells, per node)

| phase | mean ms | p50 ms |
| --- | --- | --- |
| DA reconstruct (total) | 47-54 | — |
| ingress DA-mirror flush (~33 bodies, ~0.9 ms/body) | 27-34 | 14-19 |
| body decode | 12-16 | 8-11 |
| RocksDB MultiGet | ~6 | ~3 |
| missing-body wait (fired 0-4 times of ~300) | ~0 | — |
| attest (lookup 0.1-0.3; rest digest + ed25519) | 10-12 | 7-8 |

Body-late views (parent body received after the next leader entered the view):
33-48% of views. In them, per node p50: validate 37-41 ms, tree insert 5-8 ms,
commit feed 87-112 ms, feed end to `produce_block` 0.7 ms.

## Next (step 2)

The validate flush writes the whole ingress mirror buffer, not only this
block's bodies. Candidate: MultiGet first, flush only on a miss (a hit is
already durable; a miss still flushes before the vote, so S459 holds). At stake
~30 ms mean per body-late view, ~10-15 ms per view on average. s67 and s68 cut
more consensus-thread time than this with flat throughput, so judge step 2 on
validate and receive-to-propose latency; throughput is a no-regression check.
The commit feed (87-112 ms p50 per late view) remains the larger item.

## Tests

Full `cargo test --release --workspace` on `af00202`: 1906 passed, 1 failed,
30 ignored. The failure, `commit_lag_wedge_red_knob_off_commit_freezes`, is a
timing-sensitive repro that passed 3/3 alone on both this branch and main.
