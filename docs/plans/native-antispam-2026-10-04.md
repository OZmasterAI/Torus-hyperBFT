# Native anti-spam: decisions, build, deferrals (2026-10-04)

Branch `feat/native-antispam` (worktree `/home/oz/projects/wt/antispam`, on main
`79a3752`; commit hashes below are as of that rebase). Node-local only: no
change to block validity, `validate_block`, `sort_native_actions`, the
executor, the state hash or the app.rs block flow.

## 1. Problem

At block cap 20 the matched-bench stalled: 222 matched/s, and orders were
placed in only 6 of 3,351 executed blocks. Three things combined:

1. RPC admission (s65 item B, the admission limit) shed only non-cancels.
2. The native pool selected every cancel before any order. The pool sort key
   is `(u8::from(!is_cancel), sender, nonce, seq)`
   (`crates/torus-mempool/src/native_pool.rs:293`), and a cancel arriving at
   a full pool evicted an order.
3. The bench drew a new action after each shed, so about 100% of admitted
   actions were cancel-alls.

The bench side was fixed with `--retry-busy` (merged, `ec4d069`). The chain
side stayed open: native ingress checked only decode, batch size, markets and
signature, so any fresh key could sign cancel-alls, and anyone could stop
order placement for free.

## 2. Hyperliquid reference (official docs)

| Topic | Hyperliquid | Torus main (before this branch) |
|---|---|---|
| Account activation | New accounts pay 1 USDC | None: any key may sign |
| Per-address requests | 10,000 initial + 1 per 1 USDC traded; a batch of n = n requests; cancels `min(limit + 100000, 2 * limit)`; exhausted = 1 request per 10 s | None |
| Open orders | 1000 + 1 per $5M volume, max 5000 | Same rule, merged (`2567e56`, `ded6e6c`) |
| In-block order | (1) actions that send no GTC/IOC (incl. ALO post-only, transfers, settings), (2) cancels, (3) GTC/IOC; proposer order inside each group | `classify_action` / `sort_native_actions` (`crates/torus-bridge/src/native_executor.rs:7370`, `:7411`): cancels, then IOC/FOK/market/stop/modify, then (EVM), then GTC/PostOnly/batches, lockbox, oracle, governance, staking; (sender, content hash) inside each group |
| Congestion | Each address limited to 2x its previous-day maker share of block space | None |
| Per-IP | 1200 weight/min | None |
| Fees | Trading is gas-free, fees only on fills (perp 0.045% / 0.015% tier 0), $1 withdrawal | No trading fees: `total_native_fees` (`native_executor.rs:1669`) is set to 0 (`:2151`) and only read (`:7240`), never incremented. EVM base fee frozen at 1 gwei (`crates/torus-mempool/src/lib.rs:115`) |

## 3. Options discussed

| Option | Where | Consensus change? | Outcome |
|---|---|---|---|
| Cap the cancel share of each block | Proposer selection | No | Built (C) |
| Per-sender cancel rate limit | Ingress | No | Covered by B's cancel allowance |
| Funded-account check | Ingress | No | Built (A) |
| HL-style per-address request limit | Ingress | No | Built (B), made stricter in round 2 |
| Per-IP limit | RPC server | No | Built (D) |
| HL congestion share (E) | Proposer selection | No (needs per-day maker volume) | Deferred |
| HL in-block order | Executor ordering | Yes | Deferred (18c) |
| Durable committed counter | Block-consistent view | Touches the app.rs commit path | Deferred (item 6) |
| Enforcement at execution | Executor | Yes | Deferred (18c) |

## 4. What was built

### Round 1: items A, C, B, D

