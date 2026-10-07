# Implementation Plan: ADL per-block budget, P2 escrow + rule H (P0 before testnet)

Design: `docs/plans/adl-budget.md`, sections 1-7 and **section 8 (FINAL, owner 18c s96)**:
* Q1-Q4 as recommended: C1, one ranking per (block, market, side), W = work units (rows
  visited + candidates examined + candidates read), FIFO obligation rows.
* **Q6 = P2:** terms fixed at B, two ADL escrows.
* **Q5 freeze dropped.**
* **H1 clamp kept unchanged.**
* **H:** the previous mark = the last mark different from the current one.
* **Funding:** doc-only.
* **Conservation:** a node-local sum.
* **W:** an HL-sized event closes in one block.

Liquidation semantics: `docs/plans/liquidation.md` (H1, H2, H3, M2, D4, D8, D9, D10,
invariants). Branch `perf/adl-budget` (worktree `/home/oz/projects/wt/adl-budget`, off main
aae6b9b). P2 and H change block results, so they need a fresh genesis (M1: no activation height;
fine pre-testnet).

```bash
export CARGO_TARGET_DIR=/home/oz/.cargo-target-adl-budget   # one cargo build at a time: pgrep -a cargo
F="--cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar"
```

## Design Decision

**At B** (the block where an account classifies ADL, or the vault's AV < 0), in the regular
pass:
1. D4: cancel the account's orders and stops.
2. For each **marked** position, in ascending market:
   `price = liq::adl_price(base, bankruptcy_price(adl_rest(..)), mark, is_long)`. This is
   today's one-sided H1 clamp, computed sequentially, so a later market sees the PnL the
   earlier transfers realized. `rest` is kept **running**: the account's positions are read
   once, the UPnL of all its positions is summed once, and each transfer is followed by one
   balance point read and a subtraction of that market's UPnL. That is O(P) per account. Today's
   `adl_rest` re-reads all positions for every market, which is O(P²) (~0.7 s per block at S=750,
   outside W). `base` comes from rule H. `liq::transfer` moves the position to
   the escrow of its side at that price, and the pass writes an obligation row. No candidates
   are needed.
3. D9 / `move_collateral` as today: the non-vault account's remaining collateral moves to the
   vault. Under the one-sided clamp it is **never positive**:
   * each clamped close keeps AV at the mark ≤ 0, inductively;
   * the ceil rounding goes against the trader;
   * the `b ≤ 0` mark fallback would need AV ≥ mark × size > 0, which is impossible for an
     ADL-classified account;
   * `rest` includes cash, order margin, and the other positions at the mark (unmarked at
     entry), `liquidation.rs:142-150`.

   So the vault only receives a deficit (≤ 0). If `move_collateral` ever returns > 0 at this
   point, the step logs an **error** (an invariant alarm; the move still happens, so value is
   conserved). The account ends flat at exactly 0 and leaves liquidation in B. The vault keeps
   its own balance (D8, as today).

**Rule H (changes D10).** Row `0x03 ‖ market` → `last(16 BE) ‖ [prev(16 BE)]`.
* At each step with a usable mark `M`:
  * no row → write `last = M` (no `prev`);
  * `M ≠ last` → write `(last = M, prev = old last)`;
  * `M == last` → no write.
* No usable mark → delete the row (as today).
* The pre-clamp base of this step = the old `last` if the mark changed this step, else `prev`;
  with neither, the mark (D10's fallback).
* A 16-byte row still decodes (no `prev`), so today's fixtures and `liq_rows(.., 0x03)` checks
  are unchanged for a first mark.
* The result: every account of a market gets the same pre-clamp price **within one mark
  interval**, whatever block the scan reaches it in. Across mark changes this is not true: at
  100k accounts the 2,048-per-block scan can span several (follow-up C2, noted in
  `adl-budget.md` §8).

**Escrows.** `ADL_ESCROW_LONG = b"torus-adl-escrow-lng"` takes bankrupt longs and
`ADL_ESCROW_SHORT = b"torus-adl-escrow-sht"` takes bankrupt shorts. They are protocol accounts
with no key, fixed like `LIQUIDATOR_VAULT`.
* One aggregated position per (market, side); the sides never net.
* Never classified: the pass skips them like the vault.
* No margin checks: they never place orders or withdraw.
* Never ADL candidates (`liq::adl_candidates` skips them). The vault stays a candidate.
* Nothing else credits their **native** balance. No native-to-native transfer action exists:
  `Withdraw{to}` credits the EVM balance (`CF_ACCOUNTS`), `TransferToPerp` and `LockboxDeposit`
  credit the signer itself, and the escrows have no key. EVM funds sent to an escrow address are
  stranded in `CF_ACCOUNTS` and never reach the native balance, so they cannot become dust.
  Rejecting such transfers is not needed now. Any future native transfer to another account
  must reject protocol recipients (vault deposits come with their own action later).

**Obligation queue (FIFO).** `CF_NATIVE_LIQUIDATION` row
`0x07 ‖ height(8 BE) ‖ market(8 BE) ‖ side(1: 1 = long) ‖ trader(20)` → `size raw (i128 BE) ‖
price raw (i128 BE)`. The trader stays in the key because the clamp makes prices per account.
Invariant: for each (side, market), the escrow's size equals the sum of that side's row sizes.
* Writes, as built in A4 (c782dc0): `put_obligation` either **inserts** a new row (size > 0;
  it **errors on an existing key**) or **deletes** the row (size ≤ 0). It never overwrites. A6
  adds `update_obligation`, which overwrites an **existing** row with the remainder (and errors
  if the row is missing) or deletes it at 0. The drain's remainder and the edge pairing's
  partner row go through it.
* Key collisions are impossible: an account is classified at most once per block, and a
  re-bankruptcy after it trades again gets a new height.

**Drain** (after the pass and the vault step):
* Rows are drained in key order under `W`, with one ranking per **(block, market, side)** from a
  local cache.
* `liq::adl_close` closes the escrow against the ranked opposite holders at the **row's stored
  price**, with `q = min(row remaining, candidate size)`. The row is rewritten with the
  remainder, or deleted at 0.
* A step is one row. It starts only while `used < W` and runs to the end once started, so a
  block overshoots by at most one step.
* Units per row:
  * **1 for visiting it**, also when it waits (no mark);
  * \+ the traders examined by a ranking (only when the row's (market, side) is first used in
    the block);
  * \+ the candidates `adl_close` reads;
  * \+ the rows scanned by the edge pairing.

  So every row costs at least 1, and no row is free.
* **Market without a usable mark:**
  * **listed but unmarked** (oracle stale): its rows wait. They stay queued, cost 1 unit per
    visit per block, and keep the step due.
  * **delisted** (not in `listed_market_ids()`; it will never get a mark again): its rows are
    ranked with the row's **stored price** standing in for the mark (`adl_rank(o.price, …)`),
    and closed at the stored price as usual, so the escrow can go flat. **Decided (owner 18c,
    after dbb683c).** Note: delisting is text-only today (`ProposalAction::DelistMarket`
    executes nothing, `native_executor.rs` ~9761), so this path cannot occur yet. When
    delisting is built (likely a final settlement price for every position, HL-style), the
    escrows' positions in that market must be settled with them (their rows closed at the
    settlement).

**Edge: real holders exhausted** (both escrows hold the market). The row is paired with the
opposite side's rows of the same market, in key order. The two escrows close against each other,
**each at its own stored price**, and the vault pays `(p_long − p_short) × q`: `liq::cross_close`.
The amount is tested and reported (log + gauge).

**Dust sweep.** At the end of a drain, a flat escrow's balance (dust from weighted-average
entries, any sign) moves to the vault. It is reported separately (log + gauge) and must stay
within the *Dust bound*.

**Where the Q2 cache lives.** A local
`BTreeMap<(MarketId, bool), (Vec<AdlCandidate>, usize)>` in `adl_drain`, dropped when the drain
returns. It is never node state or a context field.
* The `usize` is the per-key **position**: the index of the first candidate not yet used up.
* `adl_close` (extended in A6) takes the slice `&list[at..]` and returns
  `(closes, next, read)`:
  * `next` = how many leading candidates of the slice are used up (fully closed, vanished or
    flipped);
  * `read` = how many candidates it read.
* The drain sets `at += next` and charges `read`.
* So a later row of the same (market, side) in the block continues where the last one stopped.
  A partially used candidate stays at `at` and is re-read once.

This is deterministic because the cache is rebuilt from state in every block (nothing carries
over a restart). It is complete because during the drain only ADL closes run, and they only
shrink real positions.

**M2.** The pass's post-steps run as today (`settle_flat_deficit`, `clear_cooldown`,
`mark_pending`). The account is flat after B, so its pending row clears.
* The vault keeps its rule (pending while ADL-able), but its pending row is now set **after the
  drain**. Today it is set at `liquidation_step.rs:247-250`, before the drain would run. The
  vault is an ordinary candidate, so a drain can push its AV below 0. The pending row then keeps
  the step due, and the next block's vault check moves the vault's marked positions to the
  escrow.
* The `0x07` rows keep the step due (`liquidation_due`).

**W.** One constant, `liq::ADL_WORK_PER_BLOCK`, chosen so an HL-sized event closes the escrows
**in its own block**. Only S=750-type storms spill over. A8 measures it and reports ms per block.
* The sizing case: N = 5,000 traders, 3 bankrupt accounts, all **long** in the same 100 markets
  (one side). That is 300 rows over 100 (market, side) keys.
* Units = 100 rankings × (5,000 + 3 traders in the list: the escrow and up to two protocol
  accounts) + 300 rows × (1 visit + 1 read) = 500,900.
* A two-sided event of the same size has up to 2× the rankings and spills at most into one
  more block.
* A **non-ignored** core test asserts
  `ADL_WORK_PER_BLOCK >= 100 * (5_000 + 3) + 300 * 2`. A6 #8 (200 traders) cannot catch a W
  that is too small at N = 5,000.

**Conservation sum.** A node-local sum over **all** accounts (vault and escrows included) of
available + order margin + UPnL at the mark (unmarked: at entry).
* It is computed after every liquidation step while `ctx.liq_value_sum` is on (the node sets it
  from `TORUS_LIQ_VALUE_SUM=1`; a proof-only flag) and metrics are attached, after the step
  timer stops.
