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

Not yet measured:

- B against funded spam (needs a longer cell or a small
  `TORUS_ADDR_RATE_BUFFER`);
- spam from more than ~128 funded keys (see deferred f).

## 6. Deferred, and why

| # | Item | Why deferred | Owner |
|---|---|---|---|
| a | Durable committed counter (counts survive restart and idle eviction) | Belongs in item 6 Phase 3's block-consistent in-memory view. `remove_committed_native` (`lib.rs:1205`) receives only hashes, so exact senders would need app.rs dispatch changes next to item 6 | Coordinate with 18c |
| b | Enforcement at execution (HL-like; removes the last burst overshoot) | Consensus change plus a golden re-pin | 18c |
| c | HL in-block order (makers, then cancels, then takers). Torus runs post-only after takers and oracle after orders | Consensus change | 18c |
| d | E: congestion block-space share per address (2x previous-day maker share) | Needs per-day maker volume (the trade-history table has maker/taker) and adds per-address work to the proposer selection hot path. Separate build and bench | Later |
| e | Trading fees | Separate topic. Without fees, two funded addresses trading with each other can farm B's volume credit | Separate |
| f | Cancel quota in the pool (at most a share of the pool may be cancels) | Not needed for the measured attack: with the admission fix, 64 funded keys could not fill the pool (peak about 40k of 65,536). With more than about 128 funded keys (128 x 512 per-sender cap = 65,536), cancels could fill the pool and new orders would be refused as pool full; A makes each key cost at least 1 TRS. It gives no speed-up in normal load (cancels are about 5%), only protection. Deferred because `native_pool.rs` is also changed heavily by the crab stack (oracle lane), so it would conflict at merge | Later; measure first with a 256-key cell |

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

Run from the integration repo with the branch worktree. Arguments:
`run-cell.sh <worktree> <label> [MARKETS] [DUR] [RATE] ['EXTRA_ENV']`.
EXTRA_ENV is only read as the 6th positional argument; exporting it as an
environment variable has no effect.

1. **256 funded spam keys** (does the pool fill and refuse orders as pool
   full? decides deferred f):
   `RETRY_BUSY=1 BLOCK_CAP=20 SPAM_CANCEL_KEYS=256 SPAM_CANCEL_RATE=2000 SPAM_CANCEL_FUNDED=1 tools/matched-bench/run-cell.sh /home/oz/projects/wt/antispam spam-funded-256 10 120 76000`.
   Watch `torus_mempool_native_size` and `pool_full` rejects.
2. **B against funded spam:** the funded spam cell with `ANTISPAM=1` and
   `'TORUS_ADDR_RATE_BUFFER=1000'` as EXTRA_ENV; expect
   `addr_cancel_rate_limited` on the spam keys.
3. **Review D's custom accept loop** (HTTP/2 keep-alive configuration no
   longer applied).
4. **Merge with the crab stack** (`perf/s87-crab-fixes`): it changes the same
   files (`native_pool.rs` oracle lane, mempool `lib.rs`, `torus-rpc`
   `lib.rs` and `torus.rs`), and it adds oracle signer addresses
   (`SetOracleSigner`). After that merge, the validator-duty exemption must
   also accept a validator's registered oracle signer; crab has an
   active-validator-or-signer helper in `torus-mempool`.
5. Coordinate deferred items a-c with 18c.
