//! Item 6 Phase 1, step 0.2: the margin, match and liquidation paths read
//! `CF_NATIVE_POSITIONS` / `CF_NATIVE_BALANCES` from memory only — no RocksDB
//! read once the resident rows (C1) and the sums cache / L1 (C3 / C4) are in.
//!
//! A fed 20-block econ sequence (marks usable, moving 10 bp per block) runs
//! through the ubench's block path: each block's `NativeStateOverlay` layered
//! over the previous block's frozen set, which is flushed one block later,
//! with the resident rows R attached (`begin_resident` / `end_resident`, as
//! app.rs does). Review log 1: R answers every read of both CFs, so the DB
//! reads of both CFs are 0 from C1 on (all paths).
//! The context's backend wraps that overlay in the counting backend, whose
//! storage probe records every read of the two CFs that reached RocksDB (no
//! layer answered it) with its stack; the stack names the engine path.
//!
//! C2: the same sequence counts the oracle point reads per block: the block
//! mark table reads each market's mark once (end of `begin_block_oracle`)
//! and answers every later mark read of the block.
//!
//!   cargo test -p torus-bridge --test storage_reads_tests -- --nocapture

#[path = "common/counting_backend.rs"]
mod counting_backend;
#[path = "common/econ_load.rs"]
mod econ_load;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use counting_backend::{CountingBackend, Counts, StorageRead};
use econ_load::{base_mark, feed_setup, sender, special, Gen, Lcg, MarkWalk, REPORTERS};
use torus_bridge::native_executor::{begin_resident, end_resident, NativeExecContext, NativeExecutor, ResidentBooks};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_CONSENSUS_META, CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS, META_NATIVE_APPLIED_HEIGHT};
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

/// Storage reads per (path, cf, op) and overlay calls of the two CFs per
/// (cf, op) over all blocks, the fills, the R builds and the oracle point
/// reads of each block (oracle step through liquidation).
type FedRun = (BTreeMap<ReadKey, usize>, HashMap<(&'static str, &'static str), usize>, u64, u64, Vec<usize>);

/// Runs the fed sequence.
fn run_fed_sequence() -> FedRun {
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
    let mut holder = ResidentBooks::default();
    let mut oracle_reads = Vec::new();
    for h in 1..=BLOCKS {
        let block = gen.block(ACTIONS);
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut resident = begin_resident(Some(&mut holder), &mut overlay, h, None);
        let backend = CountingBackend::with_counts(overlay.clone(), counts.clone());
        let mut ctx = NativeExecContext::new(
            backend.clone(), h + 1, 1000 + h, 0, 1_000_000, 100, special(99), special(100), special(101),
        );
        ctx.attach_resident_block(&mut resident);
        ctx.order_books = std::mem::take(&mut books);
        ctx.next_global_order_id = next_id;
        walk.step();
        for m in 1..=MARKETS {
            for n in REPORTERS {
                ctx.oracle.submit_price(&special(n), m, walk.mark(base_mark(), m), ctx.block_height, ctx.timestamp).unwrap();
            }
        }
        backend.arm_storage_probe();
        backend.arm();
        let agg = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(agg.iter().all(|r| r.success), "mark aggregation: {agg:?}");
        NativeExecutor::execute_batch(&mut ctx, &block);
        let _ = NativeExecutor::run_liquidations(&mut ctx);
        backend.disarm();
        backend.disarm_storage_probe();
        oracle_reads.push(backend.oracle_reads());
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        fills += ctx.trade_index as u64;
        books = std::mem::take(&mut ctx.order_books);
        next_id = ctx.next_global_order_id;
        ctx.detach_resident_block(&mut resident);
        drop(ctx);
        for r in backend.take_storage_reads() {
            *per_path.entry((path_of(&r), r.cf, r.op)).or_insert(0) += 1;
        }
        for (k, v) in backend.take_layer_calls() {
            *calls.entry(k).or_insert(0) += v;
        }
        drop(backend);
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = overlay.own_pending_delta();
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, resident, &mut overlay, delta, true, None);
        if let Some(p) = parent.take() {
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
    }
    assert_eq!(holder.rows_shared_fallbacks(), 0);
    assert_eq!(holder.rows_height(), Some(BLOCKS));
    assert_memtable_only(&db);
    (per_path, calls, fills, holder.rows_builds(), oracle_reads)
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
fn fed_block_path_reads_positions_and_balances_from_memory_only() {
    let (per_path, calls, fills, builds, _) = run_fed_sequence();
    println!("STORAGE_READS blocks={BLOCKS} senders={SENDERS} markets={MARKETS} fills={fills} walk_bp={WALK_BP} r_builds={builds}");
    for ((path, cf, op), n) in &per_path {
        println!("STORAGE_READS path={path} cf={cf} op={op} reads={n}");
    }
    let mut calls: Vec<_> = calls.into_iter().collect();
    calls.sort();
    for ((cf, op), n) in &calls {
        println!("OVERLAY_CALLS cf={cf} op={op} calls={n}");
    }
    assert!(fills > 0, "the sequence must trade");
    assert_eq!(builds, 1, "R built once, then carried block to block");
    let calls_total: usize = calls.iter().map(|(_, n)| n).sum();
    assert!(calls_total > 1000, "non-vacuous: {calls_total} reads of the two CFs");
    let bad: Vec<String> = per_path.iter().map(|((p, c, o), n)| format!("{p}/{c}/{o}={n}")).collect();
    assert!(bad.is_empty(), "storage reads of R's CFs (any path): {}", bad.join(" "));
}

/// C2 (plan Step 2, Gate 2a): the block reads each market's mark ONCE — the
/// table fill at the end of `begin_block_oracle` (one point read of the
/// aggregate row per market; a fed block's aggregation itself only scans the
/// submissions) — and every later mark read of the block (Phases 2-3, the
/// liquidation step) is answered by the table. Before C2: the per-batch memo
/// plus the liquidation step read the rows again.
#[test]
fn fed_block_reads_each_mark_once() {
    let (_, _, fills, _, oracle_reads) = run_fed_sequence();
    println!("ORACLE_READS markets={MARKETS} per_block={oracle_reads:?}");
    assert!(fills > 0, "the sequence must trade");
    assert_eq!(oracle_reads, vec![MARKETS as usize; BLOCKS as usize], "oracle point reads per block");
}
