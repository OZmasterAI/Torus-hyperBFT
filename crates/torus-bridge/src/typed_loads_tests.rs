//! Item 6 Phase 4 step 4 (`docs/plans/item6-phase4-impl.md`): typed loads.
//!
//! `PositionCache` loads, `BalanceCache::load` and the `cum_volume` reads
//! take the block's own pending row first (bytes, as before), else the
//! trader's record in `Users` (no R lookup, no clone, no Borsh decode). An
//! opaque trader (an irregular row) declines and reads through the overlay.
//!
//! Seeded block sequences through the node's lifecycle (R and the slot
//! attached, pipelined overlay over the previous block's frozen set, flushed
//! one block later), each block with several `execute_batch` calls (engine
//! forced / sequential settle / parallel settle) and direct writes between
//! them: positions put and deleted, balance and `cvlm` rows put and deleted
//! (tombstones in own pending), an irregular position row (`is_long` byte 2)
//! making one trader opaque. Before and after every call, for every trader
//! and market (clean, dirty, tombstoned, absent and opaque keys) the typed
//! load of a position (`PositionManager` and `PositionCache`), a balance
//! (`BalanceCache`) and a `cum_volume` equals `PositionManager` over the
//! overlay, and the record is read exactly for the clean keys of regular
//! traders. The calls themselves take the typed path (counters), and the run
//! equals the same sequence without R block by block.

use super::*;
use torus_core::position::{cum_volume_key, position_key, Position, TypedRows};
use torus_state::cf::{CF_CONSENSUS_META, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_POSITIONS, META_NATIVE_APPLIED_HEIGHT};
use torus_state::{FrozenPending, OwnPendingRow};

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn market_row(initial_margin: i64) -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USDC".to_string(), fp(1).raw(), fp(1).raw(), fp(initial_margin).raw()))
        .unwrap()
}

fn trader(i: u64) -> Address {
    let mut b = [0x6Bu8; 20];
    b[12..].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(b)
}

const TRADERS: u64 = 8;
const MARKETS: u64 = 4;
const BLOCKS: u64 = 24;
const CALLS: u64 = 3;
const MID: i64 = 1_000;
/// The trader that holds an irregular row now and then (opaque in R).
const ODD: u64 = TRADERS - 1;
/// The irregular row's market (no book: no fill touches it).
const ODD_MARKET: MarketId = 99;

#[derive(Default, Debug)]
struct Stats {
    /// Typed loads checked per key class.
    clean: usize,
    dirty: usize,
    tombstones: usize,
    opaque: usize,
    /// Typed record reads inside `execute_batch` calls (position, balance,
    /// volume).
    batch_hits: [usize; 3],
}

/// Every typed load == the overlay read; the record is read (hit) for a
/// clean key of a regular trader, declined for a clean key of an opaque
/// trader, and not asked for a key with an own pending row.
fn check_typed<T: StateBackend>(ctx: &NativeExecContext<T>, stats: &mut Stats, tag: &str) {
    let users = ctx.sums.as_ref().and_then(|s| s.records.as_deref()).expect("records attached");
    let typed: &dyn TypedRows = users;
    let state = ctx.positions.state();
    let pm = &ctx.positions;
    let expect = |cf: &str, key: &[u8], opaque: bool, kind: usize, n: usize, before: ([usize; 3], usize), stats: &mut Stats| {
        let (hits, declined) = users.read().typed_counts();
        let own = state.own_pending_row(cf, key);
        match own {
            OwnPendingRow::Pending(row) => {
                assert_eq!((hits, declined), before, "{tag}: {cf} {key:?} pending: record not asked");
                if row.is_none() {
                    stats.tombstones += 1;
                }
                stats.dirty += 1;
            }
            OwnPendingRow::Resident if opaque => {
                assert_eq!((hits, declined), (before.0, before.1 + n), "{tag}: {cf} {key:?} opaque: declined");
                stats.opaque += 1;
            }
            OwnPendingRow::Resident => {
                let mut want = before.0;
                want[kind] += n;
                assert_eq!((hits, declined), (want, before.1), "{tag}: {cf} {key:?} clean: record read");
                stats.clean += 1;
            }
            OwnPendingRow::Unknown => panic!("{tag}: R attached, own pending lookup unknown"),
        }
    };
    for i in 0..=TRADERS {
        let t = trader(i);
        let opaque = users.read().get(&t).is_none();
        for m in (0..=MARKETS + 1).chain([ODD_MARKET]) {
            let before = users.read().typed_counts();
            let want = format!("{:?}", pm.get_position(&t, m));
            assert_eq!(format!("{:?}", pm.get_position_typed(Some(typed), &t, m)), want, "{tag}: position {i} {m}");
            let mut cache = PositionCache::new();
            assert_eq!(format!("{:?}", cache.load_typed(pm, Some(typed), &t, m)), want, "{tag}: cached position {i} {m}");
            assert_eq!(format!("{:?}", cache.load_typed(pm, Some(typed), &t, m)), want, "{tag}: cache hit {i} {m}");
            expect(CF_NATIVE_POSITIONS, &position_key(&t, m), opaque, 0, 2, before, stats);
        }
        let before = users.read().typed_counts();
        let want = format!("{:?}", pm.get_native_balance(&t));
        assert_eq!(format!("{:?}", pm.get_native_balance_typed(Some(typed), &t)), want, "{tag}: balance {i}");
        assert_eq!(format!("{:?}", BalanceCache::new().load(pm, Some(typed), &t)), want, "{tag}: cached balance {i}");
        expect(CF_NATIVE_BALANCES, t.as_slice(), opaque, 1, 2, before, stats);
        let before = users.read().typed_counts();
        let want = format!("{:?}", pm.get_cum_volume(&t));
        assert_eq!(format!("{:?}", pm.get_cum_volume_typed(Some(typed), &t)), want, "{tag}: cum_volume {i}");
        expect(CF_NATIVE_BALANCES, &cum_volume_key(&t), opaque, 2, 1, before, stats);
    }
}

