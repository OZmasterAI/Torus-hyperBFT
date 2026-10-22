//! s80 oracle-step-memo (`TORUS_S80_ORACLE_MEMO`): the block-start oracle
//! step with the per-second memo == the full step, block by block, through
//! the pipelined lifecycle with the resident rows R (`end_resident` inline
//! and on its worker). Per block: the step's results, every oracle row after
//! the step, each market's `get_price`, the mark table and whether its
//! version moved; at the end every oracle row in the DB.
//!
//! The scenario covers what may invalidate the memo: the header second
//! advancing (often several blocks per second, sometimes a jump past the
//! submission window or the aggregate max age), feed submissions after the
//! step (as actions) and before it (the block's own layer), pauses (rows
//! pruned, aggregates stale), out-of-range prices, an undecodable
//! submission row, a deleted or rewritten aggregate row (before and after
//! the step), signer-index rows (not an input), a validator jailed /
//! re-activated (stakes), a market listed (listed set), a book for an
//! unlisted market (mark table market set) and a margin-config market.
//! The memo must hit on most blocks and miss on each kind of change.

#[path = "common/econ_load.rs"]
#[allow(dead_code)]
mod econ_load;

use std::sync::Arc;

use alloy_primitives::{Address, U256};
use torus_bridge::native_executor::{
    begin_resident, end_resident, end_resident_on_worker, NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_core::order_book::OrderBook;
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_state::cf::{oracle_signer_key, CF_CONSENSUS_META, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, META_NATIVE_APPLIED_HEIGHT};
use torus_state::{FrozenPending, NativeStateOverlay, ResidentRows, StateBackend, StateDb};
use torus_types::FixedPoint;

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

const MARKETS: u64 = 8;
const REPORTERS: [u8; 4] = [150, 151, 152, 153];
const BLOCKS: u64 = 400;

fn reporter(n: u8) -> Address {
    Address::new([n; 20])
}

fn validator(n: u8, status: ValidatorStatus) -> ValidatorState {
    ValidatorState {
        address: reporter(n),
        pubkey: [n; 32],
        commission_bps: 0,
        self_stake: MIN_SELF_DELEGATION,
        total_delegated: U256::ZERO,
        status,
        jailed_until: None,
        last_commission_change_block: None,
        oracle_signer: None,
    }
}

fn setup(db: &StateDb) {
    for n in REPORTERS {
        let status = if n == 153 { ValidatorStatus::Jailed } else { ValidatorStatus::Active };
        StakingManager::new(db.clone()).put_validator(&reporter(n), &validator(n, status)).unwrap();
    }
    // Markets 1..=MARKETS - 1 listed (odd ids with a real row: a margin
    // config); MARKETS is listed mid-run.
    for m in 1..MARKETS {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &econ_load::market_row(m % 2 == 1)).unwrap();
    }
}

#[derive(Default, Debug)]
struct Counts {
    hits: usize,
    misses: usize,
}

/// Feed `markets` (a random subset of reporters each) at `ts`.
fn feed<T: StateBackend>(ctx: &NativeExecContext<T>, rng: &mut Lcg, h: u64, ts: u64) {
    for m in 1..=MARKETS {
        if rng.below(4) == 0 {
            continue;
        }
        for n in REPORTERS {
            if rng.below(6) == 0 {
                continue;
            }
            let units = match rng.below(40) {
                0 => -5,
                1 => 0,
                _ => 1_000 + rng.below(40) as i128 - 20,
            };
            ctx.oracle.submit_price(&reporter(n), m, FixedPoint::from_raw(units * FixedPoint::SCALE), h, ts).unwrap();
        }
    }
}

