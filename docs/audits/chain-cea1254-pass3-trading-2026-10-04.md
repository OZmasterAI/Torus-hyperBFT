# Trading engine audit — pass 3, 2026-10-04

Target: `merge/item6-sync2`, HEAD `cea1254e34625e6b09c58f794de8793b5c12713c`.
Scope: whole native trading engine, including matching, reservations, account margin,
liquidation/ADL, oracle, positions/arithmetic, market creation, and the EVM CoreWriter/lockbox paths.
This is a source audit, not a review limited to the current branch's diff.

No production edits, Git mutations, toolchain installation, live-chain transactions, or network
tests were performed. Cargo/rustc are unavailable. The adjacent
[Python models](chain-cea1254-pass3-trading-models.py) passed locally with revision/source guards;
they model arithmetic and interface flow and **do not execute the Rust implementation**.
The findings below need production regression tests before treatment as runtime-confirmed defects.

Historical comparison included Astra rounds 1/2, `research/ECON-AUDIT-3.4.4.md`,
`research/audit-3.4.3-evm-correctness.md`, parity fixes, margin design decisions, and the prior two
chain passes. Their F01–F11 are not repeated here. Local finding IDs T01–T05 are independent of
the consolidated report's numbering.

| ID | Priority | Finding | Provenance |
|---|---|---|---|
| T01 | P1 | Fill-versus-mark margin omission allows profitable withdrawal against unrecoverable flat vault debt | Known margin root D5; newly developed liquidation/withdrawal counterexample |
| T02 | P2 | CoreWriter returns an order ID different from the order actually created | Not located in reviewed historical reports |
| T03 | P2 | CoreWriter accepts stop order codes but executes them as ordinary limit orders | Not located in reviewed historical reports |
| T04 | P2 | Unlisted market execution remains reachable through CoreWriter | Known ECON-PF-05 / O2 deferred execution fix |
| T05 | P2 | Newly created books still ignore listed tick/lot settings | Known ECON-PF-05 / Astra round 1 |

## T01 — P1: post-fill losses can become withdrawable profit backed by a flat negative vault

