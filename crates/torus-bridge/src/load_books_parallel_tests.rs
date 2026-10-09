//! s74: the level-authority book load (`load_order_books_levels`) rebuilds and
//! boot-verifies markets on worker threads. It must be byte-identical to the
//! serial load for every worker count, and a corrupt store must yield the
//! SAME fatal error (the first failing market in ascending order, exactly as
//! the serial loop reports it).

use super::*;
use torus_core::position::NativeBalance;
use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const MARKETS: u64 = 10;

fn fp(v: u64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn trader(i: u64) -> Address {
    let mut bytes = [0x5A; 20];
    bytes[..8].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(bytes)
}

fn place(market_id: u64, is_buy: bool, price: u64, order_type: OrderType) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

/// A mode-3 store: markets 1..=9 get resting bids and asks over uneven level
/// counts plus a few stop orders; market 10 stays empty (no rows at all).
fn build_store(chunked: bool) -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mode = if chunked { BookMode::LevelAuthorityChunked } else { BookMode::LevelAuthority };
    let mut holder = ResidentBooks::default();
    for height in 1..=4u64 {
        let ov = NativeStateOverlay::new(db.clone());
        let mut ctx = NativeExecContext::new_with_mode(
            ov.clone(), height, 1000 + height, 0, 100, 10,
            Address::new([99; 20]), Address::new([100; 20]), Address::new([101; 20]),
            mode, Some(&mut holder),
        );
        let mut batch = Vec::new();
        for t in 0..6u64 {
            let who = trader(t + 6 * height);
            let funds = NativeBalance { available: fp(1_000_000_000), order_margin: FixedPoint::ZERO };
            ctx.positions.put_native_balance(&who, &funds).unwrap();
            for market_id in 1..MARKETS {
                let levels = 5 + market_id * 7; // uneven work per market
                for k in 0..40u64 {
                    let level = (t * 40 + k + height) % levels;
                    batch.push((who, place(market_id, true, 100 + level, OrderType::Limit)));
                    batch.push((who, place(market_id, false, 1000 + level, OrderType::Limit)));
                }
                if t == 0 {
                    let stop = OrderType::StopLimit { trigger: fp(50), limit: fp(50) };
                    batch.push((who, place(market_id, false, 50, stop)));
                }
            }
        }
        let _ = NativeExecutor::execute_batch(&mut ctx, &batch);
        ctx.save_order_books();
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        ctx.stash_resident(&mut holder);
        ov.flush(&db).unwrap();
    }
    (dir, db)
}

type Loaded = (Vec<(MarketId, Vec<u8>)>, u128, Option<String>);

fn load(db: &StateDb, chunked: bool, workers: usize) -> Loaded {
    let mut timings = LoadTimings::default();
    let (books, next_id, err) =
        NativeExecContext::<StateDb>::load_order_books_levels(db, chunked, &mut timings, workers);
    let mut bytes: Vec<(MarketId, Vec<u8>)> = books
        .iter()
        .map(|(id, book)| (*id, borsh::to_vec(book).unwrap()))
        .collect();
    bytes.sort_by_key(|(id, _)| *id);
    (bytes, next_id, err)
}

#[test]
fn parallel_load_is_byte_identical_to_serial_for_any_worker_count() {
    for chunked in [false, true] {
        let (_dir, db) = build_store(chunked);
        let serial = load(&db, chunked, 1);
        assert!(serial.2.is_none(), "{:?}", serial.2);
        assert_eq!(serial.0.len(), (MARKETS - 1) as usize, "markets 1..=9 have books");
        assert!(serial.1 > 1, "next global order id recovered");
        for workers in [2, 3, 4, 9, 16] {
            let spawned = || {
                torus_state::spawn_count::totals()
                    [torus_state::spawn_count::SpawnSite::LoadBooks as usize]
            };
            let before = spawned();
            let parallel = load(&db, chunked, workers);
            assert!(parallel == serial, "chunked={chunked} workers={workers} differs from serial");
            // Item 6 Phase 2 step 0.2: the load workers are counted (a lower
            // bound: the counter is process-wide).
            assert!(spawned() >= before + 2, "workers={workers}");
        }
    }
}

#[test]
fn corrupt_store_reports_the_first_failing_market_for_any_worker_count() {
    let (_dir, db) = build_store(true);
    // Corrupt markets 3 and 7 by deleting one root level row each: the boot
    // verify of both fails; the serial loop reports market 3.
    let mut corrupted = 0;
    for market_id in [7u64, 3] {
        let (key, _) = db
            .iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&market_id.to_be_bytes()))
            .unwrap()
            .into_iter()
            .find(|(key, _)| key.len() == 26 && key[8] == ROW_TAG_LEVEL)
            .expect("a level row");
        db.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &key).unwrap();
        corrupted += 1;
    }
    assert_eq!(corrupted, 2);
    let serial = load(&db, true, 1);
    let err = serial.2.clone().expect("corrupt store is fatal");
    assert!(err.contains("market 3:"), "serial reports the lowest failing market: {err}");
    assert!(serial.0.is_empty());
    for workers in [2, 3, 4, 9, 16] {
        assert!(load(&db, true, workers) == serial, "workers={workers}");
    }
}

#[test]
fn load_books_workers_parse_matches_save_books_policy() {
    assert_eq!(parse_save_books_workers(None, 18), 18);
    assert_eq!(parse_save_books_workers(Some("1".into()), 18), 1);
    assert_eq!(parse_save_books_workers(Some("4".into()), 18), 4);
    assert_eq!(parse_save_books_workers(Some("junk".into()), 18), 18);
}
