# L3 insert_persist attribution — the 18.4ms is a network round trip, not a disk write

Scope: attribute `torus_view_insert_persist_seconds` = 18.4ms/view at IDLE (empty
blocks, 3-validator loopback devnet, l3verify-18c-06de732.md) and design the fix.
Analysis-only; no code changed. Branch `perf/re-proof5` @ 30b8444.

## 1. What the metric actually measures

`ViewMetricsRecorder::insert_block` (crates/torus-telemetry/src/view_metrics.rs:146-161)
observes **first-proposal-arrival → InsertBlock event**. Arrival is
`ReceiveProposal` OR `ReceiveProposalHeader` (torus-node/src/main.rs:842-850).
`InsertBlockEvent` is published immediately after `block_tree.insert`, *before*
`block_tree.update` (implementation.rs:2094-2103), so the commit walk is NOT in
the window.

Since headers are broadcast unconditionally (`broadcast_proposal_as_header`,
implementation.rs:639-651; all propose sites 385/493/600 use it), the follower
window covers the whole **header-first body-fetch pipeline**:

```
header arrives (t=0)
  → equivocation check, hash check, justify PC verify, safe_pc,
    update_locks_only (RocksDB batch), sign vote, send vote,
    set_vote_state_atomic (RocksDB batch)          [vote_delay = 1.6ms measured]
  → send BlockDataRequest to leader (implementation.rs:1957-1967)
  → [network hop 1] command_tx → swarm loop → libp2p direct msg → leader
  → leader parked in recv() wakes, serves from in-memory pending_bodies
    (on_receive_block_data_request, implementation.rs:1985-2007)   [<0.5ms]
  → [network hop 2] response back as HotStuffMessage (bridge.rs:609-614 routes
    the REQUEST via the direct-message path; response returns the same way,
    landing in shared.inbound → 250µs poller (receiving.rs:69-72) → progress
    channel → follower recv() wakes)
  → try_insert_body: app_view ancestor reads + app.validate_block (empty:
    hash + borsh decode + a few state reads, app.rs:3343-3489) + 
    block_tree.insert                                              [~0.5-2ms]
InsertBlock event (t=18.4ms)
```

## 2. Attribution table

| # | Cost | Mechanism (anchor) | est. ms of 18.4 | Risk class |
|---|------|--------------------|----------------:|------------|
| 1 | Header verify + lock + vote + vote-state write | 2-3 ed25519 verifies, `update_locks_only`, `set_vote_state_atomic` — both non-sync RocksDB batches (internal.rs:645-664) | 1.6 (measured `vote_delay`) | in place, safety-critical ordering already correct |
| 2 | Body request hop (follower→leader) | `command_tx` → swarm loop → QUIC direct (swarm.rs:2221-2238 analogue path) | 4–8 | node-local plumbing |
| 3 | Leader serve | in-memory `pending_bodies` lookup + clone + send | <0.5 | — |
| 4 | Body response hop (leader→follower) | same stack back; 250µs poller tick; recv wake | 4–8 | node-local plumbing |
| 5 | Validate empty body | `app_view` ancestor walk + `validate_block` (incl. an `info!` log per call, app.rs:3347) | 0.5–1 | node-local |
| 6 | `block_tree.insert` RocksDB write | `children()` read + ~5-key WriteBatch, `db.write(batch)` with DEFAULT WriteOptions — **sync=false** (kv_store.rs:100-113) | 0.2–1 | **not the cost** |
| — | fsync | **none exists anywhere on this path** | 0.0 | see §5 |

Cross-evidence: `proposal_arrival` = 16.5ms for a SINGLE header delivery on the
same stack (minus leader build ~7.4ms ⇒ ~8-9ms/hop incl. skew). Two hops + two
scheduler wakes ≈ the 16.8ms residual. A non-sync memtable write of a few KB
cannot be 18ms; the "persist" in the metric name is a misnomer for the header
pipeline — it is a **fetch-then-persist** window dominated by the fetch.

## 3. Why it matters for block time (causal chain)

