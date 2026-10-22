//! perf/cloid-engine-cost: a pinned seeded cloid stream through the
//! executor. The engine-cost changes (pre-sized claimed set, one-pass log
//! drain, the book's re-check skipped behind the executor's, a compact
//! index key) must not change one result, so this drives blocks of random
//! cloid traffic (duplicates in the book, across books, inside one call and
//! inside one `PlaceOrderBatch`; fills, IOC takers, stops and triggers,
//! cancels by id / cloid, modifies, cancel-alls, reuse after a free) through
//! every executor path (batch, cancel-batch mode, sharded engine, two calls
//! per block, the single path) and folds every result, the cloid index
//! after every block and the final state root into one digest per book
//! mode, pinned on the pre-change code (c88d3073). Resident books, a
//! non-resident run and a restart must give the same digest, and the carried
//! index must equal a rebuild after every block.

use super::*;
use crate::state_root::compute_native_state_root;
use std::fmt::Write as _;
use torus_core::position::NativeBalance;
use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

/// Above u64::MAX.
const C: u128 = 0xc10d_0000_0000_0000_0000_0000_0000_0001;
const MARKETS: u64 = 5;
const TRADERS: u8 = 4;
const CLOIDS: u64 = 6;
const BLOCKS: usize = 14;
const ACTIONS: usize = 36;

const MODES: [BookMode; 4] = [
    BookMode::Classic,
    BookMode::OrderRows,
    BookMode::LevelAuthority,
    BookMode::LevelAuthorityChunked,
];

/// splitmix64: a fixed stream, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn params(
    market_id: MarketId,
    is_buy: bool,
    price: i64,
    qty: i64,
    ot: OrderType,
    tif: TimeInForce,
    cloid: Option<u128>,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: ot,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: cloid,
    }
}

fn ctx_at(
    db: &StateDb,
    height: u64,
    mode: BookMode,
    holder: Option<&mut ResidentBooks>,
) -> NativeExecContext {
    let ctx = NativeExecContext::new_with_mode(
        db.clone(),
        height,
        1000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        holder,
    );
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    ctx
}

/// One random placement (cloid from a small pool: duplicates are common).
fn random_place(rng: &mut Rng) -> PlaceOrderParams {
    let m = 1 + rng.below(MARKETS);
    let cloid = (rng.below(5) != 0).then(|| C + rng.below(CLOIDS) as u128);
    let is_buy = rng.below(2) == 0;
    match rng.below(10) {
        // An IOC taker through the book (never rests).
        0 | 1 => params(
            m,
            is_buy,
            if is_buy { 106 } else { 94 },
            rng.pick(1, 4),
            OrderType::Limit,
            TimeInForce::IOC,
            cloid,
        ),
        // A stop near the market (some fire).
        2 => {
            let trigger = if is_buy {
                rng.pick(101, 104)
            } else {
                rng.pick(96, 99)
            };
            params(
                m,
                is_buy,
                if is_buy { trigger + 5 } else { trigger - 5 },
                1,
                OrderType::StopMarket {
                    trigger: fp(trigger),
                },
                TimeInForce::GTC,
                cloid,
            )
        }
        // A resting or crossing limit.
        _ => {
            let px = if is_buy {
                rng.pick(96, 101)
            } else {
                rng.pick(99, 104)
            };
            params(
                m,
                is_buy,
                px,
                rng.pick(1, 3),
                OrderType::Limit,
                TimeInForce::GTC,
                cloid,
            )
        }
    }
}

/// Block `h`'s actions; ids for cancel / modify come from the books as
/// they are before the block (sorted: no map order leaks in).
fn random_block(rng: &mut Rng, ctx: &NativeExecContext) -> Vec<(Address, NativeAction)> {
    let mut live: Vec<(MarketId, Address, u128, OrderId)> = ctx
        .order_books
        .iter()
        .flat_map(|(&m, b)| b.cloid_entries().map(move |(t, c, id)| (m, t, c, id)))
        .collect();
    live.sort();
    let mut block = Vec::with_capacity(ACTIONS);
    for _ in 0..ACTIONS {
        let trader = addr(1 + rng.below(TRADERS as u64) as u8);
        let r = rng.below(100);
        let action = if r < 55 {
            NativeAction::PlaceOrder(random_place(rng))
        } else if r < 62 {
            // Two or three orders in one action, often the same cloid.
            let n = 2 + rng.below(2) as usize;
            let mut v: Vec<PlaceOrderParams> = (0..n).map(|_| random_place(rng)).collect();
            if rng.below(2) == 0 {
                let c = Some(C + rng.below(CLOIDS) as u128);
                for p in &mut v {
                    p.client_order_id = c;
                }
            }
            NativeAction::PlaceOrderBatch(v)
        } else if r < 74 {
            NativeAction::CancelByCloid {
                market_id: 1 + rng.below(MARKETS),
                cloid: C + rng.below(CLOIDS) as u128,
            }
        } else if r < 82 && !live.is_empty() {
            let (_, owner, _, id) = live[rng.below(live.len() as u64) as usize];
            // Mostly the owner; sometimes someone else (rejected).
            let who = if rng.below(4) == 0 { trader } else { owner };
            block.push((who, NativeAction::CancelOrder { order_id: id }));
            continue;
        } else if r < 87 && !live.is_empty() {
            let (_, owner, _, id) = live[rng.below(live.len() as u64) as usize];
            block.push((
                owner,
                NativeAction::ModifyOrder {
                    order_id: id,
                    new_price: Some(fp(rng.pick(96, 104))),
                    new_qty: (rng.below(2) == 0).then(|| fp(1)),
                },
            ));
            continue;
        } else if r < 90 {
            let market_id = (rng.below(2) == 0).then(|| 1 + rng.below(MARKETS));
            NativeAction::CancelAllOrders { market_id }
        } else {
            NativeAction::PlaceOrder(params(
                1 + rng.below(MARKETS),
                rng.below(2) == 0,
                rng.pick(97, 103),
                rng.pick(1, 3),
                OrderType::Limit,
                TimeInForce::GTC,
                None,
            ))
        };
        block.push((trader, action));
    }
    block
}

