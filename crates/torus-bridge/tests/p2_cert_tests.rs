//! s80 phase2-sender-certificate (`TORUS_S80_P2_CERT`): the per-sender
//! Phase-2 headroom certificate must not change anything execution writes or
//! returns. A near-margin, multi-block fixture (marked markets with a mark
//! walk, senders funded around the certificate's bound, mixed sides, prices
//! across the band so checked sells fill far above their limit, plus
//! reduce-only / IOC / FOK / stop / market / PostOnly orders and cancel-alls)
//! runs block by block like the node's pipelined exec path (an overlay over
//! the previous block's frozen set, resident rows attached) with the flag
//! off, on (`1`) and in shadow mode; every block's results, fills and
//! post-flush DB digest (hashed CFs, native state root, running hash) must be
//! byte-identical, and the on run must have certified and skipped orders
//! while other senders failed the bound (the fixture straddles it).

#[path = "common/econ_load.rs"]
mod econ_load;

use econ_load::{base_mark, feed_setup, sender, special, Lcg, MarkWalk, REPORTERS};
use std::sync::Arc;
use torus_bridge::native_executor::{
    begin_resident, end_resident, p2_cert_stats_take, with_p2_cert, NativeExecContext, NativeExecutor, P2CertMode,
    ResidentBooks,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
use torus_state::running_hash::{read_running_hash, HASHED_CFS};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const SENDERS: u64 = 28;
const MARKETS: u64 = 4;
const BLOCKS: u64 = 30;

fn units(v: i128) -> FixedPoint {
    FixedPoint::from_raw(v * FixedPoint::SCALE)
}

/// One sender's PlaceOrderBatch: 1-8 orders, prices up to ±40% from the mid
/// (inside the default ±50% band), a few special order types.
fn batch(rng: &mut Lcg, specials: bool) -> Vec<PlaceOrderParams> {
    let mid = base_mark().raw() / FixedPoint::SCALE;
    (0..1 + rng.below(8))
        .map(|_| {
            let is_buy = rng.below(2) == 0;
            // Mostly near the mid; 1 in 5 far (deep crossing or far resting).
            let off = if rng.chance(200) { rng.below(mid as u64 * 2 / 5) as i128 } else { rng.below(40) as i128 };
            let price = if rng.below(2) == 0 { mid + off } else { mid - off };
            let mut p = PlaceOrderParams {
                market_id: 1 + rng.below(MARKETS),
                is_buy,
                price: units(price),
                quantity: units(1 + rng.below(3) as i128),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            };
            if specials {
                match rng.below(20) {
                    0 => p.time_in_force = TimeInForce::IOC,
                    1 => p.time_in_force = TimeInForce::FOK,
                    2 => p.reduce_only = true,
                    3 => p.order_type = OrderType::Market,
                    4 => p.order_type = OrderType::StopLimit { trigger: p.price, limit: p.price },
                    5 => p.time_in_force = TimeInForce::PostOnly,
                    _ => {}
                }
            }
            p
        })
        .collect()
}

fn fixture(seed: u64) -> Vec<Vec<(alloy_primitives::Address, NativeAction)>> {
    let mut rng = Lcg(0x9C3F_0080 ^ seed);
    (0..BLOCKS)
        .map(|_| {
            (0..6 + rng.below(10))
                .map(|_| {
                    let s = rng.below(SENDERS);
                    let a = if rng.chance(80) {
                        NativeAction::CancelAllOrders { market_id: None }
                    } else {
                        // A third of the senders mix in special orders.
                        NativeAction::PlaceOrderBatch(batch(&mut rng, s % 3 == 0))
                    };
                    (sender(s), a)
                })
                .collect()
        })
        .collect()
}

/// keccak-free digest input: every hashed CF row, the native state root and
/// the running hash (the DB right after a block's flush).
fn db_digest(db: &StateDb) -> Vec<u8> {
    let mut data = Vec::new();
    for (id, cf) in HASHED_CFS {
        for (k, v) in db.iterate_cf(cf, None).unwrap() {
            data.push(*id);
            data.extend_from_slice(&(k.len() as u32).to_be_bytes());
            data.extend_from_slice(&k);
            data.extend_from_slice(&(v.len() as u32).to_be_bytes());
            data.extend_from_slice(&v);
        }
    }
    data.extend_from_slice(compute_native_state_root(db).unwrap().as_slice());
    data.extend_from_slice(format!("{:?}", read_running_hash(db)).as_bytes());
    alloy_primitives::keccak256(&data).to_vec()
}

/// Per block: results (Debug), fill count, fatal error, DB digest after the
/// block's flush.
fn run(mode: P2CertMode, seed: u64) -> (Vec<String>, [u64; 12]) {
    run_engine(mode, seed, 0)
}

/// [`run`] with the Phase-2 engine pinned: `threads >= 2` = the sharded
/// prepare (`execute_batch_engine_mode`), `0` = `execute_batch` (env).
fn run_engine(mode: P2CertMode, seed: u64, threads: usize) -> (Vec<String>, [u64; 12]) {
    let _ = p2_cert_stats_take();
    let blocks = fixture(seed);
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    {
        let ctx = NativeExecContext::new(
            db.clone(), 1, 1000, 0, 1_000_000, 100, special(99), special(100), special(101),
        );
        for i in 0..SENDERS {
            // 4.5k .. 126k units: from below one 1500-IM order batch's need
            // to far above it, so senders straddle the certificate's bound.
            ctx.positions
                .put_native_balance(
                    &sender(i),
                    &NativeBalance { available: units(4_500 * (i as i128 + 1)), order_margin: FixedPoint::ZERO },
                )
                .unwrap();
        }
    }
    feed_setup(&db, MARKETS, true);
    let mut walk = MarkWalk::new(MARKETS, 15);
    let mut holder = ResidentBooks::default();
    let mut books = alloy_primitives::map::HashMap::default();
    let mut next_id: u128 = 1;
    let parent: Option<Arc<torus_state::FrozenPending>> = None;
    let mut out = Vec::new();
    for (h, block) in (1..).zip(blocks.iter()) {
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rows = begin_resident(Some(&mut holder), &mut overlay, h, None);
        let mut ctx = NativeExecContext::new(
            overlay.clone(), h + 1, 1000 + h, 0, 1_000_000, 100, special(99), special(100), special(101),
        );
        ctx.attach_resident_block(&mut rows);
        ctx.set_order_books(std::mem::take(&mut books));
        ctx.next_global_order_id = next_id;
        walk.step();
        for m in 1..=MARKETS {
            let mark = walk.mark(base_mark(), m);
            for n in REPORTERS {
                ctx.oracle.submit_price(&special(n), m, mark, ctx.block_height, ctx.timestamp).unwrap();
            }
        }
        let (results, liq) = with_p2_cert(mode, || {
            let _ = NativeExecutor::begin_block_oracle(&mut ctx);
            let r = if threads >= 2 {
                NativeExecutor::execute_batch_engine_mode(&mut ctx, block, threads)
            } else {
                NativeExecutor::execute_batch(&mut ctx, block)
            };
            let l = NativeExecutor::run_liquidations(&mut ctx);
            (r, l)
        });
        let line = format!(
            "h={h} results={:?} liq={:?} fills={} fatal={:?} next_id={}",
            results.results,
            liq,
            ctx.trade_index,
            ctx.fatal_error,
            ctx.next_global_order_id
        );
        books = ctx.take_order_books();
        next_id = ctx.next_global_order_id;
        ctx.detach_resident_block(&mut rows);
        drop(ctx);
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = if rows.attached() { overlay.own_pending_delta() } else { Default::default() };
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, rows, &mut overlay, delta, true, None);
        // Flushed right away (the digest reads the DB after the block).
        frozen.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        out.push(format!("{line} db={}", alloy_primitives::hex::encode(db_digest(&db))));
    }
    (out, p2_cert_stats_take().0)
}

