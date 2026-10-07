//! Item 3: Hyperliquid-style liquidation, core — docs/plans/liquidation.md.

use torus_core::liquidation::{
    adl_candidates, adl_close, adl_rank, backstop, bankruptcy_price, classify, settle_flat_deficit,
    slippage_cap, stage1_qty, traders_after, AdlCandidate, Health, LIQUIDATOR_VAULT,
};
use torus_core::margin::{AccountView, MarginTier};
use torus_core::position::{MarginType, NativeBalance, PositionManager};
use torus_state::cf::CF_NATIVE_POSITIONS;
use torus_state::{StateBackend, StateDb};
use torus_types::{Address, FixedPoint, MarketId};

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn setup() -> (tempfile::TempDir, PositionManager) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, PositionManager::new(db))
}

fn set_balance(pm: &PositionManager, t: &Address, amount: FixedPoint) {
    pm.put_native_balance(t, &NativeBalance { available: amount, order_margin: FixedPoint::ZERO })
        .unwrap();
}

fn open_pair(pm: &PositionManager, long: &Address, short: &Address, m: MarketId, qty: i64, px: i64) {
    pm.apply_fill(long, m, true, fp(qty), fp(px), MarginType::Cross).unwrap();
    pm.apply_fill(short, m, false, fp(qty), fp(px), MarginType::Cross).unwrap();
}

fn view(av: FixedPoint, mm: FixedPoint) -> AccountView {
    AccountView {
        available: av,
        order_margin: FixedPoint::ZERO,
        upnl: FixedPoint::ZERO,
        position_im: FixedPoint::ZERO,
        notional: FixedPoint::ZERO,
        maintenance: mm,
    }
}

/// Decisions 2, 4, 5: Healthy at AV >= MM; ADL below 0; backstop below 2/3 MM;
/// otherwise stage 1. Exact on raw units; overflow -> None (skip).
#[test]
fn classify_follows_the_hl_thresholds_exactly() {
    let one = FixedPoint::from_raw(1);
    assert_eq!(classify(&view(fp(300), fp(300))), Some(Health::Healthy), "AV == MM");
    assert_eq!(classify(&view(fp(300) - one, fp(300))), Some(Health::Stage1));
    assert_eq!(classify(&view(fp(200), fp(300))), Some(Health::Stage1), "AV == 2/3 MM");
    assert_eq!(classify(&view(fp(200) - one, fp(300))), Some(Health::Backstop));
    assert_eq!(classify(&view(FixedPoint::ZERO, fp(300))), Some(Health::Backstop), "0 is not < 0");
    assert_eq!(classify(&view(-one, fp(300))), Some(Health::Adl));
    assert_eq!(classify(&view(-fp(5), FixedPoint::ZERO)), Some(Health::Adl));
    assert_eq!(classify(&view(FixedPoint::ZERO, FixedPoint::ZERO)), Some(Health::Healthy));
    let huge = FixedPoint::from_raw(i128::MAX / 2);
    assert_eq!(classify(&view(huge, FixedPoint::MAX)), None, "3 x AV overflows");
}

/// D2: above 100,000 notional at the mark -> 20% of the size (raw / 5); at or
/// below, or when 20% is under the lot -> the whole size.
#[test]
fn stage1_qty_chunks_above_100k_notional_at_mark() {
    assert_eq!(stage1_qty(fp(100), fp(1_000), fp(1)), (fp(100), false), "100,000 is not above");
    assert_eq!(stage1_qty(fp(101), fp(1_000), fp(1)), (FixedPoint::from_raw(fp(101).raw() / 5), true));
    assert_eq!(stage1_qty(fp(3), fp(50_000), fp(1)), (fp(3), false), "0.6 < lot 1 -> whole");
}