* It sets a gauge and logs one info line `liquidation: value sum` that the harness can parse
  from cell logs.
* With OI symmetric and every market marked, the sum does not depend on the mark
  (Σ UPnL = −Σ signed size × entry), so it must stay constant across a drain without deposits,
  withdrawals or fee debits.

### Dust bound

`apply_fill` (`position.rs:515-519`) averages the entry with
`(entry × size + price × qty) / new_size`, truncating. Each average is off by < 1 raw price unit,
and that error applies to the escrow's **whole** size afterwards. Each close truncates by < 1 raw
value unit. Per escrow and market, in raw value units (SCALE = 10^8):

`|dust| ≤ Σ over rows received ((escrow size after receiving it).raw() / SCALE + 3) + number of closes`

This is the owner's "size × 1 raw per obligation", taken with the aggregated size (open question
2).
* **The bound is a test-only assertion.** Production keeps no accumulators: nothing is
  persisted, and nothing would survive a restart.
* Production logs every sweep at info and adds it to the dust gauge. It logs an **error** when
  `|dust| ≥ 1 token` (10^8 raw). That is a coarse, node-local invariant alarm, many orders above
  any real dust (size × 1 raw ≈ size / 10^8 tokens). Production always sweeps. **Decided (owner
  18c, after dbb683c):** the error line at ≥ 1 token, nothing persisted, the exact bound asserted
  in tests only.

## Funding requirement (owner decision 5; doc-only, no funding on main)

