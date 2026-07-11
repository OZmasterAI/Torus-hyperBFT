# Work Order: S443 — Crash-recovery fixes from external review (think-dev c28a39c → aff21fe)

**To:** VPS Claude instance, repo `/home/crab/projects/Torus-hyperBFT`, branch `think-dev`
**From:** External multi-agent review (2026-07-10) of the 16 commits `c28a39c..aff21fe`
**Overall verdict:** The commit set is correct and matches the S442 intent — the HotStuff 2-chain safety fixes (2f196f9, 98b69e0, d54d025, f6fac87) were verified sound and are trusted as-is on the current testnet. The items below are the residual findings, in priority order. Item 1 is a genuine bug in the exact scenario the S442 work targets; fix it before the crash-recovery/rejoin story is declared done.

---

## 1. F-1 (BLOCKER): crash while a live body hole exists → silent permanent execution stall on reboot

**Scenario:** A live hole at height H (bodies missing on an otherwise healthy node) while consensus commits past it to K. H..K sit only in the in-memory `deferred_exec` queue — `persist_committed_block_durably` never ran for them, so `CF_BLOCK_HEADERS` ends at H−1 while hotstuff's durable `highest_committed_block` is K. Node is killed in this window (which spans the entire hole lifetime, up to the full heal budget).

**On reboot:**
- `find_last_committed_height` returns H−1, applied marker is H−1 → `replay_committed` sees **no gap** → no park (`torus-consensus/src/app.rs:1852-1854`).
- hotstuff never re-fires `on_committed_block` for H..K (commit walk excludes heights ≤ `min_height`, `hotstuff_rs/src/block_tree/accessors/internal.rs:680-690`); gap-driven backfill sees no hole in hotstuff's own contiguous commit index.
- First live commit K+1 seeds `exec_next_height = H` (`app.rs:2863-2870`); `drain_exec_queue` hits `deferred_exec.remove(H) == None` and waits forever (`app.rs:2913-2916` — the "it will arrive" assumption is false: it arrived pre-crash).
- **No log, no heal budget, no exit(70).** Execution frozen at H−1 while consensus/RPC height advances and `deferred_exec` grows unboundedly. This is the zombie-advance mode S442 exists to eliminate, and the t12 kill-during-hole harness produces exactly this trigger.

**Key fact:** the `CF_COMMIT_MANIFEST` entries for H..K are already durably on disk (written at commit, pruned only at dispatch — the CF is defined as the committed-but-not-dispatched window, `app.rs:379-384`) and contain exactly what recovery needs. Boot just never consults them, because parking is gated on a header-visible hole.

**Fix direction:**
- At boot, scan `CF_COMMIT_MANIFEST` for heights above the `CF_BLOCK_HEADERS` scan result and seed them as parked `Compact` exec sources (same park+heal path aff21fe built).
- For the residual µs window between hotstuff's block-tree batch write (`internal.rs:363`) and the manifest put, reconcile against hotstuff's durable `highest_committed_block` and re-derive the datum from the retained block tree.
- **Also (cheap, do regardless):** watchdog on the silent-wait branch of `drain_exec_queue` (`app.rs:2913-2916`) — head-of-line height not buffered for N seconds ⇒ scream in logs, and ideally start the same `note_exec_hole` fail-stop budget. Converts F-1 and any unknown cousin from silent to loud.

**Acceptance:** t15-style test — inject a body hole, SIGKILL while `deferred_exec` is non-empty, reboot ⇒ node parks the missing heights, heals from peers, and catches up (or fail-stops loudly on budget exhaustion). Must be RED on current aff21fe.

**Root cause to keep in mind (structural):** three independently-persisted frontiers — hotstuff `highest_committed_block`, the `CF_BLOCK_HEADERS` scan, the applied marker — with no boot-time reconciliation. Every residual finding lives in a disagreement between two of them. A single boot-time reconciliation pass would close the class.

## 2. F-2 (fix in the coordinated fresh-genesis relaunch): Byzantine body substitution via signature-less action hash

