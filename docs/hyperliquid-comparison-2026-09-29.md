# Torus-hyperBFT vs Hyperliquid — comparison and adoption plan (2026-09-29)

Session s513, `main` at `07a34c2`. Hyperliquid figures come from
`research/hyperliquid-deep-dive.md` (April 2026, mostly Hyperliquid's own claims).
Torus figures come from the code and the newest bench report,
`docs/perf/s76-ingress-and-cap-sweep-2026-09-29.md`.

Contents:
- Part 1: summary (as given in session)
- Part 2: side-by-side overview
- Appendix A: block time / latency findings
- Appendix B: order book ↔ EVM link findings
- Appendix C: networking findings
- Appendix D: Hyperliquid feature-parity audit

The appendices are agent research reports. Their file:line references were not
all re-checked; claims marked "suspected" or "untested" need confirming first.

---

## Part 1 — Summary

Getting to Hyperliquid's latency depends on two things: moving work off the consensus thread, and co-locating validators. Before copying more of Hyperliquid, though, there are correctness bugs to fix first. I didn't re-check every file:line the agents cited, and I've marked the claims that are suspected but untested.

### 1. Execution model and consensus

| | Hyperliquid | Torus |
|---|---|---|
| Commit rule | 2-chain, pipelined | 2-chain, pipelined. The lock bug from the July review is fixed. |
| What followers vote on | The whole block | Just the header; they fetch the body afterwards. This lets them vote in about 1.6 ms, but the next leader then has to wait for the body. |
| Execution | Runs as part of each block | Runs after commit on a separate thread. The header carries the parent's state root. |
| Order within a block | Order book first, then EVM | EVM first, then order book (`app.rs:1553` then `:1874`), the reverse of Hyperliquid |
| EVM blocks | Separate track: 1 s small blocks, 1 min large blocks | Same block as the order book. Any EVM tx forces that block onto the slow serial path (`app.rs:1499`). |
| Liveness extras | Standard view-change certificates | Adds recovery certificates, vote amplification and equivocation slashing, which Hyperliquid doesn't have |

The consensus protocols are close to equal, and Torus's is arguably more hardened. The real gap is in the execution pipeline.

### 2. Getting block time and latency down

The ~690 ms per block under load isn't network time: a message crosses loopback in about 6 ms. It is serial work on one consensus thread:

| Step, one block | ms |
|---|---|
| Leader builds the block | 139–197 |
| Wait for the parent's body, then insert it into the block tree | ~115 |
| Commit feed, then header broadcast | 65–124 |
| Votes gathered into a quorum certificate (53–59 ms of this is queue wait) | 80–84 |
| Certificate → next view starts | ~160 |

Execution then trails commit by 11–16 blocks, roughly 8–11 s. That's inferred; **end-to-end order latency has never been measured.**

Past experiments show that cutting one step mostly moves the wait somewhere else. For example, s68 cut block building by 56 ms but the view only got 8 ms faster.

**Suggested order:**
1. **Measure it first.** Add an order-latency histogram covering submit, inclusion, execution and when the fill is visible over RPC. Without it we can't tell whether we're closing the gap.
2. **Move the rig onto separate hosts.** The test box runs at load 36–40 on 18 vCPUs, and the consensus thread waits for a core about as long as it runs. Every number above is inflated by that.
3. **Try the levers nobody has benchmarked yet:**
   - Send the commit feed after the header broadcast. The s64 prediction was 660 ms → 480–570 ms.
   - Turn on `PIPELINED_WRITE`: −9% view time, but only enabled in the bench.
   - Batch the propose writes into one write: about −20–25 ms.
   - Have followers request the body before voting: about −50 ms.
   - Benchmark `TORUS_ASYNC_VALIDATE`: implemented, never tested.
   - Build the next block speculatively.
4. **Stop execution trailing commit by 10+ blocks.** This needs the parallel engine and moving the fixed per-block costs off the execution thread. Users experience latency when a fill is visible, not when a block commits.
5. **Accept that the last step to 70 ms is geography.** Hyperliquid's number comes from validators co-located in Tokyo with a 200 ms round-trip target. The testnet legs are 227–502 ms, so no code change gets a cross-continent chain to 70 ms.

### 3. Would a Hyperliquid-style order book ↔ EVM link help?

Partly. We already have most of it: read precompiles at `0x0800–0803` that see the previous block's state, and a CoreWriter at `0x0810` that queues writes for the next block.

**Worth adopting:**
- **Separate EVM blocks.** This is the one with real latency value. Today a single EVM tx forces the whole order-book block onto the slow path. A separate EVM cadence, like Hyperliquid's, keeps EVM off the trading hot path.
- **Order book first, then EVM.** EVM contracts would then react to fills from the same block.
- **A time-based CoreWriter delay** instead of one block, if front-running protection matters. At sub-second blocks, one block barely delays anything.

**Fix before any of that:**
- **Lockbox units (known).** The wallet treats amounts as 18 decimals but the lockbox treats them as 8, a 10^10 mismatch (astra round-1 audit).
- **Lockbox overwrite (suspected, untested).** A lockbox transfer inside an EVM tx may get overwritten by revm's cached account state, which could credit the order book without debiting the EVM balance. The audit flagged this as EVM-PF-05, and no test runs the lockbox from EVM bytecode.
- **Discarded CoreWriter errors.** The result of processing the CoreWriter queue is thrown away with `let _ =` (`app.rs:1899`).

### 4. Networking

Yes, Hyperliquid's approach would help, mostly over a WAN:
- **Consensus messages go over GossipSub by default.** A gossip relay can double the hop, and consensus shares a queue with large action batches, which has caused stalls before. Direct sends to validators already exist behind `TORUS_CONSENSUS_DIRECT_FAN=1`, off by default. Benchmarking and enabling it is the cheapest win here.
- **Block bodies are pulled after the header**, costing at least one extra round trip per view. Pushing small bodies together with the header (Hyperliquid style) removes that. A July test gained 7% at idle and was neutral under load; it's worth re-testing on a WAN.
- **No sentries.** Validators are fully public on QUIC and advertised via Kademlia. Sentries and a validator-only mesh are mainly DoS protection, and they matter before mainnet.
- **A separate connection or priority lane for consensus traffic**, so votes and headers never queue behind bulk data.

### 5. What else to adopt from Hyperliquid

From the feature audit, in priority order.

**Bugs, before any new features:**
1. **Market orders reserve no margin, and nothing checks it at fill** (`native_executor.rs:3779`).
2. **`reduce_only` is accepted but never enforced**, which breaks stop-loss and close-position flows.
3. **Undelegated stake is never released**: `process_unbonding` is only called from tests.
4. **Market listing is stubbed**: `ListMarket` does nothing, and governance listings always write market 0.

**Missing perp-DEX essentials:**
5. **Funding engine** (premium index, hourly payments). Without it, perp prices aren't tied to the index.
6. **Maker/taker trading fees.** Native trading currently earns nothing, so the fee-split economics have no trading income.
7. **Mark price separate from the oracle, plus open-interest caps and price bands.** These are the defences Hyperliquid added after the JELLY attack.
8. **WebSocket streams for the order book, trades and user fills.** Market makers won't poll.
9. **Dead-man switch, cancel-by-client-id, TP/SL, and an isolated-margin/leverage action.**

**Operations:**
10. **A coordinated upgrade mechanism** (halt height plus signed binaries, like Hyperliquid's visor).
11. **Periodic state snapshots.** The snapshot code exists but only tests use it.

Torus already matches Hyperliquid in several places: cancels are ordered first by consensus rule, the oracle uses a stake-weighted median of validator submissions, margin tiers work, and liquidation includes ADL and socialized loss.

---

## Part 2 — Side-by-side overview

| | **Hyperliquid** | **Torus-hyperBFT** |
|---|---|---|
| **Status** | Mainnet since Nov 2024; about $4B/day volume (older note, not re-checked) | Testnet, chain ID 7778 |
| **Source** | Closed. Signed binaries only, can't be forked. | Open, Apache-2.0 |
| **Consensus** | HyperBFT: HotStuff-derived, 2-chain commit, pipelined, no fixed block timer | Fork of `hotstuff_rs` with Monad-style extensions: 2-chain commit, votes on the block header before the body arrives, recovery certificates, equivocation detection |
| **Execution model** | HyperCore runs first, then the EVM block | Blocks are committed first and executed afterwards on a dedicated thread. Consensus agrees on order only; state agreement relies on every node replaying deterministically. |
| **Validators** | 21, permissionless, delegated stake, jailing, no slashing yet | 3 on testnet (the base genesis lists 4). Has a `slashing.rs` module. |
| **Throughput** | About 200k orders/s, self-reported, counting cancels and edits | About 65–70k matched orders/s on average, 110k at best over 60 s. Measured with 3 validators and the bench on one 18-vCPU machine, and that machine is the limit. |
| **Latency / block time** | About 70 ms median per block, 0.2 s median end-to-end | README says "sub-100ms" at idle. Under full load, 600–900 ms per block (400-order block cap). |
| **Order book** | Perps and spot, price-time priority, on-chain | Order book with margin, oracle, liquidation, auto-deleveraging and socialized loss (`crates/torus-core`) |
| **Funding** | Yes | Only a `max_funding_rate_bps` parameter; no funding engine |
| **Vaults (HLP / user vaults)** | Yes | None |
| **EVM** | HyperEVM, Cancun without blobs. Separate fast (1 s) and large (1 min) blocks. | revm, Cancun. No separate block tracks. |
| **Order book ↔ EVM link** | Read precompiles, CoreWriter, lockbox transfers | Cross-VM precompiles and a lockbox (`precompiles.rs`, `lockbox.rs`) |
| **Networking** | Custom gossip on ports 4000–4010, sentry nodes | libp2p over QUIC; large blocks are pushed whole below 8 MB and fetched by hash above that |
| **Validator hardware** | 32 vCPU, 128 GB RAM | Not specified yet |

Notes:
- The throughput numbers can't be compared directly. Hyperliquid's 200k is self-reported over a real network and counts cancels. Torus's 65–70k was measured on one machine over loopback.
- The July review's 2-chain lock bug (C1) was fixed in `4a94e8b` and hardened in `2f196f9`.
- `research/hyperliquid-deep-dive.md` §9.4 (recommend forking Sei v2 / CometBFT) is stale: the project built its own HotStuff chain instead.

---

## Appendix A — Block time and latency

**1. Block time as measured**
- **Under load, current defaults (cap 400):** 747–896 ms/block. At cap 200 it was 579–737 ms. Source: `docs/perf/s76-ingress-and-cap-sweep-2026-09-29.md` §2–3. The s75 unpinned cells ran 1.45 blk/s (~690 ms).
- **Idle:** no recent cell measures this cleanly. Older healthy idle probes read 26–29 blk/s, about 35–38 ms/view (`docs/l3-diagnosis.md:70-71`, `block-latency-campaign` §1, where probes span 12–28 blk/s). Other docs quote "~65 ms" (`design-exec-pipeline:12`) and a "~102 ms local floor" (`Torus-hyperBFT-review.md:140`). Idle is bound by loopback transport hops.

**2. One loaded view, cap 200 (s70 view join, ~692 ms mean cycle)**

| Segment | ms | Critical path? |
|---|---|---|
| Leader StartView → Propose | 340–347 | yes |
| ↳ produce_block (select 79–91, mirror 44–101, attest ~9, encode ~1) | 139–197 | yes |
| ↳ parent-body wait + block-tree write | ~115 | yes |
| ↳ finalize (commit feed, then header broadcast) | 65–124 | yes |
| Propose → last vote sent (header transit p50 ~6) | 102–114 | yes |
| Last vote → PC (53–59 of it is algo-queue wait) | 80–84 | yes |
| PC → next StartView (commit feed 73–82, AdvanceView loopback 78–84) | 158–160 | yes |

On the next leader, receiving the parent body and fully inserting it takes 170–184 ms, against a vote gather of about 105 ms. In views where the body arrives late (33–48% of views), the p50s are:
- validate: 25–26 ms after flush-on-miss, down from 40
- tree insert: 5–8 ms
- commit feed: 86–103 ms
- receive → produce_block: 133–157 ms

Sources: s70, s72 §1, s76-validate-timers, s76-flush-on-miss.

**Off the critical path** (commit-then-execute, `app.rs:4750-4753`): the engine, save_books, flush/state-root and persist all run later on the single exec thread. Per native block that costs 645–722 ms (s75 `chain_ms`), about the same as the block interval. Exec only touches consensus through back-pressure: `on_committed_block` does a blocking send into a 64-slot channel (`app.rs:3335`; `TORUS_EXEC_NONBLOCKING_DISPATCH` is off by default, `app.rs:339-360`).

**Code check:**
- There is no minimum block interval, propose delay or batch-fill wait. A leader proposes on view entry as soon as `highest_pc.block` is in its tree. Otherwise it defers and retries on a 10 ms poll (`hotstuff/implementation.rs:560-640`, `algorithm.rs:310-322`).
- The header carries the parent's state root (`app.rs:4684`), so nothing waits on execution.
- `TORUS_PROPOSER_EXEC_WATERMARK` is off by default (`implementation.rs:454-480`).
- `timeout_base_ms` is 1200 (`torus-node/src/main.rs:315,800`) and only fires on timeouts.
- The cap is `NATIVE_TOTAL_BLOCK_CAP = 400` (`torus-mempool/src/rate_limit.rs:105`).

**3. What binds block time**
- **Not network RTT on this rig.** Loopback transit is about 6 ms for the header and 10 ms for the body.
- **It is serial work on the consensus thread of a CPU-saturated host**: leader build, then the next leader's parent-body chain (validate plus commit feed), then queueing on the algo thread.
- **Cutting one piece mostly moves the wait.** s68 cut block_build by 56 ms but the view moved only 8 ms. s70's local advance saved 45–70 ms, which reappeared as +55 ms of parent-body wait. `docs/l3-work-budget.md` §0 calls this the work-conservation law.
- **Exec back-pressure bites sometimes.** In s72 the exec queue sat at p50 11–16 and p90 49–61 blocks, hitting its bound in 0–11% of samples. One stall held a leader for 1.35 s when the queue was full (s72 §4).
- **Disk latency from other tenants** causes minute-aligned dips of about −26% views/s (s73).
- **Over a WAN, RTT would dominate:** 227–502 ms legs (review:140).

**4. End-to-end order latency: not measured anywhere.** The "age-at-exec histogram" is proposed but unbuilt (`docs/mission-s470-status.md:195`), and no latency metric exists in `tools/matched-bench` or the crates. Indirect signals (inferences):
- Commit needs about 2 views (~1.3–1.8 s).
- Execution then trails commit by the exec queue, p50 11–16 blocks (roughly 8–11 s at ~700 ms/block, inferred).
- At cap 200, 2–9% of actions expired after the 60 s nonce window, so the inclusion tail reached 60 s.

**5. Levers and A/B outcomes**

| Lever | Result | Status |
|---|---|---|
| Timeout base 500 → 1200 | dead views −85%, +7% matched/s, block rate flat, views longer | adopted |
| DA-skip mirror (`cc81630`) | block_build −30%, throughput flat | merged |
| Collector local advance | saving moved into the parent-body wait, lower throughput | not adopted |
| Fix D, defer parent feed | failed the dissemination gate | not adopted |
| Serve thread (`f9b013e`) | neutral | on by default |
| Flush-on-miss (`bce54d0`) | validate −15 ms p50 | merged |
| Cap 400 | +14% total matched, block time about +20% | adopted |
| `PIPELINED_WRITE` (s46) | −9% view time | bench env only; node default still off |
| DA batch read (s46) | −2.5% view time | merged |
| Body push (July) | +7% at idle, neutral under load | not adopted |
| CPU pinning (s46, s75) | negative, −5.7% | not adopted |

Proposed but not yet tried:
- Commit feed after the header broadcast (s64 #1, predicted view 660 → 480–570 ms)
- Single-batch propose writes (−20–25 ms, never ported)
- Speculative pre-build (never implemented)
- `TORUS_ASYNC_VALIDATE` (implemented, never benchmarked on)
- Follower sending its body request before its vote (s65, ~50 ms)
- Pacemaker deadline rebase (s65)

**6. The single-box rig inflates block time.** Three nodes at about 4.5 cores each plus the bench on 18 vCPUs gives load1 36–40, and the host is about 95% busy. The algo thread waits for a core about as long as it runs (s73 §6). In s46 that wait was 19–22 ms per block against 37 ms on-CPU. s76 concludes the rig is now the ceiling, and measuring the chain itself needs separate hosts.

**For the ~70 ms target:** idle loopback is already near it, but the loaded path is 8–12× over. The work to cut is the next-leader parent-body chain and the commit feed. Getting under the "~0.8–0.9 s realistic floor" for a cap-200 block (`design-exec-pipeline:15`) needs thinner blocks with the fixed per-block costs off the exec chain, plus a parallel engine. Visible latency also needs execution to stop trailing commit by 10+ blocks.

---

## Appendix B — Order book ↔ EVM link

Torus is close to Hyperliquid in shape (EVM contracts see core state one block old, and core writes are queued) but differs in two ways: the EVM runs first each block, not core; and the write delay is one block, not a timed delay.

**1. Block execution order**
- The proposer does not execute anything in production. Execution happens after finalization, on the exec thread in `crates/torus-consensus/src/app.rs`.
- The order per block is:
  1. The EVM block runs first (`app.rs:1553-1604`, through `validate_block_for_catchup` against `self.state_db`). The loop is serial (`crates/torus-evm/src/executor.rs:275-330`).
  2. Native actions run next. `overlay.seed_from_bundle(&bundle)` (`app.rs:1780`), then `execute_batch(pre_evm)` and `execute_batch(post_evm)` (`app.rs:1874-1875`).
  3. Then `drain_core_writer`, governance, fees and the epoch step (`app.rs:1899-1902`).
- The "pre_evm/post_evm" split (`crates/torus-bridge/src/native_executor.rs:6176`) is misleading. Both batches run after the EVM. The ordering in the docstring at `proposer.rs:170-178` is not what the live path does.
- Native and EVM work share one block and are not interleaved. EVM always runs first, the reverse of Hyperliquid.

**2. EVM reads of core state**
- Read precompiles (`crates/torus-core/src/precompiles.rs:3-10,30-36`):
  - `0x…0800` OrderBookReader: book, position, open orders
  - `0x…0801` Balance / markets
  - `0x…0802` Oracle, with a stale flag if older than 100 blocks (`oracle.rs:21`, `precompiles.rs:601`)
  - `0x…0803` Staking
- Each read costs 2,600 gas (`:40`).
- Staleness: a block containing EVM txs is forced onto the serial path (`app.rs:1499-1503`, `!has_evm`), so the previous block's native writes are durable first. EVM reads native state as of the end of block N-1, plus writes made by earlier txs in the same block. That matches Hyperliquid's "one core block old".

**3. EVM writes to core**
- CoreWriter `0x0810` handles place, cancel and cancelAll. CoreWriterStaking `0x0811` handles delegate, undelegate, claim and lock. Both cost 20k gas.
- Each call only enqueues into `CF_CORE_WRITER_QUEUE` with target block = current + 1 (`precompiles.rs:999-1011`).
- The queue is drained at the tail of block N+1, after that block's own native actions (`native_executor.rs:5830-5860`). A guard rejects anything queued in the current or a future block (`:5839`).
- The delay is one block, not a wall-clock delay like Hyperliquid's.
- Queued writes are journaled per tx, and a revert discards them (`executor.rs:36-67,318-327`).
- The order ID returned to the contract is synthetic, `(block+1)<<64 | seq` (`precompiles.rs:837`). It probably does not match the ID the native engine assigns later (not verified).

**4. Asset movement (lockbox)**
- Lockbox `0x0820` exposes `depositToNative` and `withdrawFromNative` (`precompiles.rs:958-985`). Both run immediately and synchronously inside the tx (`lockbox.rs:3-4`).
- No wrapped token. It debits the EVM account's native-coin balance in `CF_ACCOUNTS` and credits `CF_NATIVE_BALANCES` in one `atomic_write` (`lockbox.rs:31-126`).
- The tests call `Lockbox::*` or `execute_precompile` directly and never go through a revm tx (`lockbox_e2e.rs:43,85`; `cross_vm_read.rs:55`). No test calls `depositToNative` from EVM bytecode.

**5. Effect on parallel matching and block time**
- EVM and matching share no lock, but they run one after the other on the same thread. EVM time adds directly to block time.
- Any block with an EVM tx loses the pipelined fast path and becomes a barrier (`app.rs:1499`; `docs/perf/design-exec-pipeline-2026-08-20.md:254,326`).
- Parallel per-market matching (`market_workers.rs`) only happens in the native phase. EVM execution is serial, and Block-STM is blocked by the writer precompiles (`docs/Torus-hyperBFT-review.md:110-111,171`).
- The only cost figure is an estimate: `exec_evm_seconds` ≈2–4 ms with few txs (`docs/l3-work-budget.md:80`). Native engine phase ~16 ms measured, flush ~28 ms measured.

**6. Gaps vs Hyperliquid, and security**
- **Order:** EVM runs before core, not after.
- **Delay:** one block, not seconds. MEV protection only comes from the next-block drain.
- **Lockbox is synchronous**, with no delay.
- **Lockbox may clobber EVM balances or mint funds (suspected, untested).** Journaled lockbox writes land in `CF_ACCOUNTS` through `commit_tx(state_db)` mid-block (`executor.rs:320`). revm's `State` has already cached the sender's account, so committing the bundle afterwards could overwrite the lockbox debit — on deposit, native funds credited while the EVM balance is restored. Audit EVM-PF-05, "Concurrent writes to same account can overwrite each other's changes" (Medium; `research/audit-3.4.3-evm-correctness.md:131-136`). No fix and no EVM-level test found. Needs a revm tx test to confirm.
- **Units:** the wallet parses 18 decimals but the lockbox treats the value as 8 decimals, a 10^10 mismatch (`docs/audits/astra-round1-2026-09-24.md:26-30`).
- **Reverts:** writer precompiles used to bypass the revm journal (review C5); fixed by T4.4, the per-call-frame checkpoint journal.
- **Older fixed findings:** zero-gas and unregistered precompiles (EVM-PF-10), non-atomic lockbox (ECON-PF-04), u128→i128 overflow (EVM-PF-06/ECON-FIND-26), swallowed drain errors (EVM-FIND-12). In the live path the drain result is still discarded with `let _ =` (`app.rs:1899`).
- **Performance:** `next_sequence` scans a prefix on every enqueue, O(N²) per block (EVM-PF-16).
- **Reads:** `getMarkets` reads the "active" flag from the wrong byte (`docs/audits/astra-round2-2026-09-24.md:37`).
- **Determinism:** the native sort keys on (category, sender, keccak(canonical_bytes)) (`native_executor.rs:6201`), so it is deterministic. The proposer's `build_block_with_native` would write precompile side effects to the DB at proposal time, but production proposals don't call it.

---

## Appendix C — Networking

Torus's networking is stock libp2p. Consensus broadcasts go over GossipSub by default and point-to-point traffic uses libp2p request/response. There is no sentry or validator-only-mesh concept.

The stack is QUIC on UDP 30333, GossipSub with strict signing and a 100 ms heartbeat, Kademlia in server mode, identify, and several request/response protocols (`/torus/direct`, block-data, native-da, native-da-shards, sync), all in `crates/torus-network/src/behaviour.rs:13-47`. Gossip topics: `/torus/consensus/1.0`, `/torus/transactions/1.0`, `/torus/native-actions/1.0`. `crates/torus-consensus/src/network.rs` is only an in-process test mesh.

**1. Message paths**
- **Broadcasts** (`bridge.rs:665`, `swarm.rs:2191-2240`): proposal headers (`hotstuff_rs/.../implementation.rs:979`), AdvanceView (`:1760`), timeout votes and other pacemaker messages (`pacemaker/implementation.rs:140,165,204,327,407`), block-sync advertisements. They go to **GossipSub by default**.
  - `TORUS_CONSENSUS_DIRECT_FAN=1` sends them over `/torus/direct` to each validator instead (`config.rs:52,98`). **Off by default**, and gossip keeps running alongside unless `TORUS_CONSENSUS_GOSSIP_MIRROR=0`.
- **Point-to-point** (`/torus/direct`, 10 s timeout, `behaviour.rs:145`): phase votes to the next leader (`implementation.rs:1458,1614,2379`), NewView (`:659-664`), block-data requests and responses (`bridge.rs:684`). The dedicated block-data protocol "silently fails in devnet", so it isn't used.
- **Block bodies:**
  - The consensus-layer push of the block to followers is off by default (`TORUS_BODY_PUSH_MAX_BYTES=0`, hard cap 64 KiB, `implementation.rs:3410-3425`). Followers pull the body after they see the header; the stuck-leader header re-send defaults to 3 s.
  - A separate pre-proposal thread (`torus-node/src/main.rs:710-720`) pushes native-action bodies directly to validators and RPC nodes (`swarm.rs:2486`).
  - Batches above **512 KB** go out as a hash manifest that peers pull (`bridge.rs:97`). The 512 KB default can be raised by env up to the 8 MB direct-push limit (`caps.rs:76`); 8 MB is a ceiling, not the default.
  - Misses fall back to a native-da pull or an erasure-shard gather by DaRecoveryWorker (`consensus/app.rs:2820`).
- **Transactions:**
  - RPC-admitted native actions are batched straight to the current leader every 25 ms (`torus-node/src/forward_batcher.rs:1-24`), and also batch-gossiped on the native-actions topic (`swarm.rs:1194-1240`).
  - EVM transactions go on the transactions topic and are also sent to the leader.

**2. Measured costs**
- Header to next leader ~6 ms p50 on local devnet (`docs/perf/s72-parent-body-2026-09-28.md` §1). Body request goes out 31-40 ms p50 after the header; body transfer ~10 ms.
- When the body arrives late, receive → produce next block is 158-210 ms p50, dominated by validate and commit.
- A follower votes ~1.6 ms after the header (`docs/l3-work-budget.md:63`).
- Header-to-body p99 ~0.9-1.3 s, against a 1 s fetch retry budget (`s63-builds-and-body-fetch-2026-09-23.md`).
- Empty-block local floor 102 ms. Chain cadence ~1 RTT per block.
- Geographic testnet inter-validator legs 227-502 ms (`docs/Torus-hyperBFT-roadmap.md:29,38`; `docs/reports/sprint-s395-report.md:11`).
- Testnet hosts: seed 95.111.231.121, val1 84.32.108.220, 18c 13.140.140.138 (Contabo VPS). No RTT matrix in `testnet/`.

**3. Sentries and validator shielding**
- Validators are directly public: listen on `0.0.0.0` over QUIC, advertise via Kademlia server mode and identify; the default bootstrap peer is itself a validator (`config.rs:198`). Non-validators join with `--p2p-peers` or that default list (`main.rs:581`).
- Shielding is per-peer only:
  - Connection limits: 100 peers max, 2 connections per peer (`behaviour.rs:216-220`).
  - Rate limits: 50 consensus messages/s per signing author, 100 transactions/s per peer (`config.rs:218-220`).
  - Peer scoring and a ban list; direct-path senders must match a registered validator key (`swarm.rs:2845-2900`).
  - Size caps: 2 MiB gossip, 8 MB direct (`caps.rs`).
  - Private addresses filtered out of Kademlia.
  - Compressed gossip removed as a DoS vector (`behaviour.rs:16`).

**4. Known bottlenecks and bugs**
- A 1-3 KB proposal could sit behind queued 128 KB gossip batches, and libp2p silently drops publishes after 5 s — caused a 21.5 → 1.15 blocks/s collapse; queue cut to 512 (`behaviour.rs:55-64`).
- Heartbeat hardcoded at 500 ms instead of 100 ms until s391 (`behaviour.rs:47-52`).
- Body fetches exhausted retries because the serving node's consensus thread blocked 1-4 s, and direct messages arrived 0.5-3.4 s late in bursts. s63 fix: 4 s wall-clock budget.
- The leader stops serving body requests while execution is backed up (s72 fix D). Serving from a separate thread helps.
- Full-body pushes stalled views at batch size ≈500, hence the manifest path.
- 6 MB pushes rejected by older nodes' 4 MB limit (`bridge.rs:108-112`).
- UDP receive-buffer drops ~70/s, not correlated with the slow deliveries.

**5. Latency costs vs a point-to-point validator protocol**
- Headers, AdvanceView and timeout votes default to GossipSub. A relay can add a hop (2×RTT), and they share the per-peer queue with bulk batches (`docs/l3-arrival-attribution.md:85-89`).
- Bodies are pulled after the header: at least one extra RTT per view. Solicited fetch retries every 100 ms and can fall back to block sync.
- Libp2p framing, signing and Kademlia churn all run over one QUIC connection carrying consensus and bulk traffic together.
- No fixed validator ports, no sentry layer, no co-location. Testnet legs are 2-7× Hyperliquid's 200 ms two-way target.

---

## Appendix D — Hyperliquid feature-parity audit

Torus has the core pieces of a perp DEX: a matching engine with the basic order types, consensus-enforced cancel-first block ordering, a stake-weighted-median validator oracle, margin tiers and liquidation with ADL. Many other items are declared types that do nothing, or code production never calls. No funding, no trading fees, no reduce-only enforcement, no working market listing, no upgrade mechanism, and market orders skip the margin check.

`executor.rs` below means `crates/torus-bridge/src/native_executor.rs`.

| # | Item | Status | Evidence |
|---|---|---|---|
| 1 | Order types / TIF | PARTIAL | Limit, Market, StopMarket, StopLimit (`crates/torus-types/src/lib.rs:1021`). TIF GTC/IOC/FOK/PostOnly (`lib.rs:1034`). Stops trigger at `crates/torus-core/src/order_book.rs:422-485, 1314`. No take-profit, scale or TWAP (TWAP only a PRD idea, `research/PRD-trading-app.md:651`). `reduce_only` stored and serialized but never enforced (grep of `.reduce_only` found no check). |
| 2 | Cancel priority | PRESENT | Mempool drains cancels first (`crates/torus-mempool/src/native_pool.rs:220, 259`), never evicts cancels when full (`:207`). Validators enforce it: `classify_action` (`executor.rs:6136`) orders cancel → IOC/FOK/market → EVM → GTC, called via `sort_native_actions` at `crates/torus-consensus/src/app.rs:1791`. In-group order deterministic (priority, sender, nonce). EVM txs get a shuffle key per parent hash (`crates/torus-mempool/src/evm_pool.rs:77`). |
| 3 | Margin | PARTIAL | Tiers work (`crates/torus-core/src/margin.rs:24-56`, used in `executor.rs:4813`). `MarginType::Isolated` exists (`crates/torus-core/src/position.rs:45`) but no action to choose isolated mode or set leverage. Margin reserved at submission (`executor.rs:3777`). `check_margin_at_match` (`margin.rs:151`) has no callers. **Market orders reserve zero margin** (`executor.rs:3779, 4915`); no fill-time check found. |
| 4 | Funding | ABSENT | Only `max_funding_rate_bps` (`lib.rs:1110`). Grep for `funding\|premium` found no engine. PRD confirms not implemented (`research/PRD-trading-app.md:703`). |
| 5 | Oracle / mark | PARTIAL | Only active validators submit (`executor.rs:5651`). Stake-weighted median with outlier rejection and 10% cap for a single reporter (`crates/torus-core/src/oracle.rs:200-270`). No feeder daemon in repo (grep binance/coinbase/okx/price_feeder: nothing). Mark = oracle price (`crates/torus-rpc/src/torus.rs:1638`). |
| 6 | Risk controls | ABSENT/STUB | No OI caps, price bands or min notional found. `ListMarket`, `DelistMarket`, `UpdateMarketParams` are stubs returning ok (`executor.rs:3366-3370`). Governance ListMarket always writes `market_id: 0` (`executor.rs:5705`); Delist/UpdateParams proposals are text-only (`:5714`). No settle-at-price action. |
| 7 | Backstop | PARTIAL | Liquidation force-closes at oracle price, 2.5% penalty to an insurance fund (`crates/torus-core/src/liquidation.rs:59-62, 209`). ADL ranks by unrealized PnL only; HL also weights by leverage (`liquidation.rs:255-280`). Socialized loss at `:345`. No HLP-style liquidator vault. |
| 8 | Accounts / ops | PARTIAL | Session keys as agent wallets, Trading/TransfersOnly/Full scopes, 24h max expiry (`lib.rs:193, 611`; `crates/torus-types/src/eip712.rs:735`). Batch place up to 1024 orders (`lib.rs:536`), plus CancelAllOrders. No batch of explicit cancels, no cancel-by-cloid (cloid stored and shown over RPC only), no dead-man switch, no vaults, no sub-accounts (PRD only, `research/PRD-trading-app.md:628`), no `expiresAfter` (60 s nonce window is the only implicit expiry). |
| 9 | Spot / HIP | ABSENT | TransferToPerp/TransferToSpot only move balance between EVM and native (`executor.rs:3330`). No spot order book, no token standard, no permissionless deployment. |
| 10 | Fees / nonce | PARTIAL | Native actions pay no gas (`gas_used` is bookkeeping). **No trading fees**: `total_native_fees` never incremented, so `distribute_fees` (`executor.rs:5951`) only distributes EVM fees. Nonces are ms timestamps within ±60 s (`eip712.rs:27, 853`); consumed (sender, nonce) pairs stored in `CF_NATIVE_NONCES` (`crates/torus-bridge/src/validator.rs:379`), no pruning found. Roughly HL's time window, not its bounded 100-highest-nonce set. |
| 11 | Validator set | PARTIAL | Epochs, top-N-by-stake, power = stake (`crates/torus-economics/src/epoch.rs:35-70`, `app.rs:463, 4032`). Delegation, jail votes, unjail, key rotation, commission (`crates/torus-economics/src/staking.rs`). Leader equivocation slashed 5% and tombstoned (`app.rs:5039`). Gaps: `process_unbonding` only called from tests, so undelegated funds are never released. `DowntimeTracker` and `DoubleSignDetector` (`crates/torus-consensus/src/slashing.rs`) exported but unused. |
| 12 | Data outputs | PARTIAL | Only Ethereum-style `newHeads` and `logs` subscriptions (`crates/torus-rpc/src/eth.rs:1082-1101`). Everything else is polling: getOrderBook, getBlockTrades, getUserTrades, getOpenOrders (`torus.rs:93-206`). No l2Book/trades/userFills streams. `--rpc-only` non-validator node (`crates/torus-node/src/main.rs:159`). Manual `--restore-from-snapshot` (`main.rs:118`); periodic `SnapshotManager` (`crates/torus-state/src/snapshot.rs:223`) only used in tests. |
| 13 | Upgrades | ABSENT | No visor/halt_height/upgrade plan. Planned only (`research/implementation-plan.md:335`); roadmap describes manual fleet upgrades. |

**Most valuable ABSENT/PARTIAL items for a perp DEX:**
1. **Funding engine (premium index, hourly payments).** Without it, perp prices aren't tied to the index.
2. **Margin check on market orders.** Market orders open positions with no margin reserved or checked; only liquidation catches them.
3. **Reduce-only enforcement.** Flag accepted and signed but ignored; breaks stop-loss and close-position flows and misleads clients.
4. **Trading fees (maker/taker).** Native trading earns nothing, so fee-split, burn and treasury get no trading income.
5. **Working market listing and delisting (with settle price) and per-market params.** Stubs block any new market; every governance listing writes to market 0.
6. **Mark price separate from oracle, plus OI caps and price bands.** Raw oracle as mark exposes liquidations to single-block oracle moves.
7. **WebSocket feeds for l2Book, trades and userFills.** Market makers need streams, not polling.
8. **Releasing unbonded stake and wiring downtime slashing.** Undelegated funds are currently stuck forever.
9. **Dead-man switch and cancel-by-cloid.** Cheap, standard market-maker safety tools.
10. **Coordinated upgrade mechanism (halt height and signed binaries).** Every consensus change today needs a manual lockstep fleet upgrade (`lib.rs:205` warns about the risk).
11. **Leverage/isolated-margin action, TP/SL and TWAP.** Expected at parity.
