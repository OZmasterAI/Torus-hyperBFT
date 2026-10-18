//! Item 6 Phase 5 step 3 (`docs/plans/item6-phase5-impl.md`): Classic book
//! rows from the level dirty set (`OrderBook::classic_row`) against the
//! reference save (`borsh::to_vec` of every dirty book). Two nodes, each on
//! its own DB with its own resident holder (the reference node's on the
//! test-only reference path), run the same random blocks; after every save
//! every CF is compared.

use super::*;
use alloy_primitives::U256;
use torus_core::position::NativeBalance;
use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

const MARKETS: [MarketId; 4] = [1, 2, 3, 4];
const TRADERS: u8 = 24;

fn order(m: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// How a block's actions run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    /// One `execute` per action (the single-action paths).
    Single,
    /// One `execute_batch` (the market workers, the batched cancel-all).
    Batch,
    /// `execute_batch` with the parallel cancel-all book pass forced
    /// (`TORUS_PARALLEL_CANCEL_ALL`: 4 jobs, no work gate).
    BatchParallelCancelAll,
}

struct Node {
    _dir: tempfile::TempDir,
    db: StateDb,
    holder: ResidentBooks,
    reference: bool,
    stats: BookRowsAccum,
    parallel_cancel_runs: u64,
}

impl Node {
    fn new(reference: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open db");
        let pm = PositionManager::new(db.clone());
        for s in 1..=TRADERS {
            let bal = NativeBalance {
                available: fp(10_000_000),
                order_margin: FixedPoint::ZERO,
            };
            pm.put_native_balance(&addr(s), &bal).unwrap();
        }
        let mut node = Self {
            _dir: dir,
            db,
            holder: ResidentBooks::default(),
            reference,
            stats: BookRowsAccum::default(),
            parallel_cancel_runs: 0,
        };
        node.reset_holder();
        node
    }

    /// A restart: the holder empty, the next block loads the books.
    fn reset_holder(&mut self) {
        self.holder = ResidentBooks::default();
        self.holder.set_reference_writes(self.reference);
    }

    fn ctx(&mut self, h: u64, mode: BookMode) -> NativeExecContext {
        let mut ctx = NativeExecContext::new_with_mode(
            self.db.clone(),
            h,
            1_000 + h,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
            mode,
            Some(&mut self.holder),
        );
        ctx.test_book_rows_check = true;
        ctx
    }

    fn run(&mut self, h: u64, mode: BookMode, how: How, actions: &[(Address, NativeAction)]) {
        let mut ctx = self.ctx(h, mode);
        match how {
            How::Single => {
                for (t, a) in actions {
                    NativeExecutor::execute(&mut ctx, t, a);
                }
            }
            How::Batch => {
                NativeExecutor::execute_batch(&mut ctx, actions);
            }
            How::BatchParallelCancelAll => {
                ctx.cancel_all_workers = 4;
                ctx.cancel_all_min_visits = 0;
                NativeExecutor::execute_batch(&mut ctx, actions);
            }
        }
        assert!(ctx.fatal_error.is_none(), "block {h}: {:?}", ctx.fatal_error);
        ctx.save_order_books();
        assert!(ctx.fatal_error.is_none(), "block {h} save: {:?}", ctx.fatal_error);
        self.stats.add(&ctx.book_rows_stats());
        self.parallel_cancel_runs += ctx.test_cancel_all_parallel_runs;
        ctx.stash_resident(&mut self.holder);
    }
}

/// `(owner, id)` of every resting order, markets and levels ascending.
fn resting(ctx: &NativeExecContext) -> Vec<(Address, u128, Option<u128>)> {
    let mut out = Vec::new();
    for m in MARKETS {
        if let Some(b) = ctx.order_books.get(&m) {
            for (_, q) in b.bid_queues().chain(b.ask_queues()) {
                out.extend(q.iter().map(|o| (o.trader, o.id, o.client_order_id)));
            }
        }
    }
    out
}