/// The certificate's counters are process-global: the tests that read
/// them run one at a time (cargo test runs tests on threads).
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn assert_same(off: &[String], on: &[String], what: &str) {
    assert_eq!(off.len(), on.len());
    for (a, b) in off.iter().zip(on) {
        assert_eq!(a, b, "{what}: first differing block");
    }
}

#[test]
fn p2_cert_on_is_byte_identical_near_margin() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for seed in 0..3 {
        let (off, s_off) = run(P2CertMode::Off, seed);
        assert_eq!(s_off, [0; 12], "flag off: the certificate never runs");
        let (on, s) = run(P2CertMode::On, seed);
        assert_same(&off, &on, "TORUS_S80_P2_CERT=1 vs off");
        eprintln!("seed {seed} on stats {s:?}");
        assert!(s[4] > 0, "seed {seed}: some orders skipped the F1 math: {s:?}");
        assert!(s[7] > 0, "seed {seed}: some senders failed the bound (the fixture straddles it): {s:?}");
        assert!(s[5] > 0, "seed {seed}: some senders bailed on special orders: {s:?}");
    }
}

#[test]
fn p2_cert_shadow_and_probe_are_byte_identical() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (off, _) = run(P2CertMode::Off, 7);
    let (shadow, s) = run(P2CertMode::Shadow, 7);
    assert_same(&off, &shadow, "shadow vs off");
    assert!(s[9] > 0, "shadow checked certified orders: {s:?}");
    assert_eq!(s[10], 0, "no shadow mismatch: {s:?}");
    assert_eq!(s[4], 0, "shadow never skips");
    let (probe, p) = run(P2CertMode::Probe, 7);
    assert_same(&off, &probe, "probe vs off");
    assert_eq!(p[4], 0, "probe never skips");
    assert_eq!(p[1], s[1], "probe and shadow certify the same senders");
}

