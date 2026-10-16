//! s58 (perf experiment sell-top-up-stitch-list, `TORUS_FAST_SELL_TOP_UPS=1`):
//! [`NativeExecutor::sell_top_ups_fast`] (B-blind top-ups from the candidate
//! list recorded at the Phase-2 stitch, with a per-sender run cache) must
//! leave exactly what [`NativeExecutor::sell_top_ups`] (the scan) leaves:
//! every prepared order's `margin_reserved` and `res_price`, the
//! `[full, partial, none]` counts, the balance cache's rows, dirty flags and
//! dirty list, the first latched reader fault and the backend reads. The
//! batched s92 counter loop ([`NativeExecutor::record_margin_cuts_batched`])
//! must leave every counter [`NativeExecutor::record_margin_cuts`] leaves.
//! The block-level flag-on vs flag-off differential is
//! `tests/sell_top_up_stitch_diff_tests.rs`.

use super::*;
use crate::market_workers::MatchResult;
use torus_core::order_book::ReduceOnlyCut;
use torus_state::cf::CF_NATIVE_BALANCES;
use torus_state::{AtomicWriteOp, StateError};

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
    fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// `v / 100`.
fn cents(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE / 100)
}

/// Balance reads of `armed.0` fail while `armed.1 > 0` (each failure uses
/// one up: a transient fault when it starts small); `hits` counts failures.
#[derive(Clone)]
struct FailBal {
    inner: StateDb,
    armed: Arc<std::sync::Mutex<Option<(Address, usize)>>>,
    hits: Arc<std::sync::Mutex<usize>>,
}

impl StateBackend for FailBal {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if cf == CF_NATIVE_BALANCES {
            let mut armed = self.armed.lock().unwrap();
            if let Some((a, left)) = armed.as_mut() {
                if a.as_slice() == key && *left > 0 {
                    *left -= 1;
                    *self.hits.lock().unwrap() += 1;
                    return Err(StateError::Io(std::io::Error::other(
                        "injected balance read failure",
                    )));
                }
            }
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

fn fail_ctx() -> (tempfile::TempDir, FailBal, NativeExecContext<FailBal>) {
    let dir = tempfile::tempdir().unwrap();
    let backend = FailBal {
        inner: StateDb::open(dir.path()).unwrap(),
        armed: Arc::default(),
        hits: Arc::default(),
    };
    let ctx = NativeExecContext::new(
        backend.clone(),
        2,
        1_001,
        0,
        1_000,
        10,
        Address::ZERO,
        Address::ZERO,
        Address::ZERO,
    );
    (dir, backend, ctx)
}

/// The candidate list the stitch records, rebuilt from prepared batches
/// (flat order; `res_qty` by `prepare_one`'s basis rule).
fn cands_of(
    batches: &HashMap<MarketId, Vec<PreparedOrder<'_>>>,
    basis: &HashMap<usize, (FixedPoint, FixedPoint)>,
) -> Vec<TopUpCand> {
    let mut v: Vec<TopUpCand> = batches
        .iter()
        .flat_map(|(&m, b)| {
            b.iter()
                .enumerate()
                .filter(|(_, p)| p.top_up_candidate)
                .map(move |(k, p)| TopUpCand {
                    index: p.index,
                    market: m,
                    k,
                    sender: p.sender,
                    reserved: p.margin_reserved,
                    res_qty: basis.get(&p.index).map_or(p.params.quantity, |b| b.1),
                })
        })
        .collect();
    v.sort_unstable_by_key(|c| c.index);
    v
}

/// A balance cache's rows: (trader, available, order margin, dirty), sorted.
type Rows = Vec<(Address, FixedPoint, FixedPoint, bool)>;

fn cache_rows(c: &BalanceCache) -> Rows {
    let mut v: Vec<_> = c
        .map
        .iter()
        .map(|(a, b)| (*a, b.balance.available, b.balance.order_margin, b.dirty))
        .collect();
    v.sort_unstable_by_key(|x| x.0);
    v
}

/// What one top-up pass leaves behind.
#[derive(Debug, PartialEq)]
struct Outcome {
    /// (flat index, margin_reserved, res_price) of every prepared order.
    orders: Vec<(usize, FixedPoint, FixedPoint)>,
    counts: [u64; 3],
    rows: Rows,
    /// The dirty list, unsorted (stronger than `flush_all` needs).
    dirty: Vec<Address>,
    fault: Option<String>,
    hits: usize,
}

fn orders_of(
    batches: &HashMap<MarketId, Vec<PreparedOrder<'_>>>,
) -> Vec<(usize, FixedPoint, FixedPoint)> {
    let mut v: Vec<_> = batches
        .values()
        .flatten()
        .map(|p| (p.index, p.margin_reserved, p.res_price))
        .collect();
    v.sort_unstable_by_key(|x| x.0);
    v
}

/// The pre-top-up cache state of a sender.
#[derive(Clone)]
enum Pre {
    /// Read (clean entry).
    Load,
    /// Written (dirty entry; after a `Load` of the same sender: occupied).
    Set(NativeBalance),
}

/// One random round's owned inputs (borrowed by the batches / markets).
struct Round {
    n_markets: u64,
    /// Per market (index `m - 1`): config, best bid, listed in `markets`.
    cfgs: Vec<Option<MarketMarginConfig>>,
    bids: Vec<Option<FixedPoint>>,
    listed: Vec<bool>,
    /// Per order, in flat order.
    index: Vec<usize>,
    sender: Vec<Address>,
    params: Vec<PlaceOrderParams>,
    reserved: Vec<FixedPoint>,
    candidate: Vec<bool>,
    basis: HashMap<usize, (FixedPoint, FixedPoint)>,
    pools: HashMap<Address, (MarketId, FixedPoint)>,
    excess: HashMap<Address, FixedPoint>,
    precache: Vec<(Address, Pre)>,
    arm: Option<(Address, usize)>,
}

impl Round {
    fn markets(&self) -> HashMap<MarketId, Phase2Market<'_>> {
        (1..=self.n_markets)
            .filter(|m| self.listed[*m as usize - 1])
            .map(|m| {
                let i = m as usize - 1;
                (
                    m,
                    Phase2Market {
                        shape: (FixedPoint::ONE, FixedPoint::ONE),
                        bid_floor: self.bids[i],
                        cfg: self.cfgs[i].as_ref(),
                        mark: None,
                        band: None,
                        orders: 0,
                    },
                )
            })
            .collect()
    }

