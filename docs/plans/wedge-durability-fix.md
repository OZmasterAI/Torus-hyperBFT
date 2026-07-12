# Design: Body-durability invariant for the QC'd-poison-ancestry wedge

Session 459 (2026-07-12). Follows the S458 wedge, the S459 fresh-genesis recovery,
and the `torus-unwedge` tool. This fixes the *formation* of the wedge, which
recovery does not.

## Problem

A block can be QC'd into committed ancestry while its native-DA **body** exists
only in one node's storage that isn't crash-durable; a restart of that node
destroys the only copy, and no node can ever backfill → permanent commit-hole
(`REFUSING to commit across a hole`, `internal.rs:719-744`). Confirmed live:
testnet wedged at ~761,992 (val2 `--inspect`: 2 holes, bodies permanently
missing), on **default** config (`native-gossip=true`) under bench load + a
restart of the DA-holder.

**Invariant we want:** a PhaseVote for a block implies the voter has that block's
body durably (crash-safely) stored. Then a QC (2f+1 votes) implies ≥ f+1 honest
nodes hold the body durably → it is always recoverable → no permanent hole.

## Context (from memory + S459 explore agent, file:line verified)

The invariant is **already mostly enforced for the default path**, then bypassed
in three places:

- **Propose** (`torus-consensus/src/app.rs:2360` `produce_block`): `COMPACT_PROPOSALS=true`,
  so proposals are hash-manifest `CompactBlock`s. Leader mirrors every referenced
  body to the durable `da_store` (RocksDB) at `app.rs:2427-2432`
  (`mempool.mirror_native_to_da`) **before** encoding — bypasses the RAM buffer.
- **Vote** (`hotstuff_rs/src/hotstuff/implementation.rs:770` `on_receive_proposal`):
  votes only inside `ValidateBlockResponse::Valid` (built 942-1035; vote sent
  988-1003). Validity = `TorusApp::validate_block` (`app.rs:2518`). For compact
  proposals, `reconstruct_native_actions_hot` (`app.rs:2120-2207`) returns
  `MissingData` (→ not `Valid`, `app.rs:2604`) unless every body hash is present
  in the durable `da_store`. **So the vote-gate exists for the compact path.**
- **Commit** (`hotstuff_rs/src/block_tree/accessors/internal.rs:663` `commit`):
  hole-refusal at 719-744. This is downstream — by here the QC already exists;
  gating here just produces the wedge we already have.

The three holes in the existing gate:

1. **Swallowed durability failures.** `da_store.put_batch` errors are logged and
   dropped (`mempool/lib.rs:655-657, 688-693, 710-716`; `app.rs:2431-2432`). A
   genuine write failure lets `produce_block`/`validate_block` proceed as if the
   body were durable — the vote/propose is a lie.
2. **Legacy inline path.** The direct-deserialize `TorusBlock` branch
   (`app.rs:2557`, reachable when a proposer sends full-body-inline) never calls
   `mirror_native_to_da` before `Valid`; its durability rides on `hotstuff_rs`'s
   own `block_tree.insert` into a *different* store, not `da_store`.
3. **Crash-safety unproven.** `put_batch` uses default write options; whether the
   body is fsync'd (survives a hard SIGKILL / `docker restart -t 0`, the repro
   trigger) before the vote fires is unconfirmed. The DA-recovery pull
   (`app.rs:1588` `try_send`) also silently drops under load (a *recovery*
   weakness, not a formation cause, but worth hardening).

## Options

### Option A (recommended): Harden the existing implicit vote-gate — fail-closed on durability
Make "durable" true instead of assumed. (a) Propagate `da_store.put_batch`
failures as errors through `mirror_native_to_da`/`flush_da_mirrors` so
`produce_block` returns an error (leader won't propose) and `validate_block`
returns `MissingData`/`Invalid` (voter won't vote) when a body can't be durably
written. (b) Add the `mirror_native_to_da`-before-`Valid` requirement to the
legacy `TorusBlock` branch (`app.rs:2557`) so both paths converge on `da_store`.
- Files: `crates/torus-mempool/src/lib.rs` (error propagation on put_batch),
  `crates/torus-consensus/src/app.rs` (produce_block ~2431, validate_block ~2557
  + return site ~2714).
- Trade-offs: surgical; restores the invariant with **no happy-path latency**
  (only changes error handling + one missing mirror call). Preserves the
  compact/direct-to-leader throughput. Con: correctness depends on the RocksDB
  write actually being crash-safe — pair with C if the capture shows a flush gap.
- Effort: Small–Medium. Risk: Low.

### Option B (rejected for now): Quorum durable-spread barrier before propose
Leader pushes bodies to ≥ f+1 nodes and waits for durable-acks before proposing,
so a QC provably implies f+1 durable copies at propose time.
- Files: `app.rs` pre-proposal path (2477-2486) + a new ack protocol (network crate).
- Trade-offs: strongest guarantee, but adds a round-trip before every proposal —
  the exact latency `direct-to-leader` was built to remove — and a new protocol.
- Effort: Large. Risk: Medium–High. Rejected unless A+C prove insufficient.

### Option C (fold into A if the capture demands it): Crash-safe durable write before vote
Ensure the consensus-critical body write is fsync'd/WAL-synced before the vote is
cast (sync write option on the body `put_batch`, or an explicit WAL flush at the
`validate_block` `Valid` boundary), scoped to consensus bodies only.
- Files: `crates/torus-mempool/src/lib.rs` (write options / flush).
- Trade-offs: guarantees "durable" survives a hard kill. Adds fsync latency/IO per
  consensus body — scope tightly. Effort: Small. Risk: Low–Medium.

## Recommendation

**Option A now** (fail-closed on durability + close the legacy path) — it restores
the invariant surgically with no throughput cost. **Add Option C** iff friend2's
instrumented gossip-off capture shows the body was lost from a *successful*
`put_batch` on a hard restart (a flush/WAL gap) rather than from a swallowed write
error or the legacy path. **Option B stays rejected** unless A+C can't hold the
invariant. DA-recovery `try_send`-drop hardening is a separate, lower-priority
follow-up (recovery robustness, not formation).

## Open Questions

1. **Which hole actually fired?** — resolved by the captured wedge timeline:
   swallowed put_batch error (→ A suffices), legacy inline path (→ A part b), or
   unflushed body on hard kill (→ need C). This is why the capture runs in parallel.
2. Does hardening `validate_block` to fail-closed risk *liveness* — could a
   transient RocksDB error now stall a node that previously limped on? (A node
   that can't durably store a body *should* not vote for it; but confirm the error
   is retried, not fatal.)
3. Crash-safety scope: sync-write only the consensus body path, or a periodic WAL
   flush gated on "a vote is imminent"? (perf vs. safety — decide with C's numbers.)
4. Regression test vehicle: `torus-wedge-inject` gives the deterministic end-state;
   a *formation* test needs a `validate_block`-level unit test asserting no `Valid`
   when the body isn't durable. Both.
