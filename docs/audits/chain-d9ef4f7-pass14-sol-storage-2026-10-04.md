# Pass 14 — storage schemas, cold reconstruction and shutdown ownership

Reviewed 2026-10-04 using GPT-6.1-sol against read-only source
`merge/item6-c3-pf1` at `d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd`.
This report is written to the separate documentation worktree on
`audit/chain-findings-2026-10-04`; source links pin the full reviewed SHA.

**Result: no additional numbered finding established.** This review examined
ordinary database open, actual row codecs and format transitions, level-book
reconstruction, intact snapshot restoration and the ownership chain that drains
execution on Ctrl+C. It adds concrete positive contracts and test boundaries;
it does not close the existing persistence findings or certify the chain.

The [audit index](README.md), [pass 13 synthesis](chain-d9ef4f7-pass13-project-wide-2026-10-04.md),
[pass 13 lifecycle](chain-d9ef4f7-pass13-astra-lifecycle-2026-10-04.md),
[pass 9 storage](chain-d52a33f-pass9-astra-storage-2026-10-04.md) and
[pass 8 integration](chain-d52a33f-pass8-sol-integration-2026-10-04.md)
were checked for duplication. No applicable `AGENTS.md` was found. Rust tests
were read, not run; Cargo/rustc are unavailable in this environment. No source,
Git, Torus, service, key or live-chain mutation was performed. Only this report
was written. Interrupted pass-5 certificate, malformed-input and adversarial
network assignments were not resumed.

## Database registration is not a general schema migration