#[test]
fn parse_p2_cert_values() {
    use torus_bridge::native_executor::parse_p2_cert;
    assert_eq!(parse_p2_cert(None), P2CertMode::Off);
    assert_eq!(parse_p2_cert(Some("0")), P2CertMode::Off);
    assert_eq!(parse_p2_cert(Some("1")), P2CertMode::On);
    assert_eq!(parse_p2_cert(Some(" probe ")), P2CertMode::Probe);
    assert_eq!(parse_p2_cert(Some("SHADOW")), P2CertMode::Shadow);
    // Review fix 7: the unsound `ub` bench arm is gone (never on a node).
    assert_eq!(parse_p2_cert(Some("ub")), P2CertMode::Off);
    assert_eq!(parse_p2_cert(Some("junk")), P2CertMode::Off);
}

// ---------------------------------------------------------------------------
// Fix round (adversarial review of a2dac3ea): invalid orders, the extreme
// price panic, a certified sender AT the bound, and the sharded path.
// ---------------------------------------------------------------------------

/// The mid in whole units.
fn mid_units() -> i128 {
    base_mark().raw() / FixedPoint::SCALE
}

/// `pct`% of the mid, in whole units (on the 1-unit tick).
fn at(pct: i128) -> FixedPoint {
    units(mid_units() * pct / 100)
}