/// D1: cap = mark ∓ mark / (2 x max leverage of the position's tier).
#[test]
fn slippage_cap_is_the_positions_mm_rate() {
    assert_eq!(slippage_cap(None, fp(1_000), fp(10_000), false), fp(975), "sell, 20x: 2.5%");
    assert_eq!(slippage_cap(None, fp(1_000), fp(10_000), true), fp(1_025), "buy");
    let t = [MarginTier { max_notional: FixedPoint::MAX, max_leverage: 50 }];
    assert_eq!(slippage_cap(Some(&t), fp(1_000), fp(10_000), false), fp(990), "50x: 1%");
}

/// Decision 5: rank = (mark/entry for longs, entry/mark for shorts) x
/// (notional at mark / AV), descending, exact; AV <= 0 last; ties by address.
#[test]
fn adl_rank_is_hl_profit_times_leverage_exact() {
    let short = |n: u8, entry: i64, av: i64| AdlCandidate {
        trader: addr(n),
        is_long: false,
        size: fp(3),
        entry_price: fp(entry),
        account_value: fp(av),
    };
    // mark 980: (1000/980)(2940/10060) = 0.298; (1100/980)(2940/1360) = 2.43
    let ranked = adl_rank(
        fp(980),
        vec![short(1, 1_000, 10_060), short(2, 1_100, 1_360), short(3, 1_000, 0), short(4, 1_000, 10_060), short(0, 1_000, 10_060)],
    );
    let order: Vec<Address> = ranked.iter().map(|c| c.trader).collect();
    assert_eq!(order, vec![addr(2), addr(0), addr(1), addr(4), addr(3)]);
    // longs at mark 1,100, size 1, AV 1,000: entry 900 (1.344) before entry 1,000 (1.21)
    let long = |n: u8, entry: i64| AdlCandidate { is_long: true, size: fp(1), account_value: fp(1_000), ..short(n, entry, 0) };
    let ranked = adl_rank(fp(1_100), vec![long(1, 1_000), long(2, 900)]);
    assert_eq!(ranked.iter().map(|c| c.trader).collect::<Vec<_>>(), vec![addr(2), addr(1)]);
}

/// Extreme magnitudes rank without panicking (U512 cross-multiplication).
#[test]
fn adl_rank_never_overflows() {
    let c = |n: u8, size: i128, av: i128| AdlCandidate {
        trader: addr(n),
        is_long: false,
        size: FixedPoint::from_raw(size),
        entry_price: FixedPoint::from_raw(i128::MAX / 4),
        account_value: FixedPoint::from_raw(av),
    };
    let r = adl_rank(FixedPoint::from_raw(1), vec![c(1, i128::MAX / 4, 1), c(2, 1, i128::MAX / 4)]);
    assert_eq!(r[0].trader, addr(1));
}

fn oi(pm: &PositionManager, m: MarketId) -> (FixedPoint, FixedPoint) {
    let (mut l, mut s) = (FixedPoint::ZERO, FixedPoint::ZERO);
    for (k, v) in pm.state().iterate_cf(CF_NATIVE_POSITIONS, None).unwrap() {
        let p: torus_core::position::Position = borsh::from_slice(&v).unwrap();
        if k[20..28] == m.to_be_bytes() {
            if p.is_long { l += p.size } else { s += p.size }
        }
    }
    (l, s)
}

/// Σ (available + order margin + UPnL at `mark`) over `ts` (one market).
fn value(pm: &PositionManager, ts: &[Address], m: MarketId, mark: FixedPoint) -> FixedPoint {
    ts.iter()
        .map(|t| {
            let b = pm.get_native_balance(t).unwrap();
            let u = pm.get_position(t, m).unwrap().map_or(FixedPoint::ZERO, |p| p.unrealized_pnl(mark));
            b.available + b.order_margin + u
        })
        .fold(FixedPoint::ZERO, |a, b| a + b)
}

