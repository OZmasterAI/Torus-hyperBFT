# Chain audit pass 8 — updated branch, two Sol 6.1 and two Astra reviews

Date: 2026-10-04. Review revision:
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`, branch `perf/item6-phase1`.

## Remote check and local update

`git fetch origin` observed these changes relative to the previously fetched refs:

| Remote branch | Previous | Fetched | Change |
| --- | --- | --- | --- |
| `main` | `92a02ed` | `92a02ed` | Unchanged |
| `merge/item6-sync2` | `cea1254` | `cea1254` | Unchanged |
| `perf/item6-phase1` | `81a9567` | `d52a33f` | 22 newly reachable commits; only two are beyond the prior checkout `cea1254` |
| `docs/market-scaling-design` | `7ad2774` | `0baea1d` | Four additional planning commits |

No other remote-tracking branch changed during this fetch. These are commit/ref
comparisons, not GitHub push timestamps. The two commits beyond the previous
checkout are `ccdb59b` (C2 block mark table) and `d52a33f` (merge of the sync branch).
Their aggregate diff is 10 files, 518 insertions and 70 deletions.

The local checkout was switched to a new tracking branch `perf/item6-phase1` at
the fetched tip. It contains the whole previous audited checkout. The local
`merge/item6-sync2` and `main` branches were preserved, together with all 24 prior
untracked audit artifacts. Planning changes were fetched and consulted, without
merging the documentation branch. Future C3/PF1 work described there is not
implemented in this reviewed revision.

## Review status

**All four reviews completed: two GPT-6.1-sol and two GPT-6-astra.** They add
three P2 source-supported candidates. Scope is ordinary correctness and
integration across the project, including the new C2 diff. Earlier interrupted
pass-five scopes are not resumed. Tests are read, not run: Cargo/rustc are
unavailable. No production code was edited and no live-chain action was taken.

| Model | Scope | Report |
| --- | --- | --- |
| GPT-6.1-sol | New C2 mark table, consumers and ownership | [Mark-table review](chain-d52a33f-pass8-sol-marks-2026-10-04.md) |
| GPT-6.1-sol | Operator/client integration and key lifecycle | [Integration review](chain-d52a33f-pass8-sol-integration-2026-10-04.md) |
| GPT-6-astra | Market, oracle and reward lifecycle | [Lifecycle review](chain-d52a33f-pass8-astra-lifecycle-2026-10-04.md) |
| GPT-6-astra | Native/EVM data-contract compatibility | [Compatibility review](chain-d52a33f-pass8-astra-compatibility-2026-10-04.md) |

## Additional source-supported candidates

These remain candidates under the audit [README](README.md), pending runtime
regressions. Existing F01–F34 and historical supplements are not counted again.
The candidates below were already present before C2; the update did not introduce
them. The [integration report](chain-d52a33f-pass8-sol-integration-2026-10-04.md)
contains full caller traces for I01 = F35 and I02 = F36. The Astra compatibility
[report](chain-d52a33f-pass8-astra-compatibility-2026-10-04.md) documents F37's
read-interface consequence of a previously noted unused CF.

| ID | Priority | Finding | Principal evidence |
| --- | --- | --- | --- |
| F35 | P2 | Explicit CLI values equal to compiled defaults are overwritten by TOML. An operator supplying `--data-dir ./data` still gets the configured directory despite the documented CLI precedence. Default RPC/P2P/log/metrics values have the same pattern. | [Merge logic](../../crates/torus-node/src/main.rs#L287), [documented precedence](../../README.md#L86) |
| F36 | P2 | Wallet/node key generation overwrites an existing output keystore, including their default paths, with a fresh identity. Wallet import uses the same replacement path. Loss of access requires that the old key has no backup; this review did not generate or replace keys. | [Wallet writer](../../tools/wallet/src/keystore.rs#L89), [node writer](../../crates/torus-node/src/keystore.rs#L95), [wallet defaults](../../tools/wallet/src/main.rs#L59) |
| F37 | P2 | `0x0800.getOpenOrders` reads a legacy order index with no production writer. A normally persisted resting order can be visible through RPC while the precompile returns empty arrays, even with Classic storage. | [Precompile reader](../../crates/torus-core/src/precompiles.rs#L545), [RPC reader](../../crates/torus-rpc/src/torus.rs#L1738) |

F35 is an explicit-input precedence error, distinct from F19's absent/mismatched
genesis restart configuration. Use parser-provided value provenance or optional
CLI fields to distinguish omission from an explicitly requested default. Regress
omitted, explicit-default and explicit-nondefault values against differing TOML.

F36 is accidental local identity replacement during normal use, not a claim of
cryptographic compromise or remotely altered keys. Passphrase confirmation only
confirms the new password, not replacement of the existing key. The repository's
[price-feeder keygen](../../tools/price-feeder/src/keyfile.rs#L55) already refuses
existing targets and publishes without overwriting a racing target. Regress a
second generation/import into a temporary existing file, requiring failure and
unchanged original bytes unless replacement is explicitly requested.

F37 is separate from the historical `getOrderBook` storage-layout limitation.
The dead `CF_NATIVE_ORDERS` writer was already noted in the
[running-hash design](../plans/running-state-hash-impl.md#L268); this pass traces
its consequence for the still-exposed selector. The
[existing test](../../crates/torus-core/tests/precompile_tests.rs#L252) manually
seeds that legacy index and only checks output length. A useful regression must
place and persist a real order, compare all returned fields with RPC and the
book, then repeat after partial fill, modification and cancellation. Integrate
the canonical layout-aware book reader rather than creating an unmaintained
second order authority.

## Negative results and coverage qualifications

- **C2 preserves the reviewed ordinary mark rule.** The application aggregates
  before actions and rebuilds the table each native block. Previous marks are
  carried only for equality/version comparisons. All AccountReader constructors
  use the table; missing keys fall back to direct oracle reads. C3/C4 version
  consumers are absent at this revision, so their future invalidation behavior
  cannot yet be certified.
- **C2 has meaningful baseline comparisons.** Its unchanged pre-C2 golden
  digests and direct-reader comparisons provide evidence beyond mode equality.
  Both resident on/off golden variants build the table, so that comparison alone
  is not a table-disabled reference. The one-read-per-market counter applies to
  its fully fed fixture, not every oracle fallback case.
- **Market and funding scope must be stated accurately.** Listings execute at
  the tail and enter the following context. Market update/delist proposals are
  text-only and remain Passed; funding is explicitly deferred. These are known
  limitations, not newly found cache regressions.
- **Oracle timing tests already check expected outcomes.** Existing fixtures
  cover exact freshness boundaries, next-block aggregation, persisted rows/roots
  and an observable pending-parent case. Reward boundary tests exercise the real
  application but use a nonzero-liability boolean; exact validator/delegator and
  permanent-staker amounts plus claim/reopen behavior need stronger assertions.
- **Shutdown remains a qualified operational follow-up.** The TERM runbook and
  node's explicit Ctrl-C wait do not establish the same graceful shutdown path.
  Historical shutdown investigations exist. No hang, state loss or fully graceful
  INT shutdown was demonstrated, and this is not another numbered finding.
- **Read interfaces require explicit representation mappings.** Native/EVM
  balance and position fields intentionally differ in units, signedness,
  aggregation and absence values; test-pinned differences were not automatically
  called defects. The staking cross-VM fixture manually writes a delegation
  prefix, so it does not prove a full typed delegation/undelegation round trip.

## Saved discoveries

| Finding | Torus issue ID |
| --- | --- |
| F35 | `1629012a-76b8-4560-a6d1-53ceb92556f2` |
| F36 | `f0ece912-bcec-4197-8c9e-f1351881fff7` |
| F37 | `0084c401-657a-4299-8d77-7883fcc647ca` |

All three discoveries were acknowledged as saved; shared-state sync was skipped
because no shared project-state file exists. Parent review independently checked
the principal source paths for all three candidates and reconciled all four
reports. All local report links were checked for file existence and source-line
bounds, and report whitespace checks passed. Tracked/staged diffs are empty;
HEAD equals the fetched `origin/perf/item6-phase1` tip. Five new pass-eight
Markdown files accompany the preserved 24 earlier audit artifacts. Rust tests
were not run, and no production finding has been resolved.
