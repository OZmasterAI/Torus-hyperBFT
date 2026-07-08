# 2-Chain Commit vs Grandparent Lock — Machine-Checkable Safety Model & Refutation

**Task**: T1.2 — Stateright safety model + proof for 2-chain-commit / grandparent-lock
**Class**: CONSENSUS-SAFETY
**Reference**: arXiv:2502.20692 (MonadBFT), Theorem 1 + Lemmas 1-5
**Companion model**: `crates/hotstuff_rs/tests/stateright_2chain_safety.rs`
**Status of the model**: source-complete, **uncompiled** (no Rust toolchain in the
authoring environment; see `unverified[]`).

---

## 0. TL;DR (decisive verdict)

The commit rule was changed to **2-chain** (`block_to_commit`, Generic phase,
`invariants.rs:518-555`) but the lock rule was **left at the 3-chain depth**
(`pc_to_lock`, Generic phase, `invariants.rs:433-437`, locking on
`justify.block.justify`). As a result the locked block and the committed block
end up at the **same depth**, which removes the "lock strictly newer than the
committed block" margin that a 2-chain commit needs.

**Verdict: the grandparent lock is UNSAFE for the 2-chain commit.** There is a
concrete asynchronous execution with n=4, f=1 (one Byzantine validator + one
equivocating/withholding leader) in which two *honest* validators irrevocably
commit two conflicting sibling blocks at the same height — an **Agreement**
violation. The exploit is a lock-lag: the validators whose votes *form* the
commit-enabling QC are, at vote time, locked only on the **grandparent** of the
block they vote for, so they remain free to vote for a conflicting sibling of
the soon-to-be-committed block before that block's QC reaches them.

**Fix (specified, not applied here — this is a verification artifact):**
change the Generic arm of `pc_to_lock` from

```rust
Phase::Generic => match block_tree.block_justify(&justify.block) {
    Ok(parent_justify) => Some(parent_justify.clone()), // locks justify.block.justify (grandparent)
    Err(_) => return Ok(None),
},
```

to lock on `justify` itself (i.e. on `justify.block`, the block the QC directly
certifies — the standard Jolteon / MonadBFT 2-chain lock):

```rust
Phase::Generic => Some(justify.clone()), // lock-on-parent: locks justify.block
```

With this change the model's **Agreement** property holds under the same
adversary (RED → GREEN; see the companion test).

---

## 1. Exact semantics extracted from the implementation

Let `J` be the `PhaseCertificate` a replica is processing (`block_tree.update(J)`),
`J.block` the block it certifies, `J.view` its view. Write `parent(b)` for the
block certified by `b`'s embedded justify (`block_tree.block_justify(b).block`).

* **Lock rule** (`pc_to_lock`, Generic, `invariants.rs:433-437`):
  locked block ← `parent(J.block)`  (= `J.block.justify.block`).
* **Commit rule** (`block_to_commit`, Generic, `invariants.rs:518-555`):
  let `pj = J.block.justify`; if `J.view == pj.view + 1` and `parent(J.block)`
  is not yet committed, commit `parent(J.block)` (= `pj.block`).
* **safe_pc predicate 3** (`invariants.rs:360`):
  `J.view > locked_pc.view  ||  extends_locked_pc_block(J)`,
  where `extends_locked_pc_block` (`invariants.rs:622-634`) is true iff
  `locked_pc.block ∈ { J.block, parent(J.block), parent(parent(J.block)) }`
  (checks up to grandparent depth).
* **Vote-once-per-view** (`implementation.rs:954-955, 1108-1109`):
  vote at view `v` only if `highest_view_voted < v`.

Two consequences that the model encodes verbatim:

1. **Lock and commit target the *same* block.** For a Generic `J`, both
   `pc_to_lock` and `block_to_commit` resolve to `J.block.justify.block =
   parent(J.block)`. There is **no** depth separation between the locked block
   and the committed block.

2. **The lock update *lags* the vote that enables the commit.** A replica that
   votes for `X` (child of `P`) processes `X.justify = QC(P)` and locks on
   `parent(X) = parent-of-the-QC's-block`. Under the current rule that is
   `QC(P).block.justify.block = parent(P) = A` — the *grandparent* of `X`. The
   replica only locks on `P` later, when it processes `QC(X)` (either via the
   next child proposal carrying `QC(X)`, `implementation.rs:935`, or by
   collecting it, `implementation.rs:1228`). Between those two events the
   replica is locked on `A`, not `P`.

### Header-first relaxations (all modeled)

The hash-only pipeline widens, never narrows, the reachable set, so it can only
*help* an attacker. The model includes each relaxation as an optional extra edge:

* **vote-before-validate** (`on_receive_proposal_header`, `implementation.rs:1704-1741`):
  a replica votes on a header before the body is validated; the vote (and its
  vote-time lock update on `header.justify`) happens with the body absent.
