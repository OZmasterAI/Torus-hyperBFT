# Hash-only native mirror (designed follow-up — MUST land before the mainnet preimage freeze)

Status: **DESIGNED, deliberately NOT implemented** in the deep-book storage
round (perf/deep-book-storage). This is the second half of swarm finding #8.

## Problem

`CF_NATIVE_HASHED` (the bucket-ordered mirror that makes the incremental
native root O(changed)) stores every native-root entry's **full value** a
second time:

- every native write is stored twice (CF + mirror) — write bytes ×2 for ALL
  six `NATIVE_ROOT_CFS`, not just books;
- `compute_native_dirty_ops` prefix-reads each changed bucket's **full
  values** back from the mirror and keccaks the whole bucket **including the
  full values** every block.

The deep-book round already collapsed the dominant case (order books) by
making the CF itself per-order rows, so each mirror entry is now ~130 B.
Remaining duplication: balances (~50 B), positions (~120 B), oracle,
staking — real but small per entry.

## Design

Change the mirror value from `value` to `keccak256(value)` (32 B), and define
the bucket leaf over per-entry hashes:

```
leaf = keccak( for each (cf_tag, key, value_hash) in bucket, canonical order:
                 cf_tag(1) ‖ len(key) u32 LE ‖ key ‖ 32 u32 LE ‖ value_hash )
```

i.e. reuse `frame_entry` unchanged with `value_hash` in the value position.

- `bucket_id`, tree shape, node keys, default nodes: unchanged.
- `build_native_trie_to_cf` / `leaves_from_db` / `compute_native_dirty_ops`:
  hash each value before framing/mirror-put. Bucket recompute no longer
  rereads full values — the mirror row IS the hash.
- `native_root_full` (the oracle) must apply the same hashing so oracle and
  incremental path stay a single preimage.

## Why it is NOT in the deep-book round

- It is a **second state-root preimage change** with blast radius across all
  six native CFs, landing in the same round as the book re-keying would have
  doubled the verification surface of an already consensus-critical change.
- The measured pain (Round-2 endurance L2) was book-value duplication, which
  per-order rows already eliminated; the residual mirror overhead did not
  show up as a wall in the deep-book proof legs (flush 7.8 ms/blk at 16 k
  resting depth, down from 52.7 ms).

## Constraint

Both this and the per-order-row keying are preimage changes. **Ideally the
LAST preimage change before mainnet freezes the preimage.** Land this (or
explicitly reject it) before that freeze; after freeze the mirror layout is
consensus-locked forever. Deploy class: coordinated fleet deploy on a fresh
chain / offline rebuild (`build_native_trie_to_cf` regenerates the mirror
from the CFs, so migration is a one-shot rebuild, no data transform).

## Test plan (when implemented)

- Same A2.1/A2.2 gates: incremental == `native_root_full` after every kind of
  change and over sequential evolving commits (native_trie.rs tests pass with
  the new framing unchanged in structure).
- A/B leg: mirror write-bytes and flush ms/blk on the L2 depth cell and on a
  balances-heavy (cross) cell.
