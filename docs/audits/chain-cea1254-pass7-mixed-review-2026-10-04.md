# Chain audit pass 7 — two Sol 6.1 and two Astra reviews

Date: 2026-10-04. Branch: `merge/item6-sync2`. Revision:
`cea1254e34625e6b09c58f794de8793b5c12713c`.

This pass reviews ordinary RPC/client behavior, governance/staking configuration,
accounting, and execution consistency across the project. It compares current
source with earlier findings rather than limiting review to branch commits.
The interrupted pass-five scopes were not resumed. No production fixes, toolchain
installation, live-chain actions, commits, pulls or pushes were performed.

**Result:** both GPT-6.1-sol reviews and both GPT-6-astra reviews completed and
saved reports. Three additional P2 source-supported candidates are recorded
below; neither Astra review promoted an additional defect. As required by the audit
[README](README.md), candidates require failing runtime regressions before being
treated as reproduced defects. Cargo/rustc are unavailable; tests were read,
not run.

## Reports and coverage

| Model | Scope | Report |
| --- | --- | --- |
| GPT-6.1-sol | RPC, wallet, explorer, subscriptions | [RPC/client review](chain-cea1254-pass7-sol-rpc-2026-10-04.md) |
| GPT-6.1-sol | Governance, staking, genesis configuration | [Governance review](chain-cea1254-pass7-sol-governance-2026-10-04.md) |
| GPT-6-astra | Accounting, orders, settlement, CoreWriter contracts | [Accounting review](chain-cea1254-pass7-astra-accounting-2026-10-04.md) |
| GPT-6-astra | Execution-mode consistency and restart | [Execution review](chain-cea1254-pass7-astra-execution-2026-10-04.md) |

The parent independently checked the principal source paths for F32–F34 and
reconciled them against the previous numbered findings. Detailed reports retain
local Q/G identifiers; the table below assigns the continuing audit identifiers.
Historical candidates and additional consequences of the same root cause are
not counted again.

## Additional candidates

| ID | Priority | Result and ordinary precondition | Detailed evidence |
| --- | --- | --- | --- |
| F32 | P2 | Wallet queries disagree with the node's wire schema. `Positions` skips every market because it expects numeric `market_id`/`id`, while the node returns hex-string `marketId`. A trader with an open position receives an empty list. Related human validator/epoch/balance output assumes incorrect fields/types. | [Q01](chain-cea1254-pass7-sol-rpc-2026-10-04.md#q01--p2-wallet-query-commands-do-not-consume-the-shipped-node-schema) |
| F33 | P2 | The shipped node accepts Ethereum log subscriptions but has no producer for their broadcast channel. Normal contract receipt logs have no corresponding subscription publication. An idle disconnected receiver also waits without observing sink closure, retaining its active slot. | [Q02](chain-cea1254-pass7-sol-rpc-2026-10-04.md#q02--p2-accepted-ethereum-log-subscriptions-have-no-shipped-production-publisher) |
| F34 | P2 | Fresh genesis accepts staking settings that do not reach runtime rules. Shipped genesis sets 604,800 unbonding blocks, while normal undelegation uses a compiled 302,400. Accepted permanent APY/multiplier and commission bounds share the configuration disconnect. | [G01](chain-cea1254-pass7-sol-governance-2026-10-04.md#g01--accepted-genesis-staking-settings-are-disconnected-from-effective-staking-rules) |

F32's principal contract is directly visible between the wallet's
[market lookup](../../tools/wallet/src/commands/query.rs#L258), the node's
[serialized market type](../../crates/torus-rpc/src/types.rs#L310), and its
[hex ID producer](../../crates/torus-rpc/src/torus.rs#L1105). Correcting only a
key name is insufficient: preserve the wire ID and enumerate all market pages.
Use serialized node response fixtures in wallet command regressions.

F33's receiver is in [eth.rs](../../crates/torus-rpc/src/eth.rs#L1250); the
[notifier](../../crates/torus-rpc/src/lib.rs#L118) creates the channel but only
exposes implemented notification methods for heads, pending transactions and
native fills. Whole-crate source search found no publication to `new_logs`.
Test the real application-to-RPC path and quiet unsubscribe/disconnect, not only
a direct synthetic send. Topic filtering is also absent from the dormant path;
it is a required follow-up assertion, not a fourth counted delivery failure.

For F34, the [shipped setting](../../devnet/genesis.json#L42),
[runtime constant](../../crates/torus-economics/src/types.rs#L335), and
[release-height assignment](../../crates/torus-economics/src/staking.rs#L188)
show the discrepancy. At undelegation height 100, runtime maturity is 302,500
instead of the configured 604,900. Support the accepted configuration or reject
unsupported nondefault settings and align the fixtures. This concerns fresh
initialization, separately from F19 restart selection and F27 governance writes.

## Corrections and safeguards established by this pass

- **Accounting coverage is substantive.** Exact-value tests already cover PnL
  surviving later reservation release, independently specified fill effects,
  and persisted position tombstones. Settlement-cache flush failures also have
  a fatal latch checked by the application. These do not close the separately
  traced F04/F30 fault paths or F02/F17 fee findings. The
  [Astra accounting report](chain-cea1254-pass7-astra-accounting-2026-10-04.md)
  identifies the existing tests and useful extensions precisely.
- **CoreWriter acknowledgement has a defined phase.** Successful enqueue,
  native action execution, order disposition and actual fills are separate
  assertions. Bool/zero enqueue returns do not promise completed settlement;
  the wrong returned order ID remains the distinct F22 issue.
- **Ethereum reads have an applied frontier.** Current block projections cap
  their header selection at the applied marker and enumerate executed receipts.
  That does not implement historical account-state snapshots or make a query
  ending at a future height complete. See the RPC report's qualifications.
- **Explorer retries missing ranges on later heads.** Earlier blanket claims
  of no reconciliation are too broad. The quiet final-block/application race,
  reconnect timing and F26 partial-row repair remain narrower concerns.
- **Unbonding has a completion path.** ClaimUnbonded has production callers
  and substantive executor tests. Historical claims that principal cannot be
  reclaimed are obsolete at this revision. Genesis propagation remains F34.
- **Abstain still uses live weight.** The governance review revalidates this
  historical F09-family behavior, without counting it as new. Regress actual
  stored vote weights for all ballot options, not just snapshot metadata.
- **Execution equivalence has substantial existing coverage.** Application tests
  compare per-block write sets, CF dumps and running hashes for serial/pipelined
  and resident-row choices within each book persistence format. They do not
  assert equal roots across different formats. Context reconstruction uses the
  same open StateDb, so it does not establish actual process/DB-reopen recovery.
  The [execution report](chain-cea1254-pass7-astra-execution-2026-10-04.md)
  also identifies predecessor-overlay and deferred-book serialization barriers,
  production defaults/gates, and expected deposit-credit ordering assertions.

## Persistence and verification

Torus discovery records were saved successfully; shared project-state sync was
skipped because this checkout has no existing shared state file:

| Finding | Issue ID |
| --- | --- |
| F32 | `db04c407-d27c-4ae4-92dd-de35ea880592` |
| F33 | `834faea5-0ea5-47bd-aa6d-c235aa8f893b` |
| F34 | `b89fea8c-d8fe-46e7-a1c7-96435162b4f0` |

All four agent reports were read and reconciled. Local report links and source
line bounds were checked, with no missing files or out-of-bounds lines; report
whitespace checks passed. HEAD and tracked/staged diffs remain unchanged. Only
five pass-seven Markdown files were added alongside prior untracked audit files.
No Rust tests or production reproduction were run. This audit has not resolved
any production finding or certified chain safety.
