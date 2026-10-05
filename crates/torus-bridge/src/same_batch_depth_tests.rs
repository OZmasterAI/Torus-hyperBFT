//! PF1 (item 6): `same_batch_bid_top_ups` reads the resting ask depth through
//! `AskDepth` (each ask level summed once per market per batch) instead of
//! walking every resting ask ORDER per crossing bid. Differential tests
//! against a verbatim copy of the pre-PF1 function (`old_top_ups`, the
//! per-order walk) on seeded random books and batches: same return value,
//! same per-order `margin_reserved`, same balance cache.

use super::*;
use torus_core::order_book::Order;
use torus_state::{AtomicWriteOp, StateError};

/// Balances come from the pre-filled cache; a miss reads ZERO.
#[derive(Clone)]
struct EmptyBackend;

impl StateBackend for EmptyBackend {
    fn get_cf_raw(&self, _: &str, _: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        Ok(None)
    }
    fn put_cf_raw(&self, _: &str, _: &[u8], _: &[u8]) -> Result<(), StateError> {
        unreachable!()
    }
    fn delete_cf_raw(&self, _: &str, _: &[u8]) -> Result<(), StateError> {
        unreachable!()
    }
    fn iterate_cf(&self, _: &str, _: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        unreachable!()
    }
    fn atomic_write(&self, _: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        unreachable!()
    }
}

/// The pre-P4(b) `can_rest_shape` (d52a33f), verbatim: P4(b) dropped the
/// tick and lot clauses, which fix A's Phase-2 reject made redundant.
fn old_can_rest_shape(o: &PlaceOrderParams, book: &OrderBook) -> bool {
    matches!(o.order_type, OrderType::Limit)
        && matches!(o.time_in_force, TimeInForce::GTC | TimeInForce::PostOnly)
        && !o.reduce_only
        && o.price > FixedPoint::ZERO
        && (book.tick_size <= FixedPoint::ZERO || o.price.raw() % book.tick_size.raw() == 0)
        && o.quantity >= book.lot_size
}

/// The pre-PF1 `same_batch_bid_top_ups` (d52a33f), verbatim: the oracle.
#[allow(clippy::too_many_arguments)]
fn old_top_ups<T: StateBackend>(
    positions: &PositionManager<T>,
    books: &HashMap<MarketId, OrderBook>,
    margin_configs: &HashMap<MarketId, MarketMarginConfig>,
    basis: &HashMap<usize, (FixedPoint, FixedPoint)>,
    pool_takers: &[(Address, MarketId, FixedPoint)],
    excess_by_sender: &HashMap<Address, FixedPoint>,
    market_batches: &mut HashMap<MarketId, Vec<PreparedOrder<'_>>>,
    bal_cache: &mut BalanceCache,
) -> u64 {
    let sat_add = |a: FixedPoint, b: FixedPoint| FixedPoint::from_raw(a.raw().saturating_add(b.raw()));
    let mut topped_up = 0;
    let pool_of: HashMap<Address, (MarketId, FixedPoint)> =
        pool_takers.iter().map(|&(s, m, pos_net)| (s, (m, pos_net))).collect();
    let mut wanted: Vec<(usize, MarketId, usize, FixedPoint)> = Vec::new();
    for (&market_id, batch) in market_batches.iter() {
        let Some(book) = books.get(&market_id) else { continue };
        let Some(ask) = book.best_ask() else { continue };
        let mut bound: Option<FixedPoint> = None;
        let mut batch_asks: std::collections::BTreeMap<FixedPoint, FixedPoint> = std::collections::BTreeMap::new();
        for (k, p) in batch.iter().enumerate() {
            let o = p.params;
            if !o.is_buy {
                if old_can_rest_shape(o, book) {
                    let q = batch_asks.entry(o.price).or_insert(FixedPoint::ZERO);
                    *q = sat_add(*q, o.quantity);
                }
                if let Some(b) = bound {
                    if NativeExecutor::takes_bid_floor(o) && pool_of.get(&p.sender).is_some_and(|(m, _)| *m != market_id) {
                        wanted.push((p.index, market_id, k, b));
                    }
                }
                continue;
            }
            if !old_can_rest_shape(o, book) {
                continue;
            }
            let lowest_ask = batch_asks.keys().next().map_or(ask, |&a| a.min(ask));
            let rests = if o.price < lowest_ask {
                true
            } else if o.time_in_force == TimeInForce::PostOnly {
                false
            } else {
                let book_depth = book
                    .ask_queues()
                    .take_while(|(price, _)| **price <= o.price)
                    .flat_map(|(_, q)| q.iter())
                    .fold(FixedPoint::ZERO, |acc, a| sat_add(acc, a.remaining_qty));
                let depth = batch_asks.range(..=o.price).fold(book_depth, |acc, (_, q)| sat_add(acc, *q));
                o.quantity > depth
            };
            if rests {
                bound = bound.max(Some(o.price.min(ask)));
            }
        }
    }
    wanted.sort_unstable_by_key(|w| w.0);
    for (i, market_id, k, bound) in wanted {
        let Some(p) = market_batches.get_mut(&market_id).and_then(|b| b.get_mut(k)) else { continue };
        let res_qty = basis.get(&i).map_or(p.params.quantity, |b| b.1);
        let Ok(need) = NativeExecutor::try_reserve_for_qty_cfg(margin_configs.get(&market_id), bound, res_qty) else {
            continue;
        };
        let extra = need - p.margin_reserved;
        if extra <= FixedPoint::ZERO {
            continue;
        }
        let Ok(mut bal) = bal_cache.load(positions, &p.sender) else { continue };
        let pos_net = pool_of.get(&p.sender).map_or(FixedPoint::ZERO, |v| v.1);
        let excess = excess_by_sender.get(&p.sender).copied().unwrap_or(FixedPoint::ZERO);
        if bal.available + pos_net - excess < extra {
            continue;
        }
        bal.available -= extra;
        bal.order_margin += extra;
        bal_cache.set(&p.sender, bal);
        p.margin_reserved += extra;
        topped_up += 1;
    }
    topped_up
}

