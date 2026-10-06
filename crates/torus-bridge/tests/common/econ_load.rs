//! s87 econ-shaped load (from `ubench_econ.rs`), shared by the µbench and the
//! item 6 storage-read test: GTC limits only, one side per (sender, market),
//! mid = 20 x target, band 5, cross 0.5, cancel-all 5% + open-order budget,
//! batches of `batch` orders per action. Plus the oracle feed of `UB_MARKS=1`
//! (three Active validators, markets listed) and the item 6 step 0.5 mark walk.
//! `UB_DEEP_LEVELS=<n>` (item 6 PF1): prices within `n` ticks of the mid instead
//! of `BAND`, so the resting orders pile up on a few deep levels (the devnet
//! cells' shape: ~50k orders at the best ask) and crossing bids meet them.

#![allow(dead_code)]

use alloy_primitives::{Address, U256};
use std::collections::HashMap;
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_state::cf::CF_NATIVE_MARKETS;
use torus_state::StateDb;
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

pub fn env(k: &str, d: u64) -> u64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

pub fn sender(i: u64) -> Address {
    let mut b = [0xA7u8; 20];
    b[12..20].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(b)
}

pub fn special(n: u8) -> Address {
    Address::new([n; 20])
}

pub struct Lcg(pub u64);
impl Lcg {
    pub fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn chance(&mut self, p_milli: u64) -> bool {
        self.below(1000) < p_milli
    }
}

pub const TARGET: i128 = 1500;
pub const LEV: i128 = 20;
pub const BAND: u64 = 5;
pub const REPORTERS: [u8; 3] = [150, 151, 152];

/// The mid every order and (walk 0) every mark sits at: `TARGET * LEV`.
pub fn base_mark() -> FixedPoint {
    FixedPoint::from_raw(TARGET * LEV * FixedPoint::SCALE)
}

/// Row 69 (s94 B): `UB_REAL_MARKETS` (default 1): listed markets get
/// genesis-layout rows ([`market_row`]). `UB_REAL_MARKETS=0`: the `b"listed"`
/// placeholders of every run before row 69, for comparisons with those runs.
/// The two are NOT comparable: a placeholder decodes as no market (default
/// margin tiers instead of one 20x tier) and each read of it costs a failed
/// borsh decode (~20 us, up to 1 MiB allocated; ~6 ms of ctx per block at
/// 300 markets in `ubench_epoch`).
pub fn real_markets() -> bool {
    env("UB_REAL_MARKETS", 1) != 0
}

/// A listed market's `CF_NATIVE_MARKETS` row: the genesis / governance layout
/// (base, quote, lot 1, tick 1, initial margin 5% = one 20x tier) when
/// `real`, else the `b"listed"` placeholder (see [`real_markets`]).
pub fn market_row(real: bool) -> Vec<u8> {
    if !real {
        return b"listed".to_vec();
    }
    let one = FixedPoint::ONE.raw();
    borsh::to_vec(&("BASE".to_string(), "USDC".to_string(), one, one, 5 * one)).unwrap()
}

/// `UB_MARKS=1`: three Active validators and markets `1..=markets` listed
/// with [`market_row`]`(real)`.
pub fn feed_setup(db: &StateDb, markets: u64, real: bool) {
    for n in REPORTERS {
        StakingManager::new(db.clone())
            .put_validator(
                &special(n),
                &ValidatorState {
                    address: special(n),
                    pubkey: [n; 32],
                    commission_bps: 0,
                    self_stake: MIN_SELF_DELEGATION,
                    total_delegated: U256::ZERO,
                    status: ValidatorStatus::Active,
                    jailed_until: None,
                    last_commission_change_block: None,
                    oracle_signer: None,
                },
            )
            .unwrap();
    }
    let row = market_row(real);
    for m in 1..=markets {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &row).unwrap();
    }
}