/// Direct writes between calls: positions put / deleted, balance and `cvlm`
/// rows put / deleted, and the odd trader's irregular row put / deleted.
fn write_rows<T: StateBackend>(ctx: &NativeExecContext<T>, rng: &mut Lcg, h: u64, call: u64) {
    let pm = &ctx.positions;
    for _ in 0..rng.below(4) {
        let t = trader(rng.below(TRADERS));
        let m = 1 + rng.below(MARKETS);
        match (rng.below(4), pm.get_position(&t, m).unwrap()) {
            (0, Some(_)) => pm.delete_position(&t, m).unwrap(),
            (_, old) => {
                let entry = fp(MID - 20 + rng.below(40) as i64);
                let size = fp(1 + rng.below(9) as i64);
                let pos = old.map(|p| Position { size: p.size + size, ..p }).unwrap_or(Position {
                    trader: t,
                    market_id: m,
                    is_long: rng.below(2) == 0,
                    size,
                    entry_price: entry,
                    cost_basis: entry * size,
                    realized_pnl: FixedPoint::ZERO,
                    isolated_margin: FixedPoint::ZERO,
                    margin_type: MarginType::Cross,
                });
                pm.put_position(&pos).unwrap();
            }
        }
    }
    for _ in 0..rng.below(3) {
        let t = trader(rng.below(TRADERS));
        match rng.below(5) {
            0 => pm.state().delete_cf_raw(CF_NATIVE_BALANCES, t.as_slice()).unwrap(),
            1 => pm.state().delete_cf_raw(CF_NATIVE_BALANCES, &cum_volume_key(&t)).unwrap(),
            2 => pm.put_cum_volume(&t, fp(rng.below(10_000_000) as i64)).unwrap(),
            _ => {
                let bal = NativeBalance { available: fp(100_000 + rng.below(400_000) as i64), order_margin: FixedPoint::ZERO };
                pm.put_native_balance(&t, &bal).unwrap();
            }
        }
    }
    // The odd trader's position in a market without a book, with `is_long`
    // byte 2: decodes (as long), does not re-encode to itself: opaque.
    if call == 0 {
        let odd = trader(ODD);
        let key = position_key(&odd, ODD_MARKET);
        match h % 5 {
            1 => {
                let p = Position {
                    trader: odd,
                    market_id: ODD_MARKET,
                    is_long: true,
                    size: fp(1),
                    entry_price: fp(MID),
                    cost_basis: fp(MID),
                    realized_pnl: FixedPoint::ZERO,
                    isolated_margin: FixedPoint::ZERO,
                    margin_type: MarginType::Cross,
                };
                let mut row = borsh::to_vec(&p).unwrap();
                row[1 + 20 + 8] = 2;
                pm.state().put_cf_raw(CF_NATIVE_POSITIONS, &key, &row).unwrap();
            }
            4 => pm.state().delete_cf_raw(CF_NATIVE_POSITIONS, &key).unwrap(),
            _ => {}
        }
    }
}

/// Crossing limit orders around the mid.
fn block_actions(rng: &mut Lcg) -> Vec<(Address, NativeAction)> {
    (0..6 + rng.below(10))
        .map(|_| {
            let params = PlaceOrderParams {
                market_id: 1 + rng.below(MARKETS),
                is_buy: rng.below(2) == 0,
                price: fp(MID - 2 + rng.below(5) as i64),
                quantity: fp(1 + rng.below(6) as i64),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            };
            (trader(rng.below(TRADERS)), NativeAction::PlaceOrder(params))
        })
        .collect()
}