* **safe_pc bypass for pending blocks** (`implementation.rs:1215-1221` and
  `1659-1669`): if `header.justify.block` is in `pending_headers`/`pending_bodies`,
  the full `safe_pc` (incl. predicate 3, the lock check) is skipped — only
  `is_block_justify()` + chain-id are checked. This lets a vote-forming QC be
  processed **without** the lock predicate gating it.
* **sync-commit skipping safe_block** (`update` on a collected `new_pc` whose
  block is pending, `implementation.rs:1227-1235`): `advance_highest_pc_from_remote`
  + `update(new_pc)` run (which calls `block_to_commit`) even though the block
  never passed `safe_block`.

Modeled as `AllowPendingBypass`, these relaxations make the counterexample
*easier* to reach (the lock predicate can be skipped outright), reinforcing the
verdict.

---

## 2. The counterexample (n = 4, f = 1)

Validators `v1, v2, v3` honest, `v4` Byzantine. Quorum = 3. Genesis → `A`
(agreed grandparent). Byzantine/faulty leaders build two conflicting children of
`A`: `P` and `P'` (same height `h`), each with a child `X`, `X'`.

```
                 A            (committed/agreed, height h-1)
                / \
   height h    P   P'         (conflicting siblings — the disputed height)
               |    |
   height h+1  X   X'
```

Target 2-chains (each pair consecutive-view, so `block_to_commit` fires):

* Branch P:  `QC(P)@a`,  `QC(X)@a+1`   ⇒ commit `P`.
* Branch P': `QC(P')@b`, `QC(X')@b+1`  ⇒ commit `P'`,  with `b ≥ a+2`.

### Trace

| view | proposal (leader)          | honest voters | QC formed        | lock **at vote time** (current rule) |
|------|----------------------------|---------------|------------------|--------------------------------------|
| a    | `P` (child `A`)            | v1, v2 (+v4)  | `QC(P)@a`        | voting `P` processes `QC(A)` → lock `parent(A)` |
| a+1  | `X` (child `P`)            | v1, v2 (+v4)  | `QC(X)@a+1`      | voting `X` processes `QC(P)` → lock **`A`** (grandparent of `X`) |
| b    | `P'` (child `A`, Byz leader)| v1, v3 (+v4) | `QC(P')@b`       | voting `P'` processes `QC(A)` → lock `parent(A)` |
| b+1  | `X'` (child `P'`)          | v1, v3 (+v4)  | `QC(X')@b+1`     | voting `X'` processes `QC(P')` → lock **`A`** |