/// Decision 4: positions + remaining collateral move to the vault at the mark;
/// OI symmetric, value conserved, nothing deleted without a counterparty.
#[test]
fn backstop_moves_positions_and_collateral_to_the_vault_at_the_mark() {
    let (_d, pm) = setup();
    let (t, s) = (addr(1), addr(2));
    set_balance(&pm, &t, fp(300));
    set_balance(&pm, &s, fp(100_000));
    open_pair(&pm, &t, &s, 1, 10, 1_000);
    let mark = fp(975);
    let who = [t, s, LIQUIDATOR_VAULT];
    let before = value(&pm, &who, 1, mark);
    backstop(&pm, &t, &LIQUIDATOR_VAULT, |_| Some(mark)).unwrap();
    assert!(pm.get_position(&t, 1).unwrap().is_none());
    let v = pm.get_position(&LIQUIDATOR_VAULT, 1).unwrap().unwrap();
    assert_eq!((v.is_long, v.size, v.entry_price), (true, fp(10), mark));
    let tb = pm.get_native_balance(&t).unwrap();
    assert_eq!(tb.available + tb.order_margin, FixedPoint::ZERO);
    assert_eq!(pm.get_native_balance(&LIQUIDATOR_VAULT).unwrap().available, fp(50), "AV 300 - 250");
    assert_eq!(oi(&pm, 1), (fp(10), fp(10)));
    assert_eq!(value(&pm, &who, 1, mark), before);
}

/// The vault nets with what it already holds (fill semantics): short 4 @ 990
/// + takes long 10 @ 975 -> closes 4 (+60 realized), long 6 @ 975.
#[test]
fn backstop_nets_against_the_vaults_existing_position() {
    let (_d, pm) = setup();
    let (t, s, l) = (addr(1), addr(2), addr(3));
    set_balance(&pm, &t, fp(300));
    open_pair(&pm, &t, &s, 1, 10, 1_000);
    open_pair(&pm, &l, &LIQUIDATOR_VAULT, 1, 4, 990);
    let mark = fp(975);
    let who = [t, s, l, LIQUIDATOR_VAULT];
    let before = value(&pm, &who, 1, mark);
    backstop(&pm, &t, &LIQUIDATOR_VAULT, |_| Some(mark)).unwrap();
    let v = pm.get_position(&LIQUIDATOR_VAULT, 1).unwrap().unwrap();
    assert_eq!((v.is_long, v.size, v.entry_price), (true, fp(6), mark));
    assert_eq!(pm.get_native_balance(&LIQUIDATOR_VAULT).unwrap().available, fp(60) + fp(50));
    assert_eq!(oi(&pm, 1), (fp(10), fp(10)), "longs L 4 + vault 6; shorts S 10");
    assert_eq!(value(&pm, &who, 1, mark), before);
}

/// Decision 5: U long 4 closes against ranked shorts at the given (previous
/// mark) price: S2 (ranked first) 3, then S1 1. OI stays symmetric.
#[test]
fn adl_close_pairs_against_ranked_counterparties_at_the_price() {
    let (_d, pm) = setup();
    let (u, s1, s2, l) = (addr(1), addr(2), addr(3), addr(4));
    open_pair(&pm, &u, &s1, 1, 4, 1_000);
    open_pair(&pm, &l, &s2, 1, 3, 1_100);
    let ranked = adl_rank(
        fp(900),
        vec![
            AdlCandidate { trader: s1, is_long: false, size: fp(4), entry_price: fp(1_000), account_value: fp(10_400) },
            AdlCandidate { trader: s2, is_long: false, size: fp(3), entry_price: fp(1_100), account_value: fp(1_600) },
        ],
    );
    // The closes, in rank order: (counterparty, size) — the bridge logs them.
    let (closes, _, _) = adl_close(&pm, &u, 1, fp(990), fp(4), &ranked).unwrap();
    assert_eq!(closes, vec![(s2, fp(3)), (s1, fp(1))]);
    assert!(pm.get_position(&u, 1).unwrap().is_none());
    assert!(pm.get_position(&s2, 1).unwrap().is_none());
    assert_eq!(pm.get_position(&s1, 1).unwrap().unwrap().size, fp(3));
    assert_eq!(pm.get_native_balance(&u).unwrap().available, -fp(40), "4 x (990 - 1,000)");
    assert_eq!(pm.get_native_balance(&s2).unwrap().available, fp(330), "3 x (1,100 - 990)");
    assert_eq!(pm.get_native_balance(&s1).unwrap().available, fp(10));
    assert_eq!(oi(&pm, 1), (fp(3), fp(3)));
}