**Status/provenance:** the opening-fill-versus-mark omission is explicitly acknowledged as D5 in
[account-level-margin-f1-impl.md:2112](../plans/account-level-margin-f1-impl.md#L2112).
This report does not claim to discover that root anew. It establishes a source-derived
counterexample connecting D5 to the now-wired liquidation and lockbox paths. That full insolvency
sequence was not located in the reviewed reports.

**Prerequisites:** a listed 20x market with usable mark 100, a previous liquidation-step mark of
100 (or no previous mark, which also uses 100), and no pre-existing opposite-side positions
other than the colluding maker. Two distinct, admitted signing identities A/B each have 60 native
collateral units and separate EVM gas funds if needed. Sizes/prices are on ordinary ONE/ONE
book parameters. No validator control, oracle forgery, storage failure, or arithmetic boundary is
required. A listed market can have an honest oracle even when otherwise empty. Both orders have
`reduce_only=false`; self-trade prevention compares the distinct addresses and does not reject.

**Guard trace and counterexample.** Values below use human native units; raw encoding is `x×10^8`.

1. B rests a GTC sell of size 1 at price 1,000 in an otherwise empty book. Placement need and
   reservation are `1,000/20=50`, which fit B's 60. B then has available 10/order margin 50.
   Do this in the preceding block to remove assumptions about ordering within the taker batch.
2. A submits a market buy of size 1, cap 1,000. Market reservation and placement use mark 100,
   so reserve/need are 5. After reservation A's free pool is 55. Match-time budget is reservation
   5 + free 55 = 60, while `TakerMarginLimit::need` charges opening fill notional
   `1,000/20=50`, so the fill passes whole. The maker check computes IM increase 50 minus
   its released reservation share 50 = 0, fitting B's remaining free 10. The fill is within
   the cap, whole-lot, non-reduce-only, and no FOK/IOC or maker guard blocks it.
3. Normal settlement releases both reservations. A has available 60 and long 1 at entry 1,000;
   B has available 60 and short 1 at entry 1,000. At the honest mark 100:
   A equity = `60−900=−840`, B equity = `60+900=960`. True mark initial margin is 5,
   so the taker's post-fill account is already bankrupt. The fill check never accounts for
   the 900 mark loss. This is one fill; another action or stale same-batch snapshot is unnecessary.
4. Production execution calls `run_liquidations` after native matching/CoreWriter. A is ADL,
   not stage 1/backstop. Its bankruptcy price is `1,000−60/1=940`; the implemented long ADL
   price is `min(previous_mark100, bankruptcy940)=100`. B is the sole opposite position and
   therefore the ADL counterparty, irrespective of ranking/address ordering. Liquidation's
   transfer closes A's long and B's short at 100, realizing A −900 and B +900.
5. A is flat at −840, B is flat at 960. `adl_account` moves A's remaining signed collateral
   into the fixed protocol vault: A becomes 0, the vault becomes −840. With no vault position,
   `liq_view(vault)` returns `None`, so the vault has no ADL counterparty path or persistent
   pending work to collect that debt. B's native balance is an ordinary positive claim.
6. In the next block B withdraws 960. Its account is flat, so UPnL/order margin/position IM/
   transfer-required notional are all zero. `amount<=available` and the SAFE margin gate both
   pass. The lockbox credits `960×10^18` wei to the EVM balance. The pair supplied only
   `120×10^18` wei-equivalent collateral; the difference is 840, offset solely by a signed
   −840 vault row that no private key or collateral backs. Recycling 120 of the proceeds can
   repeat the sequence if the market remains otherwise empty and the mark usable.

The existing signed-accounting invariant still holds: `A0+B960+vault(−840)=120` and long/short
open interest closes symmetrically. **That invariant does not establish redeemable supply
solvency:** a negative protocol balance must not fund an unrestricted positive EVM credit.

**Exact source anchors:**

- Fill need omits mark PnL: [order_book.rs:227](../../crates/torus-core/src/order_book.rs#L227),
  especially :243 and :249. Maker reservation subtraction:
  [order_book.rs:528](../../crates/torus-core/src/order_book.rs#L528), success predicate :544.
- Market reservation/account gate and pool setup:
  [native_executor.rs:6734](../../crates/torus-bridge/src/native_executor.rs#L6734), :6791, :6873;
  placement formula [margin.rs:104](../../crates/torus-core/src/margin.rs#L104).
- Production liquidation invocation:
  [app.rs:2287](../../crates/torus-consensus/src/app.rs#L2287).
- Previous mark/bankruptcy selection, ADL, and signed collateral move:
  [liquidation_step.rs:268](../../crates/torus-bridge/src/liquidation_step.rs#L268), :288, :297;
  [liquidation.rs:151](../../crates/torus-core/src/liquidation.rs#L151), :184, :260, :268.
- Flat vault exclusion:
  [liquidation_step.rs:160](../../crates/torus-bridge/src/liquidation_step.rs#L160), vault branch :130.
- Withdrawal gate and actual EVM credit:
  [native_executor.rs:8031](../../crates/torus-bridge/src/native_executor.rs#L8031), :8093;
  [margin.rs:195](../../crates/torus-core/src/margin.rs#L195);
  [lockbox.rs:158](../../crates/torus-core/src/lockbox.rs#L158), :170.

**Fees/rounding:** all numbers are exact with eight decimals and fit the production flat 20x tier
(`market_margin_config` builds a tier covering `FixedPoint::MAX`). The native execution paths
return gas-used counters but do not debit the supplied native balances; `total_native_fees`
has only initialization/read occurrences in current crates. Independent EVM gas costs do not
cover the 840 native-unit deficit. The 60-per-account example adds 10 collateral headroom above
the 50 IM requirement. No dust, reduce-only, overflow, tier-crossing, or global-scan-budget
edge is needed; two position holders fit the liquidation scan/action budgets.

**Existing tests:** `market_order_margin_tests.rs` and `account_margin_tests.rs` test IM budgets,
mark reservation, closing, and pool sharing. `liquidation_tests.rs:540` explicitly expects a
remaining deficit to move into the vault and tests signed value conservation. These do not
establish a post-fill solvency check or combine an intentionally off-mark fill with withdrawal
of its colluding counterparty's realized profit.

**Regression:** execute the above full normal placement→end-block liquidation→next-block
`TransferToSpot` sequence (withdrawal action amount `960×10^8` raw native units, not wei)
against a real listed row and valid oracle fixture, across scalar,
serial batch, and parallel batch paths. Assert that a voluntary opening fill cannot make the
account insolvent at the current mark and that the positive externally redeemable proceeds cannot
exceed collateral because debt was parked in an unbacked protocol account. Do not assert merely
signed conservation: that passes the current counterexample.

**Fix direction:** include the post-fill change of equity/UPnL and mark-valued position IM in
both maker and taker affordability, including per-fill running budgets. Keep pure closing
eligibility explicit, but ensure it cannot finance a new opening against fictitious released
equity. Define backed loss absorption/ADL settlement for genuinely bankrupt accounts before
making counterparty PnL redeemable; an unused flat negative vault row is not funded insurance.

## T02 — P2: CoreWriter's returned order ID never identifies its resting order

**Sources:** [precompiles.rs:931](../../crates/torus-core/src/precompiles.rs#L931) explicitly calls
the return value an order ID and returns `((block+1)<<64)|queue_sequence`;
[native_executor.rs:8159](../../crates/torus-bridge/src/native_executor.rs#L8159) drains by ordinary
`execute(PlaceOrder)`; :8491 carries neither queue sequence nor forced ID;
[:6862](../../crates/torus-bridge/src/native_executor.rs#L6862) allocates the actual book order
from `ctx.next_global_order_id`. Queue values contain no sequence-to-native-ID mapping.

**Counterexample/prerequisites:** funded CoreWriter caller C, empty books, existing listed market,
noncrossing valid limit order. At block 20, queue sequence 0 returns
`21<<64 = 387381625547900583936`; draining at 21 creates native order **1**. An EVM call
`cancelOrder(returnedId)` queued at 21 and drained at 22 finds no such order. A contract that
stores the promised ID cannot cancel its own resting exposure individually. Calling `cancelAll`
or discovering native order 1 through RPC is a workaround, not fulfillment of this interface.

**Guard/test check:** the forced-ID path is used for triggered stops, not CoreWriter placement.
`cross_vm_write.rs:81` checks return length and drain success; `precompile_tests.rs:485` checks
queue count, and its same-journal test checks the synthetic sequence. None bind the returned
value to the resting native ID or cancel it afterward.

**Regression/fix:** perform an actual precompile call, drain, assert the returned ID is the live
order, then queue/drain cancellation by that ID and assert removal plus exact reservation
release. Repeat with interleaved signed native placements and several CoreWriter callers. Either
reserve and carry a globally unique native ID through the queue or expose this value explicitly
as an action handle with an authoritative result/ID mapping. Returning an order ID while allocating
an unrelated ID cannot remain the contract.

## T03 — P2: accepted CoreWriter stop orders become immediately active limits

**Sources:** [precompiles.rs:895](../../crates/torus-core/src/precompiles.rs#L895) explicitly labels
2 as StopMarket and 3 as StopLimit, and rejects only codes >3. The ABI at :883 has no trigger
argument. The drain converts via [native_executor.rs:8496](../../crates/torus-bridge/src/native_executor.rs#L8496);
[:8529](../../crates/torus-bridge/src/native_executor.rs#L8529) maps all codes except 1 to `Limit`.

**Counterexample/prerequisites:** an ordinary funded caller submits code 2 or 3, price 100,
quantity 1, GTC, in a market with an ask at 100. The call accepts/queues successfully. Next-block
drain places a normal buy limit at 100 and fills immediately without ever storing a stop or
checking a trigger. On an empty book it rests as an active limit, likewise without a trigger.
It is unsupported stop input that silently takes a different trading action.

**Guard/test check:** out-of-range enum tests cover invalid codes, while the conversion explicitly
accepts the advertised in-range stops and defaults them to Limit. No trigger can be preserved
because neither the queue nor this selector carries one. Native stop handling itself is separate
and currently enforces/store triggers; it does not repair the CoreWriter conversion.

**Regression/fix:** use the real precompile+drain for code 2 and 3 against crossing liquidity.
If this selector supports only Limit/Market, reject both stop codes at enqueue. If stops are part
of the contract, add an explicit trigger/limit-cap encoding and exhaustively convert every
accepted variant. An accepted unsupported stop must not become an immediate order.

## T04 — P2, known: RPC market admission does not prevent CoreWriter phantom books

**Provenance:** known `ECON-PF-05` in `research/ECON-AUDIT-3.4.4.md:55` and explicitly deferred
as a consensus execution fix in [o2-placeorderbatch-design.md:140](../plans/o2-placeorderbatch-design.md#L140).
[torus.rs:261](../../crates/torus-rpc/src/torus.rs#L261) calls its listed-market check RPC-only.

CoreWriter `placeOrder` accepts any decoded ID at [precompiles.rs:885](../../crates/torus-core/src/precompiles.rs#L885),
and normal scalar placement constructs a missing book at
[native_executor.rs:6838](../../crates/torus-bridge/src/native_executor.rs#L6838). Neither drain
nor placement requires a market registry row. A normal funded EVM caller therefore creates and
fills a book/positions in an unlisted ID despite RPC native ingress rejecting the same action.
Oracle aggregation/liquidation marks only listed IDs. The next governance market allocator
looks at registry rows, not phantom books/positions, so pre-existing exposure under its newly
assigned ID can survive into the newly listed asset.

**Regression/fix:** precompile+drain an otherwise valid order for unknown ID 77; assert failure,
no new book/position/reservation or durable ID pollution. Also test a Byzantine body containing
signed unknown-market orders. Put the listed-market gate in the consensus executor and use
explicit initialization from registry metadata. Existing RPC ingress tests only verify the local
submission guard.

## T05 — P2, known: new books still hardcode ONE tick and ONE lot

**Provenance:** revalidated Astra round 1 market-lifecycle finding and earlier `ECON-PF-05`.
The listing ID collision from that same Astra report is now fixed by deterministic registry ID
allocation/collision rejection; it is not reproduced here.

Both batch [native_executor.rs:4595](../../crates/torus-bridge/src/native_executor.rs#L4595) and
scalar [:6838](../../crates/torus-bridge/src/native_executor.rs#L6838) create absent books with
`FixedPoint::ONE` tick and lot. The metadata-aware `rebuild_book` at :2774 reads existing book
metadata, not the listing row, so it preserves the wrong initial settings. A listed market with
tick .01/lot .001 rejects a first quantity .1 as dust; a first limit price 100.25 is off its
hardcoded 1.0 tick despite the advertised registry settings. Governance writes only its market
row, so no pre-seeded book repairs the ordinary first-placement path.

**Regression/fix:** list a real market with fractional tick/lot, use its first placement through
all normal execution paths, save/reload, and assert listed settings govern acceptance throughout.
Initialize a new book from canonical registry metadata and address already persisted mismatched
books explicitly. Restoring existing books with their own tick/lot is not a new-book fix.

## Other checked paths and limits

The historical market/stop caps, reduce-only enforcement, ModifyOrder ownership/crossing checks,
stop reservation release, and lockbox 8↔18 decimal conversion are present at this revision.
Reservations/releases use the same telescoping quantity formula; no additional ordinary
partial-fill leak was established. The historical Market+PostOnly escape is closed by the now
capped Market matching predicate. Oracle submissions are bounded, validate listed markets and
sample times, and aggregate only reporters remaining after the outlier cut with a strict stake
quorum. Header-time freshness behavior and previously reported ADL fixed-prefix starvation remain
outside this pass's new findings.

Position weighted-entry rounding and the cross-market maker snapshot allowance are documented
historical limitations; no new magnitude/runtime claim is made. The audit is not exhaustive
proof over every order/stop combination, corrupted state row, arithmetic boundary, or worker
configuration. No Rust test results or deployed-chain exploit claims are implied by the models.