/// One run; `memo`: the memo on; `worker`: `end_resident` on its worker.
fn run(seed: u64, memo: bool, worker: bool, counts: &mut Counts) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    setup(&db);
    let mut rng = Lcg(seed);
    let mut holder = ResidentBooks::default();
    let mut parent: Option<Arc<FrozenPending>> = None;
    let mut books = alloy_primitives::map::HashMap::default();
    let mut out = Vec::new();
    let mut paused_until = 0u64;
    let mut ts = 1_000u64;
    let mut prev_version = None;
    for h in 1..=BLOCKS {
        // ~4 blocks per second; now and then a jump past the window (10 s)
        // or the max age (60 s).
        match rng.below(100) {
            0..=1 => ts += 11 + rng.below(60),
            2..=25 => ts += 1,
            _ => {}
        }
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rb = begin_resident(Some(&mut holder), &mut overlay, h, None);
        assert!(rb.attached());
        let mut ctx = NativeExecContext::new(
            overlay.clone(),
            h,
            ts,
            0,
            1_000,
            10,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
        );
        ctx.oracle_memo_on = memo;
        ctx.attach_resident_block(&mut rb);
        ctx.set_order_books(std::mem::take(&mut books));
        if h >= paused_until && rng.below(40) == 0 {
            paused_until = h + 1 + rng.below(80);
        }
        let feeding = h >= paused_until && rng.below(8) == 0;
        let feed_before = rng.below(4) == 0;
        let mutation = rng.below(40);
        // Before the step (the block's own layer).
        if feeding && feed_before {
            feed(&ctx, &mut rng, h, ts);
        }
        match mutation {
            0 => {
                let key = [b"agg".as_slice(), &(1 + rng.below(MARKETS)).to_be_bytes()].concat();
                ctx.state.delete_cf_raw(CF_NATIVE_ORACLE, &key).unwrap();
            }
            1 => {
                let signer = Address::new([0x60 + rng.below(4) as u8; 20]);
                ctx.state.put_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&signer), reporter(150).as_slice()).unwrap();
            }
            _ => {}
        }
        let results = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        match ctx.oracle_memo_hit() {
            Some(true) => counts.hits += 1,
            Some(false) => counts.misses += 1,
            None => assert!(!memo),
        }
        let prices: Vec<String> = (1..=MARKETS + 2).map(|m| format!("{:?}", ctx.oracle.get_price(m, ts))).collect();
        let rows = ctx.state.iterate_cf(CF_NATIVE_ORACLE, None).unwrap();
        let version = ctx.mark_version();
        let moved = version != prev_version;
        prev_version = version;
        out.push(format!(
            "h {h} ts {ts} | {:?} | {rows:?} | {prices:?} | table {:?} | version moved {moved}",
            results.iter().map(|r| (r.action_type, r.success, &r.error, r.gas_used)).collect::<Vec<_>>(),
            ctx.block_mark_table(),
        ));
        // After the step (as the block's actions).
        if feeding && !feed_before {
            feed(&ctx, &mut rng, h, ts);
        }
        match mutation {
            2 => {
                let key = [b"sub".as_slice(), &(1 + rng.below(MARKETS)).to_be_bytes(), &[0xAB; 20]].concat();
                ctx.state.put_cf_raw(CF_NATIVE_ORACLE, &key, &[1, 2, 3]).unwrap();
            }
            3 => {
                let key = [b"agg".as_slice(), &(1 + rng.below(MARKETS)).to_be_bytes()].concat();
                ctx.state.delete_cf_raw(CF_NATIVE_ORACLE, &key).unwrap();
            }
            4 => {
                // An aggregate row rewritten with an older stamp (not an
                // input the step would write).
                let m = 1 + rng.below(MARKETS);
                let key = [b"agg".as_slice(), &m.to_be_bytes()].concat();
                if let Some(mut v) = ctx.state.get_cf_raw(CF_NATIVE_ORACLE, &key).unwrap() {
                    v[28..36].copy_from_slice(&(ts - 30).to_be_bytes());
                    ctx.state.put_cf_raw(CF_NATIVE_ORACLE, &key, &v).unwrap();
                }
            }
            5 => {
                let signer = Address::new([0x60 + rng.below(4) as u8; 20]);
                ctx.state.delete_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&signer)).unwrap();
            }
            6 => {
                let n = REPORTERS[rng.below(4) as usize];
                let status = if rng.below(2) == 0 { ValidatorStatus::Jailed } else { ValidatorStatus::Active };
                ctx.staking.put_validator(&reporter(n), &validator(n, status)).unwrap();
            }
            7 => {
                ctx.state.put_cf_raw(CF_NATIVE_MARKETS, &MARKETS.to_be_bytes(), b"listed").unwrap();
            }
            8 => {
                // A book for an unlisted market (the table's market set).
                let m = MARKETS + 1 + rng.below(2);
                ctx.insert_order_book(m, OrderBook::new(m, FixedPoint::ONE, FixedPoint::ONE));
            }
            _ => {}
        }
        books = ctx.take_order_books();
        ctx.detach_resident_block(&mut rb);
        drop(ctx);
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let frozen = overlay.freeze(h);
        if worker {
            let delta = torus_bridge::native_executor::BlockDelta::Frozen(frozen.clone());
            end_resident_on_worker(&mut holder, rb, &mut overlay, delta, true, None);
        } else {
            let delta = frozen.resident_delta();
            end_resident(&mut holder, rb, &mut overlay, delta, true, None);
        }
        if let Some(p) = parent.take() {
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
        if h % 50 == 0 {
            let cold = ResidentRows::build(&NativeStateOverlay::with_parent(db.clone(), parent.clone())).unwrap();
            assert_eq!(holder.rows(), Some(&cold), "seed {seed} memo {memo} block {h}: warm R != cold build");
        }
    }
    if let Some(p) = parent.take() {
        p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
    }
    assert_eq!(holder.rows_builds(), 1, "R built once");
    out.push(format!("final {:?}", db.iterate_cf(CF_NATIVE_ORACLE, None).unwrap()));
    out
}