/// The per-order walk of the old function: resting ask depth up to `price`.
fn walk_depth(book: &OrderBook, price: FixedPoint) -> FixedPoint {
    book.ask_queues()
        .take_while(|(p, _)| **p <= price)
        .flat_map(|(_, q)| q.iter())
        .fold(FixedPoint::ZERO, |acc, a| FixedPoint::from_raw(acc.raw().saturating_add(a.remaining_qty.raw())))
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

const S: i128 = FixedPoint::SCALE;
const BASE: i128 = 100;

fn units(u: i128) -> FixedPoint {
    FixedPoint::from_raw(u * S)
}

fn addr(n: u64) -> Address {
    let mut b = [0x5Au8; 20];
    b[12..].copy_from_slice(&n.to_be_bytes());
    Address::from(b)
}

/// Tick 1, lot 0.01; loaded orders (any seq) only.
fn new_book(market: MarketId) -> OrderBook {
    let mut book = OrderBook::new(market, units(1), FixedPoint::from_raw(S / 100));
    book.set_next_seq(u64::MAX);
    book
}

fn ask(id: u128, price: FixedPoint, qty: FixedPoint) -> Order {
    Order {
        id,
        trader: addr(1000 + (id % 7) as u64),
        side: Side::Sell,
        price,
        remaining_qty: qty,
        original_qty: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        timestamp: 0,
        reduce_only: false,
        client_order_id: None,
    }
}

/// A book with `levels` ask levels at `BASE + 2k` (k = 0..), each holding
/// `1..=max_depth` orders; `huge` makes some quantities near `i128::MAX / 4`
/// (the depth saturates).
fn random_book(rng: &mut Rng, market: MarketId, levels: u64, max_depth: u64, huge: bool) -> OrderBook {
    let mut book = new_book(market);
    let mut id: u128 = 1;
    for l in 0..levels {
        let price = units(BASE + 2 * l as i128);
        for _ in 0..=rng.below(max_depth) {
            let qty = if huge && rng.chance(20) {
                FixedPoint::from_raw(i128::MAX / 4 - rng.below(1000) as i128)
            } else {
                FixedPoint::from_raw((1 + rng.below(50)) as i128 * S / 10)
            };
            book.insert_loaded_order(ask(id, price, qty), id as u64);
            id += 1;
        }
    }
    book
}

/// A random batch order around the ask levels: below / at / between / above
/// them, buys and sells, GTC / PostOnly / IOC, some market and reduce-only.
fn random_params(rng: &mut Rng, market: MarketId, levels: u64, huge: bool) -> PlaceOrderParams {
    let span = 2 * levels.max(1) as i128 + 6;
    let price = units(BASE - 4 + rng.below(span as u64) as i128);
    let quantity = if huge && rng.chance(10) {
        FixedPoint::from_raw(i128::MAX / 2)
    } else if rng.chance(30) {
        FixedPoint::from_raw((1 + rng.below(4000)) as i128 * S / 10)
    } else {
        FixedPoint::from_raw((1 + rng.below(60)) as i128 * S / 10)
    };
    PlaceOrderParams {
        market_id: market,
        is_buy: rng.chance(55),
        price,
        quantity,
        order_type: if rng.chance(5) { OrderType::Market } else { OrderType::Limit },
        time_in_force: match rng.below(10) {
            0 => TimeInForce::PostOnly,
            1 => TimeInForce::IOC,
            _ => TimeInForce::GTC,
        },
        reduce_only: rng.chance(3),
        client_order_id: None,
    }
}

struct Case {
    books: HashMap<MarketId, OrderBook>,
    configs: HashMap<MarketId, MarketMarginConfig>,
    basis: HashMap<usize, (FixedPoint, FixedPoint)>,
    pool_takers: Vec<(Address, MarketId, FixedPoint)>,
    excess: HashMap<Address, FixedPoint>,
    /// (market, sender, params, margin_reserved), flat index = position.
    orders: Vec<(MarketId, Address, PlaceOrderParams, FixedPoint)>,
    balances: Vec<(Address, NativeBalance)>,
}

const SENDERS: u64 = 12;

fn random_case(seed: u64) -> Case {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let markets = 1 + rng.below(4);
    let huge = rng.chance(25);
    let mut books = HashMap::new();
    let mut levels_of = HashMap::new();
    for m in 1..=markets {
        // Shapes: empty book, missing book, one deep level, many shallow levels.
        let (levels, depth) = match rng.below(5) {
            0 => (0, 1),
            1 => (1, 300),
            2 => (1 + rng.below(40), 3),
            _ => (1 + rng.below(6), 1 + rng.below(80)),
        };
        levels_of.insert(m, levels);
        if !rng.chance(8) {
            books.insert(m, random_book(&mut rng, m, levels, depth, huge));
        }
    }
    let mut configs = HashMap::new();
    for m in 1..=markets {
        if rng.chance(50) {
            configs.insert(m, MarketMarginConfig::new(m, 1 + rng.below(50) as u32));
        }
    }
    let n = rng.below(60) as usize;
    let mut orders = Vec::with_capacity(n);
    let mut basis = HashMap::new();
    for i in 0..n {
        let m = 1 + rng.below(markets);
        let sender = addr(rng.below(SENDERS));
        let params = random_params(&mut rng, m, levels_of[&m], huge);
        let reserved = FixedPoint::from_raw(rng.below(200) as i128 * S / 10);
        if rng.chance(15) {
            basis.insert(i, (FixedPoint::ZERO, FixedPoint::from_raw((1 + rng.below(30)) as i128 * S / 10)));
        }
        orders.push((m, sender, params, reserved));
    }
    let mut pool_takers = Vec::new();
    let mut excess = HashMap::new();
    let mut balances = Vec::new();
    for s in 0..SENDERS {
        if rng.chance(70) {
            let net = FixedPoint::from_raw(rng.below(500) as i128 * S - 100 * S);
            pool_takers.push((addr(s), 1 + rng.below(markets + 1), net));
        }
        if rng.chance(30) {
            excess.insert(addr(s), FixedPoint::from_raw(rng.below(300) as i128 * S));
        }
        if rng.chance(85) {
            let available = FixedPoint::from_raw(rng.below(20_000) as i128 * S);
            balances.push((addr(s), NativeBalance { available, order_margin: FixedPoint::ZERO }));
        }
    }
    Case { books, configs, basis, pool_takers, excess, orders, balances }
}

/// Runs `f` on a fresh copy of the case's mutable inputs; returns
/// (top-ups, per-order margin_reserved, cached balances, dirty order).
fn run(
    case: &Case,
    f: impl FnOnce(
        &PositionManager<EmptyBackend>,
        &mut HashMap<MarketId, Vec<PreparedOrder<'_>>>,
        &mut BalanceCache,
    ) -> u64,
) -> (u64, Vec<(usize, FixedPoint)>, Vec<(Address, FixedPoint, FixedPoint)>, Vec<Address>) {
    let positions = PositionManager::new(EmptyBackend);
    let mut batches: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::new();
    for (i, (m, sender, params, reserved)) in case.orders.iter().enumerate() {
        batches.entry(*m).or_default().push(PreparedOrder {
            index: i,
            sender: *sender,
            params,
            order_id: i as u128 + 1,
            margin_reserved: *reserved,
            checked_pos_net: None,
            excess_im: FixedPoint::ZERO,
        });
    }
    let mut cache = BalanceCache::new();
    for (a, b) in &case.balances {
        cache.map.insert(*a, CachedBalance { balance: b.clone(), dirty: false });
    }
    let n = f(&positions, &mut batches, &mut cache);
    let mut reserved: Vec<(usize, FixedPoint)> =
        batches.values().flatten().map(|p| (p.index, p.margin_reserved)).collect();
    reserved.sort_unstable_by_key(|r| r.0);
    let mut bals: Vec<(Address, FixedPoint, FixedPoint)> =
        cache.map.iter().map(|(a, c)| (*a, c.balance.available, c.balance.order_margin)).collect();
    bals.sort_unstable_by_key(|b| b.0);
    (n, reserved, bals, cache.dirty)
}

fn run_both(case: &Case) -> u64 {
    let old = run(case, |pm, b, c| {
        old_top_ups(pm, &case.books, &case.configs, &case.basis, &case.pool_takers, &case.excess, b, c)
    });
    let new = run(case, |pm, b, c| {
        NativeExecutor::same_batch_bid_top_ups(
            pm, &case.books, &case.configs, &case.basis, &case.pool_takers, &case.excess, b, c,
        )
    });
    assert_eq!(new, old);
    old.0
}

#[test]
fn ask_depth_equals_the_per_order_walk_on_random_books() {
    for seed in 0..400u64 {
        let mut rng = Rng(seed.wrapping_mul(0xD6E8_FEB8_6659_FD93) | 1);
        let levels = rng.below(30);
        let (depth, huge) = (1 + rng.below(120), rng.chance(30));
        let book = random_book(&mut rng, 1, levels, depth, huge);
        let mut depth = AskDepth::new(book.ask_queues());
        // Queries in any order: below, at, between and above the levels.
        for _ in 0..40 {
            let price = FixedPoint::from_raw((BASE - 3 + rng.below(2 * levels + 8) as i128) * S + rng.below(3) as i128);
            assert_eq!(depth.upto(price), walk_depth(&book, price), "seed {seed} price {price}");
        }
    }
}

#[test]
fn ask_depth_saturates_like_the_walk() {
    let mut book = new_book(1);
    let big = FixedPoint::from_raw(i128::MAX / 3);
    let mut id = 1u128;
    for (price, qty) in [(BASE, big), (BASE, big), (BASE + 1, big), (BASE + 1, units(5)), (BASE + 2, units(1))] {
        book.insert_loaded_order(ask(id, units(price), qty), id as u64);
        id += 1;
    }
    let mut depth = AskDepth::new(book.ask_queues());
    for p in [BASE + 2, BASE - 1, BASE, BASE + 1, BASE + 5] {
        assert_eq!(depth.upto(units(p)), walk_depth(&book, units(p)), "price {p}");
    }
    assert_eq!(depth.upto(units(BASE + 1)), FixedPoint::MAX);
}

/// Deterministic deep level: 50_000 asks of 1 at one price (the prof10
/// cells' best ask). The depth is summed once, at the first crossing bid;
/// a bid of exactly the depth does not rest, one lot more does.
#[test]
fn deep_level_is_summed_once_and_decides_like_the_walk() {
    let deep_book = || {
        let mut book = new_book(7);
        for id in 1..=50_000u128 {
            book.insert_loaded_order(ask(id, units(BASE), units(1)), id as u64);
        }
        book.insert_loaded_order(ask(50_001, units(BASE + 1), units(3)), 50_001);
        book
    };
    let book = deep_book();
    let mut depth = AskDepth::new(book.ask_queues());
    assert_eq!(depth.upto(units(BASE - 1)), FixedPoint::ZERO);
    assert!(depth.through.is_empty(), "nothing summed below the best ask");
    for _ in 0..1_000 {
        assert_eq!(depth.upto(units(BASE)), units(50_000));
    }
    assert_eq!(depth.through.len(), 1, "the deep level was summed once for 1000 bids");
    assert_eq!(depth.upto(units(BASE + 1)), units(50_003));
    assert_eq!(depth.through.len(), 2);

    // Whole function: a funded non-pool sell after each bid sees a bound only
    // when that bid rests (qty > depth 50_000).
    let s = addr(1);
    let p = |is_buy: bool, qty: i128| PlaceOrderParams {
        market_id: 7,
        is_buy,
        price: units(BASE),
        quantity: units(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    for (bid_qty, want) in [(50_000, 0), (50_001, 1)] {
        let case = Case {
            books: HashMap::from([(7, deep_book())]),
            configs: HashMap::new(),
            basis: HashMap::new(),
            pool_takers: vec![(s, 8, FixedPoint::ZERO)],
            excess: HashMap::new(),
            orders: vec![(7, addr(2), p(true, bid_qty), FixedPoint::ZERO), (7, s, p(false, 1), FixedPoint::ZERO)],
            balances: vec![(s, NativeBalance { available: units(1_000_000), order_margin: FixedPoint::ZERO })],
        };
        assert_eq!(run_both(&case), want, "bid {bid_qty}");
    }
}

#[test]
fn top_ups_equal_the_per_order_walk_on_random_books_and_batches() {
    let mut total = 0;
    for seed in 0..3_000u64 {
        total += run_both(&random_case(seed));
    }
    // Non-vacuous: the random batches reach the top-up path.
    assert!(total > 100, "top-ups across all seeds: {total}");
}
