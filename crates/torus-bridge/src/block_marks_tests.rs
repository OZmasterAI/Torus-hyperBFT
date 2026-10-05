//! Item 6 Phase 1 (C2, plan 2.3): the block mark table.
//!
//! - The table == `get_price(m, now).usable()` (today's per-read call) for
//!   every market: fresh, stale by time, absent, non-positive, undecodable,
//!   a delisted market whose last aggregate is still fresh, and an unlisted
//!   book market; a market outside the table reads the oracle directly.
//! - `version` changes exactly when the table or `margin_configs` differ from
//!   the previous block's (kept in the resident rows slot), and a new version
//!   is never one used before (slot rebuilt / absent = changed).

use super::*;
use torus_core::oracle::OraclePrice;
use torus_state::cf::{CF_CONSENSUS_META, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, META_NATIVE_APPLIED_HEIGHT};

const NOW: u64 = 10_000;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// A listed market row with `initial_margin` % (decodes to a margin config).
fn market_row(initial_margin: i64) -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USDC".to_string(), fp(1).raw(), fp(1).raw(), fp(initial_margin).raw()))
        .unwrap()
}

fn agg_key(m: MarketId) -> Vec<u8> {
    [b"agg".as_slice(), &m.to_be_bytes()].concat()
}

/// The stored aggregate row: price(16) ‖ block(8) ‖ reporters(4) ‖ timestamp(8), big-endian.
fn agg_row(price_raw: i128, ts: u64) -> Vec<u8> {
    [price_raw.to_be_bytes().as_slice(), &7u64.to_be_bytes(), &3u32.to_be_bytes(), &ts.to_be_bytes()].concat()
}

/// Today's per-read mark rule.
fn oracle_mark<T: StateBackend>(ctx: &NativeExecContext<T>, m: MarketId) -> Option<FixedPoint> {
    ctx.oracle.get_price(m, ctx.timestamp).ok().and_then(|p: OraclePrice| p.usable())
}

fn ctx_at<T: StateBackend>(state: T, h: u64, now: u64) -> NativeExecContext<T> {
    NativeExecContext::new(state, h, now, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO)
}

/// Listed 1..=6 (configs), 7 has a fresh aggregate but no market row
/// (delisted), 8 an undecodable aggregate and no row.
fn seed(db: &StateDb) {
    for m in 1..=6u64 {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &market_row(5)).unwrap();
    }
    let rows: [(MarketId, Option<Vec<u8>>); 8] = [
        (1, Some(agg_row(fp(100).raw(), NOW - 5))),  // fresh
        (2, Some(agg_row(fp(200).raw(), NOW - 61))), // stale by time
        (3, None),                                   // absent
        (4, Some(agg_row(0, NOW - 5))),              // zero
        (5, Some(agg_row(-fp(3).raw(), NOW - 5))),   // negative
        (6, Some(agg_row(fp(600).raw(), NOW - 60))), // fresh at the 60 s edge
        (7, Some(agg_row(fp(700).raw(), NOW - 5))),  // delisted, fresh
        (8, Some(b"garbage".to_vec())),              // undecodable
    ];
    for (m, row) in rows {
        if let Some(row) = row {
            db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &row).unwrap();
        }
    }
}

#[test]
fn table_equals_get_price_usable_for_every_market() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    seed(&db);
    for (now, fresh_1) in [(NOW, true), (NOW + 56, false)] {
        let mut ctx = ctx_at(db.clone(), 9, now);
        assert_eq!(ctx.margin_configs.len(), 6);
        let agg = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(ctx.fatal_error.is_none());
        assert_eq!(agg.len(), 6, "one aggregation result per listed market");
        let table = ctx.block_marks.as_ref().expect("table filled by begin_block_oracle");
        // Every listed / configured market and every aggregate row is in the table.
        let mut keys: Vec<MarketId> = table.marks.keys().copied().collect();
        keys.sort_unstable();
        assert_eq!(keys, (1..=8).collect::<Vec<_>>(), "now {now}");
        for m in 1..=8u64 {
            assert_eq!(table.marks[&m], oracle_mark(&ctx, m), "now {now}: table market {m}");
        }
        let reader = AccountReader::of(&ctx);
        for m in [1, 2, 3, 4, 5, 6, 7, 8, 9, u64::MAX] {
            assert_eq!(reader.mark(m), oracle_mark(&ctx, m), "now {now}: reader market {m}");
        }
        assert_eq!(table.marks[&1].is_some(), fresh_1, "now {now}: market 1 goes stale by time");
        for m in [2, 3, 4, 5, 8] {
            assert_eq!(table.marks[&m], None, "now {now}: market {m}");
        }
        assert_eq!(table.marks[&7], (now == NOW).then(|| fp(700)), "now {now}: delisted market 7");
        // Liquidation's marks (listed markets with a mark) and the delisted
        // markets with a mark (C4's guard).
        let listed = ctx.governance.listed_market_ids().unwrap();
        let liq: Vec<MarketId> = listed.iter().copied().filter(|&m| reader.mark(m).is_some()).collect();
        assert_eq!(liq, if fresh_1 { vec![1, 6] } else { vec![] }, "now {now}");
        let want_delisted: Vec<MarketId> = if now == NOW { vec![7] } else { vec![] };
        assert_eq!(table.delisted_marked(&listed), want_delisted, "now {now}");
    }
}

