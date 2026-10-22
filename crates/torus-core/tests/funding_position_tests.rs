//! Funding engine (docs/plans/funding-engine.md): the position side — the v3
//! row, settlement on touch in the fill transition (open, increase, partial
//! close, full close, flip; cached and uncached paths alike), pending funding
//! in the account valuation, and the "no index = no funding" identity.

use std::sync::Arc;

use borsh::BorshDeserialize;
use torus_core::funding::INDEX_SCALE;
use torus_core::margin::{position_terms, position_terms_funded, AccountView};
use torus_core::position::{
    MarginType, NativeBalance, Position, PositionCache, PositionFunding, PositionManager, POSITION_V2_LEN,
    POSITION_V3_LEN,
};
use torus_state::StateDb;
use torus_types::{Address, FixedPoint};

const E18: i128 = INDEX_SCALE;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(i128::from(v) * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn setup() -> (tempfile::TempDir, PositionManager<StateDb>) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let pm = PositionManager::new(db);
    for n in 1..=4 {
        pm.put_native_balance(&addr(n), &NativeBalance { available: fp(1_000), order_margin: FixedPoint::ZERO })
            .unwrap();
    }
    (dir, pm)
}

fn with_index(pm: &mut PositionManager<StateDb>, index: i128) {
    pm.set_funding(Some(Arc::new([(1u64, index)].into_iter().collect())));
}

fn base(funding: Option<PositionFunding>) -> Position {
    Position {
        trader: addr(9),
        market_id: 0x0102,
        is_long: true,
        size: fp(3),
        entry_price: fp(100),
        cost_basis: fp(300),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
        funding,
    }
}

/// RED before the funding engine: funding state was not written.
#[test]
fn v3_row_round_trips_and_v2_stays_byte_identical() {
    let v2 = base(None);
    let bytes = borsh::to_vec(&v2).unwrap();
    assert_eq!((bytes.len(), bytes[0]), (POSITION_V2_LEN, 2), "no funding state: the v2 row");
    assert_eq!(v2.encoded_len(), POSITION_V2_LEN);
    let f = PositionFunding { entry_index: -7 * E18 / 3, paid: FixedPoint::from_raw(-12_345) };
    let v3 = base(Some(f));
    let bytes3 = borsh::to_vec(&v3).unwrap();
    assert_eq!((bytes3.len(), bytes3[0]), (POSITION_V3_LEN, 3));
    assert_eq!(v3.encoded_len(), POSITION_V3_LEN);
    assert_eq!(&bytes3[1..POSITION_V2_LEN], &bytes[1..], "v3 = v2 body + funding tail");
    assert_eq!(&bytes3[POSITION_V2_LEN..POSITION_V2_LEN + 16], &f.entry_index.to_be_bytes());
    assert_eq!(&bytes3[POSITION_V2_LEN + 16..], &f.paid.raw().to_be_bytes());
    let back = Position::try_from_slice(&bytes3).unwrap();
    assert_eq!(back.funding, Some(f));
    assert_eq!(Position::try_from_slice(&bytes).unwrap().funding, None);
    assert!(Position::try_from_slice(&bytes3[..POSITION_V3_LEN - 1]).is_err(), "truncated v3");
    let mut v2_long = bytes.clone();
    v2_long.extend_from_slice(&[0; 32]);
    assert!(Position::try_from_slice(&v2_long).is_err(), "a v2 row with a tail is refused");
}

/// RED before the funding engine: nothing was owed.
#[test]
fn owed_and_since_open_follow_the_index() {
    let p = base(Some(PositionFunding { entry_index: 2 * E18, paid: fp(5) }));
    // 3 long × (6 − 2) = 12 owed.
    assert_eq!(p.funding_owed(Some(6 * E18)), fp(12));
    assert_eq!(p.funding_since_open(Some(6 * E18)), fp(17));
    assert_eq!(p.funding_owed(None), FixedPoint::ZERO, "no index: no funding");
    let short = Position { is_long: false, ..base(None) };
    assert_eq!(short.funding_owed(Some(6 * E18)), -fp(18), "a v2 row's entry is 0");
}

