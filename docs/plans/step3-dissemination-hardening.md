# Design: Step 3 — Native-Action Dissemination Hardening

_Brainstorm doc. Branch `cap100-3val-perf`. Feeds `/writing-plans` → updates
`mempool-gossip-hash-proposals-impl.md`. Created session 297._

## Problem
At flood, validators reject the proposer's `CompactBlock` with "missing native
actions" → on a 3-validator all-3 quorum, one rejection stalls the view. The
existing impl plan (`mempool-gossip-hash-proposals-impl.md`, May 26) and the
June-1 failure memory reach **opposite conclusions on the validate_block retry**,
so we must resolve that fork before writing the implementation plan.

## Context (verified from memory + source, session 297)
- **`produce_block` already emits a `CompactBlock`** (hashes, not full actions) —
  `crates/torus-consensus/src/app.rs:861-862`. Full block cached in
  `pending_proposals` (L863).
- **Validators reconstruct from their local mempool** via `get_native_by_hash`.
- **The ONLY path delivering actions to non-proposers today is the pre-proposal
  unicast push** (req/res `/torus/direct/1.0`, `PRE_PROPOSAL_BATCH_MARKER`),
  proposer → all validator peer_ids at block-production time
  (`swarm.rs:794-810`, `main.rs:421-427`, commit `865d4e1`). It races: a peer not
  yet in the peer map, or a congested swarm loop (1.5-2s under load per mem
  `8ee99db3`), means that validator's mempool is empty at validate time.
- **Gossip-on-submission is fully built but PERMANENTLY DISABLED.**
  `native_gossip_enabled` defaults `false` (`torus-mempool/src/lib.rs:116`); the
  setter `set_native_gossip_enabled` (L121) has **zero call sites** (verified by
  grep). Outbound (T3) is plumbed end-to-end but gated off at `lib.rs:296`;
  inbound (T4) is fully wired and operational (`swarm.rs:396-433`,
  `main.rs:403-413`). This is the exact root cause of the June-1 stall
  (mem `7d887ff2`: "validators don't have actions to reconstruct from hashes").
- **Retry loop** in `validate_block` (`app.rs:941-959`, commit `4e097af`):
  **blocking `std::thread::sleep`, 5×20ms = 100ms**, on the *synchronous*
  consensus thread (NOT tokio). = 20% of the 500ms view budget. Commit msg:
  "Restores the retry removed in 865d4e1."
- **View timeout** = `timeout_base_ms` = **500ms** (`testnet/genesis.json:8`),
  linear-cumulative per epoch (`pacemaker/implementation.rs:636-638`).
- **Quorum** = `floor(total_power*2/3)+1` (`validator_set.rs:157-170`). 3 equal
  validators → quorum **3 = all-three. Zero fault tolerance.** This is why
  `4e097af` re-added the retry: at 3-val you cannot tolerate one rejection,
  contradicting mem `8ee99db3` ("remove retry, BFT tolerates") which assumed a
  4-val / 3-of-4 set.
- **Silent-drop sites** (Step 3's named symptom — all `warn!`-only, no retry):
  `swarm.rs:282/305` native batch gossip publish (batch pre-cleared → lost on
  `InsufficientPeers`; moot while gossip off), `:718` consensus proposal publish,
  `:749` unicast Vote/NewView dropped when target not in peer map.

## Options

### Option A — Enable gossip + keep the 100ms retry (stay 3-val)
**How:** Flip `set_native_gossip_enabled(true)` at startup (the one dead line).
Actions now flow continuously to all validators on RPC submit, so by proposal
time mempools are warm. Keep the pre-proposal unicast push as a second path.
Keep the 100ms blocking retry as a now-rare safety net. No topology change.
**Files:** `torus-node/src/main.rs` (+ optional `ChainConfig` flag in torus-types).
**Pros:** Smallest change; directly fixes the verified root cause; no genesis
reset. **Cons:** Still all-3 quorum (any validator offline = halt — unaddressed);
blocking thread-sleep remains; doesn't fix the `282/305` pre-clear drop.
**Effort:** Small. **Risk:** Low-Medium (re-enabling a path that may have been
disabled for a reason — see Open Questions).

### Option B — Add a 4th validator (3-of-4 quorum) + drop/shrink retry
**How:** Add validator #4 to genesis → quorum `floor(4*2/3)+1 = 3` = 3-of-4,
tolerating one missing/rejecting validator. Then remove or shrink the retry per
mem `8ee99db3`'s original conclusion.
**Files:** `testnet/genesis.json` + devnet genesis; coordinated wipe+restart of
ALL nodes; `app.rs:941-959` (remove/shrink retry).
**Pros:** Removes the fundamental 3-val fragility (state known-issue); real f=1
tolerance; lets us drop the blocking sleep. **Cons:** Needs a 4th host; forced
coordinated genesis reset (no dynamic join); does NOT fix delivery on its own —
if gossip stays off you still depend on the racing unicast push, just survive one
starved node. Really a robustness multiplier on top of A, not a standalone fix.
**Effort:** Medium. **Risk:** Medium (genesis reset + host dependency).

### Option C — Enable gossip + harden the silent-drop sites + keep 100ms retry; decouple the 4th-validator question (RECOMMENDED)
**How:** (1) Enable gossip-on-submission (A's core), gated by a `ChainConfig`
field defaulting true. (2) Fix `swarm.rs:282/305` so a failed publish doesn't
pre-clear/lose the batch; raise severity/track `:718`/`:749` drops. (3) Keep the
100ms blocking retry as last-resort safety (correct for 3-val; now rarely hit
because gossip pre-delivers). (4) Treat the 4th validator (Option B) as an
independent ops follow-up — it's about *fault tolerance*, not *delivery*.
**Files:** `main.rs`, `torus-types` (ChainConfig flag), `swarm.rs` (drop-site
hardening), `app.rs` (retry unchanged).
**Pros:** Attacks the verified root cause the way the plan intended; fixes the
exact Step-3 silent-drop sites; keeps the retry 3-val genuinely needs; decouples
delivery (this PR, code, CI-verifiable) from fault-tolerance (ops, 4th val).
**Cons:** More surface than A; still all-3 quorum until the separate 4th-val step;
blocking sleep remains. **Effort:** Small-Medium. **Risk:** Low.

## Recommendation
**Option C.** It resolves the T7 contradiction cleanly: the retry *stays* (correct
for all-3 quorum) but stops being the primary delivery mechanism — enabling the
already-built gossip is what makes missing-action rejections rare. Hardening the
named drop sites addresses Step 3's actual symptom. The 4th-validator fault-
tolerance lever (Option B) is real and worth doing, but it's an ops decision that
shouldn't block or be coupled to this code change.

## Open Questions
1. **Why was gossip disabled?** The setter is dead code — was it switched off due
   to a flooding/perf regression (mem `7d887ff2` carries an `error_pattern:gossip-flooding`
   tag), or just never finished? **Pre-flight check before flipping it on.**
2. **Do we have a 4th validator host?** Determines whether Option B is near-term
   viable. (Testnet has 3 real nodes; #3 is a friend's VPS.)
3. **Keep both delivery paths or gossip-only?** Redundant (gossip + unicast push,
   safer) vs simpler (drop the racing pre-proposal push once gossip is trusted).