The vote leaves at +1.6ms — insert_persist does NOT gate voting. It gates the
**next leader's proposal**: `enter_view` needs the parent body in-tree
(`block_height(&highest_pc.block)` → None ⇒ `proposal_deferred`,
implementation.rs:537-553), retried on the ≤10ms polling cadence
(algorithm.rs:233-246). So the body RTT lands ~1:1 in `view_duration` (53.9ms)
and in the leader's `qc_collect` (40.8ms, which spans the successor view under
rotation).

## 4. Fix designs (ranked by ms-saved / risk)

### Fix 1 — inline body for small blocks (est. −14 to −16ms at idle; risk ≈ 0)

Leader broadcasts the full `Proposal` instead of `ProposalHeader` when the
serialized body ≤ `TORUS_INLINE_BODY_MAX_BYTES` (env-gated; **default 0 =
exact-today header-only**). The receive path already exists and is the
*original, stricter* validate-then-vote path (`on_receive_proposal`,
implementation.rs:853-1158): insert happens before the vote, no body fetch, no
deferred proposal at the next leader. Change is confined to the four
`broadcast_proposal_as_header` call sites choosing full-vs-header by size.

- Tests (RED→GREEN, extend `hotstuff_rs/src/hotstuff/header_fast_path_regression_test.rs`):
  - `inline_body_small_block_broadcasts_full_proposal_and_skips_body_fetch`
  - `inline_body_default_zero_keeps_header_only`
  - `inline_body_over_threshold_stays_header_first`
  - existing `iter2_lock_safety_test`, `pc_discard_regression_test` stay green.
- Stateright: NOT required — no voting-rule or durability change; the
  full-proposal path is the originally modeled one.
- S470 wedge check: `should_yield_proposal_for_exec_backpressure` untouched;
  inlining strictly REDUCES header-first voting (fewer header-voted blocks),
  so no header-first/commit-lag divergence can be reintroduced.
- At idle every block qualifies; under load (cap-400 bodies) the threshold
  decides — bench with e.g. 64KB, keep production default 0 until sized.

### Fix 2 — proactive body push (est. −4 to −8ms for above-threshold blocks)

Leader sends the body immediately after the header (push, not pull); follower
accepts an unsolicited `BlockDataResponse` when a matching `pending_headers`
entry exists (it already validates via the normal `try_insert_body` path;
receiving a block never implies a vote). Removes hop #2 + serve. Env-gated
`TORUS_BODY_PUSH` default OFF. Test:
`body_push_unsolicited_response_accepted_when_header_pending`. Complementary to
Fix 1 for large blocks; do after Fix 1 only if load-cells still show the term.

### Fix 3 — (no action) loop wake

The response already travels the progress channel and wakes `recv()`; the 10ms
cap (algorithm.rs:240-243) only bounds the deferred-proposal retry, which Fix 1
makes moot for small blocks.

## 5. Durability honesty — the safety-mandatory floor

**Today NOTHING on the vote/insert path is fsynced in any env.** Vote-safety
state (`set_vote_state_atomic` internal.rs:645, `update_locks_only`) goes
through the same `db.write(batch)` with default WriteOptions (sync=false);
`TORUS_SYNC_WAL_ON_COMMIT` only touches the app-side commit path
(torus-state/src/db.rs:562-583), not `cf_consensus_meta`. So the HotStuff
crash-recovery clause (a replica must never re-vote a lower view after restart)
currently rests on the OS not losing the WAL page — the 18.4ms buys zero
durability.

If/when the contract is enforced: the ONLY pre-vote synchronous requirement is
the vote-safety state. Group `highest_view_phase_voted` + `LAST_VOTED_PROPOSAL`
+ `locked_pc` + `highest_pc` into ONE batch written with sync=true before the
vote is sent — exactly one ~4k dsync per view ≈ **7ms on this box**, charged to
`vote_delay`, not `insert_persist`. The block payload never needs pre-vote
durability (re-fetchable; its loss cannot cause equivocation). This IS a
consensus-visible durability-contract change: requires a stateright extension
(`vote_state_durable_before_send`) plus a lockstep crash-after-vote-before-fsync
scenario before flipping any default.

**Floor estimate (this hardware):**
- today's (non-sync) contract + Fix 1: `insert_persist` ≈ **0.5–2ms**
  (validate + non-sync insert), i.e. ~16ms recovered per view;