| Item | Rule | Switch (node default / bench devnet default) | Code |
|---|---|---|---|
| A funded account | A non-exempt action enters the pool only if its sender holds >= N whole TRS (perp available + order margin, or EVM spot, or any open position). Stake exits, claims and votes are exempt; validator duties are exempt only for registered validators (round 3) | `TORUS_INGRESS_MIN_COLLATERAL`, 1 / 0 | `crates/torus-mempool/src/funded.rs`; `Mempool::check_funded` (`lib.rs:770`), called on every path in `submit_native_action_inner` |
| C cancel share | Cancels take at most `ceil(limit * pct / 100)` slots of a selected block, then non-cancels in the unchanged order, then leftover slots go to remaining cancels. Cancels no longer evict orders; a cancel at a full pool is refused | `TORUS_CANCEL_BLOCK_SHARE_PCT`, 25 everywhere (100 = old behavior) | `native_pool.rs` (`FIRST_NON_CANCEL`, `:37`); default `rate_limit.rs:182` |
| B per-address limit | Allowance = buffer + 1 per TRS of cumulative traded volume; weight = `order_count`; cancels `min(allowance + 100000, 2 * allowance)`; in memory, bounded to 200,000 addresses | `TORUS_ADDR_RATE_LIMIT` / `_BUFFER` (10000) / `_EXEMPT`, on / off | `crates/torus-mempool/src/addr_rate.rs` (moved from torus-rpc in round 2) |
| D per-IP limit | 1200 weight/min per IP; only loopback exempt by default; at most 100 subscriptions per WebSocket connection | `TORUS_RPC_IP_WEIGHT_PER_MIN` (1200), `TORUS_RPC_IP_EXEMPT`, `TORUS_RPC_MAX_SUBS_PER_CONN` (100); off on the bench devnet | `crates/torus-rpc/src/ip_limit.rs`; custom accept loop `crates/torus-rpc/src/lib.rs:455` |

Harness: `ANTISPAM=1` in `tools/matched-bench/run-cell.sh` turns A, B and D on
for a cell.

Decisions made in round 1:

- **C refuses rather than evicts.** A cancel at a full pool is refused instead
  of evicting another cancel, so a spammer cannot push out other users'
  cancels. The cost: a market maker's cancel can be refused while the pool
  is full.
- **D needs its own accept loop.** jsonrpsee 0.26 does not pass the peer
  address to middleware. The custom loop no longer applies jsonrpsee's
  HTTP/2 keep-alive configuration. **Review item.**

### Round 2: items 1-3

| # | What | Rule as built | Commit |
|---|---|---|---|
| 1 | Strict exhausted mode (B) | Once over its allowance, an address may send only a weight-1 action (one order, one cancel, any other single action), at most one per 10 s since its last pooled action. A batch of n > 1 orders is refused (`addr_rate_limited`). Before this change, the slow mode let through one action of any weight, so a 400-order batch passed every 10 s. The cancel allowance is unchanged | `1f40b27` |
| 2 | Count on pool entry (B) | Every native action that enters this node's pool is counted against its sender, whatever its source: RPC single, batch or bin; gossip; leader forwards; local submits. The count is the action's `order_count`. Enforcement stays at RPC ingress only. Gossiped and forwarded actions are counted but never refused, so nodes never disagree on what they hold because of B | `d395687` |
| 3 | Cancel-spam bench mode | `bench-throughput consensus --spam-cancel-keys K --spam-cancel-rate R [--spam-cancel-funded]`; `run-cell.sh` `SPAM_CANCEL_KEYS` / `SPAM_CANCEL_RATE` / `SPAM_CANCEL_FUNDED=1`. Default off | `ce326cd` |

How item 2 counts each action exactly once:

- **Where the charge happens.** The single increment is
  `AddrRateLimiter::charge` (`addr_rate.rs:182`), called in
  `submit_native_action_inner` right after a successful pool insert
  (`lib.rs:743`).
- **Duplicates.** The pool dedups by hash, so a second copy of the same body
  is not counted. That covers a gossip echo of this node's own RPC action and
  copies arriving from several peers.
- **Refusals.** An insert the pool refuses (duplicate, full, unfunded) is not
  counted.
- **The RPC check is read-only.** RPC ingress calls
  `Mempool::addr_rate_admits` (`lib.rs:243`, which uses
  `AddrRateLimiter::admits`, `addr_rate.rs:115`). The check changes nothing,
  so an RPC action is not charged at the check and then again at pool entry.
  The alternative was to charge at RPC and skip the pool-entry charge for
  that action. It was rejected because it needs a provenance flag threaded
  through every add path, and it would charge actions the pool then refuses.
- **Concurrency.** The check and the charge take the limiter lock
  separately, so concurrent requests from one address can overshoot slightly
  (bounded by request concurrency). This was already true in round 1.
- **Cost on the ingress and gossip path.** One `HashMap` update under a
  short mutex, after the pool lock is released. No DB read: the volume is
  read only on the RPC check, and only once the buffer is used up.
- **Memory bound.** The bound still holds (two-generation map, 200,000
  addresses). A refused address is moved back into the current generation,
  so a sender that keeps getting refused is not forgotten.
