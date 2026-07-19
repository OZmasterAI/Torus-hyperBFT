//! rank-root (perf/exec-scaleup): the flush-path root work must be invariant
//! across every node-local execution mode.
//!
//! The native root is the ALREADY-incremental bucketed Merkle trie
//! (torus-state::native_trie), maintained inside the atomic overlay flush.
//! This round added `TORUS_NATIVE_ROOT_CACHE` (in-RAM node image + clean-write
//! elision, value-neutral) and the flush-phase stats split. THE contract:
//! for any block sequence, every combination of
//!   rows (TORUS_BOOK_ROWS) × resident (TORUS_RESIDENT_BOOKS) ×
//!   parallel settle (TORUS_PARALLEL_SETTLE) × root cache (TORUS_NATIVE_ROOT_CACHE)
//! must produce byte-identical persisted state — all 6 native CFs, the trie
//! node CF, the mirror CF and the per-block persisted roots — within a
//! persistence mode (rows changes the book CF by design; everything else is
//! node-local). Restart-mid-sequence (dropping BOTH cross-block holders) must
//! change nothing.

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor, ResidentBooks};
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_HASHED, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
    CF_NATIVE_TRIE,
};
use torus_state::native_trie::{
    build_native_trie_to_cf, native_root_full, persisted_native_root, NativeTrieCache,
};
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn order(
    market_id: MarketId,
    is_buy: bool,
    price: FixedPoint,
    qty: FixedPoint,
    order_type: OrderType,
    tif: TimeInForce,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    order(
        market_id,
        is_buy,
        fp(price),
        fp(qty),
        OrderType::Limit,
        TimeInForce::GTC,
    )
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

/// The adversarial per-block batches (same script as resident_books_tests).
fn script_batches(id0: u128) -> Vec<Vec<(Address, NativeAction)>> {
    let m1 = addr(1);
    let m2 = addr(2);
    let t1 = addr(3);
    let s1 = addr(4);
    let dusty = FixedPoint::from_raw(2 * FixedPoint::SCALE + 7);

    let b1 = vec![
        place(m1, gtc(1, true, 100, 5)),
        place(m2, gtc(1, true, 100, 3)),
        place(m1, gtc(1, false, 105, 4)),
        place(m2, order(2, true, fp(50), dusty, OrderType::Limit, TimeInForce::GTC)),
        place(m1, gtc(2, false, 55, 6)),
        place(
            s1,
            order(
                3,
                true,
                fp(10),
                fp(2),
                OrderType::StopMarket { trigger: fp(12) },
                TimeInForce::GTC,
            ),
        ),
        place(m2, gtc(3, false, 11, 1)),
    ];
    let b2 = vec![
        place(t1, gtc(1, false, 100, 2)),
        (
            m2,
            NativeAction::ModifyOrder {
                order_id: id0 + 3,
                new_price: None,
                new_qty: Some(fp(1)),
            },
        ),
        (
            m1,
            NativeAction::ModifyOrder {
                order_id: id0,
                new_price: Some(fp(99)),
                new_qty: None,
            },
        ),
        place(s1, gtc(2, true, 54, 1)),
        place(s1, gtc(2, false, 54, 1)),
    ];
    let b3 = vec![
        (m2, NativeAction::CancelOrder { order_id: id0 + 3 }),
        place(t1, gtc(1, true, 105, 4)),
        place(t1, gtc(3, true, 11, 1)),
        (m1, NativeAction::CancelAllOrders { market_id: None }),
    ];
    vec![b1, b2, b3]
}

#[derive(Clone, Copy)]
struct Modes {
    rows: bool,
    resident: bool,
    parallel: bool,
    cache: bool,
}

struct ComboResult {
    /// Persisted native root after each block's flush.
    roots: Vec<B256>,
    /// Rehashed-bucket counts per block (the O(dirty) witness).
    dirty_buckets: Vec<usize>,
    /// Final byte dump of the consensus + trie + mirror CFs.
    dumps: Vec<(&'static str, Vec<u8>, Vec<u8>)>,
    /// Full-scan oracle at the end.
    oracle: B256,
}

fn dump_cf(db: &StateDb, cf: &'static str) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    use torus_state::StateBackend;
    StateBackend::iterate_cf(db, cf, None)
        .expect("iterate")
        .into_iter()
        .map(|(k, v)| (cf, k, v))
        .collect()
}

/// Run the script the way the live node does: fresh overlay + context per
/// block, save books, atomic flush with trie maintenance (+marker), holders
/// carried across blocks. `restart_before`: drop BOTH holders before that
/// block (process restart).
fn run_combo(modes: Modes, restart_before: Option<u64>) -> ComboResult {
    let (_dir, db) = open_test_db();

    // Fund actors directly, then build the trie base (the boot-time
    // ensure_native_trie_built step) so incremental maintenance starts from a
    // consistent mirror.
    {
        let bal = NativeBalance {
            available: fp(1_000_000),
            order_margin: FixedPoint::ZERO,
        };
        let ctx0 = NativeExecContext::new_with_book_rows(
            db.clone(),
            0,
            1000,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
            modes.rows,
        );
        for t in [addr(1), addr(2), addr(3), addr(4)] {
            ctx0.positions.put_native_balance(&t, &bal).unwrap();
        }
    }
    build_native_trie_to_cf(&db).unwrap();

    let mut books = ResidentBooks::default();
    let mut trie_cache = NativeTrieCache::default();
    let batches = script_batches(1);
    let mut roots = Vec::new();
    let mut dirty_buckets = Vec::new();

    for (i, batch) in batches.iter().enumerate() {
        let height = (i + 1) as u64;
        if restart_before == Some(height) {
            books = ResidentBooks::default();
            trie_cache.invalidate();
        }
        let overlay = NativeStateOverlay::new(db.clone());
        let mut ctx = NativeExecContext::new_with_modes(
            overlay.clone(),
            height,
            1000 + height,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
            modes.rows,
            if modes.resident { Some(&mut books) } else { None },
        );
        assert!(ctx.fatal_error.is_none(), "load {height}: {:?}", ctx.fatal_error);

        let r = NativeExecutor::execute_batch_settle_mode(&mut ctx, batch, modes.parallel);
        assert!(
            r.results.iter().all(|x| x.success),
            "block {height}: {:?}",
            r.results.iter().filter(|x| !x.success).collect::<Vec<_>>()
        );
        assert!(ctx.fatal_error.is_none());
        ctx.save_order_books();
        ctx.stash_resident(&mut books);

        let stats = overlay
            .flush_with_native_trie_stats(
                &db,
                Some(height),
                if modes.cache { Some(&mut trie_cache) } else { None },
            )
            .expect("flush");
        roots.push(persisted_native_root(&db).unwrap());
        dirty_buckets.push(stats.dirty_buckets);
    }

    let mut dumps = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_TRIE,
        CF_NATIVE_HASHED,
    ] {
        dumps.extend(dump_cf(&db, cf));
    }
    ComboResult {
        roots,
        dirty_buckets,
        dumps,
        oracle: native_root_full(&db).unwrap(),
    }
}

/// All 16 mode combos: byte-identical persisted state within a persistence
/// mode; roots always equal the full-scan oracle; cached rehash counts never
/// exceed uncached.
#[test]
fn all_mode_combos_identical_state_and_roots() {
    let mut results: Vec<(Modes, ComboResult)> = Vec::new();
    for rows in [false, true] {
        for resident in [false, true] {
            for parallel in [false, true] {
                for cache in [false, true] {
                    let m = Modes { rows, resident, parallel, cache };
                    results.push((m, run_combo(m, None)));
                }
            }
        }
    }

    for rows in [false, true] {
        let group: Vec<&(Modes, ComboResult)> =
            results.iter().filter(|(m, _)| m.rows == rows).collect();
        let (bm, base) = group[0];
        assert_eq!(
            base.oracle,
            *base.roots.last().unwrap(),
            "rows={rows}: persisted root != full-scan oracle"
        );
        for (m, r) in &group[1..] {
            let label = format!(
                "rows={} resident={} parallel={} cache={} vs baseline resident={} parallel={} cache={}",
                m.rows, m.resident, m.parallel, m.cache, bm.resident, bm.parallel, bm.cache
            );
            assert_eq!(base.roots, r.roots, "{label}: per-block roots diverged");
            assert_eq!(base.dumps, r.dumps, "{label}: persisted CF bytes diverged");
            assert_eq!(base.oracle, r.oracle, "{label}: oracle diverged");
            // The O(dirty) witness: cached combos may only elide (the script
            // has no clean rewrites, so counts should be EQUAL — assert both
            // directions to catch an over- or under-eliding differ).
            assert_eq!(
                base.dirty_buckets, r.dirty_buckets,
                "{label}: rehashed-bucket counts diverged"
            );
        }
    }
}

/// Restart mid-sequence (both cross-block holders dropped) is invisible in
/// persisted state, per-block roots and rehash counts.
#[test]
fn restart_mid_sequence_invisible_all_modes() {
    for rows in [false, true] {
        let m = Modes { rows, resident: true, parallel: true, cache: true };
        let base = run_combo(m, None);
        for restart in [2u64, 3u64] {
            let r = run_combo(m, Some(restart));
            assert_eq!(base.roots, r.roots, "rows={rows} restart={restart}: roots");
            assert_eq!(base.dumps, r.dumps, "rows={rows} restart={restart}: CF bytes");
            assert_eq!(
                base.dirty_buckets, r.dirty_buckets,
                "rows={rows} restart={restart}: rehash counts"
            );
            assert_eq!(base.oracle, r.oracle);
        }
    }
}

/// Clean-write elision through the FULL flush path: re-flushing an overlay
/// that rewrites unchanged rows must rehash zero buckets under the cache and
/// leave the persisted root untouched.
#[test]
fn flush_level_clean_rewrite_elision() {
    let (_dir, db) = open_test_db();
    let bal = NativeBalance {
        available: fp(1_000),
        order_margin: FixedPoint::ZERO,
    };
    {
        let ctx0 = NativeExecContext::new_with_book_rows(
            db.clone(), 0, 1000, 0, 100, 10, addr(99), addr(100), addr(101), false,
        );
        for t in [addr(1), addr(2)] {
            ctx0.positions.put_native_balance(&t, &bal).unwrap();
        }
    }
    build_native_trie_to_cf(&db).unwrap();
    let root0 = persisted_native_root(&db).unwrap();

    let mut cache = NativeTrieCache::default();
    // An overlay that re-puts addr(1)'s balance row with IDENTICAL bytes.
    let overlay = NativeStateOverlay::new(db.clone());
    {
        use torus_state::StateBackend;
        let key = addr(1);
        let cur = StateBackend::get_cf_raw(&db, CF_NATIVE_BALANCES, key.as_slice())
            .unwrap()
            .expect("funded");
        overlay
            .put_cf_raw(CF_NATIVE_BALANCES, key.as_slice(), &cur)
            .unwrap();
    }
    let stats = overlay
        .flush_with_native_trie_stats(&db, Some(1), Some(&mut cache))
        .unwrap();
    assert_eq!(stats.dirty_buckets, 0, "clean rewrite must be elided");
    assert_eq!(persisted_native_root(&db).unwrap(), root0, "root unchanged");
    assert_eq!(native_root_full(&db).unwrap(), root0, "oracle unchanged");

    // Same overlay shape WITHOUT the cache: rehashes the bucket, same root.
    let overlay2 = NativeStateOverlay::new(db.clone());
    {
        use torus_state::StateBackend;
        let key = addr(2);
        let cur = StateBackend::get_cf_raw(&db, CF_NATIVE_BALANCES, key.as_slice())
            .unwrap()
            .expect("funded");
        overlay2
            .put_cf_raw(CF_NATIVE_BALANCES, key.as_slice(), &cur)
            .unwrap();
    }
    let stats2 = overlay2
        .flush_with_native_trie_stats(&db, Some(2), None)
        .unwrap();
    assert_eq!(stats2.dirty_buckets, 1, "uncached path rehashes the bucket");
    assert_eq!(persisted_native_root(&db).unwrap(), root0, "root still unchanged");
}