`compute_action_hash` omits the signature (`torus-types/src/lib.rs:903-907`; the comment at 909-917 already documents this collision as a consensus-fork risk for the trust cache). DA keys bodies by that hash with last-write-wins (`native_da.rs:85-106`), and `absorb_fetched_bodies` mirrors whatever a peer sends under its recomputed hash (`app.rs:2105-2117`). A Byzantine peer answering a heal pull can serve the committed payload+nonce with a swapped/invalid signature: it hash-matches `native_action_hashes`, passes reconstruction, and at execution either (a) fails sig verification → action silently skipped **and the honest proposer is slashed 100% and tombstoned** (`app.rs:869-890`), or (b) executes with a different sender. Either way the healing node's native state diverges from peers. Quorum agreement on `data_hash` never bound the signature bytes.

**Fix:** bind the full signed bytes into the DA content address (or add a parallel commitment). This is a consensus-format change → schedule it inside the already-planned coordinated fresh-genesis relaunch (the d54d025 header-preimage change forces that relaunch anyway).

## 3. Lower priority (post-relaunch backlog)

| Item | Detail | Where |
|---|---|---|
| Sync-server halt DoS | Blocks carry no proposer signature; a malicious sync server can mint an unproven sibling of a committed block and trigger `on_fatal_safety_violation` → exit(70) on an honest node. Acceptable for the 4-validator trusted testnet; before adversarial deployment, require proof-of-commit (valid QC on the conflicting block or a certified descendant) before latching fatal; demote unproven conflicts to session-teardown + blacklist. | `hotstuff_rs/src/block_sync/client.rs:401-445` |
| EVM↔native replay window | EVM bundle + metadata commit in separate batches before the native flush+marker (`app.rs:804-826`). Crash in between ⇒ replay re-executes with `skip_invalid=true`, nonce-too-low txs skipped ⇒ `computed_fee_revenue = 0` ⇒ fee-distribution divergence vs peers on fee-carrying EVM blocks. Fold EVM commit into the atomic batch or add an EVM-applied marker. | `torus-consensus/src/app.rs:804-826`, `torus-evm/src/executor.rs:291-297` |
| Vote persisted after send | Vote is sent (`implementation.rs:1798`) before `set_vote_state_atomic` persists (`:1801`) — crash between allows a same-view re-vote after restart. Pre-existing, both vote paths. Swap the order. | `hotstuff_rs/src/hotstuff/implementation.rs:1798-1801` |
| Slash durability | Buffered `pending_slashes` written unbatched (`app.rs:739-768`, `869-890`); replay passes `vec![]` (`app.rs:1873`) — crash can lose a buffered slash; a re-delivered commit overwrites a `deferred_exec` entry and drops its slashes (`app.rs:2899-2900`). Known/acknowledged limitation (c); fold into the atomic batch when convenient. | `torus-consensus/src/app.rs` |
| DA-mirror unbounded buffer | Under a persistently failing local DA store, `da_pending` grows unboundedly and every ingest re-triggers a full-batch flush (O(n²)). Degraded-mode only (a node with a dead local RocksDB is dead anyway); add a cap/backpressure eventually. | `torus-mempool` (d6b8905) |
| Cosmetics | `parent_body_starvation_test.rs`: `MAX_VIEW_TIME` comment says ~1s, value now 4000ms. Stateright `LockRule::PRODUCTION` is hand-mirrored from `invariants.rs` — keep in sync on any future lock-rule change; model covers pipelined/Generic mode only (nudge path unmodeled). | tests |

## Constraints / reminders

- All four validators must relaunch on the same post-aff21fe build with fresh genesis (d54d025 re-keys every hash; a stale binary forks on block 1) — the runbook already mandates this. F-2's format change should ride the same relaunch.
- Note: c0fd7c1 and d54d025 don't compile in isolation (they implement/use `on_fatal_safety_violation` before 2f196f9 defines it) — harmless at tip, but a bisect hazard; avoid repeating the pattern.
- The `integration/bs4a-livelock-s428` branch holds 5 commits not in think-dev (incl. 470970e: 4th validator friend2 in genesis, quorum 3-of-4) — decide whether to merge into think-dev before the relaunch so genesis and code travel together.