- honest-durability contract: `insert_persist` unchanged (~1–2ms);
  `vote_delay` ≈ 8ms (one grouped dsync). Total mandatory synchronous cost per
  view = one ~7ms dsync — nothing else on the path needs to be synchronous.

## 6. ADDENDUM — inline-body A/B post-mortem (18c cells-inlineab, 2026-07-20)

Fix #1 as implemented (commit 0c3ea78: threshold-gated switch from
`ProposalHeader` broadcast to full-`Proposal` broadcast) confirmed the latency
mechanism (idle insert_persist 12.1→0.9ms, proposal_arrival 9.0→3.1ms) but has
a **liveness defect**: idle views halved (5275→2541, view_duration 41.3→85.5ms)
and the loaded cell collapsed (0 matched, 124.8s block time, ~2 commits/300s).

### Root cause: the full-Proposal receive path lost three properties the
### header-first machinery is load-bearing for

1. **View-agnostic delivery.** `ProgressMessageStub::recv` returns a HotStuff
   message only when `msg.view == cur_view`; future views are buffered, PAST
   views are silently dropped and purged
   (crates/hotstuff_rs/src/networking/receiving.rs:146-152, 188-196).
   `ProposalHeader` is exempt via `is_block_data_msg`
   (hotstuff/messages.rs:106-113) and its handler checks the proposer against
   the HEADER's view (implementation.rs:782-787). A full `Proposal` arriving at
   any replica whose pacemaker already advanced past its view is **dropped
   forever** — no reprocessing, no fetch fallback. Each such drop burns the
   view to timeout. Loaded log signature (IB400-on/val0.log): height 943
   re-proposed in views 958/960/961 at exact view-timeout spacing (2s), then
   S470 commit-lag backoff balloons deadlines (block_sync retriggers at
   21:37:34 → 21:39:38 → 21:41:48; commit gap 21:36:31→21:40:47). Idle
   signature: views halved with leader fraction unchanged.

2. **Vote-before-DA.** On the strict path, `app.validate_block` runs BEFORE
   the vote; a CompactBlock whose out-of-band bodies are not yet locally
   durable returns `MissingData` (torus-consensus/src/app.rs:3128-3140) and
   `on_receive_proposal` silently casts NO vote and drops the proposal — the
   `if let Valid` has no else arm and none of the pending-header/deferred-body
   retry machinery engages (implementation.rs:1050-1144). The header path votes
   first and parks/retries the body. Log proof of the race: proposal (view 966)
   received at 21:38:39.901 BEFORE its own pre-proposal body batch (.906).
   This reintroduces exec/DA-before-vote under load — the exact relaxation
   header-first exists to provide.

3. **Transport-size hypothesis refuted for this bench**: order-carrying
   proposals are CompactBlocks (~11KB, `native_hashes=100`, push `bytes=11009`)
   — far under the 1MiB accept gate (torus-network/src/config.rs:216) and 2MiB
   gossip transmit (caps.rs:47). No `oversized consensus message` warns in
   val0.log. (A legacy full-TorusBlock inline body could still exceed the 1MiB
   accept gate; a v2 must cap the threshold regardless.) Leader-side inline
   branch skips nothing besides the header broadcast — bookkeeping identical.

### Verdict: current mechanism NO-GO; objective REPAIRABLE via v2 = body push

Switching receive paths is architecturally regressive. The v2 that keeps every
header-first property: leader broadcasts the `ProposalHeader` UNCHANGED and
immediately pushes the body as an unsolicited `BlockDataResponse` (existing
wire variant, `is_block_data_msg` = view-exempt) into the existing
pending-header machinery; receivers vote on the header as today and insert on
the pushed body with zero fetch RTT. Gate: `TORUS_BODY_PUSH_MAX_BYTES`,
default 0, hard-capped ≤ 64KB (≪ every transport limit). Honors all three
constraints: transport-safe, vote stays ahead of DA, empty/tiny blocks only.

**Recommendation for the in-tree code**: revert the inline branch of
0c3ea78 (`broadcast_proposal_as_header`) and its three tests in the next
change round — the knob is default-off and inert, but it is a proven
liveness hazard if ever enabled and should not survive as a footgun. The
attribution (§1-5) and the recording-network test fixtures remain valid and
reusable for v2.