- **Code moved.** The limiter and `bounded_map` moved from `torus-rpc` to
  `torus-mempool`. `torus_rpc::addr_rate` re-exports the module, and
  `RpcServer::set_addr_rate_limiter` installs the limiter on the mempool.
- **Known limits.** Counts are node-local and approximate. A node that just
  restarted, or missed gossip, under-counts until it catches up. A body
  re-admitted after its commit removed it from the pool would be counted
  again (possible in principle, bounded by the 60 s nonce window).
  Validator duties of registered validators are not counted (round 3).

How item 3 picks spam keys:

- **Funded keys** (`--spam-cancel-funded`) are the top K indices of the
  bulk-funded genesis range 60..100060 (`testnet/gen-weighted-genesis.sh`,
  the same derivation as `gen-accounts`). The bench refuses a plan that
  overlaps the load senders (`--sender-offset` .. `+ --senders`).
- **Unfunded keys** come from a seed space no genesis funds, deterministic
  per index so cells repeat.
- **Sending.** Each spammer has one request in flight at a time, sends
  `CancelAllOrders` (all markets) and never retries.
- **Reporting.** The bench prints one line apart from the load:
  `Spam cancel-all (K funded|unfunded keys, R actions/s): sent .. accepted ..
  rejected .. (unfunded= addr_rate_limited= ip_rate_limited= busy= other=)`.
  It comes right after the load's `Submitted (load-gen accepted)` line in
  `bench.log`.

### Round 3: fixes found by review and by the spam bench

| What | Rule as built | Commit |
|---|---|---|
| Validator duties only for validators (A, B) | `SubmitOraclePrices`, `AttestStateHash`, `JailVote`, `UnjailSelf`, `RotateValidatorKey`, `UpdateCommission` skip A and B only when the sender has a row in the validator table (any status, so a jailed validator can unjail; one point read, only for these kinds). Before: A exempted them for any key, because ingress never checked validator membership, so a fresh key could send them past A for free; and round-2 item 2 counted validators' oracle prices and attestations against B, which would have throttled the oracle once the allowance ran out | `c580eb0` |
| Admission counts only non-cancels | `Mempool::native_admission_backlogged` (`lib.rs:301`) compares `NativePool::non_cancel_size()` (`native_pool.rs:185`; a cancel count kept at insert and in `remove_entry_by_key`) with the admission limit, instead of the whole pool size. Cancels still skip shedding, but pooled cancels can no longer make ingress shed honest orders | `6398374` |

Why the admission fix was needed: in the round-2 spam cell (cap 20, 64 funded
keys sending cancel-alls at 2,000/s, C at 25%), matched/s fell to 5,814 and
orders landed in only 150 of 2,589 blocks. The spam cancels skipped
admission and filled the pool; the pool then counted as backlogged, so
`backlog_preverify` shed the load's orders (about 1.4M per node). C bounds
cancels per block, but it cannot place orders that never got into the pool.

Options considered for that failure, and why:

| Option | Decision |
|---|---|
| Admission counts only non-cancels | **Built.** Hits the measured cause, mempool only, no overlap with item 6 |
| Cancel quota in the pool (e.g. at most 25% of the pool) | **Deferred** (see section 6, f) |
| Stricter cancel allowance in B | Rejected: departs from Hyperliquid and hurts market makers, who cancel a lot |
| E, per-address block share | Deferred (section 6, d): reads trade history, overlaps item 6 Phase 3 |

### Merge with the crab stack (item 6 sync point 2, 2026-10-04)

`main` (this record, `92a02ed`) was merged into `perf/item6-phase1`
(`81a9567`: crab stack s87/s89, oracle signer, price feeder, liquidation,
item 6 C1) on `merge/item6-sync2`. The crab stack adds an oracle lane to the
native pool: `SubmitOraclePrices` sorts between cancels and everything else
(`PRIO_CANCEL` 0 < `PRIO_ORACLE` 1 < `PRIO_NORMAL` 2), only an Active
validator or its registered hot signer may pool one
(`Mempool::oracle_reporter`), at most `ORACLE_PENDING_PER_VALIDATOR` (4) per
validator, and at that cap a newer submission evicts the validator's oldest.

Where the oracle lane sits under A-D. Requirement: neither C nor B may crowd
oracle submissions out, and neither the admission backlog nor a full pool
may shed them.