fn limit(market_id: u64, is_buy: bool, price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// One block's outcome in the review-fix fixtures.
struct Shot {
    /// Results, fills, fatal, next id and the DB digest after the block.
    line: String,
    fills: u32,
    margin_rejects: usize,
    stats: [u64; 12],
}

/// Straight on the DB (no overlay): senders 0..3 funded with 1M units,
/// markets 1..=2 listed. Block 1 = `setup` with every mark at the mid; then
/// sender 0's `available` is set to `avail` raw; block 2 = `block2` with
/// every mark at `mark2_pct`% of the mid (the band follows the mark). The
/// stats and the outcome are block 2's. `threads >= 2` pins the sharded
/// Phase-2 prepare (`execute_batch_engine_mode`), `0` runs `execute_batch`.
fn two_blocks(
    mode: P2CertMode,
    threads: usize,
    avail: i128,
    setup: &[(alloy_primitives::Address, NativeAction)],
    block2: &[(alloy_primitives::Address, NativeAction)],
    mark2_pct: i128,
) -> Shot {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    let new_ctx = |h: u64| {
        NativeExecContext::new(db.clone(), h + 1, 1000 + h, 0, 1_000_000, 100, special(99), special(100), special(101))
    };
    {
        let ctx = new_ctx(0);
        for s in 0..3 {
            ctx.positions
                .put_native_balance(
                    &sender(s),
                    &NativeBalance { available: units(1_000_000), order_margin: FixedPoint::ZERO },
                )
                .unwrap();
        }
    }
    feed_setup(&db, 2, true);
    let mut books = alloy_primitives::map::HashMap::default();
    let mut next_id: u128 = 1;
    let mut shot = None;
    for (h, block, pct) in [(1u64, setup, 100i128), (2, block2, mark2_pct)] {
        if h == 2 {
            let ctx = new_ctx(h);
            let mut bal = ctx.positions.get_native_balance(&sender(0)).unwrap();
            bal.available = FixedPoint::from_raw(avail);
            ctx.positions.put_native_balance(&sender(0), &bal).unwrap();
        }
        let _ = p2_cert_stats_take();
        let mut ctx = new_ctx(h);
        ctx.set_order_books(std::mem::take(&mut books));
        ctx.next_global_order_id = next_id;
        let mark = FixedPoint::from_raw(base_mark().raw() / 100 * pct);
        for m in 1..=2 {
            for n in REPORTERS {
                ctx.oracle.submit_price(&special(n), m, mark, ctx.block_height, ctx.timestamp).unwrap();
            }
        }
        let r = with_p2_cert(mode, || {
            let _ = NativeExecutor::begin_block_oracle(&mut ctx);
            if threads >= 2 {
                NativeExecutor::execute_batch_engine_mode(&mut ctx, block, threads)
            } else {
                NativeExecutor::execute_batch(&mut ctx, block)
            }
        });
        let stats = p2_cert_stats_take().0;
        let margin_rejects = r.results.iter().filter(|x| format!("{:?}", x.reason) == "Margin").count();
        let line = format!(
            "results={:?} fills={} fatal={:?} next_id={}",
            r.results, ctx.trade_index, ctx.fatal_error, ctx.next_global_order_id
        );
        let fills = ctx.trade_index;
        books = ctx.take_order_books();
        next_id = ctx.next_global_order_id;
        drop(ctx);
        shot = Some(Shot {
            line: format!("{line} db={}", alloy_primitives::hex::encode(db_digest(&db))),
            fills,
            margin_rejects,
            stats,
        });
    }
    shot.unwrap()
}

fn place(s: u64, orders: Vec<PlaceOrderParams>) -> (alloy_primitives::Address, NativeAction) {
    (sender(s), NativeAction::PlaceOrderBatch(orders))
}

/// Off vs every certificate mode on one fixture: byte-identical, no
/// shadow mismatch; returns the On stats.
fn same_in_every_mode(
    threads: usize,
    avail: i128,
    setup: &[(alloy_primitives::Address, NativeAction)],
    block2: &[(alloy_primitives::Address, NativeAction)],
    mark2_pct: i128,
) -> [u64; 12] {
    let off = two_blocks(P2CertMode::Off, threads, avail, setup, block2, mark2_pct);
    assert_eq!(off.stats, [0; 12], "flag off: the certificate never runs");
    let mut on_stats = [0; 12];
    for mode in [P2CertMode::On, P2CertMode::Shadow, P2CertMode::Probe] {
        let r = two_blocks(mode, threads, avail, setup, block2, mark2_pct);
        assert_eq!(off.line, r.line, "{mode:?} vs off (threads {threads}, avail {avail})");
        assert_eq!(r.stats[10], 0, "{mode:?}: shadow mismatch: {:?}", r.stats);
        if mode == P2CertMode::On {
            on_stats = r.stats;
        }
    }
    on_stats
}

/// Review P1 (a2dac3ea): a non-positive limit price (or a non-positive
/// quantity) made `Σ notional` negative, so a sender with 100 units got a
/// 15000-IM buy past the account check. Such orders are rejected
/// (validate_order_price / shape) in both arms; they now also void the
/// sender's certificate.
#[test]
fn p2_cert_invalid_price_or_quantity_voids_the_certificate() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let avail = units(100).raw();
    let big = limit(1, true, base_mark(), units(10));
    // Control: alone, the big buy fails the bound and the account check.
    let alone = [place(0, vec![big.clone()])];
    let s = same_in_every_mode(0, avail, &[], &alone, 100);
    assert_eq!((s[1], s[7]), (0, 1), "control: the bound fails: {s:?}");
    let off = two_blocks(P2CertMode::Off, 0, avail, &[], &alone, 100);
    assert_eq!(off.margin_rejects, 1, "control: flag off rejects on margin");
    for bad in [
        limit(1, true, units(-1_000_000_000), units(1)),
        limit(1, true, FixedPoint::ZERO, units(1)),
        limit(1, false, FixedPoint::from_raw(i128::MIN + 1), units(1)),
        limit(1, true, base_mark(), units(-1_000_000)),
        limit(1, false, base_mark(), units(-1_000_000)),
        limit(2, true, base_mark(), FixedPoint::ZERO),
    ] {
        let block = [place(0, vec![big.clone(), bad.clone()])];
        let s = same_in_every_mode(0, avail, &[], &block, 100);
        assert_eq!((s[1], s[5]), (0, 1), "{bad:?}: not eligible, never certified: {s:?}");
        let off = two_blocks(P2CertMode::Off, 0, avail, &[], &block, 100);
        assert_eq!(off.margin_rejects, 1, "{bad:?}: the big buy is still a margin reject");
        // Rich sender: still never certified (the order is invalid).
        let s = same_in_every_mode(0, units(1_000_000).raw(), &[], &block, 100);
        assert_eq!(s[1], 0, "{bad:?}: rich sender not certified: {s:?}");
    }
}

