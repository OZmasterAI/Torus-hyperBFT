# Design: Hybrid Block Propagation + Consensus Pipelining

## Problem

Current throughput ceiling is ~25K orders/sec (gossip 256KB limit × 19 blocks/sec).
Target: 500K-1M orders/sec to be competitive with Hyperliquid at 25 validators on standard cloud.

Two independent changes multiply together:
- Hybrid propagation: removes the 256KB block size cap → 10-50K orders/block
- Pipelining: reduces effective block time from ~52ms to ~15-20ms → 50-65 blocks/sec

## Context

### Current State
- Proposals carry **full block data** (`Vec<Datum>`) and broadcast via GossipSub (256KB cap)
- No early view advancement — deadline is fixed per epoch (`max_view_time * view_offset`)
- PhaseVote quorum triggers AdvanceView message → view advances immediately
- Direct message path exists (`/torus/direct/1.0`, 4MB, request-response)
- Block sync already fetches blocks via direct request-response
- Per-market parallel order matching (commit 9c14360) handles execution scaling
- 2-chain commit rule: `justify.view == parent_justify.view + 1`

### Constraints
- Safety: must never commit conflicting blocks
- Liveness: stuck validators must recover (block sync already handles this)
- 4 validators (devnet) → 21 validators (mainnet target)
- Standard cloud: 10 Gbps networking per validator

## Options

### Option A: Header-First Gossip + Data Fetch

**How it works:**
Split Proposal into two messages. ProposalHeader (block hash, height, justify, data_hash, ~200 bytes) goes via gossip. Validators receive the header, verify the justify/QC, then fetch the full ProposalBody (block data) via direct request-response from the proposer or any peer that has it. Validators vote on the header (committing to data_hash) while fetching the body in parallel. Block execution happens after body arrives.

**Pipelining integration:**
Once a validator sees quorum PhaseVotes form a QC, it immediately advances the view — no waiting for the full deadline. The next proposer can start producing its block as soon as it has the QC, even if some validators are still fetching the previous body. This decouples consensus speed from data propagation.

**Files affected:**
- `hotstuff_rs/src/hotstuff/messages.rs` — split Proposal into ProposalHeader + ProposalBody
- `hotstuff_rs/src/hotstuff/implementation.rs` — vote on header, fetch body async
- `hotstuff_rs/src/networking/messages.rs` — new message variants
- `hotstuff_rs/src/algorithm.rs` — parallel body fetch alongside consensus
- `hotstuff_rs/src/pacemaker/implementation.rs` — early view advancement on QC
- `torus-network/src/swarm.rs` — header via gossip, body via direct
- `torus-network/src/behaviour.rs` — body fetch protocol

**Trade-offs:**
- Pro: No block size limit (body fetched separately), scales to 100+ validators
- Pro: Gossip stays lightweight (headers only), no bandwidth pressure
- Pro: Data availability — peers can serve bodies, not just proposer
- Con: Complexity — two-phase proposal, need timeout handling if body fetch fails
- Con: Vote-before-execute — validators commit to data_hash before running txs
- Con: Need "data availability" guarantee — what if proposer withholds body?

**Effort:** Large (2-3 weeks)
**Risk:** Medium (vote-before-execute changes the trust model slightly)

---

### Option B: Direct Proposal + Early View Advancement

**How it works:**
Switch proposal broadcast from gossip to direct messages (proposer sends full Proposal to each validator individually, 4MB limit). Keep the existing proposal structure unchanged. Add early view advancement: when the pacemaker sees a valid QC form (quorum PhaseVotes collected), immediately advance to the next view instead of waiting for the deadline. Next proposer starts immediately.

**Pipelining integration:**
The QC-triggered advancement IS the pipelining. Currently views advance on AdvanceView messages which are already triggered by QC formation — but the pacemaker may have a structural delay. The fix: ensure `update_view()` is called the instant a QC forms in the PhaseVoteCollector, and the new proposer's `enter_view()` fires immediately with no artificial delay.

**Files affected:**
- `hotstuff_rs/src/hotstuff/implementation.rs` — switch `broadcast` to `send` for proposals
- `hotstuff_rs/src/pacemaker/implementation.rs` — ensure immediate advancement on QC
- `torus-network/src/swarm.rs` — no changes needed (send path already works)
- Config: raise `NATIVE_PER_BLOCK_CAP` to match new capacity

**Trade-offs:**
- Pro: Minimal code changes — mostly config and routing
- Pro: No protocol redesign — same Proposal struct, same validation flow
- Pro: Immediate throughput gain (4MB blocks = ~20K orders)
- Con: Proposer bandwidth scales O(N) — at 25 validators, 24 × 4MB = 96MB/proposal
- Con: At 50 blocks/sec with 25 validators, proposer needs ~4.8 GB/s (impractical)
- Con: Only works well at <10 validators

**Effort:** Small (3-5 days)
**Risk:** Low (no protocol changes, just routing + timing)

---

### Option C: Hybrid with Pipelined Block Production

**How it works:**
Combine the best of A and B. Proposals go via gossip (header only) with direct body fetch, but add speculative block production: the next proposer starts building its block as soon as it receives the current proposal (before votes complete). It builds speculatively on top of the current proposal's block. If the current proposal doesn't get committed (timeout/fork), the speculative block is discarded.

**Pipelining integration:**
True pipeline overlap — while validators are voting on view N's proposal, the view N+1 proposer is already executing transactions and preparing its block. By the time the QC for N arrives, N+1's proposal is ready to broadcast immediately. Effective latency = max(vote_collection_time, block_production_time) instead of their sum.

**Files affected:**
- Everything from Option A, plus:
- `hotstuff_rs/src/algorithm.rs` — speculative block production thread
- `torus-consensus/src/app.rs` — `produce_block` must handle speculative parent
- New: block production worker that runs in parallel with consensus

**Trade-offs:**
- Pro: Maximum throughput — proposals ready instantly when QC arrives
- Pro: Scales to any validator count
- Con: Speculative execution may be wasted on forks
- Con: Highest complexity — parallel block production, fork handling, state rollback
- Con: Memory pressure from speculative state

**Effort:** Very Large (4-6 weeks)
**Risk:** High (speculative execution + rollback is hard to get right)

## Recommendation

**Option A (Header-First + Early Advancement)** for the following reasons:

1. Scales cleanly from 4 to 25+ validators without bandwidth issues
2. No speculative execution complexity (Option C risk)
3. The "vote on header, fetch body" pattern is battle-tested (Ethereum, Sui, Aptos all do this)
4. Gets you to ~500K orders/sec on standard cloud, ~2.5M on beefy hardware
5. Same codebase handles both — just a config knob for block size

Option B is tempting for its simplicity but dead-ends at ~10 validators. Option C adds marginal gains (~10-20% latency improvement) for 2× the complexity.

Start with Option A, and if block production time becomes a bottleneck later, add speculative production (Option C) as a follow-up.

## Open Questions

1. **Vote-before-execute safety:** If validators vote on the header before executing the block, a Byzantine proposer could include invalid transactions. Detection happens at body-fetch time — what's the penalty/recovery flow?
2. **Body fetch timeout:** How long should validators wait for the body before triggering a view timeout? Should they vote immediately and fetch body async, or wait for body before voting?
3. **Data availability:** If the proposer sends header but withholds body, do we need a protocol to prove unavailability? Or is view timeout + reputation sufficient at 25 validators?
4. **Existing block tree assumptions:** The block tree currently assumes blocks are fully available when inserted. Lazy-loaded bodies need a "header-only" state — how does this interact with the commit rule and sync protocol?
