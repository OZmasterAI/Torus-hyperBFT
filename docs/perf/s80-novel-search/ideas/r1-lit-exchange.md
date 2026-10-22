# r1 lens lit-exchange: exchange / matching-engine / DEX-L1 mechanisms

Read-only. Nothing was built, run or benched. The main checkout moved from `eb993e3f` to `2d2f3ed9` during this
search (batch 10 merged: L6.1, L2.1). All `file:line` anchors are at `2d2f3ed9`. NE = `crates/torus-bridge/src/native_executor.rs`.

Scratch kept in `/home/oz/projects/wt/novel-s80/proto/lit-exchange/` (`divti3-callsites.txt`, `fp-callsites.txt`):
- What they list: the i128 division call sites per function, i.e. the libgcc `__divti3`/`__modti3` calls made through the GOT.
- Source: objdump of the existing `~/.cargo-target-s80-base/release/torus-node`.
- The 430 MB disassembly has been deleted.

## Lens outcome: one new idea; the other mechanisms are already covered

I mapped each lens mechanism onto NE / `order_book.rs` and the measured costs, then checked it against the ledger and
the twelve sibling r1 files. Almost every mechanism already exists somewhere.

| lens mechanism | where it already is |
|---|---|
| Asynchronous liquidation engine (BitMEX/Binance, off the exec thread) | r1-exec-amortize #2, r1-lit-bft #2 and r1-exec-stage-pipeline #1. My exact, consensus-invisible variant (re-check dirty traders only) is refuted by r1-exec-amortize's data: ~90% of window holders are dirty every block. |
| FOK feasibility from depth aggregates, sparse COW instead of cloning the margin maps (`order_book.rs:1753-1754`) | r1-robustness #2, r1-contrarian #1 |
| Dedicated core / RT class for the serial thread (LMAX, HFT) | r1-tail-sched #2. Also, the host has RLIMIT_RTPRIO 0. |
| Instrument-indexed dense risk vectors in Phase 2 (`get_position` 4.96% incl. of E, `Projections` 6.54%, q8re) | r1-margin-algo #1, r1-hw-memory #5 |
| ITCH-style O(1) mass cancel (generation counter) | r1-contrarian #2; ledger `phase1-cancel-tombstones`, `lazy-delete-level-queues` |
| Compact resting-order record (Order ~176 B) | r1-hw-memory #4 |
| LMAX smart batching of queued blocks into one engine unit | r1-lit-bft #1, r1-exec-amortize #1, r1-exec-stage-pipeline #2. My own check: the engine histogram is bimodal (57.5% of blocks < 4 ms, 9% > 128 ms), so the fixed per-block cost is small once the liquidation scan is off E. |
| Gateway-side static order normalization (unmarshaller stage) | r1-margin-algo #2; ledger L6.3 |
| Integer ticks / constant reciprocal instead of `__divti3` | r1-margin-algo #5. libgcc frames are only 1.9% of E self (q8re), and `fixedpoint-u64-fastpath` was neutral. |
| Frequent batch auctions | ledger `frequent-batch-auctions` (proposed) |
| HL in-block tiering (non-order actions, then cancels, then GTC/IOC) | ledger `hl-in-block-order` (parked) |
| Reserved collateral buckets per book (maker over-commit) | ledger `maker-overcommit-cross-market-fix`, R05 options 4/6/7 |
| Phoenix seats / OpenBook crank (lazy maker settlement, netting) | ledger `net-balance-per-trader`, `settle-pass-b-parallel-shard-apply` |
| OpenBook fixed-capacity book with eviction; GTC TTL | r1-robustness #5; ledger `resting-order-ttl-gc` |
| Per-symbol sticky core ownership for pool jobs | dropped. Pinning was -5.7% (`t0-1-cpu-pinning`), and 3 nodes x 32 rayon threads share 32 hardware threads, so L2 residency does not survive a 45 ms block. |
| Per-account integer IDs instead of hashing 20-byte addresses | dropped. s82 precedent (`ahash-exec-maps-s82`): hash CPU -39%, matched/s flat. |
| Epoch-credited cum_volume (daily-tier snapshot) | dropped. `put_cf_raw` + `get_cum_volume_typed` are only ~0.5% of E (q8re incl. 35 + 38 samples of 15,081). |

What is left and new as a mechanism is a market-structure robustness rule from exchange practice: the CME pre-open,
and the NYSE/Nasdaq halt-and-reopen procedure.

---

## 1. stale-mark-preopen: a pre-open (cancel-only) phase whenever a market's mark is stale, e.g. on a chain resume

**Mechanism.**

What happens today on a chain resume:
- **Halt length.** n = 3, so one dead validator halts the chain. After a SIGKILL, the first commit takes 27-40 s
  (s62), and 48 s at 1M resting (s96.4).
- **Marks stay stale after the restart:**
  - The last aggregate stays usable for 60 s of header time (`oracle.rs:21`, `DEFAULT_MAX_ORACLE_AGE_SECS`).
  - Submissions older than 10 s do not count (`oracle.rs:23`), and 3 reporters are required (`:24`). So
    `aggregate_price` returns the pre-halt price (`:284-300`).
  - `begin_block_oracle` (NE:15843) fixes the block's marks at block start.
  - In-block `SubmitOraclePrices` sort after every order (`ActionCategory::Oracle = 4`, NE:16260-16268), so they only
    help the next block.
- **Effect.** The resume blocks run the F1 account check, the band check, matching and liquidations
  (`liquidation_step.rs:433-499`) against marks and resting MM quotes that are 27-48 s stale.