/// Review P1 (a2dac3ea): `r - base` on a sell at raw `i128::MIN + 1` in
/// the anchor market panicked ('FixedPoint subtraction underflow') with the
/// flag on, probe or shadow. Now: identical to flag off, no panic.
#[test]
fn p2_cert_extreme_prices_do_not_panic() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let rich = units(1_000_000).raw();
    for orders in [
        vec![limit(1, false, base_mark(), units(1)), limit(1, false, FixedPoint::from_raw(i128::MIN + 1), units(1))],
        vec![limit(1, true, base_mark(), units(1)), limit(1, true, FixedPoint::from_raw(i128::MAX), units(1))],
        vec![limit(1, false, base_mark(), units(1)), limit(1, false, FixedPoint::from_raw(i128::MAX), units(1))],
        vec![limit(1, false, FixedPoint::from_raw(i128::MAX), FixedPoint::from_raw(i128::MAX))],
    ] {
        let block = [place(0, orders)];
        same_in_every_mode(0, rich, &[], &block, 100);
    }
}

/// Review P3: lot / tick / band rejects in a certified sender's batch.
/// They reserve nothing in either arm, so the sender stays certified (they
/// only add to the bound) and every arm is byte-identical.
#[test]
fn p2_cert_shape_and_band_rejects_in_a_certified_batch() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let half = FixedPoint::from_raw(FixedPoint::SCALE / 2);
    let maker = [place(1, vec![limit(1, false, at(102), units(3)), limit(1, true, at(98), units(3))])];
    let block = [place(
        0,
        vec![
            limit(1, true, at(100), units(1)),
            limit(1, true, at(99), half),                                     // below the lot
            limit(1, true, FixedPoint::from_raw(at(99).raw() + 1), units(1)), // off tick
            limit(1, true, at(200), units(1)),                                // outside the band
            limit(1, false, at(30), units(1)),                                // outside the band
            limit(1, false, at(97), units(2)),                                // checked, crosses the bid
            limit(2, false, at(101), units(1)),
        ],
    )];
    let s = same_in_every_mode(0, units(1_000_000).raw(), &maker, &block, 100);
    assert_eq!(s[1], 1, "certified despite the rejects: {s:?}");
    assert!(s[4] > 0, "the valid orders skipped: {s:?}");
}