#[test]
fn oracle_memo_on_equals_the_full_step() {
    let mut total = Counts::default();
    for seed in 1..=4u64 {
        let seed = seed * 0x5_80_0E3;
        let mut off_counts = Counts::default();
        let off = run(seed, false, false, &mut off_counts);
        assert_eq!((off_counts.hits, off_counts.misses), (0, 0));
        for worker in [false, true] {
            let mut c = Counts::default();
            let on = run(seed, true, worker, &mut c);
            assert_eq!(on.len(), off.len());
            for (i, (a, b)) in on.iter().zip(off.iter()).enumerate() {
                assert_eq!(a, b, "seed {seed} worker {worker} block {}", i + 1);
            }
            assert_eq!(c.hits + c.misses, BLOCKS as usize);
            total.hits += c.hits;
            total.misses += c.misses;
        }
    }
    println!("ORACLE_MEMO hits={} misses={}", total.hits, total.misses);
    // Most blocks hit; every kind of change misses.
    assert!(total.hits > total.misses, "{total:?}");
    assert!(total.misses > 100, "{total:?}");
}

/// A steady feed (all markets, all Active reporters, every 15th block, after
/// the step) with the second advancing every 22 blocks: the memo misses only
/// on the block after a feed block (and the first block).
#[test]
fn oracle_memo_hit_rate_on_a_steady_feed() {
    let mut outs = Vec::new();
    for memo in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        setup(&db);
        let mut holder = ResidentBooks::default();
        let mut parent: Option<Arc<FrozenPending>> = None;
        let mut counts = Counts::default();
        let mut out = Vec::new();
        for h in 1..=200u64 {
            let ts = 1_000 + h / 22;
            let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
            let mut rb = begin_resident(Some(&mut holder), &mut overlay, h, None);
            let mut ctx = NativeExecContext::new(
                overlay.clone(),
                h,
                ts,
                0,
                1_000,
                10,
                Address::ZERO,
                Address::ZERO,
                Address::ZERO,
            );
            ctx.oracle_memo_on = memo;
            ctx.attach_resident_block(&mut rb);
            let results = NativeExecutor::begin_block_oracle(&mut ctx);
            match ctx.oracle_memo_hit() {
                Some(true) => counts.hits += 1,
                Some(false) => counts.misses += 1,
                None => {}
            }
            out.push(format!(
                "{results:?} {:?} {:?}",
                ctx.state.iterate_cf(CF_NATIVE_ORACLE, None).unwrap(),
                ctx.block_mark_table()
            ));
            if h % 15 == 1 {
                for m in 1..MARKETS {
                    for n in [150u8, 151, 152] {
                        ctx.oracle.submit_price(&reporter(n), m, FixedPoint::from_raw(1_000 * FixedPoint::SCALE), h, ts).unwrap();
                    }
                }
            }
            ctx.detach_resident_block(&mut rb);
            drop(ctx);
            overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
            let frozen = overlay.freeze(h);
            end_resident(&mut holder, rb, &mut overlay, frozen.resident_delta(), true, None);
            if let Some(p) = parent.take() {
                p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
            }
            parent = Some(frozen);
        }
        if memo {
            // Misses: block 1, and the block after each feed block (h % 15 == 2).
            let feeds = (1..=200u64).filter(|h| h % 15 == 2).count();
            assert_eq!(counts.misses, 1 + feeds, "{counts:?}");
            assert_eq!(counts.hits, 200 - 1 - feeds, "{counts:?}");
        }
        outs.push(out);
    }
    assert_eq!(outs[0], outs[1], "memo on == memo off, every block");
}