/// adl-budget P2: `adl_close` closes at most `qty` (an obligation row's
/// remainder): limit 2 -> S2 (ranked first) takes 2, U keeps long 2.
#[test]
fn adl_close_stops_at_the_qty_limit() {
    let (_d, pm) = setup();
    let (u, s1, s2, l) = (addr(1), addr(2), addr(3), addr(4));
    open_pair(&pm, &u, &s1, 1, 4, 1_000);
    open_pair(&pm, &l, &s2, 1, 3, 1_100);
    let ranked = adl_rank(
        fp(900),
        vec![
            AdlCandidate { trader: s1, is_long: false, size: fp(4), entry_price: fp(1_000), account_value: fp(10_400) },
            AdlCandidate { trader: s2, is_long: false, size: fp(3), entry_price: fp(1_100), account_value: fp(1_600) },
        ],
    );
    assert_eq!(adl_close(&pm, &u, 1, fp(990), fp(2), &ranked).unwrap().0, vec![(s2, fp(2))]);
    assert_eq!(pm.get_position(&u, 1).unwrap().unwrap().size, fp(2), "U keeps 2");
    assert_eq!(pm.get_position(&s2, 1).unwrap().unwrap().size, fp(1));
    assert_eq!(pm.get_position(&s1, 1).unwrap().unwrap().size, fp(4), "untouched");
    assert_eq!(oi(&pm, 1), (fp(5), fp(5)));
}

/// adl-budget Q2 (A6): `adl_close` also reports `next` = how many leading
/// candidates are used up (vanished, flipped or fully closed) and `read` =
/// how many it read. U long 5; candidates [gone, c1 (2), c2 (3)], qty 4:
/// closes c1 2 and c2 2; `next` = 2 (gone and c1; c2 keeps 1), `read` = 3.
#[test]
fn adl_close_reports_next_and_read() {
    let (_d, pm) = setup();
    let (u, gone, c1, c2) = (addr(1), addr(2), addr(3), addr(4));
    open_pair(&pm, &u, &c1, 1, 2, 1_000);
    open_pair(&pm, &u, &c2, 1, 3, 1_000);
    let cand = |trader, size| AdlCandidate { trader, is_long: false, size: fp(size), entry_price: fp(1_000), account_value: fp(1) };
    let ranked = [cand(gone, 5), cand(c1, 2), cand(c2, 3)];
    let (closes, next, read) = adl_close(&pm, &u, 1, fp(990), fp(4), &ranked).unwrap();
    assert_eq!(closes, vec![(c1, fp(2)), (c2, fp(2))]);
    assert_eq!((next, read), (2, 3));
    assert_eq!(pm.get_position(&u, 1).unwrap().unwrap().size, fp(1), "U keeps 1 (qty 4)");
    assert_eq!(pm.get_position(&c2, 1).unwrap().unwrap().size, fp(1), "c2 keeps 1: the next row starts at it");
}

/// adl-budget W (owner 18c s99, option 2): W = 100,000, with the s99 units
/// (adl-budget.md §12): B's transfers at `ADL_TRANSFER_UNITS` each, a
/// ranking = the holders of its market, 1 per first-sight valuation, 1 per
/// row visit and per candidate read. W still closes an HL-sized event in its
/// own block at the realistic shape — N = 5,000 accounts, ~10 % of them
/// holding a market (500 holders; + up to 3 protocol / sink accounts), 3
/// accounts x 100 markets: U <= 300 x T + 100 x (500 + 3) + (5,000 + 3) +
/// 300 x 2. (At every account holding every market it takes ~6 blocks, by
/// design: §11.5 / §12.) The bridge test (20 traders) cannot catch a W too
/// small at N.
#[test]
fn adl_work_per_block_is_option_2_and_covers_a_thin_hl_event() {
    use torus_core::liquidation::{ADL_TRANSFER_UNITS, ADL_WORK_PER_BLOCK};
    assert_eq!(ADL_WORK_PER_BLOCK, 100_000, "owner s99: option 2");
    let u = 300 * ADL_TRANSFER_UNITS + 100 * (500 + 3) + (5_000 + 3) + 300 * 2;
    assert!(ADL_WORK_PER_BLOCK >= u, "W {ADL_WORK_PER_BLOCK} covers U = {u}");
}

