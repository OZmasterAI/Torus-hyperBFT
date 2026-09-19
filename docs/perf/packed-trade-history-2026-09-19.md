# Packed trade-history batches

`TradeKvs` previously allocated three value vectors per fill (65, 74 and 74
bytes). Deferred routing allocated another three key vectors (20, 32 and 32
bytes). The production path now builds fixed arrays and appends their bytes to
one batch arena with row offsets. This removes six small allocations per fill;
the arena and row vector still allocate when they grow.

Rows retain primary → maker → taker order, including duplicate keys. Values
retain the old little-endian fields, keys retain their big-endian fields, and
the final trade index is stamped into all six locations at the same point.
Only successful fills enter the existing routing path. The legacy
`take_pending_trades` API remains available through an allocating conversion;
execution uses `take_pending_trade_batch` and `send_packed` instead.

The background writer accepts both representations through its existing bounded
queue. Chunk sizes still count rows; write priority, partial-chunk failure,
queue counters, backpressure, shutdown drain and unsent-batch handback are
unchanged. Consensus submits the packed batch at the same post-flush or pipeline
handoff point and uses borrowed row slices for the same synchronous fallback.
These history column families remain outside the native consensus root; this
does not strengthen their existing crash durability.

Qualification: 12 state writer tests passed under receipt
`9b8bf7d2-62d6-4f0c-8925-970b5e62f006`. An independent old slice-chunk writer checks
exact bytes and write-group boundaries, including missing-CF failures and mixed
raw/packed queues. Root receipt `b20a69ca-d657-49f3-9378-b029ee6e09ac` covers one
independent frozen trade encoder test, eight parallel-settlement tests, three
deferred-trade tests and the consensus background-writer integration test.
The encoder compares both sides, same-trader duplicate keys, numeric extremes,
and repeated definitive-index stamps.

Tradeoffs: `TradeKvs` grows to 297 inline bytes. Packing copies rows into an arena,
and arena growth may move its contents. The legacy drain adds conversion copies.
Allocation removal does not establish a net throughput or latency improvement;
no benchmark was run.

The existing trade microbenchmark's source now calls the production packed drain
instead of measuring the legacy allocating conversion. The harness was not run
or separately compiled during this session; this is only a two-line API update.
