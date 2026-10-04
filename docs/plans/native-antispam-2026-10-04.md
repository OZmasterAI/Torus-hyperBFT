# Native anti-spam: decisions, build, deferrals (2026-10-04)

Branch `feat/native-antispam` (worktree `/home/oz/projects/wt/antispam`, on main
`b529aee`). Node-local only: no change to block validity, `validate_block`,
`sort_native_actions`, the executor, the state hash or the app.rs block flow.

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
| A funded account | A non-exempt action enters the pool only if its sender holds >= N whole TRS (perp available + order margin, or EVM spot, or any open position). Validator duties, stake exits, claims and votes are exempt | `TORUS_INGRESS_MIN_COLLATERAL`, 1 / 0 | `crates/torus-mempool/src/funded.rs`; `Mempool::check_funded` (`lib.rs:760`), called on every path in `submit_native_action_inner` (`lib.rs:725`) |
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
| 1 | Strict exhausted mode (B) | Once over its allowance, an address may send only a weight-1 action (one order, one cancel, any other single action), at most one per 10 s since its last pooled action. A batch of n > 1 orders is refused (`addr_rate_limited`). Before this change, the slow mode let through one action of any weight, so a 400-order batch passed every 10 s. The cancel allowance is unchanged | `78461c8` |
| 2 | Count on pool entry (B) | Every native action that enters this node's pool is counted against its sender, whatever its source: RPC single, batch or bin; gossip; leader forwards; local submits. The count is the action's `order_count`. Enforcement stays at RPC ingress only. Gossiped and forwarded actions are counted but never refused, so nodes never disagree on what they hold because of B | `097bfe7` |
| 3 | Cancel-spam bench mode | `bench-throughput consensus --spam-cancel-keys K --spam-cancel-rate R [--spam-cancel-funded]`; `run-cell.sh` `SPAM_CANCEL_KEYS` / `SPAM_CANCEL_RATE` / `SPAM_CANCEL_FUNDED=1`. Default off | `8a2a799` |

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
  again (possible in principle, bounded by the 60 s nonce window). Validator
  self-submissions (state-hash attestations) are counted too; operators can
  list validator addresses in `TORUS_ADDR_RATE_EXEMPT`.

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

## 5. Bench results so far

ozarchy, 10 markets, 120 s, rate 76000, `--retry-busy`, n=1 per cell, all
cells AGREE. Round-1 binary.

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

Not yet measured:

- a cell where the limits throttle;
- the cancel-spam cells;
- the cost of count-on-pool-entry.

## 6. Deferred, and why

| # | Item | Why deferred | Owner |
|---|---|---|---|
| a | Durable committed counter (counts survive restart and idle eviction) | Belongs in item 6 Phase 3's block-consistent in-memory view. `remove_committed_native` (`lib.rs:1205`) receives only hashes, so exact senders would need app.rs dispatch changes next to item 6 | Coordinate with 18c |
| b | Enforcement at execution (HL-like; removes the last burst overshoot) | Consensus change plus a golden re-pin | 18c |
| c | HL in-block order (makers, then cancels, then takers). Torus runs post-only after takers and oracle after orders | Consensus change | 18c |
| d | E: congestion block-space share per address (2x previous-day maker share) | Needs per-day maker volume (the trade-history table has maker/taker) and adds per-address work to the proposer selection hot path. Separate build and bench | Later |
| e | Trading fees | Separate topic. Without fees, two funded addresses trading with each other can farm B's volume credit | Separate |

## 7. Torus vs Hyperliquid after round 2

| Feature | Status | Note |
|---|---|---|
| Funded account | ⚠️ | A: must hold >= 1 TRS at ingress. A balance check, not a one-time fee |
| Open-order limit | ✅ | On main (`2567e56`, `ded6e6c`) |
| Per-address limit | ✅ | B: same formula, enforced at RPC ingress |
| Strict exhausted mode | ✅ | Round 2 item 1 |
| Cross-node counting | ⚠️ | Round 2 item 2: each node counts every pooled action. Approximate global count, node-local, in memory |
| Durability | ❌ | Counts reset on restart and on idle eviction (deferred a) |
| Cancels do not starve orders | ✅ | C: 25% cancel share, cancels no longer evict orders. Different mechanism from HL |
| Congestion share | ❌ | Deferred d |
| In-block order | ❌ | Torus order differs (deferred c) |
| IP limits | ✅ | D: 1200 weight/min, loopback exempt by default |
| Fees | ❌ | No trading fees (deferred e) |

## 8. Next steps

Run from the integration repo with the branch worktree. Arguments:
`run-cell.sh <worktree> <label> [MARKETS] [DUR] [RATE] ['EXTRA_ENV']`.

1. **Throttling cell** (B fires; strict mode and cross-node counting are
   live):
   `ANTISPAM=1 RETRY_BUSY=1 tools/matched-bench/run-cell.sh /home/oz/projects/wt/antispam as-throttle 10 120 76000 'TORUS_ADDR_RATE_BUFFER=1000'`.
   Watch `torus_rpc_submit_admit_rejects_total{reason="addr_rate_limited"}`.
2. **Unfunded spam with A on** (every spam action should be refused as
   `unfunded`):
   `ANTISPAM=1 RETRY_BUSY=1 BLOCK_CAP=20 SPAM_CANCEL_KEYS=64 SPAM_CANCEL_RATE=2000 tools/matched-bench/run-cell.sh /home/oz/projects/wt/antispam spam-unfunded 10 120 76000`.
3. **Funded spam, C on vs C off** (limits otherwise off, so only C acts):
   - C on:
     `RETRY_BUSY=1 BLOCK_CAP=20 SPAM_CANCEL_KEYS=64 SPAM_CANCEL_RATE=2000 SPAM_CANCEL_FUNDED=1 tools/matched-bench/run-cell.sh /home/oz/projects/wt/antispam spam-funded-c25 10 120 76000`
   - C off: the same command with label `spam-funded-c100` and
     `'TORUS_CANCEL_BLOCK_SHARE_PCT=100'` as EXTRA_ENV.
4. **Cost of count-on-pool-entry:** cap 400, `ANTISPAM=1` on vs off, on the
   round-2 binary.
5. Coordinate deferred items a-c with 18c.