    fn batches(&self) -> HashMap<MarketId, Vec<PreparedOrder<'_>>> {
        let mut out: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::default();
        for (j, params) in self.params.iter().enumerate() {
            out.entry(params.market_id)
                .or_default()
                .push(PreparedOrder {
                    index: self.index[j],
                    sender: self.sender[j],
                    params,
                    order_id: j as u128 + 1,
                    margin_reserved: self.reserved[j],
                    checked_pos_net: None,
                    pre_pos: None,
                    top_up_candidate: self.candidate[j],
                    res_price: params.price,
                });
        }
        out
    }

    /// One top-up pass (`fast`: the stitch list + run cache) from the
    /// round's start state: a fresh cache, latch and fault arming.
    fn run(&self, ctx: &NativeExecContext<FailBal>, backend: &FailBal, fast: bool) -> Outcome {
        *backend.armed.lock().unwrap() = None;
        let mut cache = BalanceCache::new();
        for (a, pre) in &self.precache {
            match pre {
                Pre::Load => {
                    cache.load(&ctx.positions, None, a).unwrap();
                }
                Pre::Set(b) => cache.set(a, b.clone()),
            }
        }
        *backend.armed.lock().unwrap() = self.arm;
        *backend.hits.lock().unwrap() = 0;
        let markets = self.markets();
        let mut batches = self.batches();
        let fault = std::sync::OnceLock::new();
        let counts = if fast {
            let cands = cands_of(&batches, &self.basis);
            NativeExecutor::sell_top_ups_fast(
                &ctx.positions,
                None,
                &fault,
                &markets,
                &self.pools,
                &self.excess,
                &mut batches,
                &mut cache,
                cands,
            )
        } else {
            NativeExecutor::sell_top_ups(
                &ctx.positions,
                None,
                &fault,
                &markets,
                &self.basis,
                &self.pools,
                &self.excess,
                &mut batches,
                &mut cache,
            )
        };
        *backend.armed.lock().unwrap() = None;
        Outcome {
            orders: orders_of(&batches),
            counts,
            rows: cache_rows(&cache),
            dirty: cache.dirty.clone(),
            fault: fault.get().cloned(),
            hits: *backend.hits.lock().unwrap(),
        }
    }
}

/// A round's sender `s`: unique per round, so one database serves them all.
fn trader(round: u64, s: u64) -> Address {
    let mut b = [0u8; 20];
    b[..8].copy_from_slice(&round.to_be_bytes());
    b[8..16].copy_from_slice(&(s + 1).to_be_bytes());
    Address::from(b)
}