| Path | Rule after the merge | Why |
|---|---|---|
| Block selection (C) | Cancels up to `ceil(limit * pct / 100)`, then oracle submissions, then normal entries, then leftover cancels. Oracle submissions do not count against the cancel share | `FIRST_NON_CANCEL` is `(PRIO_ORACLE, ..)`, so C's phase 2 starts with the oracle lane. The lane is bounded by the per-validator cap (4 x validators), so it cannot crowd orders out either |
| Priority-only pacing tier | Same three phases, phase 2 ending before the first normal key (`select_entries_before(.., Excluded(FIRST_NORMAL))`) | Was all cancels first, unbounded: cancel spam filling `limit` would have kept prices out of the deepest pacing tier. With no oracle submission pooled the result is identical to the old walk (work-conserving phase 3) |
| Full pool | Cancels and normal entries are refused (C). An oracle submission evicts the last normal entry, or, with none left, the last pooled cancel. Oracle submissions never evict each other | Crab's design already let oracle submissions evict normals. A pool full of cancel spam would otherwise refuse prices until it drained. The churn is bounded: an insert at the per-validator cap evicts the validator's own oldest first, so only a validator below 4 pooled can displace a cancel |
| RPC pre-verify screen | Full pool: only oracle submissions proceed to verify. Backlog: cancels and oracle submissions proceed | C sheds cancels at a full pool; crab's oracle bypass is kept |
| Admission backlog | `native_admission_backlogged` compares `NativePool::normal_size()` (entries that are neither cancels nor oracle submissions) with the limit | Oracle submissions bypass the screen themselves; counting them could only let oracle traffic shed orders. Their bound (4 x validators) means they cannot hide a real backlog |
| A and B | See "Oracle signers" below | |

Tests: `oracle_lane_follows_the_capped_cancel_prefix`,
`full_pool_of_cancels_oracle_evicts_the_last_cancel`,
`normal_size_tracks_inserts_and_every_removal` (`native_pool.rs`);
`oracle_selected_next_block_under_full_pool_of_cancel_spam` (pool of 40
cancels from 40 keys, full, C at 25%: the validator's submission is admitted
and is the 6th action of the next 20-slot block, on the normal and the pacing
path) and `pooled_oracle_submissions_do_not_count_as_admission_backlog`
(`lib.rs`). Crab's `full_pool_of_priority_entries_rejects_oracle` was
replaced by the cancel-eviction test above.

## 5. Bench results so far

ozarchy, 10 markets, rate 76000, `--retry-busy`, n=1 per cell, all cells
AGREE, liveness PASS, dissemination clean.

### Round-1 binary (120 s cells)

| Cell | cap 400 matched/s | cap 20 matched/s |
|---|---|---|
| main (without the branch) | 175,233 | 75,425 |
| branch, control (only C active) | 178,124 | 75,579 |
| branch, `ANTISPAM=1` | 179,218 (+0.6%, within noise) | not run |

No limit fired in these cells:

- **A** did not fire because the bench senders are funded.
- **D** did not fire because loopback is exempt.
- **B** did not fire. Each sender sent about 7k orders spread over 3 nodes,
  about 2.3k per node against a 10k buffer. This showed the per-node
  counting gap live, which round-2 item 2 closes.
- **C** never bound, because cancels were only 5% of the load.

### Round-2 binary (`062393d` before rebase, 120 s cells)

| Cell | matched/s | Note |
|---|---|---|
| cap 400, limits off | 175,048 | baseline |
| cap 400, `ANTISPAM=1` | 178,076 | A+B+D plus count-on-pool-entry cost nothing measurable |
| cap 400, `ANTISPAM=1`, `TORUS_ADDR_RATE_BUFFER=1000` | 163,121 | B throttles (-8.4%; about 60k `addr_rate_limited` per node) |
| cap 20, 64 **unfunded** spam keys, `ANTISPAM=1` | 75,406 | A refused all 239,908 spam actions as `unfunded`; throughput as without spam |
| cap 20, 64 **funded** spam keys, C at 25% | 5,814 (replicate 8,273) | C alone does not hold; led to the admission fix |
| cap 20, 64 funded spam keys, C at 100% | 142 | the original stall |

### Round-3 binary (`6398374`, 60 s cells)

| Cell | matched/s | Blocks with orders |
|---|---|---|
| cap 20, 64 funded spam keys, C at 25% | **72,690** (was 5,814) | 1,260 / 1,356 (was 150 / 2,589) |
| same, `ANTISPAM=1` | 73,409 | 1,280 / 1,355 |
| cap-20 controls without spam | ~75,400 | ~99% |

- The spam costs about 3.6% and slower blocks (59 vs 70 ms), because the
  spam cancels take their 25% of block slots, as designed.
