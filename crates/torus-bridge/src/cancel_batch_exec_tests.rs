//! s63 `TORUS_CANCEL_BATCH` executor differential: the batched Phase-1
//! cancel-all runs must produce byte-identical results, gas, persisted state
//! and native state root to the per-action loop.

use super::*;
use crate::state_root::compute_native_state_root;
use alloy_primitives::U256;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
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

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

fn cancel_all(market_id: Option<MarketId>) -> NativeAction {
    NativeAction::CancelAllOrders { market_id }
}

const MARKETS: [MarketId; 4] = [1, 2, 3, 4];
const SENDERS: u8 = 40;

/// Resting depth like the s63 profile: two deep levels per side per market,
/// senders' orders interleaved through each FIFO queue.
fn setup_blocks() -> Vec<Vec<(Address, NativeAction)>> {
    let mut block = Vec::new();
    for round in 0..12i64 {
        for s in 1..=SENDERS {
            for &m in &MARKETS {
                let is_buy = (s as u64 + m) % 2 == 0;
                let price = if is_buy {
                    99 + round % 2
                } else {
                    110 + round % 2
                };
                block.push((addr(s), gtc(m, is_buy, price, 1 + (round % 3))));
            }
        }
    }
    vec![block]
}

/// Mixed blocks: runs of cancel-alls (None / Some(m) / unknown market /
/// duplicates / orderless senders) interleaved with places (which must not
/// break a run) and broken by CancelOrder, ModifyOrder and a transfer.
fn mixed_blocks(seed: u64, resting: &[(Address, u128)]) -> Vec<Vec<(Address, NativeAction)>> {
    let mut rng = Lcg(seed);
    let mut blocks = Vec::new();
    for _ in 0..3 {
        let mut block = Vec::new();
        for _ in 0..160 {
            let mut s = addr(1 + rng.below(SENDERS as u64 + 4) as u8);
            let m = MARKETS[rng.below(4) as usize];
            // CancelOrder always, ModifyOrder half the time, targets a real
            // setup (owner, id) pair — which misses once an earlier cancel
            // removed it; the rest are random ids (not found / wrong owner).
            let (owner, id) = resting[rng.below(resting.len() as u64) as usize];
            let owned = rng.below(2) == 0;
            let order_id = if owned {
                id
            } else {
                1 + rng.below(2_500) as u128
            };
            let action = match rng.below(20) {
                0..=3 => cancel_all(None),
                4 => cancel_all(Some(m)),
                5 => cancel_all(Some(77)),
                6 => {
                    s = owner;
                    NativeAction::CancelOrder { order_id: id }
                }
                7 => {
                    if owned {
                        s = owner;
                    }
                    NativeAction::ModifyOrder {
                        order_id,
                        new_price: None,
                        new_qty: Some(fp(1)),
                    }
                }
                8 if rng.below(4) == 0 => NativeAction::TransferToSpot {
                    amount: U256::from(1_000u64),
                },
                // Crossing places: fills against the deep levels.
                9 => {
                    let is_buy = rng.below(2) == 0;
                    gtc(m, is_buy, if is_buy { 111 } else { 99 }, 2)
                }
                _ => {
                    let is_buy = rng.below(2) == 0;
                    let price = if is_buy {
                        99 + rng.below(2)
                    } else {
                        110 + rng.below(2)
                    };
                    gtc(m, is_buy, price as i64, 1 + rng.below(3) as i64)
                }
            };
            block.push((s, action));
            // Occasionally repeat the same sender's cancel-all right away.
            if rng.below(15) == 0 {
                block.push((s, cancel_all(None)));
            }
        }
        blocks.push(block);
    }
    blocks
}

#[derive(PartialEq, Eq, Debug)]
struct RunFingerprint {
    cf_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    results: Vec<Vec<(&'static str, bool, Option<String>, u64)>>,
    total_gas: Vec<u64>,
    dirty: Vec<Vec<MarketId>>,
    books: Vec<(MarketId, Vec<u8>)>,
    trade_index: u32,
    next_global_order_id: u128,
    state_root: B256,
}

fn new_ctx(dir: &tempfile::TempDir, mode: BookMode) -> NativeExecContext {
    let db = StateDb::open(dir.path()).expect("open db");
    let ctx = NativeExecContext::new_with_mode(
        db,
        1,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        None,
    );
    for s in 1..=SENDERS + 4 {
        let bal = NativeBalance {
            available: fp(10_000_000),
            order_margin: FixedPoint::ZERO,
        };
        ctx.positions.put_native_balance(&addr(s), &bal).unwrap();
    }
    ctx
}

/// `(owner, id)` of every order resting after the setup block.
fn resting_after_setup() -> Vec<(Address, u128)> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut ctx = new_ctx(&dir, BookMode::Classic);
    NativeExecutor::execute_batch_cancel_mode(&mut ctx, &setup_blocks()[0], false);
    let mut out = Vec::new();
    for id in 0..ctx.next_global_order_id {
        for m in MARKETS {
            if let Some(order) = ctx.order_books.get(&m).and_then(|b| b.get_order(id)) {
                out.push((order.trader, id));
            }
        }
    }
    assert!(out.len() > 1_000);
    out
}