/// D9: a FLAT account's negative collateral moves to the vault (conserved);
/// an account with positions is untouched.
#[test]
fn flat_deficit_moves_to_the_vault() {
    let (_d, pm) = setup();
    let (t, s) = (addr(1), addr(2));
    pm.put_native_balance(&t, &NativeBalance { available: -fp(100), order_margin: fp(30) }).unwrap();
    assert_eq!(settle_flat_deficit(&pm, &t, &LIQUIDATOR_VAULT).unwrap(), -fp(70));
    let b = pm.get_native_balance(&t).unwrap();
    assert_eq!(b.available + b.order_margin, FixedPoint::ZERO);
    assert_eq!(pm.get_native_balance(&LIQUIDATOR_VAULT).unwrap().available, -fp(70));
    set_balance(&pm, &s, -fp(5));
    open_pair(&pm, &s, &t, 1, 1, 10);
    assert_eq!(settle_flat_deficit(&pm, &s, &LIQUIDATOR_VAULT).unwrap(), FixedPoint::ZERO);
    assert_eq!(pm.get_native_balance(&s).unwrap().available, -fp(5));
}

/// C1: candidates = distinct traders of CF_NATIVE_POSITIONS, ascending, after
/// the cursor, at most `limit`.
#[test]
fn traders_after_walks_the_positions_cf_from_the_cursor() {
    let (_d, pm) = setup();
    open_pair(&pm, &addr(9), &addr(3), 1, 1, 10);
    open_pair(&pm, &addr(9), &addr(5), 2, 1, 10); // addr(9): two rows, listed once
    let st = pm.state();
    assert_eq!(traders_after(st, None, 10).unwrap(), vec![addr(3), addr(5), addr(9)]);
    assert_eq!(traders_after(st, Some(addr(3)), 10).unwrap(), vec![addr(5), addr(9)]);
    assert_eq!(traders_after(st, None, 2).unwrap(), vec![addr(3), addr(5)]);
    assert!(traders_after(st, Some(addr(9)), 10).unwrap().is_empty());
}

/// Review H1 (user decision s517): the bankruptcy price is the close price at
/// which the account (collateral + other UPnL = `rest`) ends at 0. Rounded
/// AGAINST the bankrupt trader (a long's price down, a short's up), so the
/// close never leaves it positive: `rest + pnl` is in (-1 unit, 0].
#[test]
fn bankruptcy_price_rounds_against_the_trader() {
    // long 4 @ 1,000, rest 200: exactly 950
    assert_eq!(bankruptcy_price(fp(200), true, fp(4), fp(1_000)), Some(fp(950)));
    // short 4 @ 1,000, rest 200: exactly 1,050
    assert_eq!(bankruptcy_price(fp(200), false, fp(4), fp(1_000)), Some(fp(1_050)));
    // inexact: long / short 3 @ 1,000, rest 100 -> 33.333.. per unit, rounded against the trader
    let pnl = |is_long: bool, p: FixedPoint| {
        let per = if is_long { p - fp(1_000) } else { fp(1_000) - p };
        per * fp(3)
    };
    let l = bankruptcy_price(fp(100), true, fp(3), fp(1_000)).unwrap();
    let s = bankruptcy_price(fp(100), false, fp(3), fp(1_000)).unwrap();
    assert_eq!(l, FixedPoint::from_raw(fp(1_000).raw() - 3_333_333_334));
    assert_eq!(s, FixedPoint::from_raw(fp(1_000).raw() + 3_333_333_334));
    for (is_long, p) in [(true, l), (false, s)] {
        let end = fp(100) + pnl(is_long, p);
        assert!(end <= FixedPoint::ZERO && end > -FixedPoint::ONE, "{is_long}: {end:?}");
    }
    // negative rest (other positions losing): the price moves past entry
    assert_eq!(bankruptcy_price(-fp(40), true, fp(4), fp(1_000)), Some(fp(1_010)));
    // overflow / zero size -> None (caller falls back to the mark)
    assert_eq!(bankruptcy_price(FixedPoint::MAX, true, FixedPoint::from_raw(1), fp(1)), None);
    assert_eq!(bankruptcy_price(fp(1), true, FixedPoint::ZERO, fp(1)), None);
}

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