/// The transition settles first, then fills. Long 10 @ 100 opened at index
/// 0; the index moves to 0.5 per unit; each step's cash = trade PnL − owed.
/// RED before the funding engine: no funding paid, entry index unchanged.
#[test]
fn fills_settle_funding_before_the_transition_cached_and_uncached() {
    for cached in [false, true] {
        let (_d, mut pm) = setup();
        let t = addr(1);
        let mut cache = PositionCache::new();
        let mut fill = |pm: &PositionManager<StateDb>, is_buy: bool, qty: i64, px: i64| {
            if cached {
                let e = pm
                    .apply_fill_cached_effect(&mut cache, &t, 1, is_buy, fp(qty), fp(px), MarginType::Cross)
                    .unwrap();
                cache.flush_all(pm).unwrap();
                e
            } else {
                pm.apply_fill(&t, 1, is_buy, fp(qty), fp(px), MarginType::Cross).unwrap()
            }
        };
        with_index(&mut pm, 0);
        let e = fill(&pm, true, 10, 100);
        assert_eq!((e.funding, e.cash()), (FixedPoint::ZERO, None), "cached {cached}: open");
        let p = pm.get_position(&t, 1).unwrap().unwrap();
        assert_eq!(p.funding, Some(PositionFunding { entry_index: 0, paid: FixedPoint::ZERO }));

        // Increase 2 @ 100 at index 0.5: owes 10 × 0.5 = 5, no close part.
        with_index(&mut pm, E18 / 2);
        let e = fill(&pm, true, 2, 100);
        assert_eq!((e.funding, e.closed_pnl, e.cash()), (fp(5), None, Some(-fp(5))), "cached {cached}: increase");
        let p = pm.get_position(&t, 1).unwrap().unwrap();
        assert_eq!((p.size, p.entry_price, p.cost_basis), (fp(12), fp(100), fp(1_200)), "basis untouched by funding");
        assert_eq!(p.funding, Some(PositionFunding { entry_index: E18 / 2, paid: fp(5) }));

        // Partial close 2 @ 110 at index 1.5: owes 12 × 1 = 12; PnL +20.
        with_index(&mut pm, 3 * E18 / 2);
        let e = fill(&pm, false, 2, 110);
        assert_eq!((e.funding, e.closed_pnl, e.cash()), (fp(12), Some(fp(20)), Some(fp(8))), "cached {cached}");
        // Same index again: nothing more owed (a no-funding increase keeps
        // the pre-funding "no balance write").
        let e = fill(&pm, true, 1, 100);
        assert_eq!((e.funding, e.cash()), (FixedPoint::ZERO, None), "cached {cached}");
        // Flip: sell 15 @ 100 at index 1: the long (11) is owed −5.5; the new
        // short (4) starts at index 1 with nothing paid.
        with_index(&mut pm, E18);
        let e = fill(&pm, false, 15, 100);
        let half = FixedPoint::from_raw(FixedPoint::SCALE / 2);
        assert_eq!(e.funding, -(fp(5) + half), "cached {cached}: flip settles the old side");
        let p = pm.get_position(&t, 1).unwrap().unwrap();
        assert!(!p.is_long);
        assert_eq!(p.funding, Some(PositionFunding { entry_index: E18, paid: FixedPoint::ZERO }));
        // Full close at index 3: the short owes −4 × 2 = −8 (receives 8).
        with_index(&mut pm, 3 * E18);
        let e = fill(&pm, true, 4, 100);
        assert_eq!((e.funding, e.closed_pnl), (-fp(8), Some(FixedPoint::ZERO)), "cached {cached}");
        assert_eq!(e.cash(), Some(fp(8)));
        assert!(pm.get_position(&t, 1).unwrap().is_none());
        if !cached {
            // apply_fill credits the cash itself: 1000 − 5 + 8 + 5.5 + 8 (the
            // flip's close part at entry 100.. realized 0).
            let b = pm.get_native_balance(&t).unwrap().available;
            assert_eq!(b, fp(1_000) - fp(5) + fp(8) + fp(5) + half + fp(8));
        }
    }
}

/// Without an index the fill path is exactly the pre-funding one: v2 rows,
/// no funding, the same effects (the feature-off identity).
#[test]
fn no_index_is_the_pre_funding_path() {
    let (_d, pm) = setup();
    let t = addr(1);
    let e = pm.apply_fill(&t, 1, true, fp(10), fp(100), MarginType::Cross).unwrap();
    assert_eq!(e.funding, FixedPoint::ZERO);
    let p = pm.get_position(&t, 1).unwrap().unwrap();
    assert_eq!(p.funding, None);
    assert_eq!(p.encoded_len(), POSITION_V2_LEN);
    assert!(pm.funding_table().is_none());
    assert_eq!(pm.funding_index(1).unwrap(), None);
}

/// Pending funding is in the UPnL (with a mark), never in IM / MM, and
/// `None` is exactly the pre-funding terms. RED before the funding engine.
#[test]
fn valuation_takes_pending_funding_off_the_upnl() {
    let p = base(Some(PositionFunding { entry_index: 0, paid: FixedPoint::ZERO }));
    let plain = position_terms(&p, Some(fp(110)), None).unwrap();
    let funded = position_terms_funded(&p, Some(fp(110)), Some(2 * E18), None).unwrap();
    assert_eq!(funded.upnl, plain.upnl - fp(6), "3 × 2 owed");
    assert_eq!(
        (funded.notional, funded.position_im, funded.maintenance),
        (plain.notional, plain.position_im, plain.maintenance)
    );
    assert_eq!(position_terms_funded(&p, Some(fp(110)), None, None).unwrap(), plain);
    // No mark: UPnL 0 and no funding term (s517 decision 2).
    assert_eq!(position_terms_funded(&p, None, Some(2 * E18), None).unwrap().upnl, FixedPoint::ZERO);
    let bal = NativeBalance { available: fp(50), order_margin: FixedPoint::ZERO };
    let v = AccountView::build_funded(&bal, &[p.clone()], |_| Some(fp(110)), |_| Ok(Some(2 * E18)), |_| None).unwrap();
    let v0 = AccountView::build(&bal, &[p], |_| Some(fp(110)), |_| None).unwrap();
    assert_eq!(v.equity(), v0.equity() - fp(6));
}