/// One seeded run (`cached`: R, slot and typed loads; else no R). Returns
/// each call's results and the rows of both CFs after it.
/// Phase 5 step 2: `reference`, the holder on the Phase 4 reference path
/// (writes of the two CFs pending on the overlay); else in place.
fn run(seed: u64, cached: bool, reference: bool, stats: &mut Stats) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    for m in 1..=MARKETS {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &market_row(5)).unwrap();
    }
    {
        let ctx = NativeExecContext::new(db.clone(), 1, 1_000, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
        for i in 0..TRADERS {
            let bal = NativeBalance { available: fp(500_000), order_margin: FixedPoint::ZERO };
            ctx.positions.put_native_balance(&trader(i), &bal).unwrap();
        }
    }
    let mut rng = Lcg(seed);
    let mut holder = ResidentBooks::default();
    holder.set_reference_writes(reference);
    let mut parent: Option<Arc<FrozenPending>> = None;
    let mut books = HashMap::default();
    let mut next_id: u128 = 1;
    let mut out = Vec::new();
    for h in 1..=BLOCKS {
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rb = begin_resident(cached.then_some(&mut holder), &mut overlay, h, None);
        assert_eq!(rb.attached(), cached);
        let mut ctx =
            NativeExecContext::new(overlay.clone(), h, 10_000 + 7 * h, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
        ctx.order_books = std::mem::take(&mut books);
        ctx.next_global_order_id = next_id;
        ctx.attach_resident_block(&mut rb);
        for call in 0..CALLS {
            write_rows(&ctx, &mut rng, h, call);
            if cached {
                check_typed(&ctx, stats, &format!("seed {seed} block {h} call {call} before"));
            }
            let before = ctx.sums.as_ref().and_then(|s| s.records.as_deref()).map(|u| u.read().typed_counts().0);
            let actions = block_actions(&mut rng);
            let r = match (h + call) % 3 {
                0 => NativeExecutor::execute_batch_engine_mode(&mut ctx, &actions, 4),
                1 => NativeExecutor::execute_batch_settle_mode(&mut ctx, &actions, false),
                _ => NativeExecutor::execute_batch_settle_mode(&mut ctx, &actions, true),
            };
            assert!(ctx.fatal_error.is_none(), "seed {seed} block {h} call {call}: {:?}", ctx.fatal_error);
            if let Some(before) = before {
                let after = ctx.sums.as_ref().and_then(|s| s.records.as_deref()).unwrap().read().typed_counts().0;
                for k in 0..3 {
                    stats.batch_hits[k] += after[k] - before[k];
                }
            }
            if cached {
                check_typed(&ctx, stats, &format!("seed {seed} block {h} call {call} after"));
            }
            let mut rows = ctx.positions.state().iterate_cf(CF_NATIVE_POSITIONS, None).unwrap();
            rows.extend(ctx.positions.state().iterate_cf(CF_NATIVE_BALANCES, None).unwrap());
            out.push(format!(
                "block {h} call {call}: {:?} {rows:?}",
                r.results.iter().map(|x| (&x.error, x.success)).collect::<Vec<_>>()
            ));
        }
        books = std::mem::take(&mut ctx.order_books);
        next_id = ctx.next_global_order_id;
        ctx.detach_resident_block(&mut rb);
        drop(ctx);
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = overlay.own_pending_delta();
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, rb, &mut overlay, delta, true, None);
        if let Some(p) = parent.take() {
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
    }
    if cached {
        assert_eq!(holder.rows_builds(), 1, "R built once");
        assert_eq!(holder.rows_shared_fallbacks(), 0, "the record set came back unshared");
    }
    out
}

/// Plan step 4: `typed_loads_equal_overlay_reads` — 4 seeds x 24 blocks x 3
/// calls; every typed load == the overlay read, the record read exactly for
/// clean keys of regular traders, the calls take the typed path, and the
/// run equals the run without R call by call. Phase 5 step 2: on the
/// reference holder (Phase 4's pending rows) and in place (none pending).
#[test]
fn typed_loads_equal_overlay_reads() {
    let mut stats = Stats::default();
    let mut in_place_stats = Stats::default();
    for seed in 1..=4u64 {
        let typed = run(seed * 0x5851_F42D, true, true, &mut stats);
        let in_place = run(seed * 0x5851_F42D, true, false, &mut in_place_stats);
        let reference = run(seed * 0x5851_F42D, false, true, &mut Stats::default());
        assert_eq!(typed.len(), reference.len());
        for ((a, b), c) in typed.iter().zip(&reference).zip(&in_place) {
            assert_eq!(a, b, "seed {seed}: typed run != run without R");
            assert_eq!(c, b, "seed {seed}: in-place run != run without R");
        }
    }
    // Phase 5 step 2: in place, no key of the two CFs is ever pending on
    // the overlay (the write-through), so every typed load of a regular
    // trader reads its record.
    assert_eq!((in_place_stats.dirty, in_place_stats.tombstones), (0, 0), "in place: {in_place_stats:?}");
    assert!(in_place_stats.clean > stats.clean && in_place_stats.opaque > 20, "in place: {in_place_stats:?}");
    assert!(stats.clean > 1_000, "clean keys read from the record: {stats:?}");
    assert!(stats.dirty > 100, "keys with an own pending row: {stats:?}");
    assert!(stats.tombstones > 20, "own pending tombstones: {stats:?}");
    assert!(stats.opaque > 20, "opaque trader declined: {stats:?}");
    assert!(stats.batch_hits.iter().all(|n| *n > 50), "execute_batch takes the typed path: {stats:?}");
}