/// P2: a new obligation never overwrites a row (a re-bankruptcy of the same
/// trader comes at a later height: a new key). Writing a size > 0 row over an
/// existing key is an error and leaves the row; size 0 still deletes it.
#[test]
fn adl_obligation_rows_are_never_overwritten() {
    use torus_core::liquidation::{next_obligation, put_obligation, Obligation, ADL_OBLIGATION_TAG};
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let o = Obligation { height: 5, market: 1, is_long: true, trader: addr(8), size: fp(4), price: fp(940) };
    put_obligation(&db, &o).unwrap();
    assert!(put_obligation(&db, &Obligation { size: fp(1), price: fp(900), ..o }).is_err(), "key exists");
    assert_eq!(next_obligation(&db, &[ADL_OBLIGATION_TAG]).unwrap(), Some(o), "row unchanged");
    put_obligation(&db, &Obligation { height: 6, ..o }).unwrap();
    put_obligation(&db, &Obligation { size: FixedPoint::ZERO, ..o }).unwrap();
    assert_eq!(next_obligation(&db, &[ADL_OBLIGATION_TAG]).unwrap(), Some(Obligation { height: 6, ..o }));
}

/// P2 (A6): the drain's remainder goes through `update_obligation` — it
/// overwrites an EXISTING row's size (price and key kept), deletes it at 0,
/// and errors on a missing row, a negative size or a price <= 0 (never a
/// silent insert; `put_obligation`'s rules).
#[test]
fn update_obligation_overwrites_and_deletes() {
    use torus_core::liquidation::{next_obligation, put_obligation, update_obligation, Obligation, ADL_OBLIGATION_TAG};
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let o = Obligation { height: 5, market: 1, is_long: true, trader: addr(8), size: fp(4), price: fp(940) };
    put_obligation(&db, &o).unwrap();
    update_obligation(&db, &Obligation { size: fp(1), ..o }).unwrap();
    assert_eq!(next_obligation(&db, &[ADL_OBLIGATION_TAG]).unwrap(), Some(Obligation { size: fp(1), ..o }));
    assert!(update_obligation(&db, &Obligation { size: -fp(1), ..o }).is_err(), "negative remainder");
    assert!(update_obligation(&db, &Obligation { size: fp(1), price: FixedPoint::ZERO, ..o }).is_err(), "price 0");
    update_obligation(&db, &Obligation { size: FixedPoint::ZERO, ..o }).unwrap();
    assert_eq!(next_obligation(&db, &[ADL_OBLIGATION_TAG]).unwrap(), None, "deleted at 0");
    assert!(update_obligation(&db, &Obligation { size: fp(1), ..o }).is_err(), "missing row");
    assert!(update_obligation(&db, &Obligation { size: FixedPoint::ZERO, ..o }).is_err(), "missing row, size 0");
    assert_eq!(next_obligation(&db, &[ADL_OBLIGATION_TAG]).unwrap(), None);
}