- The native pool peaked at about 40k entries, never at its 65,536 maximum.
  The likely cap is the existing per-sender pool cap of 512 pending actions:
  64 keys x 512 = 32,768 cancels, plus the load's orders. (Inferred; the
  bench counts the spam's other refusals as `other` and does not log their
  text.)
- B did not fire against the spam: each spam key sent about 1.9k actions in
  60 s, under the 10k buffer.

### Round-3 binary, 256 funded spam keys (`6398374`, 120 s cells)

Cap 20, 256 funded keys sending cancel-alls at 2,000/s in total, C at 25%,
`--retry-busy`. Both cells AGREE, liveness PASS, dissemination clean. Runs
`ozarchy-as3-spam-funded256-cap20` and `ozarchy-as3-spam-funded256-B1000-cap20`
in `~/bench-results-matched/` (each with `.poll.txt` and `.sys/` holding
`sar` and `pidstat` output).

| Cell | matched/s (best 60 s) | Blocks with orders | Pool peak (max 65,536) | Spam sent / accepted / refused |
|---|---|---|---|---|
| 256 funded keys | **72,881** (76,156) | 2,532 / 3,039 (83%) | **65,536**, full from about 15 s to 75 s | 239,807 / 138,789 / 101,018 (all `busy`) |
| same, `ANTISPAM=1`, `TORUS_ADDR_RATE_BUFFER=1000` | **58,429** (72,033) | 2,399 / 2,935 (82%) | **65,536**, full from about 30 s to 120 s | 239,710 / 136,008 / 103,702 (all `busy`) |

- **The pool fills.** 256 x 512 per-sender slots exceed the 65,536 pool, so
  the pool sat full for about a minute. Each node refused about 12k items as
  `pool_full` and 250k-390k as `pool_full_preverify`, far more than the
  spam's 101k refusals, so most refusals hit the load's orders. Throughput
  held (72.9k, as with 64 keys) only because the bench retries refused
  orders; a client that does not retry loses them. Blocks with orders fell
  from 93% (64 keys) to 83%. This reopens deferred f.
- **B does not catch this spam.** Each spam key sent about 936 actions in
  120 s, under the 1,000 buffer, so `addr_rate_limited` refused none of
  them and `addr_cancel_rate_limited` stayed 0. B instead refused about
  27k actions per node from the load's high-volume senders, which cost
  20% of matched/s. Many keys each sending a little stay under any
  per-address limit; Hyperliquid's answer to that is the congestion share
  (deferred d), not B.
- **Host.** `sar`: 84-87% busy on 32 CPUs, iowait under 0.1% on average;
  the three nodes used about 5-7 cores each. Disk is not a factor. The
  per-thread `pidstat` capture is empty because it started before the
  nodes; the per-process numbers are valid.
- n=1 per cell; the 64-key cells were 60 s, so compare block shares, not
  totals.

Not yet measured:

- the same 256-key cell without `--retry-busy`, which would measure how
  many honest orders a pool full of cancels actually loses;
- B with a per-cancel or volume-scaled allowance against many small spam
  keys.

## 6. Deferred, and why

| # | Item | Why deferred | Owner |
|---|---|---|---|
| a | Durable committed counter (counts survive restart and idle eviction) | Belongs in item 6 Phase 3's block-consistent in-memory view. `remove_committed_native` (`lib.rs:1205`) receives only hashes, so exact senders would need app.rs dispatch changes next to item 6 | Coordinate with 18c |
| b | Enforcement at execution (HL-like; removes the last burst overshoot) | Consensus change plus a golden re-pin | 18c |
| c | HL in-block order (makers, then cancels, then takers). Torus runs post-only after takers and oracle after orders | Consensus change | 18c |
| d | E: congestion block-space share per address (2x previous-day maker share) | The answer to many funded keys that each stay under B (section 5, 256-key cells). Needs each address's previous-day maker volume on the proposer hot path: reading trade history from RocksDB there is too slow, and validators may run with `TORUS_TRADE_HISTORY=0`. Home: a rolling per-address maker-volume counter in item 6's per-trader in-memory state (Phases 3-4). Proposer-local, no consensus change. **Decided 2026-10-04: build after item 6, together with 18c** | After item 6, with 18c |
| e | Trading fees | Separate topic. Without fees, two funded addresses trading with each other can farm B's volume credit. **Must be revisited before the testnet deploy and again before mainnet** (decided 2026-10-04): rates, maker rebates, fee asset (TRS or a stable asset), where fees go; g's activation fee uses the same decisions | Before testnet, with 18c |
| f | Cancel quota in the pool (at most a share of the pool may be cancels) | The 256-key cells (section 5) showed the pool fills and refuses the load's orders as pool full. **Decided 2026-10-04: not built; wait for E**, which removes the cause (keys without maker volume get almost no block space) instead of capping cancels, and does not refuse honest cancels during an attack. Until E, more than ~128 funded spam keys can fill the pool. f would also conflict with the crab stack's oracle lane in `native_pool.rs` | Dropped in favour of E |
| g | Account activation fee (HL: 1 USDC per new account), replacing A's holding check with a one-time cost per key | Consensus change: executor charges the fee on an account's first action (or on the first transfer to a new address, as HL does), stores an activated flag, golden re-pin; A then checks the flag. Torus has no USDC: perp collateral is TRS, so a USD-priced fee needs a TRS/USD price or a stable asset. Fee destination is the same decision as e. Not harder after item 6 (one small per-account value; item 6 changes the per-trader state layout, so adding it after avoids moving it twice). Must land before mainnet: accounts that exist at the upgrade are marked activated, so keys created before it stay free | Before mainnet, with 18c, together with e |