/// A market outside the table (no market row, no aggregate at block start)
/// reads the oracle directly: an aggregate written after the fill is seen.
#[test]
fn market_outside_the_table_reads_the_oracle() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    seed(&db);
    let mut ctx = ctx_at(db.clone(), 9, NOW);
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    assert!(!ctx.block_marks.as_ref().unwrap().marks.contains_key(&9));
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(9), &agg_row(fp(900).raw(), NOW)).unwrap();
    assert_eq!(AccountReader::of(&ctx).mark(9), Some(fp(900)));
}

/// A market with a loaded book but no listing and no aggregate (the
/// unlisted ubench shape, marks off) is in the table as `None`, so its
/// positions do not read the oracle once per view (fix 1's memo covered the
/// book markets too).
#[test]
fn book_markets_are_in_the_table() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut ctx = ctx_at(db.clone(), 9, NOW);
    ctx.order_books.insert(30, OrderBook::new(30, fp(1), fp(1)));
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    let table = ctx.block_marks.as_ref().unwrap();
    assert_eq!(table.marks.get(&30), Some(&None));
    assert_eq!(table.marks.len(), 1);
}

/// No `begin_block_oracle` (or a failed oracle step): no table, every read
/// goes to the oracle (today's path).
#[test]
fn no_table_without_the_oracle_step() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    seed(&db);
    let ctx = ctx_at(db.clone(), 9, NOW);
    assert!(ctx.block_marks.is_none() && ctx.mark_version().is_none());
    for m in 1..=9u64 {
        assert_eq!(AccountReader::of(&ctx).mark(m), oracle_mark(&ctx, m));
    }
}

/// One serial native block at `h` (time `now`), wired like app.rs: R and the
/// slot's mark state attached, `prep` writes into the block's overlay before
/// the context loads (as the previous block's governance would), flush with the marker, `end_resident`. Returns the
/// block's mark version and table. `holder: None` = the reference path.
fn run_block(
    db: &StateDb,
    mut holder: Option<&mut ResidentBooks>,
    h: u64,
    now: u64,
    prep: &dyn Fn(&NativeStateOverlay),
) -> (u64, HashMap<MarketId, Option<FixedPoint>>) {
    let mut overlay = NativeStateOverlay::new(db.clone());
    let mut rb = begin_resident(holder.as_deref_mut(), &mut overlay, h, None);
    assert_eq!(rb.attached(), holder.is_some());
    prep(&overlay);
    let mut ctx = ctx_at(overlay.clone(), h, now);
    ctx.attach_resident_block(&mut rb);
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    assert!(ctx.fatal_error.is_none());
    let version = ctx.mark_version().expect("table filled");
    let table = ctx.block_marks.as_ref().unwrap().marks.clone();
    ctx.detach_resident_block(&mut rb);
    drop(ctx);
    let delta = overlay.own_pending_delta();
    overlay.flush_with_native_trie_and_marker(db, h).unwrap();
    if let Some(holder) = holder {
        end_resident(holder, rb, &mut overlay, delta, true, None);
    }
    (version, table)
}

