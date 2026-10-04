//! Item 6 Phase 1, step 0.2: the margin, match and liquidation paths read
//! `CF_NATIVE_POSITIONS` / `CF_NATIVE_BALANCES` from memory only — no RocksDB
//! read once the resident rows (C1) and the sums cache / L1 (C3 / C4) are in.
//!
//! A fed 20-block econ sequence (marks usable, moving 10 bp per block) runs
//! through the ubench's block path: each block's `NativeStateOverlay` layered
//! over the previous block's frozen set, which is flushed one block later.
//! The context's backend wraps that overlay in the counting backend, whose
//! storage probe records every read of the two CFs that reached RocksDB (no
//! layer answered it) with its stack; the stack names the engine path.
//!
//!   cargo test -p torus-bridge --test storage_reads_tests -- --ignored --nocapture

#[path = "common/counting_backend.rs"]
mod counting_backend;
#[path = "common/econ_load.rs"]
mod econ_load;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use counting_backend::{CountingBackend, Counts, StorageRead};
use econ_load::{base_mark, feed_setup, sender, special, Gen, Lcg, MarkWalk, REPORTERS};
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::{FrozenPending, NativeStateOverlay, StateBackend, StateDb};
use torus_types::FixedPoint;

const SENDERS: u64 = 48;
const MARKETS: u64 = 6;
const ACTIONS: u64 = 24;
const BLOCKS: u64 = 20;
const WALK_BP: u64 = 10;

/// Engine path of one storage read, from the OUTERMOST frame that names a
/// phase (a liquidation's stage-1 order runs triggered stops: still
/// liquidation). `margin` = Phase 2 (reservation, pools), `match` = Phase 3
/// (reduce-only policing, checked takers' prices, maker checks),
/// `liquidation` = `run_liquidations`; the rest is reported, not asserted.
fn path_of(read: &StorageRead) -> &'static str {
    const MARKERS: &[(&str, &str)] = &[
        ("prepare_one", "margin"),
        ("phase2_", "margin"),
        ("same_batch_bid_top_ups", "margin"),
        ("d2_pool_takers", "margin"),
        ("match_parallel", "match"),
        ("reduce_only_positions_for", "match"),
        ("BatchMakerAccounts", "match"),
        ("settle_market_results", "settle"),
        ("add_cum_volume", "settle"),
        ("run_triggered_stops", "settle"),
        ("exec_cancel", "phase1"),
        ("run_liquidations", "liquidation"),
        ("begin_block_oracle", "oracle"),
    ];
    let stack = read.stack.to_string();
    let frames: Vec<&str> = stack.lines().filter(|l| !l.trim_start().starts_with("at ")).collect();
    if let Some(path) = frames.iter().rev().find_map(|f| MARKERS.iter().find(|(m, _)| f.contains(m)).map(|(_, p)| *p)) {
        return path;
    }
    // Reads made by `execute_batch_phases` itself: Phase 3's checked-taker
    // prices (`AccountReader::position_px`), Phase 2's pool balances
    // (`BalanceCache::load`).
    match frames.iter().position(|f| f.contains("execute_batch_phases")) {
        Some(i) if frames[..i].iter().any(|g| g.contains("position_px")) => "match",
        Some(i) if frames[..i].iter().any(|g| g.contains("BalanceCache") && g.contains("load")) => "margin",
        Some(_) => "batch-other",
        None => "other",
    }
}

/// Fund the senders and set up the feed (DB, before block 1).
fn setup(db: &StateDb) {
    let ctx = NativeExecContext::new(db.clone(), 1, 1000, 0, 1_000_000, 100, special(99), special(100), special(101));
    for i in 0..SENDERS {
        ctx.positions
            .put_native_balance(
                &sender(i),
                &NativeBalance { available: FixedPoint::from_raw(100_000_000 * FixedPoint::SCALE), order_margin: FixedPoint::ZERO },
            )
            .unwrap();
    }
    feed_setup(db, MARKETS);
}