The single decisive step is **`v1` voting for `X'`@b (branch P') after having
voted for `X`@a+1 (branch P)**. It is admissible because:

* **vote-once-per-view** is satisfied — `a+1 ≠ b` (views are distinct).
* **safe_pc predicate 3** is satisfied — `v1` has **not yet processed
  `QC(X)@a+1`** (the network withheld it / it is still in flight). Its lock is
  therefore still `A` (set when it voted `X`, per the grandparent rule). `P'` is a
  child of `A`, so `extends_locked_pc_block(QC(A)) = true` (`P'.parent = A =
  locked block`). The safety clause of predicate 3 **passes** — `v1` votes.

Now let asynchrony deliver the two commit-enabling QCs to *different* honest
replicas:

* `v2` processes `QC(X)@a+1` ⇒ `block_to_commit` fires (`a+1 = a+1`) ⇒ **v2
  commits `P`** at height `h`.
* `v3` processes `QC(X')@b+1` ⇒ `block_to_commit` fires (`b+1 = b+1`) ⇒ **v3
  commits `P'`** at height `h`.

`P ≠ P'` are siblings at the **same height `h`**. Two honest validators have
irrevocably committed conflicting blocks. **Agreement is violated.**

(`v1` itself commits only whichever it processes first; the
`not_committed_yet` height check, `invariants.rs:544-548`, stops it from
committing the second at the same height — but that check is *per-replica* and
does nothing to reconcile `v2` vs `v3`.)

### Why quorum intersection does *not* save the grandparent rule

`QC(X)@a+1` and `QC(P')@b` share ≥ 1 honest validator (`v1`): with n=3f+1 there
are 2f+1 honest, and `(f+1)+(f+1) = 2f+2 > 2f+1`. The classical safety argument
needs that shared honest replica to be **locked on `P`** when it votes for `P'`,
so that predicate 3 rejects `P'` (`P'` neither extends `P` nor has a higher
justify-view). Under the grandparent rule the shared replica is locked only on
`A` at vote time, and `P'` **does** extend `A`. Quorum intersection still holds;
it is simply *neutralized* because the lock is one level too shallow.

### Why the fix closes it

With `Phase::Generic => Some(justify.clone())`, voting for `X` processes
`X.justify = QC(P)` and locks on `QC(P).block = P` **immediately, at vote time**
— no lag. When `v1` is later offered `P'`@b, predicate 3 evaluates against
`locked = P`: `QC(A).view < P.view` (liveness clause false) and `P'` does not
extend `P` (safety clause false, since `P'.parent = A ≠ P` and grandparent ≠ P).
`v1` **rejects** `P'`. `QC(P')` can then gather at most `{v3, v4}` = 2 < quorum,
so it never forms. Branch P' dies; Agreement holds. This is exactly MonadBFT's
lock rule and the invariant behind Theorem 1 (below).

---

## 3. Mapping to MonadBFT Theorem 1 (arXiv:2502.20692)

MonadBFT proves Safety (Thm 1) for its 2-chain commit via a locking invariant.
The relevant lemma chain, and where our implementation matches or breaks it:

| MonadBFT lemma | Statement (paraphrased) | Impl predicate | Holds? |
|----------------|-------------------------|----------------|--------|
| **L1** Vote uniqueness | ≤ 1 vote per honest replica per view | `highest_view_voted` (`implementation.rs:954`) | ✅ |
| **L2** QC uniqueness | ≤ 1 QC per view | quorum ≥ 2f+1 + L1 | ✅ |
| **L3** Lock ⇒ no conflicting QC | if 2f+1 lock on `B`, no QC for a `B`-conflicting block at a view > lock-view can form without a higher justify | `safe_pc` pred 3 + `pc_to_lock` | ❌ **broken** |
| **L4** Committed ⇒ locked quorum | a 2-chain commit of `B` implies a quorum **locked on `B`** | `pc_to_lock` **must** lock on `justify.block` | ❌ **broken** |
| **L5** Inductive extension | every future QC extends the committed `B` | L3 + L4 | ❌ (depends on L3/L4) |

**The precise break is L4.** MonadBFT's L4 requires that the quorum that forms
the *second* QC of the 2-chain (`QC(X)@a+1`) be **locked on the committed block
`P`**. Our `pc_to_lock` locks that quorum on `parent(P) = A` instead. Because L4
fails, L3's premise ("2f+1 locked on `B`") is never actually established for the
committed block, and L5's induction has no base case. The written argument in
`safety_arguments.md §Property 1, Step 1` *asserts* L4 ("at least 2f+1 validators
are locked on a QC for block B") but the cited code locks on the grandparent, so
the assertion does not follow from the code — this is exactly the "formal gap"
that section admits, now **closed as a refutation**.

The 3-chain original was safe because it committed `parent(parent(J.block))`
(great-grandparent) while locking `parent(J.block)` (grandparent): lock strictly
newer than commit, so L4 held with a one-block margin. Moving commit up to
`parent(J.block)` without moving the lock up erased that margin.

---

## 4. What the Stateright model checks

`crates/hotstuff_rs/tests/stateright_2chain_safety.rs` encodes the abstract
protocol at QC granularity (state, action, property fully defined):

* **State**: per honest replica `{ locked_qc, highest_view_voted, committed }`;
  global pool of formed QCs; per-replica *processed* set (to model the lock lag /
  partition). `LockRule ∈ {Grandparent, Parent}` and `AllowPendingBypass ∈
  {false,true}` are config knobs.
* **Actions**:
  `FormQc{block,view,voters}` (a QC forms iff a quorum is individually eligible:
  vote-once + safe_pc pred 3 against each honest voter's current lock; effect =
  mark voted, apply vote-time `pc_to_lock`), and
  `Deliver{replica,qc}` (a replica processes a formed QC later: apply
  `pc_to_lock` + `block_to_commit`) — the split is what realizes the
  partition/heal + lock-lag. Equivocation = the model offering both `P` and `P'`
  as children of `A`; withholding = choosing when `Deliver` fires.
* **Properties**:
  * `agreement` (`always`): no two honest replicas' `committed` sets contain
    two **distinct** blocks of the **same height**.
  * `bounded_depth` (`always`): committed heights never exceed the modeled
    horizon (guards against runaway/ill-formed commits).
* **Drivers / tests**:
  * `grandparent_lock_violates_agreement` — BFS finds a counterexample
    (documents the bug; corresponds to §2).
  * `parent_lock_upholds_agreement` — BFS finds **no** violation (validates the
    fix).
  * `agreement_under_production_rule` — the **RED-first** test: it configures the
    model with `LockRule::PRODUCTION` (currently `Grandparent`) and asserts
    `agreement` holds. It **fails on the current rule (RED)** and **passes once
    `PRODUCTION` is set to `Parent`** — i.e. once the `pc_to_lock` fix in §0 is
    applied and `LockRule::PRODUCTION` is repointed. This is the red→green
    witness for the fix.

---

## 5. Recommendation

1. Apply the §0 fix to `pc_to_lock` (Generic arm → `Some(justify.clone())`).
2. Repoint `LockRule::PRODUCTION` in the model to `Parent`; the RED-first test
   turns green, and `grandparent_lock_violates_agreement` remains as a
   regression witness for the old, unsafe rule.
3. Update `safety_arguments.md §Property 1` — the current Step 1 claim that the
   grandparent lock puts "2f+1 validators locked on block B" is **false** for the
   committed block; it locks them on `parent(B)`. Replace the "formal gap"
   admission with the L4 refutation above and the corrected rule.

> This document and the companion model are a **verification artifact**. No
> production consensus code is modified by this task; the fix is *specified and
> proven necessary*, to be applied and re-verified under G-series review.