Writable [open](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/db.rs#L348)
permits creating the database and missing column families, then opens the
registered [family list](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/cf.rs#L238).
That creates containers; it does not transform old values into current codecs.
The actual contracts differ by data type:

| Data | Current durable representation and compatibility boundary |
| --- | --- |
| EVM account/storage | Account value is exactly 72 bytes: balance, nonce and code hash; storage key is address plus a 32-byte slot, and zero values are deleted. Code bytes live separately. |
| Native position/balance | Both carry schema version 1; incompatible historical encodings are explicitly described as requiring recreation before launch. |
| Books | Whole-book blobs, per-order rows and level-authority rows are distinct layouts; changing the writer mode requires fresh genesis. Modes 2/3 share row shapes but differ in level-hash preimages. |
| Native trade history | Format marker 2 gates the packed-row representation. Startup intentionally deletes old-format history rather than translating it. This is node-local history. |

Sources: [account codec](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/db.rs#L1010),
[zero storage deletion](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/db.rs#L757),
[position version](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/position.rs#L81),
[balance version](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/position.rs#L161),
[book mode contract](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L1254).
No supported ordinary binary-upgrade sequence was established to violate these
declared boundaries. Absence of a universal migration engine is therefore not
promoted to a defect. F18's separate bytecode identity issue remains open.

Plain EVM state and its derived mirrors also have distinct ownership. The
[incremental root builder](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/incremental.rs#L226)
computes against the committed trie plus a bundle overlay without writing the
database. Its bundle persistence writes plain account/storage rows separately
from [keccak-ordered mirror updates](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/incremental.rs#L313).
The hashed representation drops EIP-161 empty accounts and wipes their hashed
storage prefix, while the plain account codec contains no cached storage root.
A root comparison therefore needs the correct semantic representation; exact
row equality across different families is not the contract. A useful ordinary
fixture transfers an account's remaining balance, changes a nonzero storage
slot to zero through a valid contract call, and compares full recomputation
with the maintained root after persistence/reopen. No reachable new storage
wipe failure was established here. This does not replace F30's already
reported native-state/mirror ordering issue.

The history transition has a real production caller:
[`TorusApp` construction](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L3855)
checks the format before replay. The
[migration helper](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/trade_rows.rs#L222)
deletes ranges in both history families and writes the marker in one batch.
For an ordinary upgrade with an absent old marker, losing old fills is the
documented [rollout policy](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/docs/plans/packed-trade-rows-impl.md#L6).
The control case is a current-format reopen: marker equality skips deletion.
The [helper test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/trade_rows.rs#L406)
asserts old rows disappear, unrelated metadata survives, and new rows survive
the next check. The [application test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L8921)
calls the real constructor and checks the wipe/marker. Neither closes RocksDB
between checks; a true cold reopen remains useful coverage.

## Canonical row order and cold book reconstruction

The [book codec](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/book_rows.rs#L9)
uses length/tag-disjoint keys. Level prices use sign-flipped big-endian i128,
complemented for bids, so forward iteration is best-price-first on each side.
Metadata requires the exact length selected by its last-price tag. The
[codec tests](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/book_rows.rs#L288)
assert round trips and both ordering directions across signed extremes;
adjacent tests assert exact meta/level lengths. These are codec assertions,
not signed full-node executions.

At actual execution-context construction, a cold holder selects the
[configured loader](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L2836).
Level authority does not treat the node-local full-order store as sufficient
by itself. It rebuilds books, then compares exact
[metadata, stop and level rows](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L3464)
against root-family bytes. The exact mode marker is also checked, including
when no resting orders exist. The persisted global-ID counter is combined
with reconstructed maxima; an ordinary drained book does not deliberately
reset the allocator on restart.

The ordinary control sequence is place several orders, partially match/cancel,
save, reconstruct under the same mode and continue. The
[differential test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/tests/level_rows_tests.rs#L265)
asserts equal non-book state and reconstructed books across modes 0/1/2,
zero writes on idle saves, presence of mode-2 local rows, and different roots
for different persisted layouts. Its
[mode-3 comparison](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/tests/level_rows_tests.rs#L333)
requires identical local-order bytes, metadata/stops and quantity/count fields,
while level hashes and final roots differ. This supports the declared mode
boundary. The script reconstructs contexts against retained DB handles;
“reload” is not proof of a complete database close.

Packed [trade encoding](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/trade_rows.rs#L165)
also preserves ordinary fill order: stable market/address grouping retains
trade-index order, complemented user-block keys expose newest blocks first,
and maker precedes taker for a self-trade. The
[row assertions](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/trade_rows.rs#L332)
check exact decoded entries, row sizes, 1,024-fill chunk boundaries and role
order. Identical inputs must yield identical bytes. No new index/key collision
was established for ordinary produced rows. F40's separate pagination limit
is not repaired by a canonical newest-first encoding.

## Intact snapshot copy and commitment are separate contracts

The [checkpoint](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/snapshot.rs#L78)
contains the database families; metadata is supplied by its caller. The wired
[node restore option](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-node/src/main.rs#L513)
verifies/copies before opening the writable node database. A successful intact
restore does not selectively omit node-local book rows or authoritative
families excluded from the root.

Verification recomputes an EVM root plus the
[seven native-root families](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/native_trie.rs#L47).
This retains F21's commitment limitation. Book reconstruction's additional
level-byte checks strengthen its particular local store contract; they do not
extend the snapshot root to sessions, replay nonces, governance, queue or
reward families. No new root-coverage number is assigned.

The [valid restore test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/snapshot.rs#L414)
really drops its source DB before restoring to a fresh destination and checks
one balance/nonce. It does not establish replacement of a populated target,
all-family equality or resumed execution. The previously reported directory
swap interruption is not re-audited here. Likewise, no active production
periodic-export caller was established; library `SnapshotManager` tests do
not show an enabled node snapshot scheduler.

For a stronger ordinary round trip, keep identical genesis and book settings,
and export only after execution and deferred writes are drained. Populate
positions, balances, an unfilled order, a pending stop, reward/delegation state,
replay nonces and a queued next-height action through supported callers.
Restore over a closed destination, compare exact family dumps and persisted
markers, then execute the next block and compare outputs with an uninterrupted
control. This checks successful copy/reconstruction without conflating the
composite root with omitted authoritative rows. A root-only assertion could
pass while the proposed all-family assertion fails; require both. Snapshot
metadata alone does not show those ordinary callers ran.

## The ordinary Ctrl+C drain has an ownership chain

The node retains its
[replica handle](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-node/src/main.rs#L905)
while awaiting [Ctrl+C](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-node/src/main.rs#L1259).
Returning from `run` drops it. Contrary to a possible detached-handle concern,
[`Replica::drop`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/hotstuff_rs/src/replica.rs#L675)
sends shutdown and joins the algorithm and other replica threads.
[`TorusApp::drop`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L4644)
closes the execution sender and joins execution, whose loop receives queued
messages until disconnection. Dropping its context then invokes the
[flush worker join](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/exec_pipeline.rs#L267)
and [history-writer join](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/bg_writer.rs#L304).
That is a source-supported healthy drain path; it is not an observed bounded
whole-process shutdown or power-loss durability test.

The [ordinary signed-fill test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L11433)
funds through `TransferToPerp`, executes crossing orders and drops the execution
context before requiring the exact market history row. The
[writer test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/bg_writer.rs#L587)
queues five batches and checks every row after Drop. These establish useful
component assertions, with an open DB remaining for reads.

The TERM runbook versus Ctrl+C subscription is already qualified in pass 8.
It is not promoted again, and absence of a TERM drain is not proof of lost
canonical state: durable committed inputs and replay have separate protection.
The focused remaining regression is a provisioned node test with ordinary
in-flight execution/history work, actual signal exit, all owners released,
same-configuration reopen and continued execution. Assert exact rows, applied
height, allocator continuation and no duplicate credits, alongside a quiescent
control. Existing F19/F30/F31/F38 remain distinct open seams; this pass adds no
runtime reproduction, fix or passing-test claim.