/// One block's actions from the mix: resting and crossing limits (GTC,
/// IOC, ALO), market orders, stop-limits and stop-markets, cloids, cancels
/// by id and by cloid, cancel-alls (all markets, one market; runs of
/// them), modifies (qty down in place, price moved, qty up), reduce-only
/// orders and transfers between runs.
fn actions(rng: &mut Lcg, resting: &[(Address, u128, Option<u128>)], cloid: &mut u128, h: u64) -> Vec<(Address, NativeAction)> {
    let mid = 100 + (h as i64 / 20) % 5 - 2;
    let mut out = Vec::new();
    for _ in 0..15 + rng.below(25) {
        let t = addr(1 + rng.below(TRADERS as u64) as u8);
        let m = MARKETS[rng.below(4) as usize];
        let is_buy = rng.below(2) == 0;
        let pick = (!resting.is_empty()).then(|| resting[rng.below(resting.len() as u64) as usize]);
        let action = match rng.below(20) {
            0..=5 => {
                let off = 1 + rng.below(8) as i64;
                let mut p = order(m, is_buy, if is_buy { mid - off } else { mid + off }, 1 + rng.below(4) as i64);
                if rng.below(4) == 0 {
                    p.client_order_id = Some(*cloid);
                    *cloid += 1;
                }
                if rng.below(8) == 0 {
                    p.time_in_force = TimeInForce::PostOnly;
                }
                p.reduce_only = rng.below(6) == 0;
                NativeAction::PlaceOrder(p)
            }
            6 | 7 => {
                let reach = rng.below(6) as i64;
                let mut p = order(m, is_buy, if is_buy { mid + reach } else { mid - reach }, 1 + rng.below(10) as i64);
                if rng.below(3) == 0 {
                    p.time_in_force = TimeInForce::IOC;
                }
                p.reduce_only = rng.below(5) == 0;
                NativeAction::PlaceOrder(p)
            }
            8 => NativeAction::PlaceOrder(PlaceOrderParams {
                order_type: OrderType::Market,
                ..order(m, is_buy, if is_buy { mid + 15 } else { mid - 15 }, 1 + rng.below(5) as i64)
            }),
            9 => {
                let trigger = if is_buy { mid + 1 + rng.below(3) as i64 } else { mid - 1 - rng.below(3) as i64 };
                let order_type = if rng.below(2) == 0 {
                    OrderType::StopLimit { trigger: fp(trigger), limit: fp(trigger) }
                } else {
                    OrderType::StopMarket { trigger: fp(trigger) }
                };
                let cap = if is_buy { trigger + 10 } else { trigger - 10 };
                NativeAction::PlaceOrder(PlaceOrderParams {
                    order_type,
                    ..order(m, is_buy, if matches!(order_type, OrderType::StopLimit { .. }) { trigger } else { cap }, 1 + rng.below(3) as i64)
                })
            }
            10 | 11 => match pick {
                Some((owner, id, _)) => {
                    out.push((owner, NativeAction::CancelOrder { order_id: id }));
                    continue;
                }
                None => NativeAction::CancelOrder { order_id: 1 },
            },
            12 => match pick {
                Some((owner, _, Some(c))) => {
                    out.push((owner, NativeAction::CancelByCloid { market_id: m, cloid: c }));
                    continue;
                }
                _ => NativeAction::CancelAllOrders { market_id: Some(m) },
            },
            13 | 14 => {
                // A run of cancel-alls (the batched / parallel book pass).
                for _ in 0..rng.below(4) {
                    let s = addr(1 + rng.below(TRADERS as u64) as u8);
                    out.push((s, NativeAction::CancelAllOrders { market_id: None }));
                }
                NativeAction::CancelAllOrders { market_id: (rng.below(2) == 0).then_some(m) }
            }
            15..=17 => match pick {
                Some((owner, id, _)) => {
                    let (new_price, new_qty) = match rng.below(3) {
                        0 => (None, Some(fp(1))),
                        1 => (Some(fp(mid + rng.below(5) as i64 - 2)), None),
                        _ => (None, Some(fp(6))),
                    };
                    out.push((owner, NativeAction::ModifyOrder { order_id: id, new_price, new_qty }));
                    continue;
                }
                None => NativeAction::CancelAllOrders { market_id: None },
            },
            18 => NativeAction::TransferToSpot { amount: U256::from(1_000u64) },
            _ => NativeAction::PlaceOrderBatch(vec![
                order(m, true, mid - 1 - rng.below(3) as i64, 1 + rng.below(3) as i64),
                order(MARKETS[(m as usize) % 4], false, mid + 1 + rng.below(3) as i64, 1 + rng.below(3) as i64),
            ]),
        };
        out.push((t, action));
    }
    out
}

fn dump(db: &StateDb) -> Vec<(&'static str, Vec<(Vec<u8>, Vec<u8>)>)> {
    torus_state::cf::ALL_CF_NAMES
        .iter()
        .map(|cf| (*cf, db.iterate_cf(cf, None).expect("iterate")))
        .collect()
}