Recorded in `adl-budget.md` §8 and pointed at from `liquidation.md` *Out of scope* (done in this
plan's commit):
* the positions of both escrows are excluded from funding (otherwise an escrow could not end at
  0);
* counterparties keep paying and receiving funding until their row is drained (a storm-only
  difference from HL; HL parity backlog item).

## Success Criteria

1. With more than 65,536 position rows, the top-ranked counterparty at a high address is closed
   first (fails on 7ec5eb2).
2. The C1 records path equals the fallback walk (shadow; the L1 seeded test is green).
3. Rule H:
   * the base changes only when the mark changes;
   * two accounts of one market liquidated in consecutive blocks without a mark change get the
     same pre-clamp price (an S=750-like case);
   * a market without a usable mark deletes the row.
4. Every ADL'd account is flat at exactly 0 at B. The escrow holds its marked positions at the
   clamped prices, with one row each. The vault received exactly the D9 amount, which is never
   positive.
5. The drain stops at W (± one step), resumes in key order, and has one ranking per
   (block, market, side). Two runs give identical rows and native roots.
6. After every block of a multi-block drain:
   * OI is symmetric (escrows included);
   * Σ value over ALL accounts (vault and escrows included) is conserved (exact for
     exact-average fixtures, within the dust bound otherwise);
   * escrow size = Σ rows.
7. After the drain, the escrows have 0 positions and 0 balance once the dust is swept. The vault
   = D9 at B + dust + pairing amounts. The escrow-vs-escrow pairing conserves value.
8. Escrows are never classified and never candidates.
9. An HL-sized event closes in its own block with the default W (unit test, plus a ubench at
   N = 5,000).
10. Backstop, stage 1 and the H1 clamp are unchanged: every existing liquidation test passes
    unchanged. GOLDEN_B is re-pinned for ADL blocks only.
11. The Phase C final proof list holds.

## Tasks

Order: A1+A2 (one commit) → A3 → A4 → A5+A6 (**one commit**: the existing ADL tests need the
drain) → A7 → A8 → Phase C. A3 is independent of A1/A2.

The docs part (adl-budget.md §8, the liquidation.md funding pointer) is in this plan's commit.
The remaining `liquidation.md` updates (H3 window, *ADL* section, D10 → H, the row table, the
escrows) land with the code task they describe.

---

### Task A1: core `adl_candidates` over a trader set, escrows excluded (Q1)

**Test first** (`crates/torus-core/tests/liquidation_tests.rs`). Replace
`adl_candidates_scan_at_most_max_rows` (:274-290):

```rust
/// Q1 (s96): ADL counterparties = every holder on side `want_long` among
/// `traders`, read through `get`, in `traders` order. The vault is an
/// ordinary holder; the two ADL escrows are never candidates (P2). One read
/// per non-escrow trader.
#[test]
fn adl_candidates_are_every_opposite_holder_except_the_escrows() {
    use torus_core::liquidation::{ADL_ESCROW_LONG, ADL_ESCROW_SHORT};
    let (_d, pm) = setup();
    open_pair(&pm, &addr(1), &addr(2), 1, 3, 100);
    open_pair(&pm, &addr(5), &addr(3), 1, 1, 100);
    open_pair(&pm, &addr(4), &LIQUIDATOR_VAULT, 1, 2, 100);
    open_pair(&pm, &ADL_ESCROW_LONG, &ADL_ESCROW_SHORT, 1, 7, 100);
    open_pair(&pm, &addr(7), &addr(6), 2, 1, 100); // market 2 only
    let traders = traders_after(pm.state(), None, usize::MAX).unwrap();
    let reads = std::cell::Cell::new(0);
    let shorts = adl_candidates(&traders, false, |t| { reads.set(reads.get() + 1); pm.get_position(t, 1) }, |_| Ok(fp(1)))
        .unwrap();
    assert_eq!(
        shorts.iter().map(|c| (c.trader, c.size)).collect::<Vec<_>>(),
        vec![(addr(2), fp(3)), (addr(3), fp(1)), (LIQUIDATOR_VAULT, fp(2))]
    );
    assert_eq!(reads.get(), traders.len() - 2, "escrows are not read");
    let longs = adl_candidates(&traders, true, |t| pm.get_position(t, 1), |_| Ok(fp(1))).unwrap();
    assert_eq!(longs.iter().map(|c| c.trader).collect::<Vec<_>>(), vec![addr(1), addr(4), addr(5)]);
}
```

**Implementation** (`crates/torus-core/src/liquidation.rs`):
* Delete `ADL_MAX_SCAN_ROWS` (:41-45). `SCAN_PAGE` stays (`pending_count`).
* After `LIQUIDATOR_VAULT` (:28):

```rust
/// adl-budget P2 (owner s96): the ADL escrows — protocol accounts (no known
/// key) that take a bankrupt account's positions at their ADL price in the
/// bankruptcy block, one per side so opposite obligations never net. Never
/// classified, never ADL candidates, excluded from funding (adl-budget §8).
pub const ADL_ESCROW_LONG: Address = Address::new(*b"torus-adl-escrow-lng");
pub const ADL_ESCROW_SHORT: Address = Address::new(*b"torus-adl-escrow-sht");

/// The escrow that takes a bankrupt position of side `is_long`.
pub fn adl_escrow(is_long: bool) -> Address {
    if is_long { ADL_ESCROW_LONG } else { ADL_ESCROW_SHORT }
}

pub fn is_adl_escrow(a: &Address) -> bool {
    *a == ADL_ESCROW_LONG || *a == ADL_ESCROW_SHORT
}
```

* Replace `adl_candidates` (:340-384):

```rust
/// Q1 (s96): ADL counterparties = every position on side `want_long` of
/// `traders` (ascending, each once; the escrows skipped), `get` reading a
/// trader's position in the ADL market, `av` valuing a holder (ranking only,
/// C7). The ranking's work units are `traders.len()` (Q3).
pub fn adl_candidates(
    traders: &[Address],
    want_long: bool,
    mut get: impl FnMut(&Address) -> Result<Option<Position>, CoreError>,
    mut av: impl FnMut(&Address) -> Result<FixedPoint, CoreError>,
) -> Result<Vec<AdlCandidate>, CoreError> {
    let mut out = Vec::new();
    for t in traders.iter().filter(|t| !is_adl_escrow(t)) {
        let Some(p) = get(t)? else { continue };
        if p.is_long != want_long || p.size <= FixedPoint::ZERO {
            continue;
        }
        out.push(AdlCandidate { trader: *t, is_long: p.is_long, size: p.size, entry_price: p.entry_price, account_value: av(t)? });
    }
    Ok(out)
}
```

* `liquidation.md` H3 (:63-68): the window is superseded by adl-budget Q1.

**Verify:** `cargo nextest run -p torus-core liquidation $F` and `cargo check --workspace --tests -q`.
The bridge breaks at `liquidation_step.rs:505` and `ubench_adl.rs:34,189`; commit together with
A2.
**Depends on:** none.

---

### Task A2: bridge C1 wiring + the fairness test (Q1; still today's ADL flow)

**Test first.**
1. `crates/torus-bridge/tests/liquidation_tests.rs`. It fails on 7ec5eb2: the window ends before
   `top`, so the step closes against `pad(0..4)`. It still passes after A6, because the drain
   runs in B with the default W.

```rust
/// Q1 (s96): ADL counterparties = EVERY opposite-side holder. 230 padding
/// traders (low addresses) x 301 rows = 69,230 rows > 65,536, each short 1 in
/// market 1 with a huge AV (ranks low). `top` (0xFF.., highest address) is
/// short 4 with AV 500 at 900 -> rank (1000/900) x (3600/500) = 8: first.
/// U long 4 @ 1,000, collateral 200: AV -200 at 900 -> ADL; 4 close against top.
#[test]
fn adl_reaches_a_top_ranked_counterparty_past_65536_rows() {
    let (_d, db) = liq_db(&[1]);
    let ctx = ctx_at(db.clone(), 1);
    let pad = |i: u32| {
        let mut a = [0x01u8; 20];
        a[16..].copy_from_slice(&i.to_be_bytes());
        Address::new(a)
    };
    let (u, top, sink) = (addr(0x02), Address::new([0xFF; 20]), Address::new([0xEE; 20]));
    fund(&ctx, &sink, fp(1_000_000_000));
    for i in 0..230 {
        fund(&ctx, &pad(i), fp(1_000_000));
        open_pair(&ctx, &sink, &pad(i), 1, 1, 1_000);
        for m in 2..=301 {
            open_pair(&ctx, &pad(i), &sink, m, 1, 100); // unlisted: valued at entry
        }
    }
    fund(&ctx, &u, fp(200));
    fund(&ctx, &top, fp(100));
    open_pair(&ctx, &u, &top, 1, 4, 1_000);
    assert!(all_positions(&ctx).len() > 65_536);
    let mut c = ctx_at(db.clone(), 2);
    set_mark(&c, 1, fp(900));
    let before = total_value(&c, &marks(&[(1, 900)]));
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(pos(&c, &u, 1), FixedPoint::ZERO);
    assert_eq!(pos(&c, &top, 1), FixedPoint::ZERO, "top-ranked, highest address: closed first");
    assert!((0..230).all(|i| pos(&c, &pad(i), 1) == -fp(1)), "no padding trader touched");
    assert_eq!(oi(&c, 1), (fp(230), fp(230)));
    assert_eq!(total_value(&c, &marks(&[(1, 900)])), before);
}
```

   `total_value` (:102-114) values an unmarked market at entry
   (`marks.get(&p.market_id).map_or(ZERO, |mk| p.unrealized_pnl(*mk))`). The existing callers
   are unchanged.
2. `crates/torus-bridge/src/liquidation_l1_tests.rs`: add `adl_rankings` to `Stats` (:118-144),
   collected at :360-368. Change :426 to
   `assert_eq!(s.traders_slice, 6 * BLOCKS as usize + s.adl_rankings, "E2: pass + ADL rankings from the slot: {s:?}")`
   and add `assert!(s.adl_rankings > 0)`. The shadow checks in `liq_traders_after` (:284-292)
   and `AccountReader::get_position` (NE:1499-1509) then cover every ranking, dirty traders
   included.

**Implementation:**
* NE:1100 `SumsCounters`: add `adl_rankings: std::sync::atomic::AtomicUsize`.
* `liquidation_step.rs`, after `liq_traders_after` (:295):

```rust
/// adl-budget Q1 (C1): `m`'s counterparties on side `want_long` — a point
/// read of every trader of the positions CF (the slot's sorted set merged
/// with the block's dirty traders, else the walk), through the records for a
/// clean trader and the overlay for a dirty one; escrows skipped. Returns the
/// candidates and the traders examined (the ranking's work units, Q3).
fn adl_candidates_of<T: StateBackend>(
    ctx: &NativeExecContext<T>,
    m: MarketId,
    want_long: bool,
) -> Result<(Vec<liq::AdlCandidate>, u64), CoreError> {
    let traders = Self::liq_traders_after(ctx, None, usize::MAX)?;
    let reader = AccountReader::of(ctx);
    // C7: ranking AV with entry fallback; overflow ranks last (AV 0); a
    // storage error stays an error (fail-stop).
    let cands = liq::adl_candidates(&traders, want_long, |t| reader.get_position(t, m), |t| {
        let bal = ctx.positions.get_native_balance(t)?;
        let v = match reader.view(t, &bal) {
            Ok(v) => v,
            Err(CoreError::Overflow(_)) => return Ok(FixedPoint::ZERO),
            Err(e) => return Err(e),
        };
        Ok(v.available.checked_add(v.order_margin).and_then(|x| x.checked_add(v.upnl)).unwrap_or(FixedPoint::ZERO))
    })?;
    #[cfg(test)]
    if let Some(s) = ctx.sums.as_ref() {
        bump(&s.counters.adl_rankings);
    }
    Ok((cands, traders.len() as u64))
}
```

* `adl_account` (:501-517): `let (cands, _) = Self::adl_candidates_of(ctx, m, !p.is_long)?;`
  replaces the window call. A5 replaces `adl_account`.
* `ubench_adl.rs`: drop `ADL_MAX_SCAN_ROWS` (:34, :129-133). `parts` (:183-208) times the
  fallback walk:
  `liq::adl_candidates(&liq::traders_after(&ctx.state, None, usize::MAX)?, false, |t| ctx.positions.get_position(t, m), |_| Ok(ZERO))`.

**Verify:** `cargo nextest run -p torus-bridge adl_reaches_a_top_ranked $F` (RED, then GREEN),
`cargo nextest run -p torus-bridge liquidation $F`, and
`cargo nextest run -p torus-bridge scenario_b_liquidation_digests $F`. GOLDEN_B must stay
**unchanged**: the candidate set is the same below 65,536 rows, and `adl_rank` is total.
**Depends on:** A1.

---

### Task A3: rule H — previous mark = last different mark (D10 change)

**Test first.**
* Core (`liquidation_tests.rs`):

```rust
/// H (owner s96): row `0x03 ‖ m` = last ‖ [prev]; the pre-clamp ADL base is the
/// last mark DIFFERENT from the current one; a step with the same mark writes
/// nothing; without a usable mark the row goes (and the next mark has no base).
#[test]
fn adl_base_is_the_last_different_mark() {
    use std::collections::BTreeMap;
    use torus_core::liquidation::{adl_bases, put_mark_rows};
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let (mut bases_seen, mut writes_seen) = (Vec::new(), Vec::new());
    for mark in [Some(990), Some(990), Some(900), Some(900), Some(880), None, Some(870)] {
        let marks: BTreeMap<u64, FixedPoint> = mark.map(|p| (1u64, fp(p))).into_iter().collect();
        let (bases, rows) = adl_bases(&db, &[1], &marks).unwrap();
        bases_seen.push(bases.get(&1).copied());
        writes_seen.push(put_mark_rows(&db, &[1], &marks, &rows).unwrap());
    }
    assert_eq!(bases_seen, vec![None, None, Some(fp(990)), Some(fp(990)), Some(fp(900)), None, None]);
    assert_eq!(writes_seen, vec![1, 0, 1, 0, 1, 1, 1], "a write only when the mark changes or goes");
}
```

* Bridge (`tests/liquidation_tests.rs`), the S=750-like case:
  `same_mark_interval_gives_the_same_pre_clamp_price`.
  * u1 and u2 are long 10 @ 1,000 in market 1 with collateral 1,000 each; S is short 20.
  * Block 1, mark 1,000: both healthy.
  * Before block 2: u1's balance is set to 600 (ADL at 900). Block 2, mark 900, run with
    `scan = 1`, `act = 1`: only u1 is reached.
  * Before block 3: u2's balance is set to 600. Block 3, mark still 900 (no change).
  * Both ADL prices must be `adl_price(1_000, bankruptcy(u), 900, true)`: base 1,000 for both.
    With today's D10, u2 would get base 900.
  * Until A5 exists, the test reads the price from the `liquidation: ADL` info line (a tracing
    capture like `AdlClock`). A5 switches it to the obligation rows (W = 0).
* The existing `a_prev_mark_older_than_the_previous_usable_mark_is_ignored` (:845-865: the row
  is deleted when the mark goes) and :392-403 (a first mark writes a 16-byte row) pass
  **unchanged**.

**Implementation** (`liquidation.rs`, replacing `prev_marks` / `put_prev_marks` :549-589):

```rust
/// Rule H (owner s96, changes D10): `0x03 ‖ m` -> last(16) ‖ [prev(16)]: the
/// market's last usable mark and the mark before it that DIFFERED from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkRow {
    pub last: FixedPoint,
    pub prev: Option<FixedPoint>,
}

/// The rows of `markets` and each marked market's pre-clamp ADL base this
/// step: the old `last` when the mark changed, else `prev` (absent: the
/// caller uses the mark, D10's fallback).
pub fn adl_bases<T: StateBackend>(
    state: &T,
    markets: &[MarketId],
    marks: &BTreeMap<MarketId, FixedPoint>,
) -> Result<(BTreeMap<MarketId, FixedPoint>, BTreeMap<MarketId, MarkRow>), CoreError> {
    let raw = |b: &[u8]| -> Result<FixedPoint, CoreError> {
        Ok(FixedPoint::from_raw(i128::from_be_bytes(b.try_into().map_err(|_| malformed("prev mark"))?)))
    };
    let (mut bases, mut rows) = (BTreeMap::new(), BTreeMap::new());
    for &m in markets {
        let Some(v) = state.get_cf_raw(CF_NATIVE_LIQUIDATION, &prev_mark_key(m))? else { continue };
        let row = match v.len() {
            16 => MarkRow { last: raw(&v)?, prev: None },
            32 => MarkRow { last: raw(&v[..16])?, prev: Some(raw(&v[16..])?) },
            _ => return Err(malformed("prev mark")),
        };
        if let Some(mark) = marks.get(&m) {
            if let Some(b) = if *mark != row.last { Some(row.last) } else { row.prev } {
                bases.insert(m, b);
            }
        }
        rows.insert(m, row);
    }
    Ok((bases, rows))
}

/// Rule H: a listed market with a usable mark that differs from its row's
/// `last` (or has no row) gets (last = mark, prev = old last); an unchanged
/// mark writes nothing; a listed market without a usable mark loses its row
/// (review H1: never a base from before an oracle outage). Returns the writes.
pub fn put_mark_rows<T: StateBackend>(
    state: &T,
    listed: &[MarketId],
    marks: &BTreeMap<MarketId, FixedPoint>,
    rows: &BTreeMap<MarketId, MarkRow>,
) -> Result<usize, CoreError> {
    let mut writes = 0;
    for m in listed {
        let k = prev_mark_key(*m);
        match (marks.get(m), rows.get(m)) {
            (Some(p), Some(r)) if r.last == *p => {}
            (Some(p), r) => {
                let mut v = p.raw().to_be_bytes().to_vec();
                if let Some(r) = r {
                    v.extend_from_slice(&r.last.raw().to_be_bytes());
                }
                state.put_cf_raw(CF_NATIVE_LIQUIDATION, &k, &v)?;
                writes += 1;
            }
            (None, Some(_)) => {
                state.delete_cf_raw(CF_NATIVE_LIQUIDATION, &k)?;
                writes += 1;
            }
            (None, None) => {}
        }
    }
    Ok(writes)
}
```

* `liquidation_step.rs:164` and `:251`:
  `let (prev, mark_rows) = liq::adl_bases(&ctx.state, &listed, &marks)?;` and
  `liq::put_mark_rows(&ctx.state, &listed, &marks, &mark_rows)?;`. `prev` keeps its type
  (`Marks`), so the ADL code is unchanged: `prev.get(&m)`, else the mark.
* `liquidation.md`: the D10 row (:292), the table (:225), and H1's "previous mark" wording →
  rule H, adl-budget §8 (cursor-independent within one mark interval only).

**Verify:** `cargo nextest run -p torus-core adl_base $F`, `cargo nextest run -p torus-bridge liquidation $F`,
`cargo nextest run -p torus-bridge unmarked $F` (s87 Fix 2a rows), and the L1 seeded test.
GOLDEN_B: scenario B may have a block whose mark repeats before its ADL. Re-pin only if the
first changed digest is such a block.
**Depends on:** none.

---

### Task A4: core obligation rows, `adl_close` limit, `cross_close`, `liquidation_due`

**Test first** (`crates/torus-core/tests/liquidation_tests.rs`):

```rust
/// P2 (s96): obligation rows `0x07 ‖ height ‖ market ‖ side ‖ trader` ->
/// size ‖ price, read in key order (height, market, side short-before-long,
/// trader); size 0 deletes the row.
#[test]
fn adl_obligations_are_fifo_rows() {
    use torus_core::liquidation::{next_obligation, put_obligation, Obligation, ADL_OBLIGATION_TAG};
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let o = |h, market, is_long, n, size, price| Obligation { height: h, market, is_long, trader: addr(n), size: fp(size), price: fp(price) };
    for x in [o(6, 1, true, 1, 2, 950), o(5, 2, true, 9, 1, 990), o(5, 2, false, 9, 3, 1_010), o(5, 1, true, 8, 4, 940)] {
        put_obligation(&db, &x).unwrap();
    }
    let mut got = Vec::new();
    let mut start = vec![ADL_OBLIGATION_TAG];
    while let Some(x) = next_obligation(&db, &start).unwrap() {
        start = [x.key().as_slice(), &[0]].concat();
        got.push(x);
    }
    assert_eq!(got, vec![o(5, 1, true, 8, 4, 940), o(5, 2, false, 9, 3, 1_010), o(5, 2, true, 9, 1, 990), o(6, 1, true, 1, 2, 950)]);
    put_obligation(&db, &Obligation { size: FixedPoint::ZERO, ..got[0] }).unwrap();
    assert_eq!(next_obligation(&db, &[ADL_OBLIGATION_TAG]).unwrap(), Some(got[1]));
}

/// P2 edge: escrow long sells q at p_long, escrow short buys q at p_short;
/// the vault pays (p_long - p_short) x q (credited +400 here). OI -q on both
/// sides; value at the mark conserved (escrows + vault).
#[test]
fn cross_close_conserves_value_through_the_vault() {
    use torus_core::liquidation::{cross_close, ADL_ESCROW_LONG as EL, ADL_ESCROW_SHORT as ES};
    let (_d, pm) = setup();
    pm.apply_fill(&EL, 1, true, fp(10), fp(950), MarginType::Cross).unwrap();
    pm.apply_fill(&ES, 1, false, fp(10), fp(990), MarginType::Cross).unwrap();
    let who = [EL, ES, LIQUIDATOR_VAULT];
    let before = value(&pm, &who, 1, fp(900));
    assert_eq!(cross_close(&pm, 1, fp(10), fp(950), fp(990), &LIQUIDATOR_VAULT).unwrap(), fp(400));
    assert!(pm.get_position(&EL, 1).unwrap().is_none() && pm.get_position(&ES, 1).unwrap().is_none());
    assert_eq!(pm.get_native_balance(&LIQUIDATOR_VAULT).unwrap().available, fp(400));
    assert_eq!(value(&pm, &who, 1, fp(900)), before);
}
```

Also:
* `adl_close_pairs_against_ranked_counterparties_at_the_price` (:188-211): pass `fp(4)` as the
  limit;
* new `adl_close_stops_at_the_qty_limit` (limit `fp(2)` → `[(s2, fp(2))]`, U keeps 2);
* bridge `liquidation_due_reads_cooldown_and_cursor_rows` (:773-780): a `put_obligation` makes it
  due, and size 0 makes it not due.

**Implementation** (`liquidation.rs`):

```rust
/// P2 (s96): `0x07 ‖ height(8) ‖ market(8) ‖ side(1: 1 long) ‖ trader(20)` ->
/// `size raw (16, BE) ‖ price raw (16, BE)`: what the escrow of `is_long`
/// still owes for `trader`'s position taken at block `height` at `price`
/// (per account: H1's clamp).
pub const ADL_OBLIGATION_TAG: u8 = 0x07;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Obligation {
    pub height: u64,
    pub market: MarketId,
    pub is_long: bool,
    pub trader: Address,
    pub size: FixedPoint,
    pub price: FixedPoint,
}

impl Obligation {
    pub fn key(&self) -> [u8; 38] {
        let mut k = [0u8; 38];
        k[0] = ADL_OBLIGATION_TAG;
        k[1..9].copy_from_slice(&self.height.to_be_bytes());
        k[9..17].copy_from_slice(&self.market.to_be_bytes());
        k[17] = u8::from(self.is_long);
        k[18..].copy_from_slice(self.trader.as_slice());
        k
    }
}

/// Write `o` (size > 0) or delete its row (size <= 0).
pub fn put_obligation<T: StateBackend>(state: &T, o: &Obligation) -> Result<(), CoreError> {
    if o.size <= FixedPoint::ZERO {
        state.delete_cf_raw(CF_NATIVE_LIQUIDATION, &o.key())?;
        return Ok(());
    }
    let v = [o.size.raw().to_be_bytes(), o.price.raw().to_be_bytes()].concat();
    state.put_cf_raw(CF_NATIVE_LIQUIDATION, &o.key(), &v)?;
    Ok(())
}

/// The first obligation at or after `start` (the bare tag, or a key + 0x00).
pub fn next_obligation<T: StateBackend>(state: &T, start: &[u8]) -> Result<Option<Obligation>, CoreError> {
    let Some((k, v)) = state.iterate_cf_prefix_from(CF_NATIVE_LIQUIDATION, &[ADL_OBLIGATION_TAG], start, 1)?.pop() else {
        return Ok(None);
    };
    if k.len() != 38 || v.len() != 32 || k[17] > 1 {
        return Err(malformed("adl obligation"));
    }
    let raw = |b: &[u8]| i128::from_be_bytes(b.try_into().expect("16 bytes"));
    Ok(Some(Obligation {
        height: u64::from_be_bytes(k[1..9].try_into().expect("8 bytes")),
        market: MarketId::from_be_bytes(k[9..17].try_into().expect("8 bytes")),
        is_long: k[17] == 1,
        trader: Address::from_slice(&k[18..]),
        size: FixedPoint::from_raw(raw(&v[..16])),
        price: FixedPoint::from_raw(raw(&v[16..])),
    }))
}

/// P2 edge (real holders exhausted): escrow long sells `q` at `p_long`, escrow
/// short buys `q` at `p_short`. Two prices realize (p_long - p_short) x q more
/// than one shared price would; the vault pays it, so value is conserved.
/// Returns the vault's change (+ = credited).
pub fn cross_close<T: StateBackend>(
    pm: &PositionManager<T>,
    m: MarketId,
    q: FixedPoint,
    p_long: FixedPoint,
    p_short: FixedPoint,
    vault: &Address,
) -> Result<FixedPoint, CoreError> {
    let of = |_| CoreError::Overflow("adl cross close overflows i128".into());
    pm.apply_fill(&ADL_ESCROW_LONG, m, false, q, p_long, MarginType::Cross)?;
    pm.apply_fill(&ADL_ESCROW_SHORT, m, true, q, p_short, MarginType::Cross)?;
    let paid = p_short.checked_sub(p_long).map_err(of)?.checked_mul(q).map_err(of)?;
    let mut vb = pm.get_native_balance(vault)?;
    vb.available = vb.available.checked_add(paid).map_err(of)?;
    pm.put_native_balance(vault, &vb)?;
    Ok(paid)
}
```

* `adl_close` (:309-338) gains `qty: FixedPoint` after `price`:
  `let mut remaining = up.size.min(qty);`.
* `liquidation_step.rs:144-148` `liquidation_due`:
  `|| state.prefix_exists(CF_NATIVE_LIQUIDATION, &[liq::ADL_OBLIGATION_TAG])?`.
* **As built (c782dc0):** `put_obligation` inserts and **errors on an existing key**, which
  cannot happen at B (one classification per account per block; a re-bankruptcy gets a new
  height), or deletes at size ≤ 0. It never overwrites. The drain's partial remainder therefore
  needs `update_obligation` (A6: overwrite an existing row, error if it is missing, delete at
  0). The snippet above predates the built version and only shows the row format.

**Verify:** `cargo nextest run -p torus-core liquidation $F`, `cargo nextest run -p torus-bridge liquidation_due $F`,
`cargo check --workspace --tests -q`. Bridge :517 passes `p.size` as the limit until A5.
**Depends on:** A1 (escrow constants).

---

### Task A5: at B — positions to the escrows, obligations, D9 (P2)

**Test first** (`tests/liquidation_tests.rs`, section `// ---- adl-budget P2 ----`). All tests
here run the step with **W = 0** (no drain).

Fixture `p2_fixture`, a shock from 1,000 to 900 so that the clamp binds:
* listed markets `1..=4`; block 1 at mark 1,000, then 900 from block 2 on;
* `u1 = addr(0x29)`: collateral 100 (AV 100 ≥ MM 100 at 1,000; −300 at 900);
* `u2 = addr(0x28)`: collateral 137.00000001, so its market-3 price 962.99999999 makes the
  escrow's average inexact (dust);
* `u3 = addr(0x21)`: 1,000 (healthy at 900; cut to 100 before block 3 → ADL at 3, same mark
  interval, base 1,000);
* u1, u2 and u3 are each long 1 @ 1,000 in every market;
* `c(i) = addr(0x40+i)`, `i < 4`: short 3 in every market, 10^7 each;
* `sink = addr(0x60)`: long 9 in every market, 10^7 (OI 12 / 12).

```rust
fn step(db: &StateDb, h: u64, mark: i64, w: u64) -> NativeExecContext {
    let mut c = ctx_at(db.clone(), h);
    for m in 1..=4 {
        set_mark(&c, m, fp(mark));
    }
    NativeExecutor::run_liquidations_with(&mut c, 2_048, 64, w);
    c
}

fn obligations(ctx: &NativeExecContext) -> Vec<Obligation> {
    let (mut out, mut start) = (Vec::new(), vec![ADL_OBLIGATION_TAG]);
    while let Some(o) = next_obligation(&ctx.state, &start).unwrap() {
        start = [o.key().as_slice(), &[0]].concat();
        out.push(o);
    }
    out
}

/// OI symmetric (escrows included), escrow size = Σ rows per (market, side),
/// Σ value over ALL accounts within `tol` raw of `before` (mark-independent
/// under OI symmetry, so comparable across blocks).
fn invariants(ctx: &NativeExecContext, mark: i64, before: FixedPoint, tol: i128) {
    for m in 1..=4 {
        let (l, s) = oi(ctx, m);
        assert_eq!(l, s, "OI symmetric in {m}");
        for side in [true, false] {
            let owed = obligations(ctx).iter().filter(|o| o.market == m && o.is_long == side).fold(FixedPoint::ZERO, |a, o| a + o.size);
            assert_eq!(pos(ctx, &adl_escrow(side), m).abs(), owed, "escrow {side} in {m} = Σ rows");
        }
    }
    let now = total_value(ctx, &marks(&[(1, mark), (2, mark), (3, mark), (4, mark)]));
    assert!((now - before).raw().abs() <= tol, "Σ over ALL accounts (vault, escrows): {now:?} vs {before:?}");
}
```

1. `p2_a_bankrupt_account_is_flat_with_zero_collateral_at_b` (block 1 at 1,000 with W = 0, then
   block 2 at 900 with W = 0):
   * u1 and u2 have no position, `ab == (0, 0)`, and no `0x06` row.
   * `ESCROW_LONG` is long 2 in every market.
   * There are 8 rows `(2, m, long, u)`. Each price is
     `adl_price(1_000, bankruptcy_price(rest, true, 1, 1_000), 900, true)`, computed by the test
     sequentially. For u1 the prices are 1,000, 1,000, 1,000, 900 (the clamp binds at m4). For
     u2 they are 1,000, 1,000, 962.99999999, 900.
   * The vault's available = the D9 amounts: 0 for both. Under H the deficit is ~0, as the S=750
     explanation predicts.
   * `invariants(.., 900, before, 0)`; `liquidation_due` is true; `liquidations_adl == 2`.
2. `p2_d9_remainder_is_never_positive` (replaces the earlier "positive remainder" test: a
   positive D9 remainder cannot occur under the one-sided clamp, see Design step 3). It is a
   seeded loop over 50 accounts, each ADL'd at its own B (W = 0), with:
   * positions in several markets, both sides;
   * one position in a **listed but unmarked** market (valued at entry in `rest`, not acted on,
     H2);
   * a resting order whose **order margin** D4 releases;
   * a rule-H base above **and** below the mark (earlier blocks at varying marks).

   Per account: the vault's delta at B ≤ 0; the account's cash `ab == (0, 0)` with its marked
   positions flat; and no `positive D9 remainder` error line (a tracing capture like
   `AdlClock`). In the same loop, the `#[cfg(test)]` shadow inside `adl_to_escrow` checks the
   running `rest` == `adl_rest` for every market.
3. `p2_escrows_are_never_classified`: blocks 1 and 2 with W = 0, then block 3 with W = 0,
   metered.
   * Neither escrow has a `0x06` or `0x02` row.
   * The **counters** `liquidation_scanned` / `liquidation_acted` move by exactly the
     non-protocol accounts of each block; the escrows are never counted.
   * The escrows' positions are unchanged by the pass, although their AV at 900 is negative.
4. `p2_the_vault_moves_its_positions_to_the_escrow`: the setup of
   `the_vault_is_adld_when_its_value_goes_negative` (:711-732), block 2 with W = 0.
   * The vault is flat. `ESCROW_LONG` is long 10 @ 970, with the row
     `(2, 1, long, VAULT) → (10, 970)`.
   * The vault's available = 0. There is no `move_collateral` for the vault.
5. `same_mark_interval_gives_the_same_pre_clamp_price` (from A3) switches to reading the rows.

**Implementation** (`liquidation_step.rs`):
* :166-181: `scan.saturating_add(4)`, because the vault and both escrows are skipped. Use
  `let not_protocol = |a: &&Address| **a != LIQUIDATOR_VAULT && !liq::is_adl_escrow(a);` for
  `not_vault` (:174, :175, :181).
* Replace `adl_account` (:481-544) with `adl_to_escrow` (call sites :222, :244):

```rust
    /// adl-budget P2 (owner s96): terms fixed at B. Every MARKED position of
    /// `u` (ascending market) moves to the escrow of its side at its ADL price
    /// — the rule-H base (the mark without one) clamped one-sided to `u`'s
    /// bankruptcy price (review H1, unchanged) — and its obligation is queued.
    /// `rest` (cash + the UPnL of u's OTHER positions: marked at the mark,
    /// unmarked at entry = 0) is kept running: rows read once, one balance
    /// point read per transfer — O(P) (was `adl_rest` per market: O(P^2)).
    /// Then D9: a non-vault account without marked positions hands its
    /// remaining collateral to the vault. Never positive under the clamp (an
    /// error line if it is; the move still conserves value): flat, exactly 0.
    fn adl_to_escrow<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        marks: &Marks,
        prev: &Marks,
        u: &Address,
    ) -> Result<(), CoreError> {
        let ps = ctx.positions.positions_for_trader(u)?;
        // `build`'s terms per position (same function as adl_rest's build);
        // None = overflow -> no bankruptcy price (adl_rest's None).
        let upnl = |p: &Position| {
            torus_core::margin::position_terms(p, marks.get(&p.market_id).copied(), None).ok().map(|t| t.upnl)
        };
        let mut total = ps.iter().try_fold(FixedPoint::ZERO, |a, p| a.checked_add(upnl(p)?).ok());
        for p in &ps {
            let m = p.market_id;
            let Some(&mark) = marks.get(&m) else { continue };
            let own = upnl(p);
            let bal = ctx.positions.get_native_balance(u)?;
            let rest = (|| {
                let others = total?.checked_sub(own?).ok()?;
                bal.available.checked_add(bal.order_margin).ok()?.checked_add(others).ok()
            })();
            #[cfg(test)]
            assert_eq!(rest, Self::adl_rest(ctx, marks, u, m)?, "running rest == adl_rest ({u}, {m})");
            let base = prev.get(&m).copied().unwrap_or(mark);
            let bankruptcy = rest.and_then(|r| liq::bankruptcy_price(r, p.is_long, p.size, p.entry_price));
            let px = liq::adl_price(base, bankruptcy, mark, p.is_long);
            liq::transfer(&ctx.positions, u, &liq::adl_escrow(p.is_long), m, p.size, px)?;
            total = (|| total?.checked_sub(own?).ok())();
            let o = liq::Obligation { height: ctx.block_height, market: m, is_long: p.is_long, trader: *u, size: p.size, price: px };
            liq::put_obligation(&ctx.state, &o)?; // A4 as built: the insert errors on an existing key
            tracing::info!(height = ctx.block_height, account = %u, market = m, size = %p.size, base = %base, bankruptcy = ?bankruptcy, price = %px, "liquidation: ADL to escrow");
        }
        if *u != LIQUIDATOR_VAULT
            && !ctx.positions.positions_for_trader(u)?.iter().any(|p| marks.contains_key(&p.market_id))
        {
            let moved = liq::move_collateral(&ctx.positions, u, &LIQUIDATOR_VAULT)?;
            if moved > FixedPoint::ZERO {
                tracing::error!(account = %u, %moved, "liquidation: positive D9 remainder after ADL (clamp invariant broken)");
            }
        }
        Ok(())
    }
```

* `adl_rest` (:548-570) loses its production caller and becomes `#[cfg(test)]`: it is the
  shadow reference for the running `rest` (equal whenever no partial sum overflows, which holds
  for every fixture).
* `run_liquidations_with(ctx, scan, act, work: u64)`: thread `work` through to
  `liquidation_pass`. `run_liquidations` passes `liq::ADL_WORK_PER_BLOCK`. That is 12 call
  sites: `liquidation_l1_tests.rs:339`, `tests/liquidation_tests.rs` ×8 and
  `perf_equivalence_golden.rs:218`, all passing `liq::ADL_WORK_PER_BLOCK`.
* `liquidation.rs`: `pub const ADL_WORK_PER_BLOCK: u64 = 1_000_000;` with the doc "Q3: traders
  examined by a ranking + closes (+ edge rows) per block; chosen (A8) so an HL-sized event (a
  few hundred account-markets) closes the escrows in its own block; one constant." Placeholder
  until A8.
* `liquidation.md` *ADL* (:194-208) and *Vault*: the P2 flow, the escrows, and a pointer here.

**Verify:** `cargo nextest run -p torus-bridge p2_ $F`, `cargo nextest run -p torus-bridge same_mark_interval $F`.
The existing ADL tests stay RED until A6 (same commit).
**Depends on:** A2, A3, A4.

**As built (dfa170c, one commit with A6), deviations:**
* #1 asserts Σ value within **1 raw**, not 0: the escrow's market-3 average
  `(962.99999999 + 1,000) / 2` truncates, so its UPnL at 900 is off by 1 raw at B already
  (checked: with 0 the test fails by exactly 1 raw).
* #2: the running-`rest` shadow is `#[cfg(test)]` inside the bridge library, so it cannot run
  in the `tests/liquidation_tests.rs` binary. It runs in the lib's seeded L1 test
  (`liquidation_l1_equals_reference_walk_on_seeded_sequences`: 78 ADL classifications over
  6 seeds, every market of every ADL'd account); #2 checks the rest.
* 18c review of A1-A4 landed here: `cross_close` refuses q ≤ 0 or an escrow without ≥ q on its
  side (a node fault in the step); `put_obligation` / `update_obligation` delete only at size 0
  and reject size < 0 or price ≤ 0; the `cross_close` test has entries ≠ close prices, a
  fractional q and a partial close; a malformed `0x03` row is fatal (test).

---

### Task A6: the drain under W (Q2, Q3, P2), edge pairing, dust sweep

**Test first** (`tests/liquidation_tests.rs`, `p2_fixture`; `A = traders(ctx).len()` at drain
time: escrows are in the list, and the flat accounts are not):
1. `p2_drain_stops_at_w_and_resumes_in_fifo_order`, with a fixed `W = 19`.
   * Cost model: a row costs 1 (visit) + `A_h` if it is the first row of its (market, side) in
     block h (the cache is **per block**) + 1 read. Each row needs size 1, and the top-ranked
     short always has ≥ 1 left, so `read` = 1.
   * `A_h` = traders with positions after block h's pass (escrow included). The test asserts
     `A_2 = 7` (c0..c3, sink, u3, `ESCROW_LONG`) and `A_3.. = 6` (u3 flat at its B = 3).
   * Block 2: (2,1,L,u2) 9 → (2,1,L,u1) 2 [11] → (2,2,L,u2) 9 [20 ≥ 19, stop]: **3 rows**.
   * Block 3 (u3's 4 rows at height 3 sort after every height-2 row): (2,2,L,u1) 8 →
     (2,3,L,u2) 8 [16] → (2,3,L,u1) 2 [18] → (2,4,L,u2) 8 [26, stop]: **4 rows**.
   * Block 4: (2,4,L,u1) 8 → (3,1,L,u3) 8 [16] → (3,2,L,u3) 8 [24, stop]: **3 rows**.
   * Block 5: (3,3,L,u3) and (3,4,L,u3): **2 rows**, and the queue is empty.
   * (7c2c784 expected 3 rows in block 3, which ignored that the cache is per block. The
     numbers above use the per-block cache and the per-row visit unit.)
   * The remaining keys are asserted after every block, with `invariants(.., 900, before, bound)`.
2. `p2_drain_overshoots_by_at_most_one_step` (block 2, `A = 7`): `W = A + 2 = 9` drains exactly
   one row (the first costs 9, and 9 is not < 9). `W = A + 3 = 10` drains two: the second row
   starts at 9 < 10 and completes at 11, an overshoot of one step.
3. `p2_ranks_each_market_side_once_per_block` (`liquidation_l1_tests.rs`, which has the
   counters): with a large W, block 2's `counters.adl_rankings` = 4, one per (market, short),
   not 8.
4. `p2_escrows_end_flat_with_zero_balance_and_the_vault_holds_the_deficit` (default W):
   * after the drain, both escrows have no position and `ab == (0, 0)`;
   * `dust = vault − D9_at_B`, and `|dust| ≤` the *Dust bound* (u2's 962.99999999 makes market
     3's average inexact, so the dust is not 0 by construction);
   * `invariants` within `|dust|`.
5. `p2_counterparties_are_paid_at_the_stored_price`: rows written at B with W = 0. The next block
   has mark 800 and the default W, and the closes still use the stored prices (terms fixed at
   B). Each counterparty's realized PnL = `(entry − price) × q`.
6. `p2_exhausted_counterparties_pair_the_escrows`: market 1 only. A long 10 @ 1,000 and B short
   10 @ 800 are the only holders.
   * How the fixture gets there: a helper X takes the other side of both fills via
     `apply_fill`. X sells 10 @ 1,000 to A, then buys 10 @ 800 from B. X ends flat (realized
     +2,000, which stays in X's balance and in the total). So no real holder is left except A
     and B.
   * Block 1 at 990 with both funded 10,000. Before block 2 both are set to 500. Block 2 at 900:
     both are ADL with base 990. A: bankruptcy 1,000 − 500/10 = 950, so `min(990, 950)` = 950.
     B: bankruptcy 800 + 500/10 = **850**, so `max(990, 850)` = 990.
   * D9: A ends at 0; B at −1,400, which goes to the vault.
   * Drain: the short row sorts first and has no real long holder, so it pairs with A's row:
     q = 10, and the vault receives (990 − 950) × 10 = +400, ending at −1,000.
   * Both escrows end flat with balance 0, no rows are left, and OI is (0, 0).
   * Total value over A, B, the vault and the escrows is unchanged at −1,000. The pairing amount
     shows in the stats and the log (A7 gauge).
7. `p2_drain_is_deterministic`: two fresh runs of `p2_fixture` (default W and `W = A + 3`). Per
   block, `iterate_cf` of `CF_NATIVE_POSITIONS`, `CF_NATIVE_BALANCES` and
   `CF_NATIVE_LIQUIDATION` are equal.
8. `an_hl_sized_event_closes_in_its_own_block` (default W): 200 traders hold positions in 100
   listed markets, and 3 accounts long in all 100 markets go bankrupt in one block
   (300 account-markets). After that one step: no `0x07` row, both escrows flat at 0 balance,
   every ADL'd account at 0, OI symmetric, value conserved. The ubench repeats this at
   N = 5,000 (A8).
9. The existing tests pass **unchanged** with the default W: the clamp and rule H give the same
   prices for a first mark change, and the drain finishes in B.
   * `adl_closes_against_ranked_counterparties_at_the_previous_mark`: escrow entry 950, closes at
     950, 0 realized.
   * `adl_without_a_previous_mark_…`, `the_liquidation_step_publishes_the_vault_deficit`,
     `the_vault_is_adld_when_its_value_goes_negative`, `telemetry_counts_an_adl_account`,
     `telemetry_counts_the_vault_adl_outside_the_act_budget`,
     `a_prev_mark_older_than_the_previous_usable_mark_is_ignored`.
   * The L1 seeded test and `offmark_bad_debt_tests` (:14, :278, :350, :357).

10. `p2_rows_of_an_unmarked_market_wait_and_cost_a_unit`: rows at B in markets 1 and 2. Block 3
    has market 1's mark stale (listed, unmarked).
    * With `W = 1`: the market-1 row is visited (1 unit) and the budget is used, so nothing
      closes.
    * With the default W: the market-1 rows stay, the market-2 rows drain, and
      `liquidation_due` stays true.
    * Block 4 (mark back): market 1 drains.
11. `p2_rows_of_a_delisted_market_close_at_the_stored_price` (the rule is pending 18c, open
    question 1): after B, market 2's `CF_NATIVE_MARKETS` row is deleted (delisted).
    * The next drain ranks with each row's stored price in place of the mark and closes at the
      stored price.
    * The escrow goes flat; OI and value invariants hold.
12. `p2_a_drain_that_sinks_the_vault_keeps_the_step_due` (the vault pending row is set **after**
    the drain).
    * The vault is short 5 @ 900 in market 1, with cash 10 and a marked long 1 in market 2. It is
      market 1's only short holder.
    * An escrow-long row of 5 at stored price 1,000 closes against it: the vault pays
      5 × 100 = 500, so its AV < 0.
    * After the step: a `0x06 ‖ VAULT` row exists and `liquidation_due` is true. The next (empty)
      block runs, and the vault's market-2 long moves to `ESCROW_LONG`.
    * With the pending row set before the drain (7c2c784), the row would be missing.
13. Core, **non-ignored**: `adl_work_per_block_covers_an_hl_sized_event`, asserting
    `ADL_WORK_PER_BLOCK >= 100 * (5_000 + 3) + 300 * 2`. That is N = 5,000, one side, 100
    (market, side) keys, and 300 rows × (visit + read).
14. Core: `adl_close_reports_next_and_read`. Candidates [gone, 2, 3] with qty 4: closes
    `[(c1, 2), (c2, 2)]`, `next = 2` (the vanished one and c1 used up; c2 keeps 1), `read = 3`.

**Implementation** (`liquidation_step.rs`; core `adl_close` extended):
* `liquidation.rs` `update_obligation(state, &o)`: overwrite an existing row with `o.size`, or
  delete it at size ≤ 0; error if the row is missing. Core test
  `update_obligation_overwrites_and_deletes`: insert, update to a remainder, update to 0 (gone),
  update a missing row (error).
* `liquidation.rs` `adl_close` returns `(Vec<(Address, FixedPoint)>, usize, usize)`, i.e.
  `(closes, next, read)`: `next` = leading candidates used up (vanished, flipped or fully
  closed), `read` = candidates read. It has one production caller (the drain) plus the core
  tests.
* After the vault block (:238-250, with `adl_to_escrow` in place of `adl_account`):
  `if work > 0 && liq::next_obligation(&ctx.state, &[liq::ADL_OBLIGATION_TAG])?.is_some() { Self::adl_drain(ctx, &listed, &marks, work, stats)?; }`.
  An empty queue costs one seek.
* **Vault pending after the drain:** move the vault's `set_pending` (:247-250) below the
  `adl_drain` call. Same expression, evaluated on the state after the drain.

```rust
    /// adl-budget P2 / Q2 / Q3: drain the obligation rows in key order under
    /// `work` units. One step = one row (atomic; starts only while used <
    /// work, so a block overshoots by at most one step). Every visited row
    /// costs 1, plus its (market, side) ranking (once per block: `ranked`
    /// holds the list and the position of the first candidate not yet used
    /// up, so later rows continue where the last one stopped), plus the
    /// candidates `adl_close` read, plus edge rows. The escrow of the row's
    /// side closes at the row's stored price; real holders exhausted ->
    /// [`Self::adl_cross`]. A listed market without a usable mark waits; a
    /// delisted one ranks at the stored price (open question 1). Then a flat
    /// escrow's balance (dust) goes to the vault.
    fn adl_drain<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        listed: &[MarketId],
        marks: &Marks,
        work: u64,
        stats: &mut LiqStats,
    ) -> Result<(), CoreError> {
        let mut ranked: BTreeMap<(MarketId, bool), (Vec<liq::AdlCandidate>, usize)> = BTreeMap::new();
        let (mut used, mut start) = (0u64, vec![liq::ADL_OBLIGATION_TAG]);
        while used < work {
            let Some(mut o) = liq::next_obligation(&ctx.state, &start)? else { break };
            start = [o.key().as_slice(), &[0]].concat();
            used += 1; // the visit: a waiting row is never free
            let rank_px = match marks.get(&o.market) {
                Some(&mark) => mark,
                None if listed.binary_search(&o.market).is_err() => o.price, // delisted
                None => continue,                                           // listed, stale: wait
            };
            let key = (o.market, o.is_long);
            if !ranked.contains_key(&key) {
                let (c, examined) = Self::adl_candidates_of(ctx, o.market, !o.is_long)?;
                used += examined;
                ranked.insert(key, (liq::adl_rank(rank_px, c), 0));
            }
            let escrow = liq::adl_escrow(o.is_long);
            let (list, at) = ranked.get_mut(&key).expect("ranked above");
            let (closes, next, read) = liq::adl_close(&ctx.positions, &escrow, o.market, o.price, o.size, &list[*at..])?;
            *at += next;
            used += read as u64;
            for (c, q) in &closes {
                tracing::debug!(market = o.market, account = %o.trader, counterparty = %c, size = %q, price = %o.price, "liquidation: ADL close");
                o.size -= *q;
            }
            if o.size > FixedPoint::ZERO {
                used += Self::adl_cross(ctx, &mut o, stats)?;
            }
            liq::update_obligation(&ctx.state, &o)?; // A6: overwrite the remainder, delete at 0
            stats.adl_obligations += 1;
            if o.size > FixedPoint::ZERO {
                tracing::error!(?o, "liquidation: ADL obligation left open (OI asymmetry?) — retried next block");
            }
        }
        for e in [liq::ADL_ESCROW_LONG, liq::ADL_ESCROW_SHORT] {
            if ctx.positions.positions_for_trader(&e)?.is_empty() {
                let dust = liq::move_collateral(&ctx.positions, &e, &LIQUIDATOR_VAULT)?;
                if dust != FixedPoint::ZERO {
                    stats.adl_dust += dust;
                    tracing::info!(escrow = %e, %dust, "liquidation: ADL escrow dust to the vault");
                    // Coarse node-local alarm (the exact bound is a test assertion).
                    if dust.raw().unsigned_abs() >= FixedPoint::SCALE as u128 {
                        tracing::error!(escrow = %e, %dust, "liquidation: ADL escrow dust >= 1 token (invariant alarm)");
                    }
                }
            }
        }
        stats.adl_work = used;
        Ok(())
    }

    /// P2 edge: the real opposite holders of `o.market` are exhausted (both
    /// escrows hold it). Pair `o` with the opposite side's rows of the same
    /// market in key order; each escrow closes at its own row's price, the
    /// vault pays the difference (`liq::cross_close`). Returns the rows
    /// scanned (work units).
    fn adl_cross<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        o: &mut liq::Obligation,
        stats: &mut LiqStats,
    ) -> Result<u64, CoreError> {
        let (mut units, mut start) = (0u64, vec![liq::ADL_OBLIGATION_TAG]);
        while o.size > FixedPoint::ZERO {
            let Some(mut x) = liq::next_obligation(&ctx.state, &start)? else { break };
            start = [x.key().as_slice(), &[0]].concat();
            units += 1;
            if x.market != o.market || x.is_long == o.is_long {
                continue;
            }
            let q = o.size.min(x.size);
            let (pl, ps) = if o.is_long { (o.price, x.price) } else { (x.price, o.price) };
            let paid = liq::cross_close(&ctx.positions, o.market, q, pl, ps, &LIQUIDATOR_VAULT)?;
            stats.adl_pairing += paid;
            tracing::info!(market = o.market, size = %q, p_long = %pl, p_short = %ps, vault = %paid, "liquidation: ADL escrow pairing");
            o.size -= q;
            x.size -= q;
            liq::update_obligation(&ctx.state, &x)?;
        }
        Ok(units)
    }
```

* `LiqStats` (:22-38): add `adl_obligations: u64`, `adl_work: u64`, `adl_dust: FixedPoint`
  and `adl_pairing: FixedPoint`. Add them to the info line (:120-132); `adl_obligations > 0`
  counts as `happened`.
* `GOLDEN_B` (`perf_equivalence_golden.rs:546`): scenario B has an ADL (:441, :451). Rows are
  written and deleted (tombstones in the running hash), and escrow balance rows appear.
  Re-capture with `GOLDEN_PRINT=1` only after checking that the first changed digest is the
  first ADL block. The commit message says it is a change made on purpose (P2).

**Verify:** `cargo nextest run -p torus-bridge p2_ $F`, `cargo nextest run -p torus-bridge an_hl_sized $F`,
`cargo nextest run -p torus-bridge liquidation $F`, `cargo nextest run -p torus-bridge offmark_bad_debt $F`,
`cargo nextest run -p torus-bridge perf_equivalence $F`, `cargo nextest run -p torus-bridge storage_reads $F`.
Then the full workspace before the A5+A6 commit.
**Depends on:** A5.

**As built (dfa170c), deviations:**
* #10 uses heights 70-72 (block 2's aggregate stays usable for 60 s, so "market 1 stale at
  block 3" needs a later block).
* #3 (`p2_ranks_each_market_side_once_per_block`) uses its own two-block fixture in
  `liquidation_l1_tests.rs` (P2's shape, R and the slot attached): rankings per block [0, 4].
* The drain rewrites a row only when its size changed (a row whose closes found nothing
  writes nothing).
* GOLDEN_B: the first changed digest is block 8, the first ADL block (adl 0 → 2 on 56318a9).
  `golden_repins_change_only_rule_h_and_p2_rows` pins reduced digests (the hashed CFs without
  `CF_CONSENSUS_META`, which holds the running hash and the native trie's nodes; `0x03` values
  cut to 16 bytes; no `0x07` rows and no escrow position or balance rows; plus every block's
  outputs) captured on 56318a9 (D10, no P2). They are equal on the new code for every block of
  scenarios A and B, so the A3 and A5/A6 re-pins changed only those rows.

### Task A7: e2e, gauges, conservation sum (node-local)

**Test first.**
1. `crates/torus-consensus/src/app.rs`, after :17433:

```rust
/// adl-budget P2: obligation rows keep the step DUE. T (71) long 10 @ 1,000,
/// collateral 50, plus 13 accounts (seeds 80..=92) seeded the same way against
/// S (72, short 140). Mark 970 (block 1's submissions aggregate at 2): all 14
/// flat at B = 2 (14 rows, ESCROW_LONG long 140). `test_adl_work = 2`: one row
/// per block (a ranking costs >= 2), blocks 2..=15. Oracle rows are pruned at
/// 12, so blocks 13..=15 carry no action, oracle row, cursor or pending row:
/// only the 0x07 rows run them. Two runs: equal native root and dumps.
#[test]
fn liquidation_e2e_adl_obligations_drain_over_empty_blocks() { … }
```

   Body shape: `liquidation_e2e_partial_stage1_keeps_the_step_due` (:17415-17433) and
   :17442-17462 for the two runs.
   **Usable mark in blocks 13..=15:** the submission rows are pruned at 12, but block 2's
   aggregate (ts 1,002) stays usable until ts 1,062 (60 s staleness rule). So blocks 13..=15
   (ts 1,013-1,015) still have a mark, the same reason the M2 test's block 14 works. The test
   asserts `AccountReader::mark(ORACLE_MARKET).is_some()` at block 15 via a `make_exec_ctx`
   read. Without a mark the rows would wait (A6 #10).
1b. `liquidation_e2e_adl_drain_survives_a_restart`: the same blocks. Run 1 is uninterrupted.
   Run 2 executes blocks 1..=8 (mid-drain: 7 rows left), drops the `ExecutionContext`, reopens
   it with `make_exec_ctx(&config, &db)` on the same DB (cold R slot, no resident state), and
   executes 9..=15.
   * Equal `persisted_native_root` and `dump_all_cfs` after every block from 9 on, and at the
     end.
   * This proves the drain keeps no in-memory state across blocks: the ranking cache is per
     block, and the dust bound is test-only.
2. `tests/liquidation_tests.rs`, `telemetry_reports_the_adl_queue_escrow_and_value_sum`:
   * `p2_fixture` at B with W = 0: `torus_liquidation_adl_queue == 8` (**rows**),
     `torus_liquidation_adl_escrow_notional == 8 × 900` tokens, and
     `torus_liquidation_adl_queue_deficit` = Σ over both escrows of (available + UPnL at the
     mark). Here that is `ESCROW_LONG`'s Σ (900 − price) × 1 over its 8 rows.
   * After the drain: both are 0, `torus_liquidation_adl_dust` equals the swept dust, and in the
     edge test `torus_liquidation_adl_pairing == 400`.
   * With `ctx.liq_value_sum = true`, `torus_liquidation_value_sum` equals the test's
     `total_value(..)` after every drain block, and is constant across the drain (within the
     dust bound).
   * Without metrics, or with the flag off: identical rows and results (extend
     `telemetry_does_not_change_results_or_state` :1143), and no value-sum walk
     (`CountingBackend`: no `CF_NATIVE_BALANCES` iteration).
3. `liquidation_l1_tests.rs`: **work units equal on the records path and the walk path.** The
   seeded L1 run already compares the L1 run (R + records) with the reference walk
   (`begin_resident(None, ..)`) block by block. Add per block:
   `metrics.liquidation_adl_work_total` equal in both runs, and > 0 in at least 6 blocks
   (non-vacuous: the seeds produce ADL rows). A ranking examines `liq_traders_after(None, MAX)`,
   and E2 guarantees the slot list == the walk list, so the units must match.

**Implementation:**
* `app.rs:645`: add `#[cfg(test)] test_adl_work: Option<u64>`, initialized `None` at :3984 and
  :10815. At :2339, under `#[cfg(test)]`, if it is `Some(w)`, call `run_liquidations_with(.., w)`
  (the shape of `engine_threads` :2287-2291). Update the comment at :1966-1969.
* Also in `app.rs`: a `liq_value_sum: bool` field on `TorusApp`, read once from
  `TORUS_LIQ_VALUE_SUM` at construction and copied into `ctx.liq_value_sum` per block.
* NE:3588: `pub liq_value_sum: bool` (node-local, never read by execution), `false` at NE:~4057.
* `torus-telemetry/src/lib.rs`, next to `liquidation_deferred` (:91, :1042-1047, :2335):
  * `liquidation_adl_queue: Gauge` (obligation **rows**, not accounts);
  * `liquidation_adl_queue_deficit: Gauge<f64, AtomicU64>` (Σ over both escrows of
    available + UPnL at the step's marks, signed tokens; owner decision 12);
  * `liquidation_adl_work_total: Counter` (drain work units, for the records-vs-walk check and
    the bench);
  * `liquidation_adl_escrow_notional: Gauge<f64, AtomicU64>`;
  * `liquidation_adl_dust: Gauge<f64, AtomicU64>` (cumulative, signed);
  * `liquidation_adl_pairing: Gauge<f64, AtomicU64>` (cumulative, signed);
  * `liquidation_value_sum: Gauge<f64, AtomicU64>`.
* `liq::pending_count` (:485-495) becomes `tag_count(state, tag)`, with two call sites.
* `liquidation_telemetry` (:73-136), only with metrics: the row count, plus the escrow notional
  from both escrows' positions at the step's marks.
* When `ctx.liq_value_sum` is on, every step runs `value_sum(ctx, marks)`: a paged walk of
  `CF_NATIVE_BALANCES` (20-byte keys) and `CF_NATIVE_POSITIONS`, with UPnL at the marks
  (unmarked: entry). It sets the gauge and logs
  `info!(height, value_sum, "liquidation: value sum")`. A read error skips it.
* `docs/monitoring-setup.md` rows; the `liquidation.md` telemetry paragraph.

**Verify:** `cargo nextest run -p torus-consensus liquidation_e2e $F`, `cargo nextest run -p torus-bridge telemetry_ $F`.
**Depends on:** A6.

**As built (a21a21e), deviations:**
* `liq_value_sum` lives on `ExecutionContext` (where the block's context is built), read once
  from `TORUS_LIQ_VALUE_SUM=1` at construction.
* The e2e test does not read `AccountReader::mark` (private to the bridge). The last row
  drains in block 15, which proves the mark was usable (rows of a listed market without a
  mark wait).
* Open question 5 confirmed: `total_native_fees` is never incremented (no native fee debits
  `CF_NATIVE_BALANCES`), so only deposits, withdrawals and transfers move the value sum.

---

### Task A8: measure, set `W`, prove an HL-sized event closes in one block

**Test first** (`crates/torus-bridge/tests/ubench_adl.rs`, still ignored):
* Run blocks until `next_obligation(..)` is `None` (cap 100,000), with `UB_ADL_WORK` (default
  `ADL_WORK_PER_BLOCK`).
* Per block, print the drain ms and units (the `adl_work` field of the info line, recorded by
  `AdlClock` :47-63). Summary: blocks to drain, ms per block (p50/p90/max), ns/unit.
* An **HL mode** (`UB_ADL_HL=1`): N = 5,000 traders in 300 markets, 3 bankrupt accounts in 100
  markets each (300 account-markets). It asserts the escrows are closed after the bankruptcy
  block with the default W, and prints that block's units and ms.
* Assertions:
  * per-block units ≤ `W + max row cost`, where a row costs 1 (visit) + A (ranking) + `read` +
    the rows `adl_cross` scanned. That is one step of overshoot, edge pairing included.
  * Afterwards: OI symmetric everywhere, the escrows flat at 0, every bankrupt account flat at
    0.

**Measurement** (bench-runner, quiet box, release, then rig_factor ≈ 1.9-2):
* the HL mode's units `U_hl` and ms;
* the S=750 mode (N = 5,000, K = 10, 270 markets) at `UB_ADL_WORK = W`.

* **block-B ms**, i.e. the regular pass at B: classification + the O(P) escrow transfers. Do it
  for the S=750 mode (100 accounts × 270 markets) and for the HL mode. B's work is outside W
  (bounded by the act budget of 64 accounts), so it must be measured separately; it was ~0.7 s
  per block with the O(P²) `adl_rest`.

Note (after A5): `AdlClock` (`ubench_adl.rs`) keys on the `counterparties` field of the old
`liquidation: ADL` line, which A5 removed; time block B with the `liquidation: ADL to escrow`
events (one per account-market) and the drain with the `liquidation step` line's `adl_work`
and `ms` fields.

**Block B outside W: the condition (owner 18c, after dbb683c).** Block B's cost (moving the
positions to the escrows) stays outside W, bounded by the 64-account act budget × O(P) with the
running `rest`, **on condition**: if A8 measures block B above the W budget (~250 ms for an
HL-sized event), it is charged into W, or a lower per-block act limit for ADL accounts is used.

Set `W = max(U_hl × 1.25, 100 × (5,000 + 3) + 300 × 2)`, rounded up to a multiple of 10,000.
The case is N = 5,000, one side (all long), 100 (market, side) keys and 300 rows; the second
term is the non-ignored test's floor. Report the ms per block for the HL event, for W
(× rig_factor), and for block B, plus the S=750-mode blocks to drain, in
`docs/plans/adl-budget.md` §9 *Budget measurement*, with the commands. If ms(W) on the rig is
too high for the block cadence, report it as a finding for the owner (open question 6). The
earlier "≤ ~20 ms" target is superseded by "an HL-sized event in one block".

**Verify:** `cargo nextest run -p torus-bridge an_hl_sized $F`, then
`UB_ADL_HL=1 UB_ADL_TRADERS=5000 cargo test -p torus-bridge --release --test ubench_adl -- --ignored --nocapture`
and `UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=10 …` (bench-runner).
**Depends on:** A6.

---

## Phase C: verification end-to-end (proof)

1. Per task, the Verify commands. For the core crate also run
   `cargo nextest run -E 'rdeps(torus-core)' $F`.
2. Before each commit: `cargo nextest run --workspace $F` (in the background, output to a file)
   + the doc-test command from `TESTING.md`. A `flaky` count above 0 is reported. Before
   merging to main: one full `cargo test --workspace -q`.
3. ubench (bench-runner): 7ec5eb2 vs after (ms per block at W, ns/unit, the HL event's block,
   S=750-mode blocks to drain), with and without R.
4. **Final proof list (owner 18c s96)**, from liquidation-stress cells (bench-runner; s17
   harness fixes first: `liq_stress.py` `_count`, stale genesis, reflink-stale binaries), run
   with `TORUS_LIQ_VALUE_SUM=1`, plus one run without it for the timing figures:
   1. **Every ADL'd account ends at exactly 0**: unit tests A5/A6; at S=750, a balance and
      position read of the 100 thin accounts after B.
   2. **S=750: the vault gets ~0 from ADL**, because all 100 accounts clamp under H (was
      26,516,805.13; `adl-budget.md` §8). Check the base each ADL used, in two ways:
      * the `liquidation: ADL to escrow` lines of heights 788-790 log `base`, `bankruptcy` and
        `price`; every account's `base` per market must equal the pre-shock mark;
      * the `0x03 ‖ market` rows (`last ‖ prev`) read from a node's DB at 787, 788 and 790
        (state dump) show `last` = the post-shock mark and `prev` = the pre-shock mark from 788
        on.
   3. **Escrows end at 0 positions / 0 balance**; the dust is reported separately (gauge + log),
      and so is the pairing amount.
   4. **S=400 unchanged:** 100 BACKSTOP, vault +19,984,975.69, step times in the same range.
   5. **An HL-sized event closes in one block:** unit test A6 #8, the ubench HL mode (A8), and a
      cell if cheap (S=750's setup with ~3 thin accounts over ~100 markets).
   6. **Conservation sum checked in every drain block:** the `liquidation: value sum` line is
      constant from the block before B to the last drain block (no transfers in the window).
   7. **S=750 liveness PASS + drain time in blocks** until no `0x07` row is left; AGREE, with
      the vault and escrow balances identical on every node.

## Rollback

* One commit per task (A1+A2 together, A5+A6 together), on `perf/adl-budget`. A failing task
  is reverted with `git revert <sha>`, never with `reset --hard`.
* Consensus: A2 changes ADL only above 65,536 rows; A3 and A5-A6 change block results (fresh
  genesis, M1). Rolling back after a deploy means redeploying the previous binary from genesis
  (pre-testnet).
* A7's gauges and value sum are node-local.

## Open questions (recommendation in brackets)

1. **Delisted market — DECIDED (owner 18c): rank at the stored price** (see *Drain*; delisting
   is text-only today). Rows of a market that is no longer
   listed would never get a mark, so the escrow would never go flat.
   [Rank them with the row's **stored price** standing in for the mark, and close at the stored
   price (A6 #11). A listed market without a usable mark keeps waiting at 1 unit per row visit
   (A6 #10). The alternative is that delisted rows stay forever and are only reported (queue
   gauge, escrow notional), with no close.]
2. **Dust bound wording.** "Size × 1 raw per obligation" holds only with the escrow's
   **aggregated** size after each row: an average's error applies to the whole position. [Use
   the formula in *Dust bound*, as a test-only assertion.]
3. **The production dust alarm — DECIDED (owner 18c): as recommended.** [An error log at |dust| ≥ 1 token, node-local, no persisted
   counters; always sweep. Never fail-stop: value is still conserved through the vault.]
4. **Edge-pairing cost.** `adl_cross` scans the queue from its start for opposite rows of the
   same market: O(queue) per paired row, counted in units. [Accept: rare; add an index only if
   the proof shows it.]
5. **The value sum and fees.** Deposits, withdrawals and possibly per-action gas fees change it
   (`total_native_fees` NE:3542, distributed NE:10100: check whether it debits
   `CF_NATIVE_BALANCES`). [Compare over a window with no user transfers; confirm the fee path in
   A7.]
6. **W vs block time.** An HL-sized event at N = 5,000 is 500,900 units (one side). At s18's
   ~0.1-0.4 µs per unit that is roughly 50-200 ms in one block. A two-sided event of the same
   size has up to 2× the rankings and spills at most one block. [Measure in A8. The owner's rule
   (the HL event in one block) sets W; if the rig time is too high, C2 (market index) cuts a
   ranking to the holders of the market.]
7. **The vault as a candidate** can take escrow rows and later be ADL'd itself (new rows).
   [Accept: D8 unchanged.]
8. **Unmarked positions at B** stay with the account (H2), and D9 still moves all of its
   collateral, as today. "Every ADL'd account ends at exactly 0" therefore means its cash and its
   marked positions. [Keep.]
9. **Rule H after an outage.** A market whose mark disappears loses its row, so the first mark
   after the gap has no base (the mark fallback). [Keep: H1's "never a base from before an
   outage".]
10. **Block-B cost is outside W — DECIDED (owner 18c): accepted on the A8 condition** (charged
    into W or a lower ADL act limit if block B measures above ~250 ms). It is bounded by the act
    budget (64 accounts per block) × O(P) transfers. [Accept, with the running `rest`. A8 reports the block-B ms for S=750; if it
    is too high, the act budget is the lever.]
11. **Positive D9 remainder.** Impossible under the one-sided clamp (Design step 3). [An error
    log as an invariant alarm; no special handling.]
