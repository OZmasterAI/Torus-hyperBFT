# Pass 14 — project-wide follow-up with Sol 6.1

Reviewed 2026-10-04 against `merge/item6-c3-pf1` at
`d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd`. Documentation starts from
`f39a352` on the separate `audit/chain-findings-2026-10-04` branch.
The user requested another round and explicitly requested no Astra reviewers;
all four delegated reviews in this round use GPT-6.1-sol.

This round follows paths across the project rather than limiting review to the
C3/PF1 commits: history/receipt projection, economics and configuration,
storage/shutdown, and client error recovery. It adds F47, a source-supported
faucet nonce-reservation candidate. Earlier findings remain open; repeated
evidence does not create additional finding numbers. See the
[pass-13 whole-project report](chain-d9ef4f7-pass13-project-wide-2026-10-04.md)
for the current broad risk inventory and C3/PF1 review.

| Reviewer | Focus | Report |
| --- | --- | --- |
| GPT-6.1-sol | Transactions, receipts, history and explorer recovery | [History](chain-d9ef4f7-pass14-sol-history-2026-10-04.md) |
| GPT-6.1-sol | Economic precision, collateral and configuration transitions | [Economics](chain-d9ef4f7-pass14-sol-economics-2026-10-04.md) |
| GPT-6.1-sol | Persistent schemas, snapshots and shutdown ownership | [Storage](chain-d9ef4f7-pass14-sol-storage-2026-10-04.md) |
| GPT-6.1-sol | Independent challenge of the faucet lead and audit conclusions | [Recheck](chain-d9ef4f7-pass14-sol-recheck-2026-10-04.md) |

## F47 — P2: a failed gas-price lookup leaves a hole in faucet nonces

The shipped faucet's ordinary
[POST handler](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L357)
checks the recipient cooldown and balance, then
[reserves a nonce](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L446)
by setting its cache to `n + 1`. Only afterward does it request the gas price.
A [gas-price RPC failure](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L470)
returns HTTP 500 before signing or sending, without undoing that reservation.
The next handler invocation uses the cached successor instead of querying the
unchanged chain nonce. The reset at
[send failure](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L516)
does not run on this earlier return.

A minimal ordinary sequence is:

1. Use a funded faucet with on-chain nonce `n`, no pending transactions and an
   empty local nonce cache. A valid recipient is outside its cooldown.
2. Balance and nonce queries succeed. The handler caches `n + 1`. A transient
   RPC transport/error response during `eth_gasPrice` makes this request return
   500; no transaction with nonce `n` has been submitted.
3. After RPC recovery, retry. The handler signs nonce `n + 1`, leaving `n`
   absent. This can happen sequentially; it does not need concurrent requests.
4. The node's [admission rule](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/validate.rs#L102)
   accepts a modest future nonce if the other checks pass. The faucet can then
   return a hash/HTTP 200 and start the recipient cooldown even though the
   missing predecessor prevents that payment from executing normally.

The impact is faucet delivery/retry availability, not a consensus safety or
fund-conservation defect. The already recorded R2 selection/ownership issue
can make handling future-nonce transactions worse, but is not needed to prove
that this client omitted the required nonce. F44 concerns the wallet reusing
the latest chain nonce for a second pending send; F47 concerns a cached faucet
reservation consumed before any send, so switching the initial query to
`pending` alone does not fix it.

**Bounds and controls.** Balance-query and nonce-query failures occur before
the increment. A successful gas lookup and submission use the reserved nonce
normally. A send rejection resets the cache. The node caps its accepted nonce
gap at 64, so continued requests can eventually reach a rejection and reset;
restart or an external transaction filling the hole can also change the
situation. This report does not claim a permanent wedge, loss of all later
payments, or an observed production incident. A faucet key concurrently used
by another sender is a separate coordination problem, not a prerequisite.

Move fallible pre-send work such as gas-price lookup before nonce reservation,
and make reservation/recovery explicitly account for outstanding submissions.
Blind decrement/reset is not generally safe with concurrent requests. Test
the real async handler using a controlled RPC server: fund the faucet, let
nonce lookup succeed, fail gas-price lookup exactly once, then decode the
next signed envelope and require nonce `n`, not `n + 1`. Assert the failed
request never called send, and cover initial cache population, an existing
pending reservation, send rejection and concurrent callers separately.

The current [signing test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L718)
checks envelope prefix and length. The
[cooldown test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L736)
checks an isolated HashMap. Neither drives the handler's RPC failure sequence.
These are inspected assertions, not tests executed in this review.

## Other conclusions from this round

- **History identity:** the explorer uses replacement for executed outcomes
  and ignore-on-conflict for skipped ones. That falsifies a suspected case
  where an earlier skipped transaction would permanently hide its later
  execution. The reverse-transition regression is still missing. Receipt/hash
  alignment and dense Ethereum indices were traced separately from explorer
  body positions. F26's partial history visibility and F40's capped user
  recovery remain open; eventual writer completion does not close those gaps.
- **Storage formats:** creating missing column families is not a migration
  guarantee. Current book modes and the intentional old-history wipe have
  explicit compatibility boundaries. Same-mode book reconstruction checks
  persisted metadata/level bytes and carries the order allocator forward.
  These safeguards do not close F21's snapshot commitment limitation.
- **Economics:** bounded Python integer checks preserved leverage for inputs
  1–1000 and confirmed selected close/flip and collateral-dust calculations.
  They do not execute Rust or prove arbitrary accepted leverage values. Existing
  tests contain independent PnL constants and all-dust/nonround rejection
  assertions. A lockbox helper comment incorrectly includes delegation among
  eight-decimal amounts; actual staking and wallet callers agree on EVM wei.
  That is a documentation qualification, not a new denomination defect.
- **Shutdown:** the ordinary Ctrl+C path has a real ownership chain from
  replica shutdown through execution, flush and history writer joins. Component
  tests exercise parts of it. A process-level signal/reopen test is still
  needed; the previously documented TERM qualification is not evidence of
  demonstrated canonical-state loss.

The detailed reports distinguish actual failing predicates from unsupported
scenarios, documentation ambiguities and missing test coverage. No additional
finding number is assigned merely because an integration test is absent.

## Evidence limits and disposition

The companion reports record concrete safeguards and rejected leads as well
as unresolved issues. This is a bounded source audit, not a claim that every
line or every possible chain execution was covered. The previously interrupted
pass-5 certificate/malformed-input/adversarial-network assignments remain
outside this round.

Cargo, rustc and promtool remain unavailable. Rust and rule tests were read,
not run; no node, external RPC, live chain, service, database-reopen experiment
or benchmark was started. Only the bounded arithmetic checks described in the
economics report were executed. The fixture above is source-derived and still needs
a failing production-code regression under the [audit convention](README.md).
Torus memory/index results were treated as navigation and history, then checked
against local source; index freshness was not assumed.

All source links pin `d9ef4f7`, since the audit worktree contains older
production source. This round commits Markdown reports and the index only.
The source branch remains separate and unchanged; no source merge or push is
part of this task. No prior finding is marked fixed by this documentation.
