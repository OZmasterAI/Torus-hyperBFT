//! s138 product batch: the funding engine (P0.0 `funding_engine`, id 3),
//! trading fees (`trading_fees`, id 4) and same-block CoreWriter (item 7 T,
//! `same_block_core_writer`, id 2) together.
//!
//! - One fill that settles funding AND pays a fee: each side's cash change is
//!   closed PnL - funding - fee (`FillEffect::cash`), in every settle path
//!   (single action, sequential, forced parallel at caps 1 / 2 / 8, the
//!   engine, L2.1 streamed on and off) with the same native root, for both
//!   bits on and for each alone; with both on the change is the sum of the
//!   two single-feature changes.
//! - A multi-market stream with funding settlements and fees is
//!   byte-identical in every settle mode (resident rows on and off), and the
//!   fees stay conserved (the `cfee` rows sum to the treasury credit).
//! - CoreWriter orders drained with T and fees: the fees are charged as for a
//!   native order, whatever the T bit, and T's same-block deposit still runs.

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    begin_resident, end_resident, with_settle_streamed, NativeActionResult, NativeBatchResult,
    NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::funding::{self, FundingRow};
use torus_core::position::{NativeBalance, Position, PositionManager};
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_state::cf::{
    CF_CONSENSUS_META, CF_FEE_CONFIG, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE,
    CF_NATIVE_POSITIONS, META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::trading_fees::cum_fee_key;
use torus_types::{
    Feature, FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

const FUND: u128 = Feature::FundingEngine.bit();
const FEES: u128 = Feature::TradingFees.bit();
const T: u128 = Feature::SameBlockCoreWriter.bit();
const S: i128 = FixedPoint::SCALE;
const TREASURY: u8 = 100;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(i128::from(v) * S)
}

fn order(m: MarketId, is_buy: bool, px: i64, qty: i64, tif: TimeInForce) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(px),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn place(t: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (t, NativeAction::PlaceOrder(p))
}

/// The stored aggregate row: price(16) ‖ block(8) ‖ reporters(4) ‖ ts(8).
/// Every market here is marked (owner s138: no order on a never-marked one).
fn set_mark<St: StateBackend>(s: &St, m: MarketId, price: FixedPoint, ts: u64) {
    let row = [
        price.raw().to_be_bytes().as_slice(),
        &7u64.to_be_bytes(),
        &3u32.to_be_bytes(),
        &ts.to_be_bytes(),
    ]
    .concat();
    s.put_cf_raw(
        CF_NATIVE_ORACLE,
        &[b"agg".as_slice(), &m.to_be_bytes()].concat(),
        &row,
    )
    .unwrap();
}

fn set_param(db: &StateDb, key: &str, v: u64) {
    db.put_cf_raw(CF_FEE_CONFIG, key.as_bytes(), v.to_string().as_bytes())
        .unwrap();
}

/// `markets` listed (placeholder rows), traders 1..=`traders` funded.
fn setup(markets: u64, traders: u8, fund: i64) -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    for m in 1..=markets {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed")
            .unwrap();
    }
    let pm = PositionManager::new(db.clone());
    for n in 1..=traders {
        pm.put_native_balance(
            &addr(n),
            &NativeBalance {
                available: fp(fund),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    }
    (dir, db)
}

fn ctx_at<St: StateBackend>(state: St, h: u64, ts: u64, features: u128) -> NativeExecContext<St> {
    let mut ctx = NativeExecContext::new(
        state,
        h,
        ts,
        0,
        1_000,
        10,
        addr(99),
        addr(TREASURY),
        addr(101),
    );
    ctx.features = features;
    ctx
}

/// available + order margin (raw).
fn cash<St: StateBackend>(s: &St, n: u8) -> i128 {
    let b = PositionManager::new(s.clone())
        .get_native_balance(&addr(n))
        .unwrap();
    b.available.raw() + b.order_margin.raw()
}

fn cum_fee<St: StateBackend>(s: &St, n: u8) -> i128 {
    s.get_cf_raw(CF_NATIVE_BALANCES, &cum_fee_key(&addr(n)))
        .unwrap()
        .map_or(0, |v| {
            i128::from_be_bytes(v.as_slice().try_into().expect("16-byte cfee row"))
        })
}

fn position<St: StateBackend>(s: &St, n: u8, m: MarketId) -> Option<Position> {
    PositionManager::new(s.clone())
        .get_position(&addr(n), m)
        .unwrap()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Every action through `NativeExecutor::execute` (the single path).
    Single,
    Seq,
    Par(usize),
    Engine(usize),
}

fn exec<St: StateBackend>(
    ctx: &mut NativeExecContext<St>,
    batch: &[(Address, NativeAction)],
    mode: Mode,
    streamed: bool,
) -> Vec<NativeActionResult> {
    with_settle_streamed(streamed, || match mode {
        Mode::Single => batch
            .iter()
            .map(|(t, a)| NativeExecutor::execute(ctx, t, a))
            .collect(),
        Mode::Seq => {
            NativeExecutor::execute_batch_settle_workers(ctx, batch, false, Some(1)).results
        }
        Mode::Par(cap) => {
            NativeExecutor::execute_batch_settle_workers(ctx, batch, true, Some(cap)).results
        }
        Mode::Engine(t) => {
            let r: NativeBatchResult = NativeExecutor::execute_batch_engine_mode(ctx, batch, t);
            r.results
        }
    })
}

const MODES: [(Mode, bool); 8] = [
    (Mode::Single, false),
    (Mode::Seq, false),
    (Mode::Par(1), false),
    (Mode::Par(2), true),
    (Mode::Par(8), false),
    (Mode::Par(8), true),
    (Mode::Engine(1), false),
    (Mode::Engine(4), true),
];

fn label(features: u128) -> String {
    let mut s = Vec::new();
    if features & FUND != 0 {
        s.push("funding");
    }
    if features & FEES != 0 {
        s.push("fees");
    }
    if features & T != 0 {
        s.push("T");
    }
    if s.is_empty() {
        "none".into()
    } else {
        s.join("+")
    }
}

// ---- Funding and fees on the same fill ----

const A: u8 = 1;
const B: u8 = 2;
const C: u8 = 3;
const F1: u8 = 4;
const F2: u8 = 5;

/// What one run of [`two_blocks`] leaves behind.
struct FillOut {
    /// Block 2's cash change of A, B, C (raw).
    delta: [i128; 3],
    /// Block 2's `cfee` change of A, B, C and the treasury's credit (raw).
    fee: [i128; 3],
    treasury: i128,
    /// Market 1's funding index after block 2's settlement (0 without it).
    index: i128,
    root: B256,
}

/// Block 1 (ts 1000): A buys 10 @ 100 from B (B short 10); a filler pair
/// trades market 2. Block 2 (ts 3600, funding's first settlement): B rests a
/// buy of 2 @ 100 (a close of its short, as the maker), C rests a buy of 4 @
/// 100 (an open), A sells 6 @ 100 IOC (a close of its long, as the taker):
/// A fills against B then C. Every close is at the entry price, so the closed
/// PnL is 0 and the cash change is exactly -(funding) - (fee).
fn two_blocks(features: u128, mode: Mode, streamed: bool) -> FillOut {
    let tag = format!("{} {mode:?} streamed {streamed}", label(features));
    let (_d, db) = setup(2, 8, 1_000_000);
    let mut ctx = ctx_at(db.clone(), 1, 1_000, features);
    set_mark(&ctx.state, 1, fp(100), 1_000);
    set_mark(&ctx.state, 2, fp(100), 1_000);
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    let r = exec(
        &mut ctx,
        &[
            place(addr(B), order(1, false, 100, 10, TimeInForce::GTC)),
            place(addr(F1), order(2, false, 100, 1, TimeInForce::GTC)),
            place(addr(A), order(1, true, 100, 10, TimeInForce::IOC)),
            place(addr(F2), order(2, true, 100, 1, TimeInForce::IOC)),
        ],
        mode,
        streamed,
    );
    assert!(r.iter().all(|x| x.success), "{tag}: {r:?}");
    assert!(ctx.fatal_error.is_none(), "{tag}: {:?}", ctx.fatal_error);
    ctx.save_order_books();
    drop(ctx);
    let before: Vec<i128> = [A, B, C].iter().map(|&n| cash(&db, n)).collect();
    let fee_before: Vec<i128> = [A, B, C].iter().map(|&n| cum_fee(&db, n)).collect();
    let treasury_before = cash(&db, TREASURY);

    let mut ctx = ctx_at(db.clone(), 2, 3_600, features);
    set_mark(&ctx.state, 1, fp(100), 3_600);
    set_mark(&ctx.state, 2, fp(100), 3_600);
    let _ = NativeExecutor::begin_block_oracle(&mut ctx);
    let index = funding::read_row(&ctx.state, 1)
        .unwrap()
        .map_or(0, |r: FundingRow| r.cum_index);
    let r = exec(
        &mut ctx,
        &[
            place(addr(B), order(1, true, 100, 2, TimeInForce::GTC)),
            place(addr(C), order(1, true, 100, 4, TimeInForce::GTC)),
            place(addr(F2), order(2, false, 100, 1, TimeInForce::GTC)),
            place(addr(A), order(1, false, 100, 6, TimeInForce::IOC)),
            place(addr(F1), order(2, true, 100, 1, TimeInForce::IOC)),
        ],
        mode,
        streamed,
    );
    assert!(r.iter().all(|x| x.success), "{tag}: {r:?}");
    assert!(ctx.fatal_error.is_none(), "{tag}: {:?}", ctx.fatal_error);
    assert!(
        ctx.reader_fault.get().is_none(),
        "{tag}: {:?}",
        ctx.reader_fault.get()
    );
    ctx.save_order_books();
    drop(ctx);
    assert_eq!(
        position(&db, A, 1).map(|p| p.size),
        Some(fp(4)),
        "{tag}: A long 4"
    );
    assert_eq!(
        position(&db, B, 1).map(|p| p.size),
        Some(fp(-8)),
        "{tag}: B short 8"
    );
    assert_eq!(
        position(&db, C, 1).map(|p| p.size),
        Some(fp(4)),
        "{tag}: C long 4"
    );
    let mut out = FillOut {
        delta: [0; 3],
        fee: [0; 3],
        treasury: cash(&db, TREASURY) - treasury_before,
        index,
        root: compute_native_state_root(&db).unwrap(),
    };
    for (i, &n) in [A, B, C].iter().enumerate() {
        out.delta[i] = cash(&db, n) - before[i];
        out.fee[i] = cum_fee(&db, n) - fee_before[i];
    }
    out
}

/// Funding + fees on one fill (both bits, then each alone), in every settle
/// path: cash change = closed PnL (0) - funding - fee per side, the treasury
/// takes exactly the fees, and every path ends at the same native root.
/// With both bits the change is the sum of the two single-bit changes.
#[test]
fn funding_and_fees_on_one_fill_compose_in_every_settle_path() {
    // Default rates: taker 450 ppm, maker 150 ppm.
    let (fee_a, fee_b, fee_c) = (
        600 * S * 450 / 1_000_000,
        200 * S * 150 / 1_000_000,
        400 * S * 150 / 1_000_000,
    );
    // Market 2's filler pair, 1 @ 100 each block 2 side.
    let fillers = 100 * S * 450 / 1_000_000 + 100 * S * 150 / 1_000_000;
    let mut by_features = Vec::new();
    for features in [FUND | FEES, FUND, FEES] {
        let mut root: Option<B256> = None;
        let mut first: Option<[i128; 3]> = None;
        for (mode, streamed) in MODES {
            let tag = format!("{} {mode:?} streamed {streamed}", label(features));
            let out = two_blocks(features, mode, streamed);
            let (paid_a, paid_b) = if features & FUND != 0 {
                assert_ne!(out.index, 0, "{tag}: funding settled");
                (
                    funding::owed(true, fp(10), 0, out.index).raw(),
                    funding::owed(false, fp(10), 0, out.index).raw(),
                )
            } else {
                assert_eq!(out.index, 0, "{tag}: no funding row");
                (0, 0)
            };
            let fees = features & FEES != 0;
            let (fa, fb, fc) = if fees {
                (fee_a, fee_b, fee_c)
            } else {
                (0, 0, 0)
            };
            assert_eq!(out.fee, [fa, fb, fc], "{tag}: cfee rows");
            assert_eq!(
                out.treasury,
                if fees { fa + fb + fc + fillers } else { 0 },
                "{tag}: treasury"
            );
            assert_eq!(
                out.delta,
                [-paid_a - fa, -paid_b - fb, -fc],
                "{tag}: cash = pnl - funding - fee"
            );
            match root {
                None => root = Some(out.root),
                Some(r) => assert_eq!(
                    r, out.root,
                    "{tag}: native root differs from the single path"
                ),
            }
            match first {
                None => first = Some(out.delta),
                Some(d) => assert_eq!(d, out.delta, "{tag}"),
            }
        }
        by_features.push((features, first.unwrap(), root.unwrap()));
    }
    let (both, fund, fees) = (by_features[0].1, by_features[1].1, by_features[2].1);
    for i in 0..3 {
        assert_eq!(
            both[i],
            fund[i] + fees[i],
            "side {i}: both = funding + fees"
        );
        assert_ne!(both[i], 0);
    }
    assert_ne!(
        by_features[0].2, by_features[1].2,
        "roots differ by feature set"
    );
    assert_ne!(
        by_features[0].2, by_features[2].2,
        "roots differ by feature set"
    );
}

// ---- Stream determinism with both features ----

const MARKETS: u64 = 4;
const TRADERS: u8 = 24;
const BLOCKS: u64 = 24;

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

struct Block {
    ts: u64,
    marks: Vec<(MarketId, i64)>,
    batches: [Vec<(Address, NativeAction)>; 2],
}

/// funding_tests' stream: a ladder on every market, then crossing storms
/// (opens, reduces, flips, closes), marks wandering 95..=105 around a book at
/// 96..=104, 250 s per block (interval 600 s: a settlement about every other
/// block), so fills settle funding and pay fees on the same balances.
fn stream(seed: u64) -> Vec<Block> {
    let mut rng = Lcg(seed);
    let mut ladder = Vec::new();
    for m in 1..=MARKETS {
        for lvl in 0..4i64 {
            let t = addr(1 + rng.below(u64::from(TRADERS)) as u8);
            ladder.push(place(
                t,
                order(m, true, 97 + lvl, 1 + rng.below(4) as i64, TimeInForce::GTC),
            ));
            let t = addr(1 + rng.below(u64::from(TRADERS)) as u8);
            ladder.push(place(
                t,
                order(
                    m,
                    false,
                    101 + lvl,
                    1 + rng.below(4) as i64,
                    TimeInForce::GTC,
                ),
            ));
        }
    }
    let storm = |rng: &mut Lcg| -> Vec<(Address, NativeAction)> {
        (0..30)
            .map(|i| {
                let m = 1 + rng.below(MARKETS);
                let t = addr(1 + rng.below(u64::from(TRADERS)) as u8);
                let tif = if rng.below(3) == 0 {
                    TimeInForce::IOC
                } else {
                    TimeInForce::GTC
                };
                if i % 17 == 16 {
                    (t, NativeAction::CancelAllOrders { market_id: Some(m) })
                } else {
                    place(
                        t,
                        order(
                            m,
                            rng.below(2) == 0,
                            96 + rng.below(9) as i64,
                            1 + rng.below(5) as i64,
                            tif,
                        ),
                    )
                }
            })
            .collect()
    };
    (1..=BLOCKS)
        .map(|h| {
            let marks = (1..=MARKETS)
                .map(|m| (m, 95 + rng.below(11) as i64))
                .collect();
            let first = if h == 1 {
                ladder.clone()
            } else {
                storm(&mut rng)
            };
            let second = storm(&mut rng);
            Block {
                ts: 1_000 + 250 * h,
                marks,
                batches: [first, second],
            }
        })
        .collect()
}

type Dump = Vec<(&'static str, Vec<u8>, Vec<u8>)>;

const DUMP_CFS: [&str; 5] = [
    CF_NATIVE_POSITIONS,
    CF_NATIVE_BALANCES,
    CF_NATIVE_ORACLE,
    CF_NATIVE_MARKETS,
    CF_FEE_CONFIG,
];

fn dump(db: &StateDb) -> Dump {
    DUMP_CFS
        .iter()
        .flat_map(|cf| {
            db.iterate_cf(cf, None)
                .unwrap()
                .into_iter()
                .map(move |(k, v)| (*cf, k, v))
        })
        .collect()
}

struct RunOut {
    dumps: Vec<Dump>,
    results: Vec<Vec<(bool, Option<String>)>>,
    root: B256,
    treasury: i128,
    cfee_sum: i128,
}

/// Each block through an overlay (the node's shape): marks, the block-start
/// oracle / funding step, two batches, flush.
fn run(blocks: &[Block], features: u128, mode: Mode, streamed: bool, resident: bool) -> RunOut {
    let (_d, db) = setup(MARKETS, TRADERS, 1_000_000);
    set_param(&db, torus_types::FUNDING_INTERVAL_PARAM, 600);
    set_param(&db, torus_types::FUNDING_IMPACT_NOTIONAL_PARAM, 150);
    let tag = format!(
        "{} {mode:?} streamed {streamed} resident {resident}",
        label(features)
    );
    let mut holder = ResidentBooks::default();
    let mut out = RunOut {
        dumps: Vec::new(),
        results: Vec::new(),
        root: B256::ZERO,
        treasury: 0,
        cfee_sum: 0,
    };
    for (i, b) in blocks.iter().enumerate() {
        let h = i as u64 + 1;
        let mut overlay = NativeStateOverlay::new(db.clone());
        let mut rb = begin_resident(resident.then_some(&mut holder), &mut overlay, h, None);
        let mut ctx = ctx_at(overlay.clone(), h, b.ts, features);
        ctx.attach_resident_block(&mut rb);
        for &(m, px) in &b.marks {
            set_mark(&ctx.state, m, fp(px), b.ts);
        }
        let _ = NativeExecutor::begin_block_oracle(&mut ctx);
        let mut results = Vec::new();
        for batch in &b.batches {
            let r = exec(&mut ctx, batch, mode, streamed);
            results.extend(
                r.iter()
                    .map(|x| (x.success, x.error.as_deref().map(str::to_string))),
            );
            assert!(
                ctx.fatal_error.is_none(),
                "{tag} block {h}: {:?}",
                ctx.fatal_error
            );
        }
        assert!(
            ctx.reader_fault.get().is_none(),
            "{tag} block {h}: {:?}",
            ctx.reader_fault.get()
        );
        ctx.save_order_books();
        ctx.detach_resident_block(&mut rb);
        drop(ctx);
        overlay
            .put_cf_raw(
                CF_CONSENSUS_META,
                META_NATIVE_APPLIED_HEIGHT,
                &h.to_be_bytes(),
            )
            .unwrap();
        let delta = if rb.attached() {
            overlay.own_pending_delta()
        } else {
            Default::default()
        };
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, rb, &mut overlay, delta, true, None);
        frozen
            .flush_with_native_trie_stats(&db, None, None, None)
            .expect("flush");
        out.results.push(results);
        out.dumps.push(dump(&db));
    }
    out.root = compute_native_state_root(&db).expect("root");
    out.treasury = cash(&db, TREASURY);
    out.cfee_sum = (1..=TRADERS).map(|n| cum_fee(&db, n)).sum();
    out
}

fn assert_same(a: &RunOut, b: &RunOut, tag: &str) {
    assert_eq!(a.dumps.len(), b.dumps.len(), "{tag}: blocks");
    for (h, (x, y)) in a.dumps.iter().zip(&b.dumps).enumerate() {
        assert_eq!(a.results[h], b.results[h], "{tag}: block {} results", h + 1);
        assert!(x == y, "{tag}: block {} rows differ", h + 1);
    }
    assert_eq!(a.root, b.root, "{tag}: native root");
}

/// The stream exercised what it claims: funding settled into the index and
/// positions paid it (funding bit); fees reached the treasury, conserved to
/// the raw unit against the `cfee` rows (fees bit).
fn assert_features_happened(out: &RunOut, features: u128, tag: &str) {
    let last = out.dumps.last().unwrap();
    let rows = last
        .iter()
        .filter(|(cf, k, _)| *cf == CF_NATIVE_ORACLE && k.starts_with(b"fnd"))
        .count();
    let paid = last
        .iter()
        .filter(|(cf, _, _)| *cf == CF_NATIVE_POSITIONS)
        .filter_map(|(_, _, v)| borsh::from_slice::<Position>(v).ok())
        .filter(|p| p.funding.is_some_and(|f| { f.paid } != FixedPoint::ZERO))
        .count();
    if features & FUND != 0 {
        assert_eq!(rows, MARKETS as usize, "{tag}: one funding row per market");
        assert!(paid > 0, "{tag}: some position settled funding on touch");
    } else {
        assert_eq!((rows, paid), (0, 0), "{tag}: no funding without the bit");
    }
    if features & FEES != 0 {
        assert!(out.treasury > 0, "{tag}: fees charged");
        assert_eq!(
            out.cfee_sum, out.treasury,
            "{tag}: cfee rows sum to the treasury credit"
        );
    } else {
        assert_eq!(
            (out.treasury, out.cfee_sum),
            (0, 0),
            "{tag}: no fees without the bit"
        );
    }
}

/// Funding + fees over a 24-block stream: every settle mode (and the
/// resident rows / sums cache) gives byte-identical rows, results and native
/// root, for both bits and for each alone.
#[test]
fn funding_and_fees_stream_is_identical_in_every_settle_mode() {
    let blocks = stream(0xFEE_F00D);
    let mut roots = Vec::new();
    for features in [FUND | FEES, FUND, FEES] {
        let reference = run(&blocks, features, Mode::Seq, false, false);
        assert_features_happened(&reference, features, &label(features));
        for (mode, streamed) in [
            (Mode::Par(1), false),
            (Mode::Par(2), true),
            (Mode::Par(8), false),
            (Mode::Par(8), true),
            (Mode::Engine(1), false),
            (Mode::Engine(4), true),
        ] {
            // Resident rows on as well for the combined case.
            let residents: &[bool] = if features == FUND | FEES {
                &[false, true]
            } else {
                &[false]
            };
            for &resident in residents {
                let tag = format!(
                    "{} {mode:?} streamed {streamed} resident {resident}",
                    label(features)
                );
                assert_same(
                    &reference,
                    &run(&blocks, features, mode, streamed, resident),
                    &tag,
                );
            }
        }
        roots.push(reference.root);
    }
    assert_ne!(roots[0], roots[1]);
    assert_ne!(roots[0], roots[2]);
}

// ---- Item 7 T + fees: CoreWriter orders ----

fn cw_order(side: u8, qty: i64, tif: u8) -> QueuedActionKind {
    QueuedActionKind::PlaceOrder {
        market_id: 1,
        side,
        order_type: 0,
        price: fp(100),
        quantity: fp(qty),
        time_in_force: tif,
        cloid: None,
    }
}

/// CoreWriter orders queued by block 1 (K = 1) drain in block 2: maker 21
/// rests a sell of 10 @ 100, taker 22 buys it IOC (notional 1000). With the
/// fees bit both pay the default rates (taker 0.45, maker 0.15) into the
/// treasury, as a native order does; without it nothing. The T bit changes
/// neither the orders nor their fees, and still runs block 2's own lockbox
/// deposit (trader 23) in block 2 (off: it waits for block 3).
#[test]
fn core_writer_orders_pay_fees_with_and_without_same_block_t() {
    let mut fee_outcomes = Vec::new();
    for features in [0, T, FEES, T | FEES] {
        let tag = label(features);
        let t_on = features & T != 0;
        let (_d, db) = setup(1, 22, 1_000_000);
        set_mark(&db, 1, fp(100), 1_000);
        db.put_cf_raw(CF_FEE_CONFIG, b"core_writer_order_delay", b"1")
            .unwrap();
        let q = |trader: u8, kind: QueuedActionKind, queued: u64| {
            CoreWriterQueue::enqueue_with(
                &db,
                &QueuedAction {
                    trader: addr(trader),
                    kind,
                    block_queued: queued,
                },
                t_on,
            )
            .unwrap()
        };
        q(21, cw_order(1, 10, 0), 1);
        q(22, cw_order(0, 10, 1), 1);
        q(23, QueuedActionKind::LockboxDeposit { amount: fp(5) }, 2);
        let before = [cash(&db, 21), cash(&db, 22)];

        let mut ctx = ctx_at(db.clone(), 2, 2_000, features);
        let entries = NativeExecutor::drain_core_writer_entries(&mut ctx).expect("queue reads");
        assert_eq!(ctx.take_fatal_error(), None, "{tag}");
        let placed: Vec<bool> = entries
            .iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    torus_state::action_status::CoreWriterKind::PlaceOrder
                )
            })
            .map(|e| e.result.success)
            .collect();
        assert_eq!(
            placed,
            vec![true, true],
            "{tag}: both CoreWriter orders run in block 2"
        );
        ctx.save_order_books();
        drop(ctx);
        assert_eq!(
            position(&db, 22, 1).map(|p| p.size),
            Some(fp(10)),
            "{tag}: the IOC filled"
        );
        let (taker, maker) = (1_000 * S * 450 / 1_000_000, 1_000 * S * 150 / 1_000_000);
        let fees = features & FEES != 0;
        let (want_t, want_m) = if fees { (taker, maker) } else { (0, 0) };
        assert_eq!(
            (cum_fee(&db, 22), cum_fee(&db, 21)),
            (want_t, want_m),
            "{tag}: cfee rows"
        );
        assert_eq!(cash(&db, TREASURY), want_t + want_m, "{tag}: treasury");
        assert_eq!(
            [cash(&db, 21) - before[0], cash(&db, 22) - before[1]],
            [-want_m, -want_t],
            "{tag}: cash"
        );
        let deposit = PositionManager::new(db.clone())
            .get_native_balance(&addr(23))
            .unwrap()
            .available;
        assert_eq!(
            deposit,
            if t_on { fp(5) } else { FixedPoint::ZERO },
            "{tag}: T's same-block deposit"
        );
        fee_outcomes.push((
            features & FEES,
            cum_fee(&db, 21),
            cum_fee(&db, 22),
            cash(&db, TREASURY),
        ));
    }
    assert_eq!(fee_outcomes[0], fee_outcomes[1], "T alone charges nothing");
    assert_eq!(
        fee_outcomes[2], fee_outcomes[3],
        "T does not change the fees"
    );
}