fn run(blocks: &[Vec<(Address, NativeAction)>], mode: BookMode, batch: bool) -> RunFingerprint {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut ctx = new_ctx(&dir, mode);
    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    let mut dirty = Vec::new();
    for block in blocks {
        let r = NativeExecutor::execute_batch_cancel_mode(&mut ctx, block, batch);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        results.push(
            r.results
                .iter()
                .map(|a| (a.action_type, a.success, a.error.clone(), a.gas_used))
                .collect(),
        );
        total_gas.push(r.total_gas);
        let mut marked: Vec<_> = ctx.dirty_books.iter().copied().collect();
        marked.sort_unstable();
        dirty.push(marked);
        ctx.save_order_books();
        // Production builds a fresh context (empty dirty set) per block.
        ctx.dirty_books.clear();
    }
    let mut books: Vec<_> = ctx
        .order_books
        .iter()
        .map(|(&m, b)| (m, borsh::to_vec(b).unwrap()))
        .collect();
    books.sort_unstable();
    let mut cf_dump = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_MARKETS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
        CF_BOOK_ORDER_ROWS,
    ] {
        for (k, v) in ctx.state.iterate_cf(cf, None).expect("iterate cf") {
            cf_dump.push((cf.to_string(), k, v));
        }
    }
    RunFingerprint {
        cf_dump,
        results,
        total_gas,
        dirty,
        books,
        trade_index: ctx.trade_index,
        next_global_order_id: ctx.next_global_order_id,
        state_root: compute_native_state_root(&ctx.state).expect("state root"),
    }
}

#[test]
fn cancel_batch_flag_on_matches_off_mixed_blocks() {
    let resting = resting_after_setup();
    for (seed, mode) in [
        (1u64, BookMode::Classic),
        (2, BookMode::LevelAuthority),
        (3, BookMode::LevelAuthorityChunked),
        (4, BookMode::Classic),
    ] {
        let mut blocks = setup_blocks();
        blocks.extend(mixed_blocks(seed, &resting));
        let off = run(&blocks, mode, false);
        let on = run(&blocks, mode, true);
        // Sanity: the scenario cancels real depth, fills, and has both
        // successful and failing run-breaking actions.
        let flat: Vec<_> = off.results.iter().flatten().collect();
        assert!(flat.iter().filter(|r| r.0 == "cancel_all").count() > 60);
        assert!(flat.iter().any(|r| r.0 == "cancel_order" && r.1));
        assert!(flat.iter().any(|r| r.0 == "cancel_order" && !r.1));
        assert!(flat.iter().any(|r| r.0 == "modify_order" && r.1));
        let trades = off
            .cf_dump
            .iter()
            .filter(|(cf, _, _)| cf == CF_NATIVE_TRADES)
            .count();
        assert!(trades > 10, "scenario must fill: {trades} trades");
        assert_eq!(off, on, "TORUS_CANCEL_BATCH diverged (seed {seed})");
    }
}

/// One long run: every sender cancels everything, twice, with places mixed
/// in — books empty out, then the run's places rest on fresh levels.
#[test]
fn cancel_batch_flag_on_matches_off_long_run_emptying_books() {
    let mut blocks = setup_blocks();
    let mut block = Vec::new();
    for s in (1..=SENDERS + 2).rev() {
        block.push((addr(s), cancel_all(None)));
        if s % 7 == 0 {
            block.push((addr(s), gtc(2, true, 98, 1)));
        }
    }
    for s in 1..=SENDERS {
        block.push((addr(s), cancel_all(Some(MARKETS[s as usize % 4]))));
    }
    blocks.push(block);
    for mode in [BookMode::Classic, BookMode::LevelAuthorityChunked] {
        let off = run(&blocks, mode, false);
        let on = run(&blocks, mode, true);
        assert_eq!(off, on);
    }
}