pub fn econ_order(rng: &mut Lcg, s: u64, market_id: u64, band: u64) -> PlaceOrderParams {
    let is_buy = s.wrapping_add(market_id).is_multiple_of(2);
    let aggressive = rng.chance(500);
    let d = 1 + rng.below(band) as i128;
    let mid = TARGET * LEV;
    let units = if is_buy == aggressive { mid + d } else { mid - d };
    let price = FixedPoint::from_raw(units * FixedPoint::SCALE);
    let target = FixedPoint::from_raw(TARGET * FixedPoint::SCALE);
    let lev = FixedPoint::from_raw(LEV * FixedPoint::SCALE);
    let mut quantity = target * lev / price;
    if quantity < FixedPoint::ONE {
        quantity = FixedPoint::ONE;
    }
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

pub struct Gen {
    pub rng: Lcg,
    pub senders: u64,
    pub markets: u64,
    pub batch: u64,
    pub budget: u64,
    pub open: HashMap<u64, u64>,
}

impl Gen {
    pub fn block(&mut self, actions: u64) -> Vec<(Address, NativeAction)> {
        let mut out = Vec::with_capacity(actions as usize);
        let band = match env("UB_DEEP_LEVELS", 0) {
            0 => BAND,
            n => n,
        };
        for _ in 0..actions {
            let s = self.rng.below(self.senders);
            let open = self.open.entry(s).or_insert(0);
            let a = if *open + self.batch > self.budget || self.rng.chance(50) {
                *open = 0;
                NativeAction::CancelAllOrders { market_id: None }
            } else {
                *open += self.batch;
                let orders = (0..self.batch)
                    .map(|_| {
                        let m = 1 + self.rng.below(self.markets);
                        econ_order(&mut self.rng, s, m, band)
                    })
                    .collect();
                NativeAction::PlaceOrderBatch(orders)
            };
            out.push((sender(s), a));
        }
        out
    }
}

/// Item 6 step 0.5: a deterministic, bounded, mean-reverting walk of every
/// market's submitted mark, so the aggregated mark (and the margin valuation
/// that depends on it) changes every block. Each step moves a market's
/// offset by exactly `walk_bp` basis points, toward the base mid with
/// probability `1/2 + |offset| / (2 * bound)`, `bound = 8 * walk_bp`: never
/// further than `bound` from the mid (80 bp at 10 bp per block, far inside the
/// 250 bp IM−MM gap at 20x, so liquidations stay rare). `walk_bp = 0` keeps
/// every mark at the base, exactly the fixed-price feed. The same rule as
/// `bench-throughput oracle-feed --walk-bp` (one step per feed round there).
pub struct MarkWalk {
    walk_bp: i64,
    offsets_bp: Vec<i64>,
    steps: u64,
}

/// SplitMix64 finaliser: the step's draw for one market.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl MarkWalk {
    /// Bound of the walk as a multiple of the step.
    pub const BOUND_STEPS: i64 = 8;

    /// Markets `1..=markets`, all at the base mid.
    pub fn new(markets: u64, walk_bp: u64) -> Self {
        Self { walk_bp: walk_bp as i64, offsets_bp: vec![0; markets as usize], steps: 0 }
    }

    /// One block (one feed round): every market moves `±walk_bp`.
    pub fn step(&mut self) {
        self.steps += 1;
        if self.walk_bp == 0 {
            return;
        }
        let bound = Self::BOUND_STEPS * self.walk_bp;
        for (i, x) in self.offsets_bp.iter_mut().enumerate() {
            let draw = mix(self.steps.wrapping_mul(0xD6E8_FEB8_6659_FD93) ^ (i as u64 + 1)) % (2 * bound) as u64;
            let toward_mid = (draw as i64) < bound + x.abs();
            let down = if *x == 0 { toward_mid } else { toward_mid == (*x > 0) };
            *x += if down { -self.walk_bp } else { self.walk_bp };
        }
    }

    /// Offset of market `m` from the base mid, in basis points.
    pub fn offset_bp(&self, m: u64) -> i64 {
        self.offsets_bp[(m - 1) as usize]
    }

    /// `base * (10_000 + offset) / 10_000` (raw integer math).
    pub fn mark(&self, base: FixedPoint, m: u64) -> FixedPoint {
        FixedPoint::from_raw(base.raw() * i128::from(10_000 + self.offset_bp(m)) / 10_000)
    }
}