/// Review P3: a certified sender AT the bound. The sender holds a losing
/// long, its checked sell crosses a bid 25% above the mark (the pool pays
/// the IM above its reservation), a sell in its second market takes the
/// bid floor and the B-blind top-up, and it rests orders in both markets.
/// The smallest certified `available` (slack 0) is found by bisection;
/// there every arm is byte-identical, shadow sees no mismatch, the exact
/// path has no margin reject and fills exactly as with unlimited balance
/// (the pool never binds). One raw unit less is not certified.
#[test]
fn p2_cert_at_the_bound() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for threads in [0usize, 4] {
        let setup = [
            place(1, vec![limit(1, false, at(130), units(3)), limit(1, true, at(120), units(5)), limit(2, true, at(120), units(5))]),
            place(0, vec![limit(1, true, at(130), units(1))]),
        ];
        // Sender 2 (rich, always certified) makes the sharded prepare
        // engage (it needs >= 2 sender groups).
        let block = [
            place(
                0,
                vec![
                    limit(1, false, at(90), units(2)),
                    limit(1, true, at(80), units(1)),
                    limit(2, false, at(95), units(1)),
                    PlaceOrderParams { time_in_force: TimeInForce::PostOnly, ..limit(2, true, at(85), units(1)) },
                ],
            ),
            place(2, vec![limit(2, true, at(70), units(1))]),
        ];
        let certified = |avail: i128| two_blocks(P2CertMode::Probe, threads, avail, &setup, &block, 95).stats[1] == 2;
        let exact_ok = |avail: i128| two_blocks(P2CertMode::Off, threads, avail, &setup, &block, 95).margin_rejects == 0;
        let bisect = |ok: &dyn Fn(i128) -> bool| {
            let (mut lo, mut hi) = (units(-1_000_000).raw(), units(1_000_000).raw());
            assert!(!ok(lo) && ok(hi), "bracket");
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if ok(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            hi
        };
        let x_cert = bisect(&certified);
        let x_exact = bisect(&exact_ok);
        eprintln!(
            "threads {threads}: certified from available {x_cert} raw, exact path passes from {x_exact} raw (margin {} units)",
            (x_cert - x_exact) / FixedPoint::SCALE
        );
        assert!(x_exact <= x_cert, "sound: the certificate never certifies below the exact threshold");
        assert!(!certified(x_cert - 1), "slack 0 at the bisected bound");
        let unlimited = two_blocks(P2CertMode::Off, threads, units(1_000_000).raw(), &setup, &block, 95);
        for avail in [x_cert, x_cert + 1, x_cert - 1] {
            let s = same_in_every_mode(threads, avail, &setup, &block, 95);
            if avail >= x_cert {
                assert_eq!(s[1], 2, "certified at {avail}: {s:?}");
                assert_eq!(s[4], 5, "every order skipped at {avail}: {s:?}");
                let off = two_blocks(P2CertMode::Off, threads, avail, &setup, &block, 95);
                assert_eq!(off.margin_rejects, 0, "exact path: no margin reject at {avail}");
                assert_eq!(off.fills, unlimited.fills, "the pool never binds at {avail}");
            }
        }
    }
}

/// Review P2: the sharded Phase-2 prepare (TORUS_PARALLEL_ENGINE >= 2)
/// certifies too, and stays byte-identical to flag off over the
/// near-margin fixture.
#[test]
fn p2_cert_sharded_prepare_certifies_and_is_byte_identical() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (off, s_off) = run_engine(P2CertMode::Off, 1, 4);
    assert_eq!(s_off, [0; 12]);
    let (on, s) = run_engine(P2CertMode::On, 1, 4);
    assert_same(&off, &on, "sharded: TORUS_S80_P2_CERT=1 vs off");
    eprintln!("sharded on stats {s:?}");
    assert!(s[4] > 0, "sharded: some orders skipped: {s:?}");
    assert!(s[7] > 0, "sharded: some senders failed the bound: {s:?}");
    let (shadow, s) = run_engine(P2CertMode::Shadow, 1, 4);
    assert_same(&off, &shadow, "sharded: shadow vs off");
    assert!(s[9] > 0 && s[10] == 0, "sharded shadow checked, no mismatch: {s:?}");
}