/// Run `block` on the path the stream picks for it.
fn run_block(
    rng: &mut Rng,
    ctx: &mut NativeExecContext,
    block: &[(Address, NativeAction)],
    out: &mut String,
) -> Vec<NativeActionResult> {
    let path = rng.below(5);
    let _ = writeln!(out, "path {path}");
    match path {
        0 => NativeExecutor::execute_batch(ctx, block).results,
        1 => NativeExecutor::execute_batch_cancel_mode(ctx, block, true).results,
        2 => NativeExecutor::execute_batch_engine_mode(ctx, block, 4).results,
        // A block's pre- and post-EVM calls.
        3 => {
            let half = block.len() / 2;
            let mut r = NativeExecutor::execute_batch(ctx, &block[..half]).results;
            r.extend(NativeExecutor::execute_batch(ctx, &block[half..]).results);
            r
        }
        _ => block
            .iter()
            .map(|(s, a)| NativeExecutor::execute(ctx, s, a))
            .collect(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Run {
    Resident,
    NonResident,
    /// Resident, the holder dropped before this block.
    RestartAt(usize),
}

/// FNV-1a 64 of the run's transcript: a fixed fold, not a security hash.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, x| {
        (h ^ x as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// `(digest, duplicate-cloid rejects, successes)` of `seed`'s stream.
fn seeded_run(seed: u64, mode: BookMode, run: Run) -> (u64, usize, usize) {
    // Owner s138: a fixture from before the never-marked rule (test-only seam).
    torus_core::oracle::test_treat_unmarked_as_marked();
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=512u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    let mut holder = ResidentBooks::default();
    let ctx = ctx_at(&db, 1, mode, None);
    for n in 1..=TRADERS {
        let bal = NativeBalance {
            available: fp(100_000_000),
            order_margin: FixedPoint::ZERO,
        };
        ctx.positions.put_native_balance(&addr(n), &bal).unwrap();
    }
    drop(ctx);
    let mut rng = Rng(seed);
    let mut out = String::new();
    let (mut dups, mut oks) = (0, 0);
    for h in 0..BLOCKS {
        if run == Run::RestartAt(h) {
            holder = ResidentBooks::default();
        }
        let resident = run != Run::NonResident;
        let mut ctx = ctx_at(&db, h as u64 + 1, mode, resident.then_some(&mut holder));
        let block = random_block(&mut rng, &ctx);
        let results = run_block(&mut rng, &mut ctx, &block, &mut out);
        for r in &results {
            let _ = writeln!(out, "{} {:?} {:?}", r.success, r.reason, r.error);
            dups += usize::from(r.reason == FailureReason::DuplicateCloid);
            oks += usize::from(r.success);
        }
        let (_, rebuilt) = ctx.cloid_index();
        let _ = writeln!(out, "index {rebuilt:?}");
        ctx.save_order_books();
        ctx.stash_resident(&mut holder);
        if resident {
            let (carried, rebuilt_after) = holder.cloid_index().expect("resident books");
            assert_eq!(rebuilt_after, rebuilt, "{mode:?} {run:?} block {h}");
            if let Some(carried) = carried {
                assert_eq!(
                    carried, rebuilt,
                    "{mode:?} {run:?} block {h}: carried index"
                );
            }
        }
    }
    let ctx = ctx_at(&db, 99, mode, None);
    let root = compute_native_state_root(&ctx.state).unwrap();
    let _ = writeln!(out, "root {root}");
    (fnv(&out), dups, oks)
}

/// Pinned on c88d3073, before the cloid engine-cost changes: per book
/// mode, the digest of every result, the index after every block and the
/// state root. Replicas, a non-resident run and a restart agree.
#[test]
fn a_seeded_cloid_stream_gives_the_pinned_results_and_root() {
    const SEED: u64 = 0x5eed_c10d;
    let want: [u64; 4] = [
        0x2dbb_2aaa_327a_45f0,
        0xfd5d_e250_a290_684b,
        0x337e_7868_feeb_0a5b,
        0xeb0e_f99f_d6c3_63f4,
    ];
    let mut got = Vec::new();
    for mode in MODES {
        let one = seeded_run(SEED, mode, Run::Resident);
        assert_eq!(
            one,
            seeded_run(SEED, mode, Run::Resident),
            "{mode:?}: replicas"
        );
        assert_eq!(
            one,
            seeded_run(SEED, mode, Run::NonResident),
            "{mode:?}: non-resident"
        );
        assert_eq!(
            one,
            seeded_run(SEED, mode, Run::RestartAt(BLOCKS / 2)),
            "{mode:?}: restart"
        );
        // The stream exercises what it claims to.
        assert!(one.1 >= 20, "{mode:?}: duplicate rejects {}", one.1);
        assert!(one.2 >= 150, "{mode:?}: successes {}", one.2);
        got.push(one.0);
    }
    assert_eq!(got, want, "got {got:#x?}");
}

/// Two placements of one trader's cloid in the SAME book in one call (as
/// actions and inside one `PlaceOrderBatch`): the first wins, the second is
/// a duplicate whatever path runs it; another trader's same cloid is fine.
/// (The batch rejects it in the pre-pass's claimed set; the single path in
/// the cross-market check, the first order resting by then.)
#[test]
fn a_second_order_with_the_cloid_in_the_same_book_and_call_is_a_duplicate() {
    // Owner s138: a fixture from before the never-marked rule (test-only seam).
    torus_core::oracle::test_treat_unmarked_as_marked();
    let gtc = |px: i64, cloid: u128| {
        params(
            1,
            true,
            px,
            1,
            OrderType::Limit,
            TimeInForce::GTC,
            Some(cloid),
        )
    };
    for path in 0..5u64 {
        let what = format!("path {path}");
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        // Pre-testnet P0: orders need a listed market (placeholder rows,
        // tick / lot 1, as the old implicit book).
        for m in 1..=512u64 {
            db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
        }
        let mut ctx = ctx_at(&db, 1, BookMode::Classic, None);
        for n in 1..=2 {
            let bal = NativeBalance {
                available: fp(100_000_000),
                order_margin: FixedPoint::ZERO,
            };
            ctx.positions.put_native_balance(&addr(n), &bal).unwrap();
        }
        let block = vec![
            (addr(1), NativeAction::PlaceOrder(gtc(50, C))),
            (addr(1), NativeAction::PlaceOrder(gtc(49, C))),
            (addr(2), NativeAction::PlaceOrder(gtc(48, C))),
            (
                addr(1),
                NativeAction::PlaceOrderBatch(vec![gtc(47, C + 1), gtc(46, C + 1)]),
            ),
        ];
        // `run_block` picks the path from the stream: drive it directly.
        let results = match path {
            0 => NativeExecutor::execute_batch(&mut ctx, &block).results,
            1 => NativeExecutor::execute_batch_cancel_mode(&mut ctx, &block, true).results,
            2 => NativeExecutor::execute_batch_engine_mode(&mut ctx, &block, 4).results,
            3 => NativeExecutor::execute_batch_engine_mode(&mut ctx, &block, 1).results,
            _ => block
                .iter()
                .map(|(s, a)| NativeExecutor::execute(&mut ctx, s, a))
                .collect(),
        };
        let reasons: Vec<(bool, FailureReason)> =
            results.iter().map(|r| (r.success, r.reason)).collect();
        assert!(results[0].success, "{what}: {:?}", results[0]);
        assert_eq!(
            results[1].reason,
            FailureReason::DuplicateCloid,
            "{what}: {reasons:?}"
        );
        assert!(
            results[2].success,
            "{what}: another trader {:?}",
            results[2]
        );
        let book = &ctx.order_books[&1];
        assert_eq!(book.cloid_count(), 3, "{what}: {reasons:?}");
        assert!(book.order_id_by_cloid(&addr(1), C + 1).is_some(), "{what}");
        let resting_c1 = book
            .cloid_entries()
            .filter(|&(t, c, _)| t == addr(1) && c == C + 1)
            .count();
        assert_eq!(resting_c1, 1, "{what}: one of the batch's pair rests");
        book.verify_invariants();
    }
}

/// fix 4: the cross-market index's entry is 64 bytes (was 80) and its
/// packed value gives back the market and id it was made from.
#[test]
fn the_cloid_index_entry_is_64_bytes_and_its_slot_lossless() {
    assert_eq!(std::mem::size_of::<(CloidKey, CloidSlot)>(), 64);
    assert_eq!(
        std::mem::size_of::<((Address, Cloid), (MarketId, OrderId))>(),
        80
    );
    for (m, id) in [(0, 0), (u64::MAX, u128::MAX), (7, 1u128 << 64 | 5), (1, C)] {
        let s = CloidSlot::new(m, id);
        assert_eq!((s.market, s.id()), (m, id));
        assert_eq!(s, CloidSlot::new(m, id));
        assert_ne!(s, CloidSlot::new(m, id ^ 1));
        assert_ne!(s, CloidSlot::new(m ^ 1, id));
    }
}
