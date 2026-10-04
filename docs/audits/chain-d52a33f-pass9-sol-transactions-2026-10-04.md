# Pass 9 — ordinary transaction lifecycle and pool accounting

Reviewed 2026-10-04 on `perf/item6-phase1`, HEAD
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`. Scope: valid native/EVM
admission, ordinary proposal selection, nonce/replacement bookkeeping,
execution versus skipping, and ordinary client visibility. No production edits,
Git mutations, network requests, live-chain actions, key generation, toolchain
installation, or Rust tests were performed. Cargo/rustc are unavailable under
the parent task's constraints. Parent owns Torus persistence. No applicable
`AGENTS.md` was found in the repository or its ancestors.

The [audit policy](README.md), September Astra rounds
[one](astra-round1-2026-09-24.md) and
[two](astra-round2-2026-09-24.md), pass-three
[consolidation](chain-cea1254-pass3-project-wide-2026-10-04.md) and
[surface report](chain-cea1254-pass3-surface-2026-10-04.md), pass-four
[consolidation](chain-cea1254-pass4-missed-issues-2026-10-04.md) and
[atomicity report](chain-cea1254-pass4-atomicity-2026-10-04.md), pass-six
[correctness report](chain-cea1254-pass6-correctness-review-2026-10-04.md),
pass-seven [RPC review](chain-cea1254-pass7-sol-rpc-2026-10-04.md) and
[consolidation](chain-cea1254-pass7-mixed-review-2026-10-04.md), and pass-eight
[consolidation](chain-d52a33f-pass8-mixed-review-2026-10-04.md) and
[compatibility review](chain-d52a33f-pass8-astra-compatibility-2026-10-04.md)
were consulted for exclusions. The September raw 271-finding exclusion files
referenced under `/tmp/torus-astra-exploration-20260924/` are absent here;
deduplication is against the available reports and current source.

**Result: one additional P2 static candidate, local T01, mapped by the parent
to F39.** There is no runtime reproduction or verified fix. Prior R2/R3,
F17/F20/F22/F26/F32/F33/F37 and other F01–F37 findings are not counted again.
The blocked pass-five certificate, malformed-input and adversarial scopes were
not resumed.

## T01 / F39 — EVM byte-cap admission uses gross and stale occupancy

The [configured budget](../../crates/torus-mempool/src/lib.rs#L84) is a maximum
pool-memory setting; its actual [counter](../../crates/torus-mempool/src/lib.rs#L413)
tracks retained raw EVM envelope lengths. It is not a measurement of process
RSS. The decoded entry and its copied raw bytes already exist before the
[capacity check](../../crates/torus-mempool/src/lib.rs#L417), so this check does
not reserve the peak temporary memory required to receive/decode a transaction.
The candidate below uses the retained-byte interpretation of that setting.

Admission validates a signed transaction, then rejects when
`memory_used + new_raw_len > max_memory_bytes`, before acquiring the EVM pool
write lock. Only later does [insertion](../../crates/torus-mempool/src/lib.rs#L430)
discover the bytes that a replacement or count-cap eviction frees. The inner
pool explicitly [replaces the same sender/nonce](../../crates/torus-mempool/src/evm_pool.rs#L139),
records the old raw length at line 155, and skips count-cap checks for that
replacement. For a nonreplacement at the transaction-count cap, it
[evicts a cheaper entry](../../crates/torus-mempool/src/evm_pool.rs#L180)
and reports its freed length. The outer counter
[subtracts those bytes](../../crates/torus-mempool/src/lib.rs#L437) on successful
insertion. Those decrements are present and correct for this narrow question;
the issue is the earlier admission decision.

A normal fee bump therefore can fail even when the resulting retained pool
fits the budget. Let retained bytes be `R`, capacity `C`, old raw length `O`,
and replacement raw length `N`. For `R + N > C` but `R - O + N <= C`, the code
returns `PoolFull` before testing the otherwise sufficient fee bump. The old
transaction remains pooled; the replacement never enters the pool. A small
cancellation/replacement of a larger pending contract call is an ordinary
example. A full-count pool has the equivalent false rejection when an eligible
eviction would free enough bytes. This is an availability/selection-contract
candidate, not a claim that an accepted replacement loses either transaction.

The same pre-lock check is not an atomic retained-byte reservation. With room
for one new envelope of size `N`, two valid transactions from different funded
senders can both read `R = C - N` and pass. Their insertions and counter updates
then serialize under the write lock without another budget check; if neither
replaces or evicts, retained bytes become `C + N`. Stronger atomic ordering on
the load alone would not close this interleaving. The shipped
[four-worker RPC runtime](../../crates/torus-node/src/main.rs#L1087) calls the
shared [admission method](../../crates/torus-rpc/src/eth.rs#L710), as does the
separate [forwarded-EVM consumer](../../crates/torus-node/src/main.rs#L748).
Thus this is a source-supported ordinary concurrency schedule. No cap overshoot,
load, throughput, exhaustion, or process failure was measured. Count and sender
caps still bound the pool; this is not an unbounded-ingress claim like F15/N1.

### Reachability and existing limits

The [defaults](../../crates/torus-mempool/src/lib.rs#L110) are 4,096 EVM entries,
16 per sender, a 10% replacement fee bump, and 64 MiB retained-byte capacity
(line 122). The [shipped node configuration](../../crates/torus-node/src/main.rs#L575)
overrides chain ID, block gas limit and native ingress collateral, and inherits
those EVM limits. The [default EVM proposal gas budget](../../crates/torus-mempool/src/rate_limit.rs#L21)
is 5,000,000 and also bounds each admitted transaction's gas limit through
[admission](../../crates/torus-mempool/src/lib.rs#L397).

These limits do not make the byte boundary unreachable with normal supported
transactions: 4,096 envelopes with 20 KiB calldata would exceed 64 MiB before
envelope overhead. Such an individual call can have ample intrinsic gas within
the 5M budget; sender/nonce/funding requirements still apply. This is arithmetic
showing that bytes can bind before entry count during a sustained legitimate
backlog, not an observed production workload. Small-budget library fixtures can
exercise the exact same check without any load experiment. The node's defaults
do not expose a small-memory CLI override in this construction, and this report
does not assume such an override exists.

### Required regression and correction

The existing [replacement test](../../crates/torus-mempool/src/lib.rs#L2567)
checks underpriced rejection, successful fee replacement and pool count. The
[eviction test](../../crates/torus-mempool/src/lib.rs#L2684) checks a full
three-entry count cap. Both leave the 64 MiB byte limit far away. The
[memory test](../../crates/torus-mempool/src/lib.rs#L3129) checks increments,
drain-to-zero and subsequent admission with a 1 MiB cap; it does not cover
replacement/eviction at the byte boundary or concurrent admission. Tests were
read, not run.

Use existing fixed test signer helpers to build valid old/new envelopes. With
capacity equal to an old larger envelope's length, admit it, then submit a valid
smaller same-nonce fee bump. Assert replacement succeeds, pool count stays one,
the selected bytes are the new envelope, pending nonce is unchanged, and
`memory_used == new_raw_len`. Preserve the old entry and all indexes when the
*net* new occupancy would exceed capacity. Add the analogous full-count eviction
case and a deterministic two-caller interleaving at the capacity decision; assert
the sum of retained envelope lengths never exceeds the configured cap.

Compute candidate replacement/eviction bytes and the resulting retained total
under the same pool lock as mutation and counter updates. A failed capacity
decision must preserve every old index and transaction. Merely checking the
counter after `insert` would be too late, because insertion has already replaced
or evicted entries. No implementation or test edits were made in this pass.

## Native and EVM lifecycle contracts checked

| Stage | Current behavior and qualification |
| --- | --- |
| Native acknowledgement | [Verification](../../crates/torus-rpc/src/torus.rs#L320) checks the parsed signed action and returns `keccak(canonical JSON)`; [submission](../../crates/torus-rpc/src/torus.rs#L1375) acknowledges successful pool admission. This is an admission acknowledgement, not a business-success receipt. The distinct [content-address hash](../../crates/torus-types/src/lib.rs#L1047) used by DA/selection commits to canonical action bytes, nonce and signature; conflating these identities would be an incorrect new finding. |
| Native proposal selection | The live [selection path](../../crates/torus-consensus/src/app.rs#L5682) combines pending proposal hashes and the independent in-flight ledger. [Pool selection](../../crates/torus-mempool/src/lib.rs#L1227) keeps unselected/paced-out entries, skips excluded hashes before budget checks, and lazily expires nonce-old entries. Native selection is not the destructive EVM drain. |
| Native replay/nonce | [Execution](../../crates/torus-consensus/src/app.rs#L2078) resolves each sender and checks `(sender, nonce)` against the overlay and a block-local set. A reused nonce is skipped; a dispatched action consumes its nonce even if its native handler returns a business error. Native pool dedup is by signed action identity, not an Ethereum-style nonce replacement rule. No guarantee of executing two different actions with the same native nonce is inferred. |
| Native status | The [bitmap](../../crates/torus-consensus/src/app.rs#L2129) is decided before handler dispatch. [Business results](../../crates/torus-bridge/src/native_executor.rs#L47) have `success` and `error`, but the application [discards batch return values](../../crates/torus-consensus/src/app.rs#L2251). Accordingly `nativeActionStatus = executed` means dispatched past signature/replay gates, and does not prove an order rested, a transfer succeeded, or every member of a PlaceOrderBatch succeeded. This is a status-contract qualification; no new erroneous business-success field was established. |
| EVM admission/selection | [Admission](../../crates/torus-mempool/src/validate.rs#L31) checks supported type, chain, upper gas budget, fee floor, signer, nonce window and individual affordability. [Selection](../../crates/torus-mempool/src/evm_pool.rs#L278) walks consecutive pooled nonces and rechecks fee floor/share/gas budgets. Destructive drain, future nonce loss and missing in-flight nonce accounting remain prior R2; intrinsic-gas admission remains prior R3. |
| EVM execution/status | [Executor](../../crates/torus-evm/src/executor.rs#L296) distinguishes pre-execution transaction rejection from an executed revert/halt. Rejected transactions have no receipt; executed reverts have status-0 receipts and consume EVM nonce/gas. The [application bitmap](../../crates/torus-consensus/src/app.rs#L1807) marks receipt-bearing transactions executed, including reverted ones. Receipt success and executed status must not be treated as synonyms. |
| Ordinary client projection | [Applied frontier](../../crates/torus-rpc/src/eth.rs#L176) gates Ethereum block visibility. [Receipt-driven location indexing](../../crates/torus-bridge/src/committer.rs#L186) excludes skipped transactions. [Ethereum indices](../../crates/torus-rpc/src/eth.rs#L220) are dense over receipt-bearing transactions; [body indices](../../crates/torus-rpc/src/eth.rs#L202) retain original positions. [Native block-body status](../../crates/torus-rpc/src/torus.rs#L1596) is null before a matching execution record exists. No new ordinary index mismatch was established. |
| Pending lookup | [Pending nonce](../../crates/torus-mempool/src/lib.rs#L1338) examines executed state plus consecutive pooled nonces. [Transaction-by-hash](../../crates/torus-rpc/src/eth.rs#L724) reads only the persisted receipt/location index, with no pending-pool fallback. A valid queued hash can therefore return null even while the pending nonce includes it. This is a documented review limitation of the current mined-only projection, not another counted R2 manifestation or runtime-confirmed compatibility finding. |
| Delayed writer/lockbox | A successful EVM call may only queue its native leg. [Lockbox contract](../../crates/torus-core/src/precompiles.rs#L1071) explicitly permits next-block withdrawal failure if native balance is then insufficient, and says `true` means queued. [Queue enqueue](../../crates/torus-core/src/precompiles.rs#L1153) targets the next block; [due detection](../../crates/torus-bridge/src/native_executor.rs#L8208) runs even for an otherwise empty block. [Drain](../../crates/torus-bridge/src/native_executor.rs#L8226) returns native results, while the application discards them. Queue acknowledgement is not evidence of final native execution success. Prior F22's order handle, F37's reader index, and historical drain/storage faults are excluded. |

The [native in-flight selection test](../../crates/torus-consensus/src/app.rs#L9159)
already checks exclusion from both a full proposal and the missing-body ledger,
then releases only the full proposal. The
[native replay test](../../crates/torus-consensus/src/app.rs#L11711) checks one
consumed nonce across two inclusions of the same batch. The
[block-body RPC test](../../crates/torus-rpc/src/lib.rs#L3112) manually installs
status records and checks executed/skipped/null/empty projections. The
[Ethereum-view test](../../crates/torus-rpc/src/lib.rs#L3200) uses real bridge
execution for success/skipped/revert and checks dense indices and the applied
frontier, while manually installing the application bitmap/marker. These are
meaningful existing safeguards, not full end-to-end admission-to-application
regressions.

Additional ordinary regression coverage should submit valid native actions
through the actual RPC/pool/proposer/application path and distinguish dispatch
from handler success, including a batch with one insufficient-margin member.
For delayed writers, compare the enqueue receipt at height H with actual native
state after H+1, both for a successful action and an ordinary insufficient-balance
withdrawal. Pin the intended pending-hash lookup contract separately. None of
these scenarios was executed here, and none is promoted as a second new defect.

This bounded pass ends with T01/F39 and the stated contract/test qualifications.
It neither closes the previous candidates nor claims exhaustive transaction,
client, resource, or cross-VM correctness.
