//! AB-DIFFER microbench (round 3a, scratch branch `perf/ab-differ`).
//!
//! WHY: our re-proof residual is root+state_write+flush ≈ 2-2.5s/block at
//! 14-18k dirty buckets/block with ~700k-1.1M total resting orders, while a
//! parallel per-order-row implementation (`order_book_store::save_book_delta`)
//! reported flush 7.8ms/block at ~16k resting depth. This decomposes the gap.
//!
//! WHAT it times, per block, after warm state, across depth D:
//!   (a) our C4 shadow-differ full-walk save  (`save_book_rows`, non-resident)
//!   (b) our resident journal-driven save     (`save_book_rows_journaled`)
//!   (d) the downstream native trie flush (`compute_native_dirty_ops`) with
//!       and without `TORUS_NATIVE_ROOT_CACHE`, plus a per-dirty-bucket
//!       breakdown (member prefix-scan vs leaf keccak).
//! (c) — their `save_book_delta` — is O(touched) by construction, byte-for-byte
//! the same algorithm as (b); it is built + timed standalone on the
//! deep-book-storage checkout (see docs/ab-differ-findings.md). All numbers are
//! MICROBENCH wall-clock from this harness (no devnet), never mission
//! throughput.
//!
//! Run (VPS only, release):
//!   AB_DIFFER=1 cargo test -p torus-bridge --release --test ab_differ_microbench -- --ignored --nocapture
//! Depth set overridable: AB_DEPTHS="16000,200000,700000" (default).

use std::time::Instant;

use alloy_primitives::Address;
use torus_core::order_book::OrderBook;
use torus_state::native_trie::{microbench_bucket_scan, NativeTrieCache};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

const CF_ORDER_BOOKS: &str = "cf_native_order_books";
const TAG_ORDER_BOOKS: u8 = 1;
const N_MARKETS: u64 = 10;
const BLOCK_N: usize = 400; // mixed ops per measured block
const VALUE_BYTES: usize = 96; // seq(8) + Order borsh ≈ realistic row value