/// The two nodes through `blocks` random blocks in `mode`; a restart (both
/// holders emptied: the books load from the DB) every 50 blocks. Returns
/// the typed and the reference node's save stats and the parallel
/// cancel-all runs.
fn run(mode: BookMode, seed: u64, blocks: u64) -> (BookRowsAccum, BookRowsAccum, u64) {
    let mut rng = Lcg(seed);
    let mut a = Node::new(true);
    let mut b = Node::new(false);
    let mut cloid = 1u128;
    for h in 1..=blocks {
        if h % 50 == 0 {
            a.reset_holder();
            b.reset_holder();
        }
        let how = match h % 3 {
            0 => How::Single,
            1 => How::Batch,
            _ => How::BatchParallelCancelAll,
        };
        // The actions from the reference node's books at the block start
        // (a view loaded from its DB, so the holders are left alone).
        let view = NativeExecContext::new_with_mode(
            a.db.clone(), h, 1_000 + h, 0, 100, 10, addr(99), addr(100), addr(101), mode, None,
        );
        let rest = resting(&view);
        drop(view);
        let acts = actions(&mut rng, &rest, &mut cloid, h);
        a.run(h, mode, how, &acts);
        b.run(h, mode, how, &acts);
        for ((cf, x), (_, y)) in dump(&a.db).iter().zip(&dump(&b.db)) {
            assert!(x == y, "{mode:?} seed {seed} block {h} ({how:?}): {cf} differs ({} vs {} rows)", x.len(), y.len());
        }
    }
    (b.stats, a.stats, a.parallel_cancel_runs + b.parallel_cancel_runs)
}

/// Plan step 3 `book_dirty_rows_equal_save_order_books`: random places,
/// cancels (by id, by cloid), cancel-alls (single, batched runs, the
/// parallel book pass), fills, stops, modifies and reduce-only orders over
/// every block, single-action and batched (market workers): the Classic
/// rows from the level dirty set equal the reference save's, every CF
/// equal after every block, with restarts (full builds) in between.
#[test]
fn book_dirty_rows_equal_save_order_books() {
    let mut typed = BookRowsAccum::default();
    let mut parallel = 0;
    for seed in [1u64, 2, 3] {
        let (t, r, p) = run(BookMode::Classic, seed, 150);
        assert_eq!(r, BookRowsAccum::default(), "the reference node never takes the cached path");
        typed.add(&t);
        parallel += p;
    }
    println!("P5 step 3 Classic rows: {typed:?}, parallel cancel-all runs {parallel}");
    assert!(typed.saves > 1_000, "{typed:?}");
    assert!(typed.full_builds >= 3 * 4, "the first block and every restart build: {typed:?}");
    assert!(typed.levels_reused > 1_000 && typed.levels_encoded > 1_000, "{typed:?}");
    assert!(typed.levels_removed > 100 && typed.checked == typed.saves, "{typed:?}");
    assert!(parallel > 20, "the parallel cancel-all book pass ran: {parallel}");
}

/// Modes 1-3 keep their saves: no node takes the cached path, and both
/// nodes write the same rows.
#[test]
fn book_rows_modes_1_to_3_unchanged() {
    for mode in [BookMode::OrderRows, BookMode::LevelAuthority, BookMode::LevelAuthorityChunked] {
        let (t, r, _) = run(mode, 7, 60);
        assert_eq!(t, BookRowsAccum::default(), "{mode:?}");
        assert_eq!(r, BookRowsAccum::default(), "{mode:?}");
    }
}

/// Without resident books (`TORUS_RESIDENT_BOOKS=0`, every block reloads
/// its books) the Classic save stays the reference encoding.
#[test]
fn classic_rows_need_resident_books() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    PositionManager::new(db.clone())
        .put_native_balance(&addr(1), &NativeBalance { available: fp(1_000_000), order_margin: FixedPoint::ZERO })
        .unwrap();
    for h in 1..=3 {
        let mut ctx = NativeExecContext::new_with_mode(
            db.clone(),
            h,
            1_000 + h,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
            BookMode::Classic,
            None,
        );
        let r = NativeExecutor::execute(&mut ctx, &addr(1), &NativeAction::PlaceOrder(order(1, true, 90 + h as i64, 1)));
        assert!(r.success, "{r:?}");
        ctx.save_order_books();
        assert_eq!(ctx.book_rows_stats(), BookRowsAccum::default(), "block {h}");
    }
    let mut holder = ResidentBooks::default();
    let mut ctx = NativeExecContext::new_with_mode(
        db.clone(), 4, 1_004, 0, 100, 10, addr(99), addr(100), addr(101), BookMode::Classic, Some(&mut holder),
    );
    NativeExecutor::execute(&mut ctx, &addr(1), &NativeAction::PlaceOrder(order(1, true, 80, 1)));
    ctx.save_order_books();
    let s = ctx.book_rows_stats();
    assert!(s.saves == 1 && s.full_builds == 1, "a resident node takes it: {s:?}");
}