## 7. Torus vs Hyperliquid after round 3

| Feature | Status | Note |
|---|---|---|
| Funded account | ⚠️ | A: must hold >= 1 TRS at ingress. A balance check, not a one-time fee. Validator duties only from validators |
| Open-order limit | ✅ | On main (`2567e56`, `ded6e6c`) |
| Per-address limit | ✅ | B: same formula, enforced at RPC ingress |
| Strict exhausted mode | ✅ | Round 2 item 1 |
| Cross-node counting | ⚠️ | Round 2 item 2: each node counts every pooled action. Approximate global count, node-local, in memory |
| Durability | ❌ | Counts reset on restart and on idle eviction (deferred a) |
| Cancels do not starve orders | ✅ | C (25% cancel share per block, no eviction of orders) plus the round-3 admission fix; measured under funded spam at cap 20 (72.7k vs ~75.4k without spam). Different mechanism from HL. Open beyond ~128 funded spam keys (deferred f) |
| Congestion share | ❌ | Deferred d |
| In-block order | ❌ | Torus order differs (deferred c) |
| IP limits | ✅ | D: 1200 weight/min, loopback exempt by default |
| Fees | ❌ | No trading fees (deferred e) |

## 8. Next steps

Run the branch worktree's own `tools/matched-bench/run-cell.sh` (the
integration repo's copy has no `SPAM_CANCEL_*` options). Arguments:
`run-cell.sh <worktree> <label> [MARKETS] [DUR] [RATE] ['EXTRA_ENV']`.
EXTRA_ENV is only read as the 6th positional argument; exporting it as an
environment variable has no effect.

1. Done 2026-10-04: **256 funded spam keys** and **B against funded spam**
   (section 5).
2. Optional: the 256-key cell without `--retry-busy`, to measure how many
   honest orders a full pool loses.
3. Done 2026-10-04: **review of D's custom accept loop** against
   jsonrpsee 0.26. Connection limit (a shared per-request guard, as
   upstream), shutdown, accept errors and nodelay behave as before. The
   config's keep-alive and nodelay options are not applied by
   `serve_with_graceful_shutdown`; unused today (keep-alive defaults off),
   now documented at the config. New test
   `ip_weight_limit_applies_over_websocket`: WebSocket calls are limited
   per IP (removing the peer-IP tag fails both IP tests). No runtime change.
   **Deploy note:** behind a reverse proxy on the same host every client
   looks like loopback, which is exempt by default, so D is off; set
   `TORUS_RPC_IP_EXEMPT=` (empty) and limit at the proxy.
4. **Merge with the crab stack** (`perf/s87-crab-fixes`): it changes the same
   files (`native_pool.rs` oracle lane, mempool `lib.rs`, `torus-rpc`
   `lib.rs` and `torus.rs`), and it adds oracle signer addresses
   (`SetOracleSigner`). After that merge, the validator-duty exemption must
   also accept a validator's registered oracle signer; crab has an
   active-validator-or-signer helper in `torus-mempool`.
5. Coordinate deferred items a-c, d (E, after item 6; ask for a per-address
   maker-volume counter in item 6's per-trader state) and g (activation fee,
   with e) with 18c.
