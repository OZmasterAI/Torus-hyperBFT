//! Item 3: Hyperliquid-style liquidation, core — docs/plans/liquidation.md.

use torus_core::liquidation::{
    adl_close, adl_rank, backstop, bankruptcy_price, classify, settle_flat_deficit,
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
    let closed = adl_close(&pm, &u, 1, fp(990), &ranked).unwrap();
    assert_eq!(closed, fp(4));
    assert!(pm.get_position(&u, 1).unwrap().is_none());
    assert!(pm.get_position(&s2, 1).unwrap().is_none());
    assert_eq!(pm.get_position(&s1, 1).unwrap().unwrap().size, fp(3));
    assert_eq!(pm.get_native_balance(&u).unwrap().available, -fp(40), "4 x (990 - 1,000)");
    assert_eq!(pm.get_native_balance(&s2).unwrap().available, fp(330), "3 x (1,100 - 990)");
    assert_eq!(pm.get_native_balance(&s1).unwrap().available, fp(10));
    assert_eq!(oi(&pm, 1), (fp(3), fp(3)));
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