/// Review L1: a stored `0x07` value with size <= 0 or price <= 0 is malformed
/// (the writers never store one: `put_obligation` / `update_obligation`
/// reject it), so reading it is an error (fatal in the step), never a row
/// the drain would act on.
#[test]
fn next_obligation_rejects_a_stored_size_or_price_at_or_below_zero() {
    use torus_core::liquidation::{next_obligation, Obligation, ADL_OBLIGATION_TAG};
    use torus_state::cf::CF_NATIVE_LIQUIDATION;
    let o = Obligation { height: 5, market: 1, is_long: true, trader: addr(8), size: fp(4), price: fp(940) };
    for (size, price) in [(0, 940), (-1, 940), (4, 0), (4, -1)] {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let v = [fp(size).raw().to_be_bytes(), fp(price).raw().to_be_bytes()].concat();
        db.put_cf_raw(CF_NATIVE_LIQUIDATION, &o.key(), &v).unwrap();
        let got = next_obligation(&db, &[ADL_OBLIGATION_TAG]);
        assert!(got.as_ref().is_err_and(|e| e.to_string().contains("malformed")), "({size}, {price}): {got:?}");
    }
}

/// P2 edge: escrow long sells q at p_long, escrow short buys q at p_short;
/// the vault pays (p_short - p_long) x q. Entries differ from the close
/// prices (realized PnL != 0) and q is fractional (a partial close): EL long
/// 10 @ 940, ES short 10 @ 1,010, q = 2.5 at (950, 990): EL +25, ES +50,
/// vault +100; 7.5 left on each side. Value at the mark conserved (escrows
/// + vault); then the rest (7.5) closes both flat.
#[test]
fn cross_close_conserves_value_through_the_vault() {
    use torus_core::liquidation::{cross_close, ADL_ESCROW_LONG as EL, ADL_ESCROW_SHORT as ES};
    let (_d, pm) = setup();
    let half = |v: i64| FixedPoint::from_raw(v as i128 * FixedPoint::SCALE / 2);
    pm.apply_fill(&EL, 1, true, fp(10), fp(940), MarginType::Cross).unwrap();
    pm.apply_fill(&ES, 1, false, fp(10), fp(1_010), MarginType::Cross).unwrap();
    let who = [EL, ES, LIQUIDATOR_VAULT];
    let before = value(&pm, &who, 1, fp(900));
    assert_eq!(cross_close(&pm, 1, half(5), fp(950), fp(990), &LIQUIDATOR_VAULT).unwrap(), fp(100));
    let avail = |t: &Address| pm.get_native_balance(t).unwrap().available;
    assert_eq!((avail(&EL), avail(&ES), avail(&LIQUIDATOR_VAULT)), (fp(25), fp(50), fp(100)));
    let (l, s) = (pm.get_position(&EL, 1).unwrap().unwrap(), pm.get_position(&ES, 1).unwrap().unwrap());
    assert_eq!((l.is_long, l.size, l.entry_price), (true, half(15), fp(940)));
    assert_eq!((s.is_long, s.size, s.entry_price), (false, half(15), fp(1_010)));
    assert_eq!(oi(&pm, 1), (half(15), half(15)));
    assert_eq!(value(&pm, &who, 1, fp(900)), before);
    assert_eq!(cross_close(&pm, 1, half(15), fp(950), fp(990), &LIQUIDATOR_VAULT).unwrap(), fp(300));
    assert!(pm.get_position(&EL, 1).unwrap().is_none() && pm.get_position(&ES, 1).unwrap().is_none());
    assert_eq!(value(&pm, &who, 1, fp(900)), before);
}