/// Every key of both probed CFs sits in the active memtable (no SST): the
/// perf-context probe then counts every point read and every seek.
fn assert_memtable_only(db: &StateDb) {
    for cf in [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES] {
        let h = db.inner().cf_handle(cf).unwrap();
        let prop = |p: &str| db.inner().property_int_value_cf(&h, p).unwrap().unwrap_or(0);
        assert_eq!(prop("rocksdb.total-sst-files-size"), 0, "{cf}: rows flushed to SST");
        assert!(prop("rocksdb.num-entries-active-mem-table") > 0, "{cf}: empty memtable");
    }
}

/// Path, CF and op of a storage read.
type ReadKey = (&'static str, &'static str, &'static str);

/// Runs the fed sequence; returns storage reads per (path, cf, op) and the
/// overlay calls of the two CFs per (cf, op), over all blocks.
fn run_fed_sequence() -> (BTreeMap<ReadKey, usize>, HashMap<(&'static str, &'static str), usize>, u64) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    setup(&db);
    let counts = Arc::new(Counts::default());
    let mut gen = Gen { rng: Lcg(0x5EED_0006), senders: SENDERS, markets: MARKETS, batch: 30, budget: 90, open: HashMap::new() };
    let mut walk = MarkWalk::new(MARKETS, WALK_BP);
    let mut books = HashMap::new();
    let mut next_id: u128 = 1;
    let mut parent: Option<Arc<FrozenPending>> = None;
    let mut per_path: BTreeMap<ReadKey, usize> = BTreeMap::new();
    let mut calls: HashMap<(&'static str, &'static str), usize> = HashMap::new();
    let mut fills = 0u64;
    for h in 1..=BLOCKS {
        let block = gen.block(ACTIONS);
        let overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let backend = CountingBackend::with_counts(overlay.clone(), counts.clone());
        let mut ctx = NativeExecContext::new(
            backend.clone(), h + 1, 1000 + h, 0, 1_000_000, 100, special(99), special(100), special(101),
        );
        ctx.order_books = std::mem::take(&mut books);
        ctx.next_global_order_id = next_id;
        walk.step();
        for m in 1..=MARKETS {
            for n in REPORTERS {
                ctx.oracle.submit_price(&special(n), m, walk.mark(base_mark(), m), ctx.block_height, ctx.timestamp).unwrap();
            }
        }
        backend.arm_storage_probe();
        let agg = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(agg.iter().all(|r| r.success), "mark aggregation: {agg:?}");
        NativeExecutor::execute_batch(&mut ctx, &block);
        let _ = NativeExecutor::run_liquidations(&mut ctx);
        backend.disarm_storage_probe();
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        fills += ctx.trade_index as u64;
        books = std::mem::take(&mut ctx.order_books);
        next_id = ctx.next_global_order_id;
        drop(ctx);
        for r in backend.take_storage_reads() {
            *per_path.entry((path_of(&r), r.cf, r.op)).or_insert(0) += 1;
        }
        for (k, v) in backend.take_layer_calls() {
            *calls.entry(k).or_insert(0) += v;
        }
        let frozen = overlay.freeze(h);
        if let Some(p) = parent.take() {
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
    }
    assert_memtable_only(&db);
    (per_path, calls, fills)
}

/// The instrument itself: a DB-resident balance read through the overlay is a
/// storage read; the same key from the parent layer or the own pending set is
/// not (the C3 / C4 asserts below can only pass for the right reason).
#[test]
fn storage_probe_sees_db_reads_and_not_layer_hits() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    setup(&db);
    let (a, b) = (sender(0), sender(1));
    db.put_cf_raw(CF_NATIVE_POSITIONS, &[sender(2).as_slice(), &[0u8; 8]].concat(), b"db").unwrap();
    let counts = Arc::new(Counts::default());

    let first = NativeStateOverlay::with_parent(db.clone(), None);
    first.put_cf_raw(CF_NATIVE_BALANCES, a.as_slice(), b"parent").unwrap();
    let parent = first.freeze(1);
    let overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent));
    overlay.put_cf_raw(CF_NATIVE_POSITIONS, &[b.as_slice(), &[0u8; 8]].concat(), b"own").unwrap();
    let probe = CountingBackend::with_counts(overlay, counts);
    probe.arm_storage_probe();

    let reads = |probe: &CountingBackend<NativeStateOverlay>| {
        probe.take_storage_reads().iter().map(|r| (r.cf, r.op)).collect::<Vec<_>>()
    };
    probe.get_cf_raw(CF_NATIVE_BALANCES, a.as_slice()).unwrap(); // parent layer
    probe.get_cf_raw(CF_NATIVE_POSITIONS, &[b.as_slice(), &[0u8; 8]].concat()).unwrap(); // own pending
    probe.prefix_exists(CF_NATIVE_POSITIONS, b.as_slice()).unwrap(); // own pending write under it
    assert_eq!(reads(&probe), vec![]);

    probe.get_cf_raw(CF_NATIVE_BALANCES, b.as_slice()).unwrap(); // DB row
    probe.get_cf_raw(CF_NATIVE_BALANCES, sender(SENDERS + 7).as_slice()).unwrap(); // absent: DB miss
    probe.iterate_cf(CF_NATIVE_POSITIONS, Some(b.as_slice())).unwrap(); // prefix scan merges the DB
    probe.iterate_cf_from(CF_NATIVE_BALANCES, &[0u8], 4).unwrap(); // seek
    probe.prefix_exists(CF_NATIVE_BALANCES, a.as_slice()).unwrap(); // a parent write under it: no DB walk
    probe.prefix_exists(CF_NATIVE_POSITIONS, sender(2).as_slice()).unwrap(); // DB walk
    let got = reads(&probe);
    assert_eq!(
        got,
        vec![
            (CF_NATIVE_BALANCES, "get_cf_raw"),
            (CF_NATIVE_BALANCES, "get_cf_raw"),
            (CF_NATIVE_POSITIONS, "iterate_cf"),
            (CF_NATIVE_BALANCES, "iterate_cf_from"),
            (CF_NATIVE_POSITIONS, "prefix_exists"),
        ],
    );
    let path = |p: &CountingBackend<NativeStateOverlay>| {
        p.get_cf_raw(CF_NATIVE_BALANCES, b.as_slice()).unwrap();
        p.take_storage_reads().iter().map(path_of).collect::<Vec<_>>()
    };
    assert_eq!(path(&probe), vec!["other"]);
    assert_memtable_only(&db);
}

#[test]
#[ignore = "item 6 Phase 1: un-ignored by steps C3/C4"]
fn fed_block_path_reads_positions_and_balances_from_memory_only() {
    let (per_path, calls, fills) = run_fed_sequence();
    println!("STORAGE_READS blocks={BLOCKS} senders={SENDERS} markets={MARKETS} fills={fills} walk_bp={WALK_BP}");
    for ((path, cf, op), n) in &per_path {
        println!("STORAGE_READS path={path} cf={cf} op={op} reads={n}");
    }
    let mut calls: Vec<_> = calls.into_iter().collect();
    calls.sort();
    for ((cf, op), n) in &calls {
        println!("OVERLAY_CALLS cf={cf} op={op} calls={n}");
    }
    assert!(fills > 0, "the sequence must trade");
    let mut bad = Vec::new();
    for path in ["margin", "match", "liquidation"] {
        for cf in [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES] {
            let n: usize = per_path.iter().filter(|((p, c, _), _)| *p == path && *c == cf).map(|(_, n)| n).sum();
            if n != 0 {
                bad.push(format!("{path}/{cf}={n}"));
            }
        }
    }
    assert!(bad.is_empty(), "storage reads on the in-memory paths: {}", bad.join(" "));
}