fn depths() -> Vec<usize> {
    std::env::var("AB_DEPTHS")
        .ok()
        .map(|s| {
            s.split(',')
                .filter_map(|x| x.trim().parse::<usize>().ok())
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![16_000, 200_000, 700_000])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}
fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

/// 25-byte order row key: market_id(8 BE) ‖ 0x01 ‖ order_id(16 BE) — the shared
/// per-order key shape (ours == theirs).
fn order_row_key(market_id: u64, order_id: u128) -> [u8; 25] {
    let mut k = [0u8; 25];
    k[..8].copy_from_slice(&market_id.to_be_bytes());
    k[8] = TAG_ORDER_BOOKS;
    k[9..].copy_from_slice(&order_id.to_be_bytes());
    k
}

fn row_value(seq: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(VALUE_BYTES);
    v.extend_from_slice(&seq.to_be_bytes());
    v.resize(VALUE_BYTES, 0xab);
    v
}

fn open_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

// ===========================================================================
// (d) TRIE: per-dirty-bucket cost vs depth, cached vs uncached.
// ===========================================================================
//
// Seeds D order rows into CF_NATIVE_ORDER_BOOKS (spread over N_MARKETS, so the
// mirror holds D members hashed uniformly across 65,536 buckets), builds the
// trie once, then measures the trie flush for a BLOCK_N-op dirty set (real
// value changes + deletes on existing rows). This is the exact production
// flush path (`flush_with_native_trie_stats`).
fn bench_trie(d: usize) {
    let (_dir, db) = open_db();

    // --- seed D rows + build trie (one-time, O(D), not measured) ---
    let seed = NativeStateOverlay::new(db.clone());
    for i in 0..d {
        let mkt = (i as u64) % N_MARKETS;
        let id = i as u128;
        seed.put_cf_raw(CF_ORDER_BOOKS, &order_row_key(mkt, id), &row_value(i as u64))
            .unwrap();
    }
    let t = Instant::now();
    seed.flush_with_native_trie_stats(&db, None, None).unwrap();
    let seed_secs = t.elapsed().as_secs_f64();

    // --- build a BLOCK_N-op dirty block: half value-changes to existing rows,
    //     half deletes of other existing rows (a realistic fill/cancel mix) ---
    let apply_block = |ov: &NativeStateOverlay, salt: u64| {
        for j in 0..BLOCK_N {
            let idx = ((j as u64 * 2_654_435_761).wrapping_add(salt) as usize) % d;
            let mkt = (idx as u64) % N_MARKETS;
            let id = idx as u128;
            if j % 2 == 0 {
                // value change (partial fill / modify): rewrite with new bytes
                ov.put_cf_raw(CF_ORDER_BOOKS, &order_row_key(mkt, id), &row_value(salt + j as u64))
                    .unwrap();
            } else {
                // delete (cancel / full fill)
                ov.delete_cf_raw(CF_ORDER_BOOKS, &order_row_key(mkt, id))
                    .unwrap();
            }
        }
    };

    // Capture the dirty set + decompose scan vs keccak (uncached bucket loop).
    let probe = NativeStateOverlay::new(db.clone());
    apply_block(&probe, 1);
    let dirty = probe.dirty_native_keys();
    let scan = microbench_bucket_scan(&db, &dirty).unwrap();

    // --- UNCACHED measured flush ---
    let ov_u = NativeStateOverlay::new(db.clone());
    apply_block(&ov_u, 2);
    let su = ov_u
        .flush_with_native_trie_stats(&db, None, None)
        .unwrap();

    // --- CACHED: warm the cache with one flush, then measure a second block ---
    let mut cache = NativeTrieCache::default();
    let ov_warm = NativeStateOverlay::new(db.clone());
    apply_block(&ov_warm, 3);
    ov_warm
        .flush_with_native_trie_stats(&db, None, Some(&mut cache))
        .unwrap();
    let ov_c = NativeStateOverlay::new(db.clone());
    apply_block(&ov_c, 4);
    let sc = ov_c
        .flush_with_native_trie_stats(&db, None, Some(&mut cache))
        .unwrap();

    let occ = d as f64 / 65_536.0;
    println!("TRIE D={d} seed={seed_secs:.2}s occ={occ:.2}mem/bucket");
    println!(
        "TRIE D={d} dirty_entries={} dirty_buckets={} members_scanned={} bytes_framed={}",
        dirty.len(),
        scan.dirty_buckets,
        scan.members_scanned,
        scan.bytes_framed
    );
    println!(
        "TRIE D={d} UNCACHED root={:.4}ms write={:.4}ms buckets={} | per_bucket={:.1}us",
        su.root_seconds * 1e3,
        su.write_seconds * 1e3,
        su.dirty_buckets,
        su.root_seconds * 1e6 / (su.dirty_buckets.max(1) as f64)
    );
    println!(
        "TRIE D={d}   CACHED root={:.4}ms write={:.4}ms buckets={} | per_bucket={:.1}us",
        sc.root_seconds * 1e3,
        sc.write_seconds * 1e3,
        sc.dirty_buckets,
        sc.root_seconds * 1e6 / (sc.dirty_buckets.max(1) as f64)
    );
    println!(
        "TRIE D={d} SCAN-DECOMP scan={:.4}ms keccak={:.4}ms | scan_per_bucket={:.1}us keccak_per_bucket={:.1}us",
        scan.scan_seconds * 1e3,
        scan.keccak_seconds * 1e3,
        scan.scan_seconds * 1e6 / (scan.dirty_buckets.max(1) as f64),
        scan.keccak_seconds * 1e6 / (scan.dirty_buckets.max(1) as f64),
    );
}

// ===========================================================================
// (a)/(b) DIFFER: full-walk vs journal-driven save at depth D for an N-op block.
// ===========================================================================
//
// Builds a book with ~D/N_MARKETS resting orders per market, does one initial
// full save (populates the row shadow, not measured), applies a BLOCK_N-op
// block (cancels + new places), then times `save_order_books`. Full-walk mode
// (journal off) re-walks every queue = O(D); journal mode = O(touched).
fn build_book(market_id: u64, orders: usize) -> (OrderBook, Vec<u128>) {
    let mut book = OrderBook::new(market_id, fp(1), fp(1));
    let mut ids = Vec::with_capacity(orders);
    let levels: i64 = 512; // several orders per price level (realistic depth)
    for i in 0..orders {
        let params = PlaceOrderParams {
            market_id,
            is_buy: true,
            price: fp((i as i64 % levels) + 1),
            quantity: fp(10),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let r = book.place_order(params, addr((i % 200) as u8 + 1), 1000);
        ids.push(r.order_id);
    }
    (book, ids)
}

fn place_new(book: &mut OrderBook, market_id: u64, price: i64) -> u128 {
    book.place_order(
        PlaceOrderParams {
            market_id,
            is_buy: true,
            price: fp(price),
            quantity: fp(10),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        },
        addr(7),
        1001,
    )
    .order_id
}

fn make_ctx(ov: NativeStateOverlay) -> torus_bridge::native_executor::NativeExecContext<NativeStateOverlay> {
    torus_bridge::native_executor::NativeExecContext::new_with_book_rows(
        ov, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101), /* book_rows */ true,
    )
}

fn bench_differ(d: usize, journaled: bool) {
    let per_market = (d / N_MARKETS as usize).max(1);
    let (_dir, db) = open_db();
    let ov = NativeStateOverlay::new(db.clone());
    let mut ctx = make_ctx(ov);

    // Build books across markets, insert into ctx, mark dirty.
    let mut market_ids: Vec<(u64, Vec<u128>)> = Vec::new();
    for m in 0..N_MARKETS {
        let (book, ids) = build_book(m, per_market);
        ctx.order_books.insert(m, book);
        ctx.dirty_books.insert(m);
        market_ids.push((m, ids));
    }

    // Initial full save: populates the row shadows for every market (O(D), not
    // measured — this is the resident rebuild / genesis seed).
    ctx.save_order_books();

    if journaled {
        for m in 0..N_MARKETS {
            ctx.order_books.get_mut(&m).unwrap().enable_mutation_journal();
        }
    }

    // Measured block: BLOCK_N mixed ops spread across markets — half cancels of
    // existing orders, half new places — then time save_order_books.
    ctx.dirty_books.clear();
    let mut next_price = 10_000i64;
    for j in 0..BLOCK_N {
        let (m, ids) = &mut market_ids[j % N_MARKETS as usize];
        let book = ctx.order_books.get_mut(m).unwrap();
        if j % 2 == 0 && !ids.is_empty() {
            let victim = ids[(j * 2_654_435usize) % ids.len()];
            let _ = book.cancel_order(victim);
        } else {
            next_price += 1;
            place_new(book, *m, next_price);
        }
        ctx.dirty_books.insert(*m);
    }

    let t = Instant::now();
    let written = ctx.save_order_books();
    let secs = t.elapsed().as_secs_f64();
    let mode = if journaled { "JOURNALED(b)" } else { "FULL-WALK(a)" };
    println!(
        "DIFFER D={d} {mode} save={:.4}ms rows_written={written} block_ops={BLOCK_N}",
        secs * 1e3
    );
}

#[test]
#[ignore = "AB-DIFFER microbench — VPS only, release, heavy setup"]
fn ab_differ_matrix() {
    assert_eq!(std::env::var("AB_DIFFER").ok().as_deref(), Some("1"), "set AB_DIFFER=1");
    println!("==== AB-DIFFER MICROBENCH (wall-clock, no devnet) ====");
    for d in depths() {
        println!("---- depth D={d} ----");
        bench_differ(d, false);
        bench_differ(d, true);
        bench_trie(d);
    }
    println!("==== END ====");
}