/// 18c review: `cross_close` never flips or opens an escrow position — q <= 0,
/// q above either escrow's size, or an escrow on the wrong side (or without
/// a position) is an error, and nothing is written.
#[test]
fn cross_close_rejects_a_mismatched_pairing() {
    use torus_core::liquidation::{cross_close, ADL_ESCROW_LONG as EL, ADL_ESCROW_SHORT as ES};
    let snapshot = |pm: &PositionManager| {
        let s = pm.state();
        (s.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap(), s.iterate_cf(torus_state::cf::CF_NATIVE_BALANCES, None).unwrap())
    };
    let cases: [(i64, i64, i64, &str); 5] = [
        (3, -3, 0, "q 0"),
        (3, -3, -1, "q < 0"),
        (2, -3, 3, "q above the long escrow's size"),
        (3, -2, 3, "q above the short escrow's size"),
        (-3, 3, 1, "both escrows on the wrong side"),
    ];
    for (el, es, q, what) in cases {
        let (_d, pm) = setup();
        for (e, size) in [(EL, el), (ES, es)] {
            pm.apply_fill(&e, 1, size > 0, fp(size.abs()), fp(1_000), MarginType::Cross).unwrap();
        }
        let before = snapshot(&pm);
        assert!(cross_close(&pm, 1, fp(q), fp(950), fp(990), &LIQUIDATOR_VAULT).is_err(), "{what}");
        assert!(snapshot(&pm) == before, "{what}: nothing written");
    }
    let (_d, pm) = setup();
    pm.apply_fill(&ES, 1, false, fp(1), fp(1_000), MarginType::Cross).unwrap();
    assert!(cross_close(&pm, 1, fp(1), fp(950), fp(990), &LIQUIDATOR_VAULT).is_err(), "no long escrow position");
}

/// 18c review: an obligation is deleted only at size 0; a negative size or a
/// price <= 0 is an error (a negative remainder never vanishes silently).
#[test]
fn put_obligation_rejects_a_negative_size_or_a_non_positive_price() {
    use torus_core::liquidation::{next_obligation, put_obligation, Obligation, ADL_OBLIGATION_TAG};
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let o = Obligation { height: 5, market: 1, is_long: true, trader: addr(8), size: fp(4), price: fp(940) };
    assert!(put_obligation(&db, &Obligation { price: FixedPoint::ZERO, ..o }).is_err(), "price 0");
    assert!(put_obligation(&db, &Obligation { price: -fp(1), ..o }).is_err(), "price < 0");
    put_obligation(&db, &o).unwrap();
    assert!(put_obligation(&db, &Obligation { size: -fp(1), ..o }).is_err(), "size < 0");
    assert_eq!(next_obligation(&db, &[ADL_OBLIGATION_TAG]).unwrap(), Some(o), "row kept");
}

/// Telemetry: `tag_count` counts exactly the pending rows across seek
/// pages (1,030 rows > one 1,024-row page), ignoring the other tags (a
/// cooldown row, the cursor); `pending_among` counts the rows of a sorted
/// trader list only. `set_pending` reports whether it changed the row.
#[test]
fn pending_rows_are_counted_across_pages() {
    use torus_core::liquidation::{pending_among, put_cursor, set_cooldown, set_pending, tag_count, PENDING_TAG};
    let pending_count = |db: &StateDb| tag_count(db, PENDING_TAG);
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    assert_eq!(pending_count(&db).unwrap(), 0);
    let t = |i: u32| {
        let mut a = [0u8; 20];
        a[16..].copy_from_slice(&i.to_be_bytes());
        Address::new(a)
    };
    for i in (0..1_030u32).rev() {
        assert!(set_pending(&db, &t(i), true).unwrap(), "{i}: new row");
    }
    assert!(!set_pending(&db, &t(7), true).unwrap(), "already set: no write");
    assert!(!set_pending(&db, &addr(250), false).unwrap(), "absent: no delete");
    set_cooldown(&db, &addr(7), 1_000).unwrap();
    put_cursor(&db, Some(addr(9))).unwrap();
    assert!(set_pending(&db, &t(5), false).unwrap(), "cleared");
    assert_eq!(pending_count(&db).unwrap(), 1_029);
    // Sorted list: t(4), t(5) (cleared), t(6), t(1_029), t(2_000) (never set).
    let among = [t(4), t(5), t(6), t(1_029), t(2_000)];
    assert_eq!(pending_among(&db, &among).unwrap(), 3);
    assert_eq!(pending_among(&db, &[]).unwrap(), 0);
}
