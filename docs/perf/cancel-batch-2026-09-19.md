# Bounded two-ended cancel-all candidate

This candidate is based on frozen runtime `080c4fa`. It changes only the core
order book's cancellation implementation. No throughput run or microbenchmark
was performed, and no measured speedup or resolution of the sustained-run
degradation is claimed.

The original loop binary-searches each target and calls `VecDeque::remove`.
Canceling multiple middle orders can move the same large surviving orders many
times. The candidate groups a bounded batch by level and finds each target's
position once, before changing queues. Each level either removes in the cheaper
of ascending/descending position order, or uses a new stable compaction:

1. Find the largest interval containing no cancellation targets.
2. Leave that middle interval untouched. Scan the affected prefix backward and
   suffix forward, swapping survivors toward the middle.
3. Pop the canceled values from the two ends, moving rather than cloning them.
4. Restore cancellation output order through the original trader-index slots.
   Replay index/sequence removal, row and level journals, epoch increments and
   dirty-chunk marks in that same original order.

Each affected survivor is moved at most once by compaction. No survivor crosses
the untouched middle interval, so FIFO order is preserved across all three
regions. The selected split minimizes the touched prefix-plus-suffix length.
Compaction is used only when three times the affected-survivor count is below
both directional repeated-removal movement estimates. This bounds movement;
it does not predict CPU time, because deque memmoves and safe swaps differ.
Planning memory scales with the cancellation batch, never queue depth.

Admission is deliberately conservative. The trader must have 32–200 targets
and the book at least 1,024 resting orders. Before allocating, sorting, grouping
or probing queue positions, a sample requires the first target's level to have
at least 1,024 orders and four of the first 32 targets to share that level.
This costs at most 32 index reads and one level lookup. Singleton/scattered
levels and shallow prefixes stay on the original loop; a later deep suffix
does not override that decision. This may miss useful opportunities.

Read-only preflight rejects stale/missing/duplicate target indexes and missing
target sequences. Epoch overflow also selects the old loop before mutation,
preserving its debug-panic partial state and release wrapping behavior. Books
restored above the 200-order limit remain on the original path. No new runtime
flag is added and existing experimental flags are unchanged.

The generator's batch size of 400 and the chain's cap of 200 are distinct from
the book's per-trader, per-market limit. `place_order` enforces the latter on
every individual placement; an all-market cancellation invokes `cancel_all`
separately for each book. Archived deeper-run diagnostics aggregate 4,327,789
orders across 73,280 market calls (59.1 orders per market call), but contain no
per-market target-count histogram. Neither that average nor the source limit
establishes this candidate's activation share. The generator fixes side per
trader/market and uses a five-price passive band; aggressive remainders can
occupy other levels.

Initial correctness receipt `f522dbbf-2146-4fdc-a7a3-7dee17dbe654` passed 118 core
library, five determinism, three fuzz and 12 level-row tests, with three existing
ignored tests remaining ignored. Eight new tests compare against the frozen
original loop and exhaust all nonempty target subsets and physical wraps for
queue lengths 1–9. They cover cancellation order, classic bytes, order rows,
journals, flat/chunked commitments, modified priority, partial fills, level
recreation, missing/duplicate indexes, stop-only early return and epoch overflow.
That receipt predates the concentration guard. The follow-up adds differential
coverage for 32 singleton/shallow/deep-distinct levels and a shallow prefix with
a deep suffix, plus positive admission assertions for concentrated fixtures.
The final guard qualification is recorded in the session verification receipt.
Release checks exercise wrapping arithmetic; the debug-panic branch has not
been executed in this session. Independent source review found no correctness
blocker and prompted the concentration guard to avoid wasted shallow planning.

Future performance qualification must measure admission and compaction shares,
planning overhead, queue work and accepted end-to-end throughput on the exact
candidate. The previously rejected deep-compaction threshold variants do not
provide performance evidence for this different algorithm.