- **Backlog.** At the same moment the backlog arrives: up to 5,000 senders x MAX_IN_FLIGHT 4 = 20k actions, all still
  valid in the 60 s nonce window.

**Rule** (consensus, P0.0-gated):
- **Trigger.** A market is PRE-OPEN while the aggregate behind its usable mark is older than T_fresh (~8 s of header
  time; the feed runs every 2 s). The age is computed once in `begin_block_oracle`.
- **While PRE-OPEN:**
  - Placements in that market are rejected with a new `FailureReason::PreOpen`. This happens at the pre-book reject
    site (NE ~10011-10040, next to `book_shape_violation` and the band check) and on the single path.
  - Cancels, oracle and lockbox actions run as usual.
  - The liquidation step values accounts but does not act.
- **Exit.** A fresh aggregate exists (all reporters inside the window), plus T_hold (~1 s of header time) so that
  queued cancel-alls land first.
- **Optional.** A resume block processes the Oracle category first, which shortens the exit by one block. A LULD-style
  trigger on a large aggregate jump can be added.
- **Node-local.** Leaders skip placements for PRE-OPEN markets at selection, so these stay in the pool instead of
  burning block space.

**Target metric.** Robustness and security:
- no matching, margin checks or liquidation on stale marks;
- protection of resting quotes after a halt or an oracle outage;
- deep-book stability on resume.

Steady-state matched/s: 0 change.

**Expected gain.**
- **Exposure window.** Today a 27-48 s halt (usable for 60 s) leaves up to 48 s of mark and quote staleness at resume.
  With the rule, no matching happens on a mark older than T_fresh.
- **MM protection.** MMs get T_hold plus one oracle round (~2-3 s) in which their cancels execute and nothing crosses
  their stale quotes.
- **Resume exec load.**
  - Without the rule, the first blocks are 400-action blocks of backlog: ~98k orders each (61.5% places x 400 x 400).
    At ~2 us of E per placed order that is ~200-300 ms of E per block (**est.**).
  - With the rule they stay cancel and oracle blocks, like today's non-placing blocks. Those are 57.5% of all blocks,
    with engine time < 4 ms (std CL21 `torus_exec_engine_seconds` histogram). The exec queue drains first.
- **Bench.** 0, provided the trigger never fires under load (that is the falsifier below).

**Evidence.**
- `oracle.rs:21-24`, `:284-300`.
- NE:16260-16268 / 16271 (`classify_action`): Oracle sorts after GtcOrder.
- NE:15843 (`begin_block_oracle`).
- `liquidation_step.rs:413-416` (marks from the block table), `:433-499` (acts).
- measured.md section 5.6 (27-40 s and 48 s halts).
- `oracle-feed-stats.json`: 3 reporters, 2 s interval, 948/948 accepted.
- 9j2 std CL21 engine histogram: 4,194 of 7,290 blocks < 4 ms.

**Nearest prior.**

Grepped: `pre-open|preopen|cancel.only|post-halt|resume|circuit|stale mark|stale oracle|staleness|halt|suspend|post-only mode|reduce-only mode|max_age|oracle age|fresh mark`.
Memory search: no pre-open or stale gate decision.

| prior | status | difference |
|---|---|---|
| `oracle-begin-block-aggregation` | merged | Defines the 10 s window and the 60 s staleness; no trading gate. |
| `frequent-batch-auctions` | proposed | Replaces continuous matching. This idea is a gate only, plus an optional single reopening auction. |
| `hl-in-block-order` | parked | Category order. |
| `mempool-priority-classes-oracle` | merged | Selection only. |
| `start-networking-before-replay` / restart knobs | — | Restart speed, not market safety. |
| sibling r1-lit-bft #3 (vote-extension oracle) | — | Keeps marks fresh while consensus is live. It does not gate the resume block's matching against stale resting quotes, nor liquidation acts. |

**Risks.**
- **Ownership.** Consensus-visible and overlaps the owner's P2 mark price / bands. The owner decides.
- **Trading-liveness lever.** An oracle outage, or a leader censoring oracle actions, freezes placements in a market.
  That is the intended exchange behaviour, but it is a lever. The oracle priority class and validator reporters
  mitigate it.
- **Trigger choice.** The header-time gap must not be the primary trigger, because leaders set timestamps.
- **T_fresh calibration.** T_fresh must exceed the worst aggregate age under load. Oracle commit age p99 is ~5 s in
  burst phase A.
- **Parity.** Not HL parity.

**Effort.** 3-5 person-days:
- the rule and P0.0 gating;
- FailureReason and RPC text;
- the selection skip;
- the liquidation act gate;
- a byte-identical differential with the trigger off.

**Prototype (<= 3 h).**
1. **Falsifier and calibration.** From the 9j2 cells (`metrics-1hz.txt.gz`, node logs), extract the per-block age of
   the usable aggregate and the header-time gaps under load. If the aggregate age ever reaches 8 s in std or deep CL21,
   raise T_fresh or drop the idea.
2. **ubench_econ resume scenario** (`UB_MARKS=1 UB_MARK_WALK=10`):
   - After 40 warm blocks, advance header time by 40 s, shift the fed mark by ±2%, and inject a 2x action backlog.
   - Compare today's path with a 20-line prototype gate: an aggregate-age check in `prepare_one` plus the liquidation
     act gate.
   - Count fills priced outside fresh mark ±1%, Σ maker PnL vs the fresh mark, and liquidations acted on stale marks.
   - Time E ms for resume blocks 1-5.
