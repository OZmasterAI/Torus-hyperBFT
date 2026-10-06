//! Item 6 E3: the oracle rows (`CF_NATIVE_ORACLE`) in the resident rows R.
//! The block-start oracle step (prune, aggregation, mark table) over R ==
//! the same step without R (today's path: DB + parent layer), block by
//! block, through the pipelined lifecycle (overlay over the previous
//! block's frozen set, flushed one block later), with `end_resident` inline
//! and on its worker (step 2). The feed submits for a varying subset of
//! markets and reporters, pauses (the window empties: prune deletes rows,
//! aggregates age and go stale), sends out-of-range prices, and the block
//! sometimes writes an undecodable submission row, deletes an aggregate or
//! moves a signer-index row. After every block R == a cold build.

use std::sync::Arc;

use alloy_primitives::{Address, U256};
use torus_bridge::native_executor::{
    begin_resident, end_resident, end_resident_on_worker, NativeExecContext, NativeExecutor, ResidentBooks,
};
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

const MARKETS: u64 = 6;
const REPORTERS: [u8; 4] = [150, 151, 152, 153];
const BLOCKS: u64 = 150;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Off,
    Inline,
    Worker,
}

fn reporter(n: u8) -> Address {
    Address::new([n; 20])
}

fn setup(db: &StateDb) {
    for n in REPORTERS {
        StakingManager::new(db.clone())
            .put_validator(
                &reporter(n),
                &ValidatorState {
                    address: reporter(n),
                    pubkey: [n; 32],
                    commission_bps: 0,
                    self_stake: MIN_SELF_DELEGATION,
                    total_delegated: U256::ZERO,
                    status: if n == 153 { ValidatorStatus::Jailed } else { ValidatorStatus::Active },
                    jailed_until: None,
                    last_commission_change_block: None,
                    oracle_signer: None,
                },
            )
            .unwrap();
    }
    for m in 1..=MARKETS {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
}

/// One run: per block, the step's results, every oracle row after it and
/// each market's price as `get_price` reads it at the block time.
fn run(seed: u64, mode: Mode, cold_checks: &mut usize) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    setup(&db);
    let mut rng = Lcg(seed);
    let mut holder = ResidentBooks::default();
    let mut parent: Option<Arc<FrozenPending>> = None;
    let mut out = Vec::new();
    let mut paused_until = 0u64;
    for h in 1..=BLOCKS {
        let ts = 1_000 + 3 * h + rng.below(3);
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut rb = begin_resident((mode != Mode::Off).then_some(&mut holder), &mut overlay, h, None);
        assert_eq!(rb.attached(), mode != Mode::Off);
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
        ctx.attach_resident_block(&mut rb);
        // The feed: pauses of up to 40 blocks (> the window and the max age).
        if h >= paused_until && rng.below(25) == 0 {
            paused_until = h + 1 + rng.below(40);
        }
        if h >= paused_until {
            for m in 1..=MARKETS {
                if rng.below(4) == 0 {
                    continue;
                }
                for n in REPORTERS {
                    if rng.below(5) == 0 {
                        continue;
                    }
                    let units = match rng.below(30) {
                        0 => -5,
                        1 => 0,
                        _ => 1_000 + rng.below(40) as i128 - 20,
                    };
                    let price = FixedPoint::from_raw(units * FixedPoint::SCALE);
                    ctx.oracle.submit_price(&reporter(n), m, price, h, ts).unwrap();
                }
            }
        }
        match rng.below(12) {
            0 => {
                let key = [b"sub".as_slice(), &(1 + rng.below(MARKETS)).to_be_bytes(), &[0xAB; 20]].concat();
                ctx.state.put_cf_raw(CF_NATIVE_ORACLE, &key, &[1, 2, 3]).unwrap();
            }
            1 => {
                let key = [b"agg".as_slice(), &(1 + rng.below(MARKETS)).to_be_bytes()].concat();
                ctx.state.delete_cf_raw(CF_NATIVE_ORACLE, &key).unwrap();
            }
            2 => {
                let signer = Address::new([0x60 + rng.below(4) as u8; 20]);
                ctx.state.put_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&signer), reporter(150).as_slice()).unwrap();
            }
            3 => {
                let signer = Address::new([0x60 + rng.below(4) as u8; 20]);
                ctx.state.delete_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&signer)).unwrap();
            }
            _ => {}
        }
        let results = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        let prices: Vec<String> = (1..=MARKETS).map(|m| format!("{:?}", ctx.oracle.get_price(m, ts))).collect();
        let rows = ctx.state.iterate_cf(CF_NATIVE_ORACLE, None).unwrap();
        let due = NativeExecutor::oracle_due(&ctx.state).unwrap();
        out.push(format!(
            "{:?} | {rows:?} | {prices:?} | due {due}",
            results.iter().map(|r| (r.action_type, r.success, &r.error)).collect::<Vec<_>>()
        ));
        ctx.detach_resident_block(&mut rb);
        drop(ctx);
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = if rb.attached() { overlay.own_pending_delta() } else { Default::default() };
        let frozen = overlay.freeze(h);
        match mode {
            Mode::Worker => end_resident_on_worker(&mut holder, rb, &mut overlay, delta, true, None),
            _ => end_resident(&mut holder, rb, &mut overlay, delta, true, None),
        }
        if let Some(p) = parent.take() {
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
        if mode != Mode::Off {
            // Warm == cold: R after the block == a build over DB + this block's layer.
            let cold = ResidentRows::build(&NativeStateOverlay::with_parent(db.clone(), parent.clone())).unwrap();
            assert_eq!(holder.rows(), Some(&cold), "seed {seed} {mode:?} block {h}: warm R != cold build");
            *cold_checks += 1;
        }
    }
    if mode != Mode::Off {
        assert_eq!(holder.rows_builds(), 1, "{mode:?}: R built once");
    }
    out
}

#[test]
fn oracle_step_over_r_equals_the_step_without_r() {
    let mut cold_checks = 0;
    let (mut pruned, mut fresh, mut stale_or_absent) = (0usize, 0usize, 0usize);
    for seed in 1..=4u64 {
        let seed = seed * 0x0E3_0E3;
        let off = run(seed, Mode::Off, &mut cold_checks);
        for mode in [Mode::Inline, Mode::Worker] {
            let on = run(seed, mode, &mut cold_checks);
            for (h, (a, b)) in on.iter().zip(off.iter()).enumerate() {
                assert_eq!(a, b, "seed {seed} {mode:?} block {}", h + 1);
            }
        }
        for w in off.windows(2) {
            let subs = |s: &str| s.matches("[115, 117, 98").count();
            pruned += usize::from(subs(&w[1]) < subs(&w[0]));
        }
        fresh += off.iter().map(|s| s.matches("stale: false").count()).sum::<usize>();
        stale_or_absent += off.iter().map(|s| s.matches("stale: true").count() + s.matches("Err(").count()).sum::<usize>();
    }
    println!("ORACLE_R cold_checks={cold_checks} pruned_blocks={pruned} fresh={fresh} stale_or_absent={stale_or_absent}");
    assert_eq!(cold_checks, 4 * 2 * BLOCKS as usize);
    assert!(pruned > 20, "submission rows pruned: {pruned}");
    assert!(fresh > 500 && stale_or_absent > 100, "fresh and stale / absent marks: {fresh} / {stale_or_absent}");
}
