# Trace 6: Adversarial — Byzantine Leader Attempts to Commit Un-proposed Block

**Property**: P2 (Safety — Validity): If a correct validator irrevocably
commits block B at height h, then B was proposed by some leader.

**Setup**: n=4, f=1. V1 (honest), V2 (Byzantine), V3 (honest), V4 (honest).

## Attack Goal

V2 tries to get an honest validator to irrevocably commit a block that was
never included in a valid Proposal message from a legitimate leader.

## Attack Vector 1: Forge a block directly into the block tree

### Attempt

V2 crafts a raw block B* and tries to get it inserted into honest
validators' block trees WITHOUT a Proposal message.

### Why it fails

Blocks enter the block tree through exactly ONE path:
`on_receive_proposal()` at `implementation.rs:627-631`.

Before insertion, the code checks at `implementation.rs:419-437`:
```
if matches!(msg, HotStuffMessage::Proposal(_)) || matches!(msg, HotStuffMessage::Nudge(_)) {
    if !is_proposer_with_reputation(origin, self.view_info.view, ...) {
        return Ok(());  // silently drop
    }
}
```

V2 cannot bypass this because:
1. All messages are received via `on_receive_msg` which dispatches by type
2. Only `Proposal` and `Nudge` messages trigger block insertion/voting
3. The sender (`origin`) is verified against the leader schedule
4. V2 can only propose when it IS the scheduled leader

**Code anchor**: `implementation.rs:419-437` (proposer check)

## Attack Vector 2: Propose a block for a view where V2 is NOT leader

### Attempt

V2 sends `Proposal { view=3, block=B* }` to all validators, but V3 is the
actual leader of view 3.

### Why it fails

`is_proposer_with_reputation(V2, view=3, ...)` returns false because V3
is the leader of view 3. The proposal is silently dropped.

Leader selection is deterministic:
- `select_leader(view, validator_set)` at `pacemaker/implementation.rs:737-770`
  uses Interleaved Weighted Round Robin
- `select_leader_with_reputation(view, validator_set, reputation)` adjusts
  by reputation score but remains deterministic

All honest validators compute the same leader for the same view. V2 cannot
convince them it's the leader when it isn't.

**Code anchor**: `pacemaker/implementation.rs:737-770` (leader selection)

## Attack Vector 3: Propose a block with forged justify QC

### Attempt

V2 IS the leader of view 2. V2 proposes block B* with a fabricated justify
QC (claiming a quorum voted for B*'s parent when they didn't).

### Why it fails

`proposal.block.is_correct(block_tree)?` at `implementation.rs:597` verifies
the block's structural integrity, including the justify QC.

The QC correctness check at `types.rs:48-188`:
1. Verifies signature count matches validator set size
2. Verifies each signature individually against the signer's public key
3. Tallies voting power and checks quorum

V2 cannot forge honest validators' Ed25519 signatures (assumption: signature
unforgeability). With only 1 Byzantine validator (f=1), V2 can contribute
at most 1 valid signature. Quorum requires 3 (2f+1). The forged QC fails
`is_correctly_signed` at `types.rs:149-188`.

**Code anchor**: `types.rs:149-188` (signature verification)

## Attack Vector 4: Manipulate block sync to inject blocks

### Attempt

V2 exploits the block sync protocol to insert B* into honest validators'
block trees without going through `on_receive_proposal`.

### Why it fails

Block sync responses contain `highest_pc` which is validated via
`is_correct()` before use. Sync-provided blocks must still be justified
by a valid QC. The sync path ultimately calls the same block tree update
logic that requires `safe_pc` checks.

Even if a block enters via sync, it can only be COMMITTED through
`block_to_commit()` at `invariants.rs:491-573`, which requires a valid
2-chain of consecutive QCs. Forging two consecutive QCs requires forging
two sets of 2f+1 signatures — impossible with only f Byzantine validators.

## Attack Vector 5: Equivocate to get an "un-proposed" block committed

### Attempt

V2 proposes B in view 2 to validators V1, V3. V2 proposes B' in view 2
to V4. V2 hopes that through some chain of reproposals, B gets committed
even though it was never "proposed" to V4.

### Why it partially succeeds (but Validity still holds)

V4 never saw B, but if B gets a QC (via V1, V3, V2's votes), and the
2-chain completes, V4 will commit B when it processes the relevant QCs.
V4 commits a block it never directly received in a Proposal — but it was
still PROPOSED by V2 (just not to V4). The Validity property says
"proposed by SOME leader," not "proposed to THIS validator."

The block exists in V4's tree because:
- QC_B is received via NewView or AdvanceView from other validators
- `block_tree.update(&QC_B)` is called
- But `safe_pc` predicate 2 requires `block_tree.contains(&QC_B.block)`
  If V4 doesn't have the block, safe_pc returns false. So V4 cannot
  process the QC until it gets the block via sync.
- Block sync retrieves B from other validators, inserting it into V4's tree
- Only THEN can V4 process the QC and eventually commit B

At every stage, the block was originally proposed by leader V2.

**Validity holds**: B was proposed by V2 (a leader). Even though V4 got it
via sync rather than directly, the block was still produced by a leader.

## Conclusion

There is no path for a block to be irrevocably committed without having
been originally proposed by a leader:
1. Block insertion requires `on_receive_proposal` → proposer check
2. QC formation requires 2f+1 valid signatures → cannot forge
3. Commit requires valid 2-chain → two consecutive valid QCs
4. Block sync provides blocks but they still need valid QCs to commit
5. Even equivocation doesn't create "un-proposed" blocks — both B and B'
   were proposed by the equivocating leader

The property is enforced by the single insertion path + cryptographic
signature verification. No combination of Byzantine behavior can produce
a committed block that wasn't proposed by a leader.
