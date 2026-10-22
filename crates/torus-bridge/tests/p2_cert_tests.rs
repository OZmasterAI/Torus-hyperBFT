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
fn run(mode: P2CertMode, seed: u64) -> (Vec<String>, [u64; 10]) {
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
            let r = NativeExecutor::execute_batch(&mut ctx, block);
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
        assert_eq!(s_off, [0; 10], "flag off: the certificate never runs");
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
    assert_eq!(parse_p2_cert(Some("ub")), P2CertMode::Ub);
    assert_eq!(parse_p2_cert(Some("junk")), P2CertMode::Off);
}