#[test]
fn version_changes_exactly_when_table_or_configs_change() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    seed(&db);
    let mut holder = ResidentBooks::default();
    let mut seen: Vec<u64> = Vec::new();
    let mut step = |h: u64, now: u64, prep: &dyn Fn(&NativeStateOverlay), holder: &mut ResidentBooks| {
        let (v, _) = run_block(&db, Some(holder), h, now, prep);
        let new = !seen.contains(&v);
        if new {
            assert!(seen.iter().all(|&s| s < v), "block {h}: a new version is above every earlier one");
        }
        seen.push(v);
        (v, new)
    };
    let none = |_: &NativeStateOverlay| {};
    let (v1, new) = step(1, NOW - 10, &none, &mut holder);
    assert!(new, "first block: the slot is built, the version is new");
    let (v, new) = step(2, NOW - 9, &none, &mut holder);
    assert!(!new && v == v1, "nothing changed: same version");
    // Same price re-aggregated later (row bytes differ, table does not).
    let (v, new) = step(3, NOW - 8, &|o| o.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(1), &agg_row(fp(100).raw(), NOW - 8)).unwrap(), &mut holder);
    assert!(!new && v == v1, "same marks, new aggregate timestamp: same version");
    let (v4, new) = step(4, NOW - 7, &|o| o.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(1), &agg_row(fp(101).raw(), NOW - 7)).unwrap(), &mut holder);
    assert!(new && v4 != v1, "a mark moved: new version");
    let (v, new) = step(5, NOW - 6, &none, &mut holder);
    assert!(!new && v == v4);
    // Markets 2 and 6 (aggregates at NOW - 61 / NOW - 60) go stale by time.
    let (v6, new) = step(6, NOW + 1, &none, &mut holder);
    assert!(new && v6 != v4, "a mark went stale: new version");
    // A margin config changes (market 2: 5% -> 10%); marks unchanged.
    let (v7, new) = step(7, NOW + 2, &|o| o.put_cf_raw(CF_NATIVE_MARKETS, &2u64.to_be_bytes(), &market_row(10)).unwrap(), &mut holder);
    assert!(new && v7 != v6, "configs changed: new version");
    // A config row rewritten with identical bytes: nothing changed.
    let (v, new) = step(8, NOW + 3, &|o| o.put_cf_raw(CF_NATIVE_MARKETS, &2u64.to_be_bytes(), &market_row(10)).unwrap(), &mut holder);
    assert!(!new && v == v7);
    // A new listing (no aggregate): the table gains a `None` key: new version.
    let (v9, new) = step(9, NOW + 4, &|o| o.put_cf_raw(CF_NATIVE_MARKETS, &20u64.to_be_bytes(), &market_row(5)).unwrap(), &mut holder);
    assert!(new && v9 != v7);
    // Slot rebuilt (invalidate): nothing changed, but the version is new.
    holder.invalidate();
    let (v10, new) = step(10, NOW + 5, &none, &mut holder);
    assert!(new && v10 != v9, "slot rebuilt: never reuse the version");
    let (v, new) = step(11, NOW + 6, &none, &mut holder);
    assert!(!new && v == v10, "carried again after the rebuild");
    // Skipped height: the guard trips, the slot is rebuilt: new version.
    let (v13, new) = step(13, NOW + 7, &none, &mut holder);
    assert!(new && v13 != v10, "guard trip: never reuse the version");
    // An untouched (non-native) block in between advances the slot: the
    // contents decide, so an unchanged table keeps the version.
    // (app.rs: the marker-only job writes the marker, then advances.)
    db.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &14u64.to_be_bytes()).unwrap();
    holder.advance_untouched_with(true, 14);
    assert_eq!(holder.rows_height(), Some(14));
    let (v, new) = step(15, NOW + 8, &none, &mut holder);
    assert!(!new && v == v13, "after an advanced untouched block");
}

/// The reference path (no slot): every block gets a new version; the table
/// itself is the same as with the slot.
#[test]
fn reference_path_tables_match_and_versions_are_never_reused() {
    let mk = || {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        seed(&db);
        (dir, db)
    };
    let ((_d1, db1), (_d2, db2)) = (mk(), mk());
    let mut holder = ResidentBooks::default();
    let mut versions = Vec::new();
    for h in 1..=4u64 {
        let (v_ref, t_ref) = run_block(&db1, None, h, NOW + h, &|_| {});
        let (_, t_slot) = run_block(&db2, Some(&mut holder), h, NOW + h, &|_| {});
        assert_eq!(t_ref, t_slot, "block {h}: same table with and without the slot");
        assert!(!versions.contains(&v_ref), "block {h}: reference version reused");
        versions.push(v_ref);
    }
}

/// Item 6 M1 (cut 7): the dense per-market indexes answer exactly as the
/// maps for every id (present, absent, mark `None`, past the last key, and
/// a map with an id at or past `DENSE_MARKETS`, which keeps the map).
#[test]
fn dense_indexes_equal_the_maps() {
    let mut seed = 0x5EED_D3E5u64;
    let mut below = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    let mut dense_used = 0;
    for round in 0..300 {
        let span = if round % 10 == 0 { DENSE_MARKETS + 50 } else { 1 + below(400) };
        let mut marks: HashMap<MarketId, Option<FixedPoint>> = HashMap::new();
        let mut configs: HashMap<MarketId, MarketMarginConfig> = HashMap::new();
        for _ in 0..below(60) {
            let m = below(span);
            marks.insert(m, (below(3) > 0).then(|| fp(1 + below(1000) as i64)));
            if below(2) == 0 {
                configs.insert(m, MarketMarginConfig::new(m, 1 + below(50) as u32));
            }
        }
        let table = BlockMarks::new(marks.clone(), 1);
        dense_used += usize::from(!table.dense.is_empty());
        let tiers = DenseTiers::of(&configs);
        for m in (0..span + 3).chain([DENSE_MARKETS - 1, DENSE_MARKETS, u64::MAX]) {
            assert_eq!(table.get(m), marks.get(&m).copied(), "round {round}: mark {m}");
            let dense = tiers.tiers.get(m as usize).copied().flatten();
            let want = configs.get(&m).map(|c| c.tiers.as_slice());
            if !tiers.tiers.is_empty() {
                assert_eq!(dense.map(<[MarginTier]>::as_ptr), want.map(<[MarginTier]>::as_ptr), "round {round}: tiers {m}");
            }
        }
    }
    assert!(dense_used > 200, "non-vacuous: {dense_used}");
}