fn random_round(rng: &mut Lcg, round: u64, ctx: &NativeExecContext<FailBal>) -> Round {
    let n_markets = 1 + rng.below(5);
    let n_senders = 2 + rng.below(5);
    let senders: Vec<Address> = (0..n_senders).map(|s| trader(round, s)).collect();
    let mut cfgs = Vec::new();
    let mut bids = Vec::new();
    let mut listed = Vec::new();
    for m in 1..=n_markets {
        cfgs.push(match rng.below(3) {
            0 => None,
            1 => Some(MarketMarginConfig::new(m, 20)),
            _ => Some(MarketMarginConfig {
                tiers: vec![
                    MarginTier {
                        max_notional: fp(500),
                        max_leverage: 10,
                    },
                    MarginTier {
                        max_notional: FixedPoint::from_raw(i128::MAX),
                        max_leverage: 3,
                    },
                ],
                ..MarketMarginConfig::new(m, 10)
            }),
        });
        bids.push(match rng.below(10) {
            // No bid: no top-up in this market.
            0 | 1 => None,
            // `sell_top_up_price` overflows: no top-up.
            2 => Some(FixedPoint::from_raw(i128::MAX / 5_000)),
            _ => Some(cents(5_000 + rng.below(10_000) as i64)),
        });
        listed.push(!rng.one_in(15));
    }
    // Balances: none, thin, tight, comfortable, rich; order margin some.
    for s in &senders {
        if rng.one_in(6) {
            continue; // no row: the zero balance
        }
        let available = match rng.below(5) {
            0 => FixedPoint::ZERO,
            1 => cents(rng.below(300) as i64),
            2 => cents(1_000 + rng.below(5_000) as i64),
            3 => fp(100 + rng.below(400) as i64),
            _ => fp(1_000_000),
        };
        let order_margin = cents(rng.below(10_000) as i64);
        ctx.positions
            .put_native_balance(
                s,
                &NativeBalance {
                    available,
                    order_margin,
                },
            )
            .unwrap();
    }
    let n_orders = 1 + rng.below(40) as usize;
    let (mut index, mut sender, mut params, mut reserved, mut candidate) =
        (vec![], vec![], vec![], vec![], vec![]);
    let mut basis = HashMap::default();
    let mut flat = 0usize;
    let mut cur = senders[0];
    for _ in 0..n_orders {
        flat += 1 + rng.below(3) as usize;
        // Runs: half the time the previous order's sender again.
        if !rng.one_in(2) {
            cur = senders[rng.below(n_senders) as usize];
        }
        let market_id = 1 + rng.below(n_markets);
        let is_buy = rng.one_in(5);
        let mut p = PlaceOrderParams {
            market_id,
            is_buy,
            price: cents(4_000 + rng.below(12_000) as i64),
            quantity: cents(1 + rng.below(2_000) as i64),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        match rng.below(10) {
            0 => p.order_type = OrderType::Market,
            1 => p.time_in_force = TimeInForce::IOC,
            2 => p.time_in_force = TimeInForce::FOK,
            3 => p.time_in_force = TimeInForce::PostOnly,
            _ => {}
        }
        if rng.one_in(25) {
            // price x quantity overflows: `try_reserve_for_qty_cfg` errs.
            p.quantity = FixedPoint::from_raw(i128::MAX / 3);
        }
        let cfg = cfgs[market_id as usize - 1].as_ref();
        let own =
            NativeExecutor::try_reserve_for_qty_cfg(cfg, p.price, p.quantity).unwrap_or(fp(1));
        let r = match rng.below(6) {
            0 => FixedPoint::ZERO,
            // Above any top-up target: extra <= 0.
            1 => fp(1_000_000_000),
            2 => own + cents(rng.below(500) as i64),
            _ => own,
        };
        // Market / IOC / FOK: the reservation basis overrides (mark price,
        // the quantity beyond the closing allowance, possibly zero).
        if NativeExecutor::never_rests(&p) && !rng.one_in(3) {
            let q = if rng.one_in(4) {
                FixedPoint::ZERO
            } else {
                cents(1 + rng.below(2_000) as i64)
            };
            basis.insert(flat, (cents(4_000 + rng.below(12_000) as i64), q));
        }
        candidate.push(!is_buy && !rng.one_in(4));
        index.push(flat);
        sender.push(cur);
        params.push(p);
        reserved.push(r);
    }
    let mut pools = HashMap::default();
    let mut excess = HashMap::default();
    for s in &senders {
        if !rng.one_in(4) {
            let pos_net = fp(rng.below(400) as i64) - fp(200);
            pools.insert(*s, (1 + rng.below(n_markets), pos_net));
        }
        if rng.one_in(2) {
            excess.insert(*s, cents(rng.below(20_000) as i64));
        }
    }
    let mut precache = Vec::new();
    for s in &senders {
        match rng.below(5) {
            0 => precache.push((*s, Pre::Load)),
            1 => precache.push((
                *s,
                Pre::Set(NativeBalance {
                    available: fp(rng.below(300) as i64),
                    order_margin: cents(rng.below(900) as i64),
                }),
            )),
            2 => {
                precache.push((*s, Pre::Load));
                precache.push((
                    *s,
                    Pre::Set(NativeBalance {
                        available: cents(rng.below(30_000) as i64),
                        order_margin: fp(3),
                    }),
                ));
            }
            _ => {}
        }
    }
    // A fault on a sender the cache does not hold: once (transient), twice,
    // or for good.
    let arm = rng.one_in(3).then(|| {
        let s = senders[rng.below(n_senders) as usize];
        (!precache.iter().any(|(a, _)| *a == s))
            .then(|| (s, [1, 2, usize::MAX][rng.below(3) as usize]))
    });
    Round {
        n_markets,
        cfgs,
        bids,
        listed,
        index,
        sender,
        params,
        reserved,
        candidate,
        basis,
        pools,
        excess,
        precache,
        arm: arm.flatten(),
    }
}

/// 600 random rounds: interleaved senders (runs, and senders with several
/// non-adjacent runs), tight funding (full, partial and no top-ups), runs
/// whose first candidate needs nothing, Market / IOC / FOK sells with basis
/// overrides, markets without a bid, unlisted, or whose top-up price or
/// notional overflows, pre-cached clean and dirty senders, and transient or
/// permanent balance-read faults. The scan and the stitch list leave the
/// same orders, counts, cache rows, dirty flags and list, fault and reads.
#[test]
fn fast_top_ups_equal_the_scan_on_random_batches() {
    let (_dir, backend, ctx) = fail_ctx();
    let mut rng = Lcg(0x5E11_70B5);
    let mut st = [0usize; 12];
    for round in 0..600u64 {
        let r = random_round(&mut rng, round, &ctx);
        let slow = r.run(&ctx, &backend, false);
        let fast = r.run(&ctx, &backend, true);
        assert_eq!(fast, slow, "round {round}");

        // Non-vacuity, from the scan's outcome.
        for (total, n) in st.iter_mut().zip(slow.counts) {
            *total += n as usize;
        }
        st[3] += usize::from(slow.fault.is_some());
        // A transient fault, then a successful read of the same sender.
        st[4] +=
            usize::from(r.arm.is_some_and(|(a, _)| {
                slow.fault.is_some() && slow.rows.iter().any(|row| row.0 == a)
            }));
        let cands = cands_of(&r.batches(), &r.basis);
        let mut runs: Vec<Address> = Vec::new();
        for c in &cands {
            if runs.last() != Some(&c.sender) {
                runs.push(c.sender);
            }
        }
        let mut seen = std::collections::HashSet::new();
        st[5] += usize::from(runs.iter().any(|s| !seen.insert(*s))); // non-adjacent runs
        let start: HashMap<usize, FixedPoint> = r
            .index
            .iter()
            .copied()
            .zip(r.reserved.iter().copied())
            .collect();
        let granted = |c: &TopUpCand| {
            slow.orders
                .iter()
                .any(|o| o.0 == c.index && o.1 != start[&c.index])
        };
        if let Some(last) = runs.last() {
            st[6] += usize::from(
                cands
                    .iter()
                    .rev()
                    .take_while(|c| c.sender == *last)
                    .any(granted),
            );
        }
        st[7] += cands
            .iter()
            .filter(|c| r.basis.contains_key(&c.index))
            .count();
        st[8] += cands
            .iter()
            .filter(|c| r.bids[c.market as usize - 1].is_none())
            .count();
        st[9] += cands
            .iter()
            .filter(|c| NativeExecutor::try_reserve_for_qty_cfg(None, fp(50), c.res_qty).is_err())
            .count();
        // A clean entry the pass read without granting (an unconditional
        // set at run end would dirty it).
        st[10] += slow
            .rows
            .iter()
            .filter(|row| !row.3 && !r.precache.iter().any(|(a, _)| *a == row.0))
            .count();
        // A run whose first candidate needs no top-up but a later one is granted.
        let mut i = 0;
        while i < cands.len() {
            let j = i + cands[i..]
                .iter()
                .take_while(|c| c.sender == cands[i].sender)
                .count();
            let first_skipped = slow
                .orders
                .iter()
                .any(|o| o.0 == cands[i].index && o.1 == start[&cands[i].index]);
            st[11] += usize::from(first_skipped && cands[i + 1..j].iter().any(granted));
            i = j;
        }
    }
    let [full, partial, none, faults, transient, nonadj, last_granted, basis, no_bid, overflow, clean_read, first_skip] =
        st;
    assert!(
        full > 300
            && partial > 30
            && none > 30
            && faults > 10
            && transient > 3
            && nonadj > 50
            && last_granted > 50
            && basis > 30
            && no_bid > 30
            && overflow > 5
            && clean_read > 30
            && first_skip > 10,
        "non-vacuous: {st:?}"
    );
}

/// A sender's later non-adjacent run sees its earlier run's grant (one set
/// per granted run), a run without a grant leaves its clean entry clean,
/// and the run after the last sender change is written too.
#[test]
fn fast_top_ups_non_adjacent_runs_and_last_run() {
    let (_dir, backend, ctx) = fail_ctx();
    let (a, b) = (trader(9_000, 0), trader(9_000, 1));
    let sell = |m: MarketId| PlaceOrderParams {
        market_id: m,
        is_buy: false,
        price: fp(90),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let own = NativeExecutor::reserve_for_qty_cfg(None, fp(90), fp(1));
    let target = NativeExecutor::reserve_for_qty_cfg(
        None,
        NativeExecutor::sell_top_up_price(fp(100)).unwrap(),
        fp(1),
    );
    let extra = target - own;
    // a: one and a half top-ups of free margin; b: none.
    ctx.positions
        .put_native_balance(
            &a,
            &NativeBalance {
                available: extra + extra / FixedPoint::from_raw(2 * FixedPoint::SCALE),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    ctx.positions
        .put_native_balance(
            &b,
            &NativeBalance {
                available: FixedPoint::ZERO,
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    let r = Round {
        n_markets: 2,
        cfgs: vec![None, None],
        bids: vec![Some(fp(100)), Some(fp(100))],
        listed: vec![true, true],
        index: vec![1, 2, 3, 4],
        sender: vec![a, b, a, b],
        params: vec![sell(1), sell(2), sell(2), sell(1)],
        reserved: vec![own; 4],
        candidate: vec![true; 4],
        basis: HashMap::default(),
        pools: [(a, (3, FixedPoint::ZERO)), (b, (3, FixedPoint::ZERO))]
            .into_iter()
            .collect(),
        excess: HashMap::default(),
        precache: vec![],
        arm: None,
    };
    let slow = r.run(&ctx, &backend, false);
    assert_eq!(
        slow.counts,
        [1, 1, 2],
        "a: full then partial; b: none twice"
    );
    assert_eq!(slow.dirty, vec![a]);
    assert_eq!(
        slow.rows.iter().find(|row| row.0 == b).map(|row| row.3),
        Some(false),
        "b read, clean"
    );
    assert_eq!(r.run(&ctx, &backend, true), slow);
    // b granted in the last run: the run after the last sender change is set.
    ctx.positions
        .put_native_balance(
            &b,
            &NativeBalance {
                available: fp(1_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    let slow = r.run(&ctx, &backend, false);
    assert_eq!(slow.counts, [3, 1, 0]);
    assert_eq!(slow.dirty, vec![a, b]);
    assert_eq!(r.run(&ctx, &backend, true), slow);
}

/// The fault path: the run's first candidate's read fails once (latched,
/// skipped), its second candidate reads again and is granted, as today.
#[test]
fn fast_top_ups_retry_after_a_transient_read_fault() {
    let (_dir, backend, ctx) = fail_ctx();
    let a = trader(9_001, 0);
    ctx.positions
        .put_native_balance(
            &a,
            &NativeBalance {
                available: fp(1_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    let sell = |m: MarketId| PlaceOrderParams {
        market_id: m,
        is_buy: false,
        price: fp(90),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let own = NativeExecutor::reserve_for_qty_cfg(None, fp(90), fp(1));
    let r = Round {
        n_markets: 1,
        cfgs: vec![None],
        bids: vec![Some(fp(100))],
        listed: vec![true],
        index: vec![5, 9],
        sender: vec![a, a],
        params: vec![sell(1), sell(1)],
        reserved: vec![own; 2],
        candidate: vec![true; 2],
        basis: HashMap::default(),
        pools: HashMap::default(),
        excess: HashMap::default(),
        precache: vec![],
        arm: Some((a, 1)),
    };
    let slow = r.run(&ctx, &backend, false);
    assert_eq!(
        (slow.counts, slow.hits, slow.dirty.clone()),
        ([1, 0, 0], 1, vec![a])
    );
    assert!(
        slow.fault
            .as_deref()
            .is_some_and(|f| f.starts_with("sell top-up balance read")),
        "{:?}",
        slow.fault
    );
    assert_eq!(r.run(&ctx, &backend, true), slow);
}

/// The stitch records exactly the scan's candidates: on 80 real Phase-2
/// runs (the `option_b_fold_pool_tests` shapes, serial fold and 2-worker
/// sharded prepare), the list built from `stitch_outcome`'s returns is
/// [`cands_of`] the prepared batches, and the top-ups from it equal the
/// scan's on the fold's own cache, pools and excess.
#[test]
fn stitch_records_the_scan_candidates_on_real_phase2_runs() {
    let mut rng = Lcg(0x5717_C4ED);
    let (mut listed, mut granted) = (0usize, 0u64);
    for round in 0..80u64 {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut ctx = NativeExecContext::new(
            db,
            9,
            10_000,
            0,
            1_000,
            10,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
        );
        let n_senders = 2 + rng.below(6);
        let n_markets = 2 + rng.below(4);
        let senders: Vec<Address> = (0..n_senders)
            .map(|i| Address::new([10 + i as u8; 20]))
            .collect();
        for s in &senders {
            let available = fp([0, 30, 60, 120, 1_000_000][rng.below(5) as usize]);
            ctx.positions
                .put_native_balance(
                    s,
                    &NativeBalance {
                        available,
                        order_margin: FixedPoint::ZERO,
                    },
                )
                .unwrap();
        }
        let maker = Address::new([200; 20]);
        for m in 1..=n_markets {
            if !rng.one_in(4) {
                let mut b = OrderBook::new(m, FixedPoint::ONE, FixedPoint::ONE);
                let bid = PlaceOrderParams {
                    market_id: m,
                    is_buy: true,
                    price: fp(95 + rng.below(15) as i64),
                    quantity: fp(5),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: None,
                };
                b.place_order(bid, maker, 1);
                ctx.insert_order_book(m, b);
            }
        }
        let orders: Vec<(Address, PlaceOrderParams)> = (0..4 + rng.below(40))
            .map(|_| {
                let s = senders[rng.below(n_senders) as usize];
                let price = fp(85 + rng.below(30) as i64);
                let mut p = PlaceOrderParams {
                    market_id: 1 + rng.below(n_markets),
                    is_buy: rng.one_in(4),
                    price,
                    quantity: fp(1 + rng.below(6) as i64),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: None,
                };
                match rng.below(10) {
                    0 => p.order_type = OrderType::Market,
                    1 => p.time_in_force = TimeInForce::IOC,
                    2 => p.time_in_force = TimeInForce::FOK,
                    3 => p.time_in_force = TimeInForce::PostOnly,
                    4 => p.reduce_only = true,
                    _ => {}
                }
                (s, p)
            })
            .collect();
        let place_orders: Vec<(usize, Address, &PlaceOrderParams)> = orders
            .iter()
            .enumerate()
            .map(|(i, (s, p))| (i, *s, p))
            .collect();
        let basis = NativeExecutor::phase2_reservation_basis(&ctx, &place_orders);
        let open_at_start = HashMap::default();
        let reader = AccountReader::of(&ctx);
        let markets =
            NativeExecutor::phase2_markets(&ctx.order_books, &ctx.state, &reader, &place_orders);

        let pipeline = |sharded: bool, fast: bool| -> (Outcome, usize) {
            let mut batches: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::default();
            let mut results: Vec<NativeActionResult> = (0..orders.len())
                .map(|_| NativeActionResult::ok("pending", 0))
                .collect();
            let mut next_id = 1u128;
            let mut cands: Option<Vec<TopUpCand>> = Some(Vec::new());
            let out = if sharded {
                let mut groups: Vec<(Address, Vec<(usize, &PlaceOrderParams)>)> = Vec::new();
                for (i, (s, p)) in orders.iter().enumerate() {
                    match groups.iter_mut().find(|g| g.0 == *s) {
                        Some(g) => g.1.push((i, p)),
                        None => groups.push((*s, vec![(i, p)])),
                    }
                }
                let (mut outcomes, out) = NativeExecutor::phase2_parallel_prepare(
                    &reader,
                    &open_at_start,
                    &basis,
                    &markets,
                    &groups,
                    2,
                    orders.len(),
                    false,
                )
                .expect("no worker panic");
                for (i, (s, p)) in orders.iter().enumerate() {
                    let o = outcomes[i].take().unwrap();
                    let stitched = NativeExecutor::stitch_outcome(
                        &mut next_id,
                        &None,
                        &markets,
                        &mut batches,
                        &mut results,
                        i,
                        s,
                        p,
                        o,
                    );
                    TopUpCand::record(&mut cands, stitched, i, s, p, &basis);
                }
                out
            } else {
                let mut fold = SenderFold::default();
                for (i, (s, p)) in orders.iter().enumerate() {
                    let o = NativeExecutor::prepare_one(
                        &reader,
                        &open_at_start,
                        &basis,
                        &markets,
                        &mut fold,
                        i,
                        s,
                        p,
                        false,
                    );
                    let stitched = NativeExecutor::stitch_outcome(
                        &mut next_id,
                        &None,
                        &markets,
                        &mut batches,
                        &mut results,
                        i,
                        s,
                        p,
                        o,
                    );
                    TopUpCand::record(&mut cands, stitched, i, s, p, &basis);
                }
                fold.finish()
            };
            let cands = cands.expect("recording");
            assert_eq!(
                cands,
                cands_of(&batches, &basis),
                "round {round}: sharded={sharded}"
            );
            let n = cands.len();
            let FoldOut {
                cache,
                pools,
                excess,
            } = out;
            let mut bal_cache = BalanceCache::new();
            bal_cache.merge_disjoint(cache);
            let fault = std::sync::OnceLock::new();
            let counts = if fast {
                NativeExecutor::sell_top_ups_fast(
                    &ctx.positions,
                    None,
                    &fault,
                    &markets,
                    &pools,
                    &excess,
                    &mut batches,
                    &mut bal_cache,
                    cands,
                )
            } else {
                NativeExecutor::sell_top_ups(
                    &ctx.positions,
                    None,
                    &fault,
                    &markets,
                    &basis,
                    &pools,
                    &excess,
                    &mut batches,
                    &mut bal_cache,
                )
            };
            let mut dirty = bal_cache.dirty.clone();
            dirty.sort_unstable(); // sharded merge order differs; flush sorts
            let o = Outcome {
                orders: orders_of(&batches),
                counts,
                rows: cache_rows(&bal_cache),
                dirty,
                fault: fault.get().cloned(),
                hits: 0,
            };
            (o, n)
        };
        let (golden, n) = pipeline(false, false);
        for (sharded, fast) in [(false, true), (true, false), (true, true)] {
            assert_eq!(
                pipeline(sharded, fast).0,
                golden,
                "round {round}: sharded={sharded} fast={fast}"
            );
        }
        // The flag off records nothing.
        let mut none: Option<Vec<TopUpCand>> = None;
        TopUpCand::record(
            &mut none,
            Some((0, FixedPoint::ONE)),
            0,
            &senders[0],
            &orders[0].1,
            &basis,
        );
        assert!(none.is_none());
        listed += n;
        granted += golden.counts[0] + golden.counts[1];
    }
    assert!(
        listed > 200 && granted > 50,
        "non-vacuous: {listed} candidates, {granted} granted"
    );
}

/// Part b: on random book outcomes (maker margin cancels, reduce-only cuts,
/// sell and buy cuts at every tick distance, zero ticks, prepared lists
/// shorter than the results), the batched loop leaves every counter of a
/// fresh `Metrics` exactly where `record_margin_cuts` per order leaves it.
#[test]
fn batched_margin_cut_counters_equal_record_margin_cuts() {
    let mut rng = Lcg(0xC0_47E5);
    let counters = |m: &torus_telemetry::Metrics| -> Vec<u64> {
        let mut v = vec![m.maker_margin_cancels.get(), m.reduce_only_cuts.get()];
        v.extend(
            m.sell_margin_cuts
                .iter()
                .flatten()
                .flatten()
                .map(|c| c.get()),
        );
        v
    };
    let cut = |rng: &mut Lcg| ReduceOnlyCut {
        order_id: 7,
        trader: Address::ZERO,
        price: fp(1),
        qty: cents(1 + rng.below(100) as i64),
    };
    let (slow_all, fast_all) = (
        torus_telemetry::Metrics::new(),
        torus_telemetry::Metrics::new(),
    );
    for round in 0..300 {
        let n = rng.below(30) as usize;
        let tick = [FixedPoint::ONE, cents(1), cents(50), FixedPoint::ZERO][rng.below(4) as usize];
        let params: Vec<PlaceOrderParams> = (0..n)
            .map(|_| PlaceOrderParams {
                market_id: 1,
                is_buy: rng.one_in(3),
                price: fp(100),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            })
            .collect();
        let prepared: Vec<PreparedOrder<'_>> = params
            .iter()
            .take(n.saturating_sub(if rng.one_in(5) {
                rng.below(3) as usize
            } else {
                0
            }))
            .enumerate()
            .map(|(k, params)| PreparedOrder {
                index: k,
                sender: Address::ZERO,
                params,
                order_id: k as u128,
                margin_reserved: FixedPoint::ZERO,
                checked_pos_net: None,
                pre_pos: None,
                top_up_candidate: rng.one_in(2),
                res_price: cents(9_000 + rng.below(2_000) as i64),
            })
            .collect();
        let results: Vec<MatchResult> = (0..n)
            .map(|k| {
                let res = prepared.get(k).map_or(fp(100), |p| p.res_price);
                let fills = (0..rng.below(3))
                    .map(|_| Fill {
                        maker_order_id: 1,
                        taker_order_id: 2,
                        price: fp(100),
                        quantity: fp(1),
                        maker: Address::ZERO,
                        taker: Address::ZERO,
                        maker_side: Side::Buy,
                        timestamp: 0,
                    })
                    .collect();
                let margin_cut_price = (!rng.one_in(3)).then(|| {
                    let step = tick.raw().max(1);
                    let ticks = rng.below(45) as i128 - 4;
                    FixedPoint::from_raw(res.raw() + ticks * step + rng.below(3) as i128 - 1)
                });
                MatchResult {
                    sender: Address::ZERO,
                    order_id: k as u128,
                    result: PlaceResult {
                        order_id: k as u128,
                        status: OrderStatus::Cancelled,
                        fills,
                        self_trade_cancels: vec![],
                        rested_qty: FixedPoint::ZERO,
                        reduce_only_cuts: (0..rng.below(3)).map(|_| cut(&mut rng)).collect(),
                        triggered_stops: vec![],
                        margin_cancels: (0..rng.below(4)).map(|_| cut(&mut rng)).collect(),
                        margin_cut_price,
                        reject: None,
                    },
                }
            })
            .collect();
        let (slow, fast) = (
            torus_telemetry::Metrics::new(),
            torus_telemetry::Metrics::new(),
        );
        for m in [&slow, &slow_all] {
            for (r, p) in results.iter().zip(&prepared) {
                NativeExecutor::record_margin_cuts(
                    m,
                    p.params.is_buy,
                    &r.result,
                    p.res_price,
                    tick,
                    p.top_up_candidate,
                );
            }
        }
        for m in [&fast, &fast_all] {
            NativeExecutor::record_margin_cuts_batched(m, &results, &prepared, tick);
        }
        assert_eq!(counters(&fast), counters(&slow), "round {round}");
    }
    let all = counters(&slow_all);
    assert_eq!(counters(&fast_all), all);
    assert!(
        all[0] > 500 && all[1] > 300 && all[2..].iter().filter(|&&c| c > 0).count() >= 20,
        "non-vacuous: {all:?}"
    );
}

/// s58 bundle: on by default (unset or empty); `0` or `false` (any case)
/// turns it off; an invalid value is the default (on).
#[test]
fn fast_sell_top_ups_toggle_parse() {
    use crate::native_executor::settle_defer_results_flag_tests::{INVALID, OFF, ON};
    assert!(parse_fast_sell_top_ups(None));
    for v in ON.into_iter().chain(INVALID) {
        assert!(parse_fast_sell_top_ups(Some(v)), "{v:?}");
    }
    for v in OFF {
        assert!(!parse_fast_sell_top_ups(Some(v)), "{v:?}");
    }
}

/// The per-thread pin wins over the env, nests, and is restored on exit.
#[test]
fn fast_sell_top_ups_pin_nests_and_restores() {
    with_fast_sell_top_ups(true, || {
        assert!(fast_sell_top_ups_enabled());
        with_fast_sell_top_ups(false, || assert!(!fast_sell_top_ups_enabled()));
        assert!(fast_sell_top_ups_enabled());
    });
    assert_eq!(FAST_SELL_TOP_UPS_PIN.with(|c| c.get()), None);
}

/// `execute_batch` routes by the flag (read once per call): pinned off, the
/// scan and the per-order counter loop run and nothing is recorded; pinned
/// on, the stitch list and the batched loop run instead — same state.
#[test]
fn execute_batch_routes_top_ups_by_the_flag() {
    let (t, b, mk) = (
        Address::new([1; 20]),
        Address::new([2; 20]),
        Address::new([3; 20]),
    );
    let gtc = |m: MarketId, is_buy: bool, price: i64| PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let run = |fast: bool| {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = NativeExecContext::new(
            StateDb::open(dir.path()).unwrap(),
            1,
            1_000,
            0,
            100,
            10,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
        );
        ctx.metrics = Some(Arc::new(torus_telemetry::Metrics::new()));
        for a in [t, b, mk] {
            ctx.positions
                .put_native_balance(
                    &a,
                    &NativeBalance {
                        available: fp(1_000),
                        order_margin: FixedPoint::ZERO,
                    },
                )
                .unwrap();
        }
        FAST_TOP_UP_PATHS.with(|c| c.set((0, 0)));
        let b1 = vec![(b, NativeAction::PlaceOrder(gtc(1, true, 100)))];
        // t pools in market 2 (its first checked taker), then sells into
        // market 1's bid: a top-up candidate.
        let b2 = vec![
            (t, NativeAction::PlaceOrder(gtc(2, false, 200))),
            (t, NativeAction::PlaceOrder(gtc(1, false, 90))),
        ];
        let roots = with_fast_sell_top_ups(fast, || {
            [b1, b2]
                .iter()
                .map(|batch| {
                    NativeExecutor::execute_batch_engine_mode(&mut ctx, batch, 0);
                    assert!(ctx.fatal_error.is_none());
                    crate::state_root::compute_native_state_root(&ctx.state).unwrap()
                })
                .collect::<Vec<_>>()
        });
        let m = ctx.metrics.clone().unwrap();
        (
            roots,
            m.sell_top_ups_full.get(),
            ctx.phase_accum.sell_top_ups,
            FAST_TOP_UP_PATHS.with(|c| c.get()),
        )
    };
    let (slow_roots, slow_full, slow_accum, slow_paths) = run(false);
    let (fast_roots, fast_full, fast_accum, fast_paths) = run(true);
    assert_eq!(slow_paths, (0, 0), "flag off: today's scan and loop");
    assert!(
        fast_paths.0 >= 2 && fast_paths.1 >= 1,
        "flag on: the fast paths ({fast_paths:?})"
    );
    assert_eq!(
        (slow_full, slow_accum),
        (1, 1),
        "the candidate is topped up in full"
    );
    assert_eq!(
        (fast_roots, fast_full, fast_accum),
        (slow_roots, slow_full, slow_accum)
    );
}
