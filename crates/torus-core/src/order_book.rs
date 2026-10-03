//! Order book matching engine — price-time priority CLOB.
//!
//! Central Limit Order Book with:
//! - Price-time priority matching (best price first, FIFO within level)
//! - Order types: Limit, Market, StopMarket, StopLimit, PostOnly
//! - Time-in-force: GTC, IOC, FOK
//! - O(1) cancel via order index, per-trader index for cancel-all
//! - Self-trade prevention (cancel-resting)
//! - All arithmetic via FixedPoint (no f64)
//! - Deterministic: same input sequence → same state

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{self, Read as IoRead, Write as IoWrite};

use borsh::{BorshDeserialize, BorshSerialize};
use torus_types::{
    Address, FixedPoint, MarketId, OrderId, OrderType, PlaceOrderParams, Side, TimeInForce,
};

use crate::error::CoreError;
use crate::position::{borsh_read_address, borsh_read_fp, borsh_write_address, borsh_write_fp};

mod cancel_batch;
mod trader_orders;

use trader_orders::TraderOrders;

#[cfg(test)]
mod matching_entry_tests;

/// Open orders (resting + pending stops) of each sender in `senders` (value =
/// its index in the result), summed over `books`. The walk costs, per book,
/// min(its traders, senders) probes plus its stops; it runs on scoped
/// workers, one per `min_work_per_thread` of that (0 = always `max_threads`),
/// at most `max_threads`, each on a chunk of books. The chunk counts are
/// added: an integer sum, so the split never changes a count.
pub fn open_order_counts(
    books: &[&OrderBook],
    senders: &HashMap<Address, usize>,
    max_threads: usize,
    min_work_per_thread: usize,
) -> Vec<usize> {
    let count = |chunk: &[&OrderBook]| {
        let mut counts = vec![0usize; senders.len()];
        for book in chunk {
            book.add_open_order_counts(senders, &mut counts);
        }
        counts
    };
    let work: usize = books
        .iter()
        .map(|b| b.trader_orders.len().min(senders.len()) + b.pending_stops.len())
        .sum();
    let threads = match work.checked_div(min_work_per_thread) {
        Some(n) => n.min(max_threads),
        None => max_threads,
    };
    if threads < 2 || books.len() < 2 {
        return count(books);
    }
    std::thread::scope(|s| {
        let workers: Vec<_> = books
            .chunks(books.len().div_ceil(threads))
            .map(|chunk| s.spawn(move || count(chunk)))
            .collect();
        let mut total = vec![0usize; senders.len()];
        for worker in workers {
            let counts = worker.join().expect("open-order count worker panicked");
            for (t, n) in total.iter_mut().zip(counts) {
                *t += n;
            }
        }
        total
    })
}

// ============================================================================
// Types
// ============================================================================

/// An order resting on the book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub id: OrderId,
    pub trader: Address,
    pub side: Side,
    pub price: FixedPoint,
    pub remaining_qty: FixedPoint,
    pub original_qty: FixedPoint,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub timestamp: u64,
    pub reduce_only: bool,
    pub client_order_id: Option<u64>,
}

/// A fill (execution) between a maker and taker order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fill {
    pub maker_order_id: OrderId,
    pub taker_order_id: OrderId,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    pub maker: Address,
    pub taker: Address,
    pub maker_side: Side,
    pub timestamp: u64,
}

/// Result of placing an order.
#[derive(Clone, Debug)]
pub struct PlaceResult {
    pub order_id: OrderId,
    pub status: OrderStatus,
    pub fills: Vec<Fill>,
    /// A5: resting maker orders auto-cancelled by self-trade prevention,
    /// captured WHOLE (not just the id) so the executor can release the
    /// cancelled maker's remaining order-margin reservation
    /// (`price × remaining_qty` at cancel time).
    pub self_trade_cancels: Vec<Order>,
    /// s515: quantity of THIS order still holding a reservation after
    /// placement — the remainder put on the book (Resting / PartiallyFilled),
    /// the whole stop quantity (PendingTrigger), else zero. The executor's
    /// taker release is `reserve(qty) - reserve(rested_qty)`, which stays
    /// exact when a reduce-only clamp shrank the order below `params.quantity`.
    pub rested_qty: FixedPoint,
    /// s515 (reduce-only): quantity removed from resting reduce-only orders
    /// (makers cut at match time, or shrunk / cancelled by the post-fill
    /// sweep) so they can never open or increase a position. The executor
    /// releases these like maker consumption.
    pub reduce_only_cuts: Vec<ReduceOnlyCut>,
    /// s515: stop orders whose trigger fired on this order's fills. They are
    /// removed from the book's pending set but NOT executed here — the
    /// executor places each one through its normal placement path (price
    /// cap, reduce-only re-check against the position AT TRIGGER TIME,
    /// margin, settlement of its fills).
    pub triggered_stops: Vec<TriggeredStop>,
    /// F1 (s517 #4, HL `marginCanceled`): resting makers cancelled WHOLE at
    /// match time because their account could not afford the fill. Released
    /// by the executor exactly like reduce-only cuts (A5 telescoping).
    pub margin_cancels: Vec<ReduceOnlyCut>,
}

impl PlaceResult {
    fn rejected(order_id: OrderId) -> Self {
        PlaceResult {
            order_id,
            status: OrderStatus::Rejected,
            fills: vec![],
            self_trade_cancels: vec![],
            rested_qty: FixedPoint::ZERO,
            reduce_only_cuts: vec![],
            triggered_stops: vec![],
            margin_cancels: vec![],
        }
    }
}

/// s515: quantity cut from a resting reduce-only order (see
/// [`PlaceResult::reduce_only_cuts`]). `price` is the order's resting price —
/// its margin reservation is `reserve(price, remaining)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReduceOnlyCut {
    pub order_id: OrderId,
    pub trader: Address,
    pub price: FixedPoint,
    pub qty: FixedPoint,
}

/// s515: a stop whose trigger fired; `params` is the order to place (Market
/// capped at the stop's price cap, or Limit at the stop-limit price) and
/// `id` the stop's own order id, which the placed order keeps.
#[derive(Clone, Debug)]
pub struct TriggeredStop {
    pub id: OrderId,
    pub trader: Address,
    pub params: PlaceOrderParams,
    pub timestamp: u64,
}

/// s515 (review 4, Hyperliquid "margin is checked when orders are placed and
/// again when they match"): the match-time margin limit of ONE taker order,
/// computed by the executor from placement-time data only.
///
/// `budget` = the order's own placement reservation; F1 (s517): the book adds
/// the sender's running free margin from [`AccountMargins`] (absent = 0, i.e.
/// the pre-F1 budget) and charges the IM DELTA of the taker's position at its
/// position-size tier (see `need`). Pre-F1 wording: before each fill the book requires
/// `IM(charged fill notional so far + this fill's charged part + hold_price ×
/// quantity left after it) <= budget` (`IM` =
/// [`crate::margin::order_initial_margin`] with `tiers`); the hold term is
/// the reservation a resting remainder keeps (`hold_price` = the limit of an
/// order that can rest, `None` otherwise). A fill that does not fit is cut to
/// the largest lot multiple that does, and matching stops: the remainder is
/// cancelled (never rests). A FOK order whose complete fill would not fit is
/// rejected whole.
///
/// Review 5 (F2, Hyperliquid: reducing a position needs no margin): the part
/// of a fill that reduces the taker's opposite-side position is FREE — only
/// fill quantity beyond [`reduce_only_allowance`] of its current position
/// (the book's [`ReduceOnlyPositions`] entry, advanced through its own fills;
/// a taker absent from it is treated as flat) is charged. F4: fills and hold
/// form ONE notional at its own tier. For a checked taker every fill is at a
/// price >= `hold_price` (only limit sells carry a hold), so that notional —
/// hence the need — is non-increasing in the fill size inside the free part
/// and non-decreasing beyond it; with the need of a zero fill always fitting
/// (the previous state fitted; initially it is the placement reservation),
/// the quantities that fit are one interval from zero and the binary search
/// in `affordable` is exact.
#[derive(Clone, Debug)]
pub struct TakerMarginLimit {
    pub budget: FixedPoint,
    pub tiers: Option<std::sync::Arc<[crate::margin::MarginTier]>>,
    pub hold_price: Option<FixedPoint>,
}

impl TakerMarginLimit {
    /// F1 (s517): IM delta of this taker's market (position-size tier) after
    /// filling `q` at `price` on top of the fills so far: the position
    /// valued at `px` shrinks by what the fills closed, the opening fills
    /// add their notional, and — for an order that can rest — the part of
    /// the remainder that would OPEN adds `hold_price` × it (resting closing
    /// quantity needs no margin, HL). `free` is the live closing capacity
    /// (the book's position map). `None` on overflow. Without an account
    /// (`px` 0, see [`AccountMargins`]) this is the pre-F1 per-order need
    /// `IM(charged + price × opening + hold)`.
    fn need(
        &self,
        m: &MatchMargin<'_>,
        price: FixedPoint,
        q: FixedPoint,
        free: FixedPoint,
        left_before: FixedPoint,
    ) -> Option<FixedPoint> {
        let closing = q.min(free);
        let closed = m.allowance0 - free + closing;
        let before = m.size0.checked_mul(m.px).ok()?;
        let mut after = (m.size0 - closed)
            .checked_mul(m.px)
            .ok()?
            .checked_add(m.charged)
            .ok()?
            .checked_add(price.checked_mul(q - closing).ok()?)
            .ok()?;
        if let Some(hp) = self.hold_price {
            let open_rest = ((left_before - q) - (free - closing)).max(FixedPoint::ZERO);
            after = after.checked_add(hp.checked_mul(open_rest).ok()?).ok()?;
        }
        Some(crate::margin::im_delta(self.tiers.as_deref(), before, after))
    }

    /// Largest quantity `<= q` (all of `q`, or a multiple of the lot) that
    /// fits: a purely closing quantity (`<= free`) always does (HL: reducing
    /// needs no margin, even under water); beyond it the need must be
    /// `<= budget`. Beyond the closing part the need is non-decreasing
    /// (checked takers fill at prices >= their hold), so the quantities that
    /// fit are one interval from zero; `lo` = 0 always fits.
    fn affordable(
        &self,
        m: &MatchMargin<'_>,
        price: FixedPoint,
        q: FixedPoint,
        free: FixedPoint,
        left_before: FixedPoint,
    ) -> FixedPoint {
        let fits = |q: FixedPoint| {
            q <= free || self.need(m, price, q, free, left_before).is_some_and(|n| m.fits(n))
        };
        if fits(q) {
            return q;
        }
        let step = if m.lot > FixedPoint::ZERO { m.lot.raw() } else { 1 };
        let (mut lo, mut hi) = (0i128, q.raw() / step + 1);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            let cand = FixedPoint::from_raw(mid * step);
            if cand < q && fits(cand) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        FixedPoint::from_raw(lo * step)
    }
}

/// Running state of a [`TakerMarginLimit`] during one taker's matching.
struct MatchMargin<'a> {
    limit: &'a TakerMarginLimit,
    lot: FixedPoint,
    /// F1 (s517): |position| at the taker's start, the price it is valued
    /// at, and its closing capacity then (opposite-side size; 0 if same side).
    size0: FixedPoint,
    px: FixedPoint,
    allowance0: FixedPoint,
    /// F1: `limit.budget` + the sender's running free margin.
    budget: FixedPoint,
    /// Notional of the charged (non-closing) part of the fills so far.
    charged: FixedPoint,
    exhausted: bool,
    /// B2 (s87): the sender's entry here is a taker-only budget (D2
    /// non-pool market), which gets the makers' rounding allowance.
    taker_only: bool,
}

impl MatchMargin<'_> {
    /// B2 (s87): what a need costs the sender's running free margin beyond
    /// the order's own reservation. Each IM difference is floored, so a
    /// taker adding to a same-side position can need its reservation + 1 raw
    /// (floor(A + x) − floor(A) = floor(x) + 1) at no price improvement —
    /// rounding, not cost. As for makers ([`maker_fill_fits`]), a taker-only
    /// budget does not pay that unit: it was cancelled with 0 fills at its
    /// own limit price for it. Shared accounts (single path, D2 pool market)
    /// are unchanged.
    fn over_reservation(&self, need: FixedPoint) -> FixedPoint {
        let over = need - self.limit.budget;
        if self.taker_only && over == FixedPoint::from_raw(1) {
            FixedPoint::ZERO
        } else {
            over
        }
    }

    /// Whether `need` fits: its part beyond the reservation must fit the
    /// running free margin (`budget − limit.budget`).
    fn fits(&self, need: FixedPoint) -> bool {
        need <= self.budget || self.over_reservation(need) <= self.budget - self.limit.budget
    }
}

/// s515: signed positions (+long / -short) in this book's market of the
/// traders whose reduce-only orders the book must police during a placement
/// (or a batch of placements). Supplied by the executor via
/// [`OrderBook::set_reduce_only_positions`]; the book keeps it current across
/// its own fills. A trader ABSENT from the map is not policed (book-only
/// callers and legacy tests), so the executor must insert every trader with
/// a resting reduce-only order ([`OrderBook::reduce_only_traders`]) plus
/// every reduce-only sender — flat traders as zero. Review 5: also every
/// taker with a [`TakerMarginLimit`], whose closing fills are free; absent,
/// it is charged as if flat (conservative).
#[derive(Clone, Debug, Default)]
pub struct ReduceOnlyPositions {
    positions: BTreeMap<Address, FixedPoint>,
}

impl ReduceOnlyPositions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, trader: Address, signed_size: FixedPoint) {
        self.positions.insert(trader, signed_size);
    }

    pub fn get(&self, trader: &Address) -> Option<FixedPoint> {
        self.positions.get(trader).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    fn apply_fill(&mut self, trader: &Address, is_buy: bool, qty: FixedPoint) {
        if let Some(p) = self.positions.get_mut(trader) {
            if is_buy {
                *p += qty;
            } else {
                *p -= qty;
            }
        }
    }
}

/// s515: how much a reduce-only order on side `is_buy` may still fill against
/// signed position `signed_pos` — the position size when the order reduces
/// it, zero when flat or when the order would increase it.
pub fn reduce_only_allowance(signed_pos: FixedPoint, is_buy: bool) -> FixedPoint {
    if is_buy && signed_pos < FixedPoint::ZERO {
        -signed_pos
    } else if !is_buy && signed_pos > FixedPoint::ZERO {
        signed_pos
    } else {
        FixedPoint::ZERO
    }
}

/// F1 (s517): account-level margin state of ONE book for the current
/// placement / batch — the running free margin (and position valuation
/// price) of each trader whose fills the book must check. Installed by the
/// executor, advanced by the book, cleared with the reduce-only map. A
/// trader ABSENT from it is checked as before F1 (book-only callers).
#[derive(Clone, Debug, Default)]
pub struct AccountMargins {
    /// The market's leverage tiers (maker check).
    tiers: Option<std::sync::Arc<[crate::margin::MarginTier]>>,
    /// Takers' running budgets (installed by the executor; D2 pools).
    accounts: BTreeMap<Address, AccountMargin>,
    /// Review fix 2 (s517): makers' running free margins, loaded from
    /// their D8 snapshot — kept apart from the taker pools (a sender's pool
    /// is 0 outside its first checked taker's market, which is not its
    /// maker free margin).
    makers: BTreeMap<Address, AccountMargin>,
    /// Review fix 4 (s517): traders whose `accounts` entry is only a taker
    /// budget of 0 (D2: a sender's markets other than its pool market) —
    /// NOT their account, so their makers keep the snapshot. Every other
    /// `accounts` entry IS the sender's running account in this book and is
    /// shared by its takers and its makers.
    taker_only: BTreeSet<Address>,
}

/// F1 (s517): one trader's entry in [`AccountMargins`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountMargin {
    /// Running free margin (may be negative).
    pub free: FixedPoint,
    /// Price the trader's position here is valued at (mark, else entry;
    /// ZERO = use the book's last trade price).
    pub px: FixedPoint,
}

impl AccountMargins {
    pub fn new(tiers: Option<std::sync::Arc<[crate::margin::MarginTier]>>) -> Self {
        Self {
            tiers,
            accounts: BTreeMap::new(),
            makers: BTreeMap::new(),
            taker_only: BTreeSet::new(),
        }
    }

    pub fn insert(&mut self, trader: Address, free: FixedPoint, px: FixedPoint) {
        self.accounts.insert(trader, AccountMargin { free, px });
    }

    /// Review fix 4 (s517): a taker budget of 0 that is NOT `trader`'s
    /// account (D2 non-pool market): its takers fill within their own
    /// reservation; its makers are checked against their snapshot.
    pub fn insert_taker_only(&mut self, trader: Address, px: FixedPoint) {
        self.accounts.insert(trader, AccountMargin { free: FixedPoint::ZERO, px });
        self.taker_only.insert(trader);
    }

    /// Review fix 4: whether `trader`'s `accounts` entry is its shared account.
    fn shared(&self, trader: &Address) -> bool {
        self.accounts.contains_key(trader) && !self.taker_only.contains(trader)
    }

    pub fn get(&self, trader: &Address) -> Option<AccountMargin> {
        self.accounts.get(trader).copied()
    }

    fn set_free(&mut self, trader: &Address, free: FixedPoint) {
        if let Some(a) = self.accounts.get_mut(trader) {
            a.free = free;
        }
    }

    /// F1 (s517 #4): `trader`'s entry, snapshotted from `src` the first
    /// time it is needed in this placement / batch (also seeding its
    /// position in `ro` when the book does not track it yet).
    fn load(
        &mut self,
        trader: Address,
        market_id: MarketId,
        src: &dyn MakerAccountSource,
        ro: &mut ReduceOnlyPositions,
    ) -> AccountMargin {
        // Review fix 4: in the book holding its account (single path, D2
        // pool market) a maker shares the running entry its takers spend.
        if self.shared(&trader) {
            return self.accounts[&trader];
        }
        if let Some(a) = self.makers.get(&trader) {
            return *a;
        }
        let m = src.maker_account(&trader, market_id);
        if ro.get(&trader).is_none() {
            ro.insert(trader, m.signed_pos);
        }
        let a = AccountMargin { free: m.free, px: m.px };
        self.makers.insert(trader, a);
        a
    }
}

/// F1 (s517 #4): where the book gets a maker's account the first time it
/// fills in this placement / batch. MUST be deterministic and read-only
/// (the executor reads the frozen pre-batch state); `Sync` for workers.
pub trait MakerAccountSource: Sync {
    fn maker_account(&self, maker: &Address, market_id: MarketId) -> MakerAccount;
}

/// F1 (s517 #4): a maker's account snapshot — free margin (available +
/// UPnL − position IM), signed position in this market and the price it
/// is valued at (mark, else entry; ZERO = the fill price).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MakerAccount {
    pub free: FixedPoint,
    pub signed_pos: FixedPoint,
    pub px: FixedPoint,
}

/// F1 (s517 #4, HL `marginCanceled`): can `maker` take a fill of `q` at
/// `price` on its order resting `rem`? Its IM delta (position tier, closing
/// part free and releasing IM) minus this fill's share of the order's
/// reservation (the A5 telescoping piece) must fit its running free margin;
/// a purely closing fill always fits. Commits the running free on success.
/// Shared by matching and the FOK pre-check.
#[allow(clippy::too_many_arguments)]
fn maker_fill_fits(
    accounts: &mut AccountMargins,
    ro: &mut ReduceOnlyPositions,
    src: &dyn MakerAccountSource,
    market_id: MarketId,
    maker: Address,
    maker_is_buy: bool,
    price: FixedPoint,
    q: FixedPoint,
    rem: FixedPoint,
) -> bool {
    let a = accounts.load(maker, market_id, src, ro);
    let s = ro.get(&maker).unwrap_or(FixedPoint::ZERO);
    let size = if s < FixedPoint::ZERO { -s } else { s };
    let closing = q.min(reduce_only_allowance(s, maker_is_buy));
    let px = if a.px > FixedPoint::ZERO { a.px } else { price };
    let t = accounts.tiers.clone();
    let t = t.as_deref();
    let delta = (|| {
        let before = size.checked_mul(px).ok()?;
        let after = (size - closing)
            .checked_mul(px)
            .ok()?
            .checked_add(price.checked_mul(q - closing).ok()?)
            .ok()?;
        let share = crate::margin::order_initial_margin(t, price.checked_mul(rem).ok()?)
            - crate::margin::order_initial_margin(t, price.checked_mul(rem - q).ok()?);
        Some(crate::margin::im_delta(t, before, after) - share)
    })();
    // Review fix 2 (s517): each IM difference is floored, so without a tier
    // change `delta` can be +1 raw unit (floor(B + x) − floor(B) = floor(x)
    // + 1 while the share is floor(x)). That is rounding, not cost.
    let delta = delta.map(|d| if d == FixedPoint::from_raw(1) { FixedPoint::ZERO } else { d });
    match delta {
        Some(d) if closing == q || d <= a.free => {
            if accounts.shared(&maker) {
                accounts.set_free(&maker, a.free - d);
            } else if let Some(e) = accounts.makers.get_mut(&maker) {
                e.free = a.free - d;
            }
            true
        }
        _ => false,
    }
}

/// Order placement outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrderStatus {
    /// Completely filled.
    Filled,
    /// Partially filled, remainder resting on book.
    PartiallyFilled,
    /// No match, resting on book.
    Resting,
    /// Remainder cancelled (IOC/Market with partial or no fill).
    Cancelled,
    /// Rejected (PostOnly cross, FOK not fillable, empty book for Market).
    Rejected,
    /// Stop order pending trigger.
    PendingTrigger,
}

/// A pending stop order awaiting trigger.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct StopOrder {
    id: OrderId,
    trader: Address,
    market_id: MarketId,
    side: Side,
    trigger_price: FixedPoint,
    limit_price: Option<FixedPoint>,
    /// s515: a StopMarket's slippage cap (worst acceptable price) — the
    /// placed order's `params.price`. ZERO for StopLimit (its limit is the
    /// cap) and for legacy rows persisted before caps existed (such a stop
    /// is rejected at trigger time: a market order needs a positive cap).
    price_cap: FixedPoint,
    quantity: FixedPoint,
    time_in_force: TimeInForce,
    timestamp: u64,
    reduce_only: bool,
    client_order_id: Option<u64>,
}

/// Internal: where an order lives in the book.
#[derive(Clone, Debug)]
struct OrderLocation {
    side: Side,
    price: FixedPoint,
}

// ============================================================================
// L3 level-hash sponge cache (node-local, in-RAM only)
// ============================================================================
//
// See docs/design-levelhash-cache.md. Keccak absorbs sequentially and pads
// only at finalization, so for a level whose queue only APPENDED at the tail
// since the last save we can keep the un-finalized hasher state (post-absorb
// of the first `frame_count` frames), absorb just the new tail frames, and
// finalize a clone — a byte-identical digest in O(new frames) instead of
// O(depth). Validity of the cached prefix is proven by the per-level
// monotonic `level_epoch`: EVERY non-append queue mutation (front pop,
// mid-queue removal, in-place qty change — see the bump sites) increments it;
// an entry is usable only if its recorded epoch still matches. Any mismatch,
// or any doubt, falls back to the full rehash (today's exact behavior).
//
// The cache is never serialized, never part of any hash or row, and every
// book-rebuild path constructs a fresh OrderBook (empty cache + empty epoch
// map) — a dropped cache is only a cold start, never a correctness event.

/// Estimated in-RAM cost of one cache entry (keccak state ~200 B + block
/// buffer ~136 B + metadata + map overhead), used to convert a byte budget
/// into an entry cap.
const LEVEL_CACHE_ENTRY_COST: usize = 512;

/// One cache slot for a price level. **Staged seeding** (the miss-path fix):
/// a level that just missed holds only a cheap `Probe` (epoch + frame count,
/// no sponge). The un-finalized sponge is built (`Seeded`) only once the level
/// survives one full append-only save interval — i.e. the NEXT save observes
/// the SAME epoch, proving no invalidating op ran in between. A churning level
/// therefore never pays the streaming absorb + hasher-clone that the frozen
/// one-shot path (asm-backed `alloy_primitives::keccak256`) avoids: its miss is
/// just the plain one-shot digest plus a cheap probe record.
enum LevelHashCacheEntry {
    /// Recorded after a miss: no sponge yet. Promoted to `Seeded` on the next
    /// save of this level iff its epoch is unchanged (append-only interval).
    Probe {
        /// `level_epoch` observed when the probe was recorded.
        epoch: u64,
        /// Queue length observed (bookkeeping only; promotion needs epoch).
        frame_count: u32,
        /// LRU tick for eviction.
        last_used: u64,
    },
    /// Full un-finalized keccak-256 sponge state after a confirmed append-only
    /// interval; extended in O(new tail) on subsequent append-only saves.
    Seeded {
        /// `level_epoch` value observed when this state was absorbed.
        epoch: u64,
        /// Number of queue frames absorbed into `hasher`.
        frame_count: u32,
        /// Total bytes absorbed (bookkeeping / debug identity check).
        absorbed_len: u64,
        /// Sum of `remaining_qty` over the absorbed prefix, accumulated in the
        /// same front→back `+=` order as the one-shot path.
        prefix_qty: FixedPoint,
        /// Un-finalized keccak-256 sponge state after absorbing the prefix.
        hasher: sha3::Keccak256,
        /// LRU tick for eviction.
        last_used: u64,
    },
}

impl LevelHashCacheEntry {
    #[inline]
    fn last_used(&self) -> u64 {
        match self {
            LevelHashCacheEntry::Probe { last_used, .. }
            | LevelHashCacheEntry::Seeded { last_used, .. } => *last_used,
        }
    }
}

/// Per-book incremental level-hash cache (L3, `TORUS_LEVEL_HASH_CACHE`).
struct LevelHashCache {
    entries: HashMap<(u8, i128), LevelHashCacheEntry>,
    max_entries: usize,
    tick: u64,
    /// O(new-tail) sponge extensions on a `Seeded` entry.
    hits: u64,
    /// Plain one-shot fallbacks (no entry, churning probe, or stale seed) —
    /// each records/refreshes a cheap `Probe`.
    misses: u64,
    /// Sponge investments: a `Probe` promoted to `Seeded` after a confirmed
    /// append-only interval (one full front→back absorb, amortized by hits).
    seeds: u64,
}

impl LevelHashCache {
    fn insert_bounded(&mut self, key: (u8, i128), entry: LevelHashCacheEntry) {
        if self.entries.len() >= self.max_entries && !self.entries.contains_key(&key) {
            // Evict the least-recently-used quarter in one batch (rare;
            // amortized cheap). Eviction is always safe — cold start only.
            let mut by_age: Vec<((u8, i128), u64)> = self
                .entries
                .iter()
                .map(|(k, e)| (*k, e.last_used()))
                .collect();
            by_age.sort_unstable_by_key(|(_, t)| *t);
            let evict = (self.max_entries / 4).max(1);
            for (k, _) in by_age.into_iter().take(evict) {
                self.entries.remove(&k);
            }
        }
        self.entries.insert(key, entry);
    }
}

// ============================================================================
// OrderBook
// ============================================================================
//
// 3c journal-in-book (ported from the deep-book/level-rows lineage, replacing
// the rank8 `BookJournal`): every primitive that changes RESTING-order state
// journals the order id (`row_journal`) AND its `(side_tag, raw_price)` level
// (`level_journal`). Capture is at the OrderBook PRIMITIVES (insert_order /
// cancel_order / cancel_all / modify_order / match_at_level) — stop triggers,
// STP cancels and every executor path funnel through these, so no caller can
// bypass the journals. Pending stops are NOT journaled (the save path diffs
// the tiny stop set directly). The journals are in-memory bookkeeping only:
// never serialized, never part of any hash; the SAVE PATH decides what (if
// anything) to do with them per persistence mode.

/// Price-time priority Central Limit Order Book.
pub struct OrderBook {
    pub market_id: MarketId,
    /// Buy side: price → time-ordered queue. Best bid = last key (highest).
    bids: BTreeMap<FixedPoint, VecDeque<Order>>,
    /// Sell side: price → time-ordered queue. Best ask = first key (lowest).
    asks: BTreeMap<FixedPoint, VecDeque<Order>>,
    /// O(1) order lookup by ID → location.
    order_index: HashMap<OrderId, OrderLocation>,
    /// Per-trader order tracking for cancel-all.
    trader_orders: HashMap<Address, TraderOrders>,
    /// Pending stop orders.
    pending_stops: Vec<StopOrder>,
    pub tick_size: FixedPoint,
    pub lot_size: FixedPoint,
    next_id: OrderId,
    last_trade_price: Option<FixedPoint>,

    // ---- s515 reduce-only policing (in-RAM only, never serialized) ----
    /// `(trader, order_id)` of resting reduce-only orders — derived from the
    /// orders themselves (added by `insert_order` / `insert_loaded_order`).
    /// Removal is best-effort (cancel paths, sweeps); a stale entry is purged
    /// by the next sweep of its trader and is otherwise harmless.
    reduce_only_index: BTreeSet<(Address, OrderId)>,
    /// Positions of the traders policed during the current placement(s);
    /// empty = no reduce-only enforcement. See [`ReduceOnlyPositions`].
    reduce_only_positions: ReduceOnlyPositions,
    /// F1 (s517): account margins of the current placement(s) — in-RAM
    /// only, never serialized. See [`AccountMargins`].
    account_margins: AccountMargins,

    // ---- Per-order-row persistence state (journal-in-book, 3c) ----
    //
    // CONSENSUS-CRITICAL: intra-price-level queue order is NOT ascending
    // order-id (`modify_order` cancel+reinserts with the SAME id at the BACK
    // of the queue), so every resting order carries an explicit insertion
    // sequence, ASSIGNED AT INSERT TIME by the book. `next_seq` is persisted
    // in the meta row — recomputing it as max(seq)+1 at reload would assign
    // different seqs on a restarted node whenever cancels left a gap at the
    // top, forking the state root.
    /// Insertion sequence per resting order (assigned by `insert_order`,
    /// preserved by in-place modifies, reassigned on cancel+reinsert).
    order_seq: HashMap<OrderId, u64>,
    /// Monotonic seq allocator. Part of the persisted meta row (consensus).
    next_seq: u64,
    /// Row journal: order ids whose persisted row must be upserted (still
    /// resting) or deleted (gone) at the next save. Drained by `take_row_ops`.
    row_journal: BTreeSet<OrderId>,
    /// Order ids that currently have a persisted row — lets the save skip
    /// deletes for orders placed AND removed between two saves.
    row_exists: HashSet<OrderId>,
    /// Level journal: `(side_tag, raw_price)` of every price level touched
    /// since the last save. Drained by `take_level_ops`.
    level_journal: BTreeSet<(u8, i128)>,
    /// Levels that currently have a persisted level row (skip-useless-
    /// tombstones role, mirrors `row_exists`).
    level_exists: HashSet<(u8, i128)>,

    // ---- L3 level-hash sponge cache (node-local, in-RAM only) ----
    /// Per-level prefix-invalidation epoch: bumped by EVERY non-append queue
    /// mutation while the cache is enabled. Entries are NEVER removed for the
    /// lifetime of this book instance (a re-created level must not see a
    /// reset epoch). Never serialized.
    level_epoch: HashMap<(u8, i128), u64>,
    /// The cache itself. `None` = disabled (default; `take_level_ops` runs
    /// today's exact one-shot path). Never serialized.
    level_hash_cache: Option<Box<LevelHashCache>>,

    // ---- Chunked level digest (mode 3, `TORUS_BOOK_ROWS=3`) ----
    //
    // See `book_rows::LevelRowData` for the preimage and the impl block
    // "Chunked level digest" below for the maintenance contract. All of this
    // is in-RAM bookkeeping: never serialized, never part of any row.
    /// Which `level_hash` `take_level_ops` / `full_level_ops` emit: `false` =
    /// the flat mode-2 digest (exact-today), `true` = the chunked mode-3
    /// digest. Set by the executor for its mode BEFORE the first drain; the
    /// dirty-chunk marks below are only recorded while this is on.
    level_hash_chunked: bool,
    /// Per-level chunk aggregates (`chunk_idx → (digest, Σqty, count)`),
    /// built in full on a level's first drain and maintained incrementally
    /// afterwards. A level with NO entry is (re)built from its queue.
    level_chunks: HashMap<(u8, i128), BTreeMap<u64, ChunkAgg>>,
    /// `(side_tag, raw_price, chunk_idx)` of every chunk touched since its
    /// level was last drained — recorded at EVERY queue-mutation site (the
    /// same sites that journal the level). Drained per level by
    /// `take_level_ops`; a mark whose level is not journaled stays until it is.
    dirty_chunks: BTreeSet<(u8, i128, u64)>,
}

/// One chunk's aggregate for the chunked level digest: the keccak of its
/// member frames plus the parts of the level aggregate it contributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChunkAgg {
    digest: [u8; 32],
    /// Σ `remaining_qty` over the chunk's members, accumulated front→back.
    qty: FixedPoint,
    /// Member count (>= 1: empty chunks are removed, never stored).
    count: u32,
}

impl OrderBook {
    pub fn new(market_id: MarketId, tick_size: FixedPoint, lot_size: FixedPoint) -> Self {
        Self {
            market_id,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            order_index: HashMap::new(),
            trader_orders: HashMap::new(),
            pending_stops: Vec::new(),
            tick_size,
            lot_size,
            next_id: 1,
            last_trade_price: None,
            reduce_only_index: BTreeSet::new(),
            reduce_only_positions: ReduceOnlyPositions::default(),
            account_margins: AccountMargins::default(),
            order_seq: HashMap::new(),
            next_seq: 1,
            row_journal: BTreeSet::new(),
            row_exists: HashSet::new(),
            level_journal: BTreeSet::new(),
            level_exists: HashSet::new(),
            level_epoch: HashMap::new(),
            level_hash_cache: None,
            level_hash_chunked: false,
            level_chunks: HashMap::new(),
            dirty_chunks: BTreeSet::new(),
        }
    }

    fn alloc_id(&mut self) -> OrderId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    // ========================================================================
    // Public API
    // ========================================================================

    /// Place an order. Main entry point dispatching by order type and TIF.
    ///
    /// s515 (Hyperliquid parity):
    /// - `Market` / `StopMarket` carry a REQUIRED slippage cap in
    ///   `params.price` (a market order is an aggressive IOC limit): matching
    ///   never crosses it and the unfilled remainder is cancelled.
    /// - Reduce-only orders are policed against the positions installed by
    ///   [`Self::set_reduce_only_positions`] (placement clamp, match-time
    ///   maker cap, post-fill re-fit of resting reduce-only orders).
    /// - Stops that fire are returned in [`PlaceResult::triggered_stops`] for
    ///   the executor to place — never executed inline.
    pub fn place_order(
        &mut self,
        params: PlaceOrderParams,
        trader: Address,
        timestamp: u64,
    ) -> PlaceResult {
        self.place_order_with_margin(params, trader, timestamp, None)
    }

    /// [`Self::place_order`] with the taker's match-time margin limit
    /// (s515 review 4, see [`TakerMarginLimit`]); `None` = unchecked (makers
    /// and orders whose fills can never cost more than they reserved).
    pub fn place_order_with_margin(
        &mut self,
        params: PlaceOrderParams,
        trader: Address,
        timestamp: u64,
        margin: Option<&TakerMarginLimit>,
    ) -> PlaceResult {
        self.place_order_with_accounts(params, trader, timestamp, margin, None)
    }

    /// [`Self::place_order_with_margin`] with, F1 (s517 #4), the source of
    /// makers' accounts: every maker fill is checked ([`MakerAccountSource`],
    /// HL `marginCanceled`); `None` = makers unchecked.
    pub fn place_order_with_accounts(
        &mut self,
        params: PlaceOrderParams,
        trader: Address,
        timestamp: u64,
        margin: Option<&TakerMarginLimit>,
        makers: Option<&dyn MakerAccountSource>,
    ) -> PlaceResult {
        let order_id = self.alloc_id();
        let side = if params.is_buy { Side::Buy } else { Side::Sell };

        // Dust order rejection (2.1b.2): qty must be >= lot_size
        if params.quantity < self.lot_size {
            return PlaceResult::rejected(order_id);
        }

        // FIX 7 (ECON-FIND-10): Limit orders must have positive price.
        // s515: so must Market / StopMarket (the price is the slippage cap)
        // and a StopLimit's limit.
        let price_invalid = match params.order_type {
            OrderType::Limit | OrderType::Market | OrderType::StopMarket { .. } => {
                params.price <= FixedPoint::ZERO
            }
            OrderType::StopLimit { limit, .. } => limit <= FixedPoint::ZERO,
        };
        if price_invalid {
            return PlaceResult::rejected(order_id);
        }

        // FIX 8 (ECON-FIND-11): Enforce tick size for limit orders
        if matches!(params.order_type, OrderType::Limit)
            && self.tick_size > FixedPoint::ZERO
            && params.price.raw() % self.tick_size.raw() != 0
        {
            return PlaceResult::rejected(order_id);
        }

        // FIX 10 (ECON-FIND-17): open orders are limited per user across all
        // markets, enforced by the executor before matching
        // (`crate::position::open_order_limit`), not per book.

        // Stop orders → store in pending_stops
        let stop = match params.order_type {
            OrderType::StopMarket { trigger } => Some((trigger, None, params.price)),
            OrderType::StopLimit { trigger, limit } => {
                Some((trigger, Some(limit), FixedPoint::ZERO))
            }
            _ => None,
        };
        if let Some((trigger, limit_price, price_cap)) = stop {
            // FIX 9 (ECON-FIND-12): Validate trigger direction
            if let Some(current_price) = self.last_trade_price {
                let invalid_trigger = match side {
                    Side::Buy => trigger <= current_price,
                    Side::Sell => trigger >= current_price,
                };
                if invalid_trigger {
                    return PlaceResult::rejected(order_id);
                }
            }
            // F4 (s515 review): a policed reduce-only stop must be able to reduce
            // the CURRENT position when placed — the same rule the executor's
            // single-action pre-check applies (the batch path relies on this one).
            // It is re-checked, and clamped, at trigger time.
            if params.reduce_only {
                if let Some(pos) = self.reduce_only_positions.get(&trader) {
                    if reduce_only_allowance(pos, params.is_buy) <= FixedPoint::ZERO {
                        return PlaceResult::rejected(order_id);
                    }
                }
            }
            self.pending_stops.push(StopOrder {
                id: order_id,
                trader,
                market_id: params.market_id,
                side,
                trigger_price: trigger,
                limit_price,
                price_cap,
                quantity: params.quantity,
                time_in_force: params.time_in_force,
                timestamp,
                reduce_only: params.reduce_only,
                client_order_id: params.client_order_id,
            });
            return PlaceResult {
                status: OrderStatus::PendingTrigger,
                rested_qty: params.quantity,
                ..PlaceResult::rejected(order_id)
            };
        }

        // PostOnly: reject if would cross the spread
        if params.time_in_force == TimeInForce::PostOnly && self.would_cross(side, params.price) {
            return PlaceResult::rejected(order_id);
        }

        let is_market = matches!(params.order_type, OrderType::Market);

        // Market order: reject if no liquidity
        if is_market {
            let has_liquidity = match side {
                Side::Buy => !self.asks.is_empty(),
                Side::Sell => !self.bids.is_empty(),
            };
            if !has_liquidity {
                return PlaceResult::rejected(order_id);
            }
        }

        // s515 (reduce-only, placement + match time): a policed reduce-only
        // order may only reduce the trader's CURRENT position — rejected when
        // flat or on the increasing side, clamped to the position size when
        // larger (Hyperliquid resizes reduce-only orders to the position).
        // Its own fills reduce the position one-for-one (STP keeps it from
        // ever matching the trader's own orders), so the clamp holds through
        // matching.
        let mut quantity = params.quantity;
        if params.reduce_only {
            if let Some(pos) = self.reduce_only_positions.get(&trader) {
                let allowed = reduce_only_allowance(pos, params.is_buy);
                if allowed <= FixedPoint::ZERO {
                    return PlaceResult::rejected(order_id);
                }
                quantity = quantity.min(allowed);
            }
        }

        let mut order = Order {
            id: order_id,
            trader,
            side,
            price: params.price,
            remaining_qty: quantity,
            original_qty: quantity,
            order_type: params.order_type,
            time_in_force: params.time_in_force,
            timestamp,
            reduce_only: params.reduce_only,
            client_order_id: params.client_order_id,
        };

        // FOK: pre-check full fill availability — and, with a margin limit,
        // that the complete fill fits it (s515 review 4: a FOK order never
        // rests, so its committed margin is the IM of its charged fills,
        // monotonic in their notional: fitting at the end means fitting
        // throughout). Review 5: the closing part (the first `free` units it
        // fills) is not charged, exactly as in matching.
        // F1 (s517): the taker's account (absent ⇒ pre-F1 check: no position
        // valuation, budget = the limit's own). Position from the policed /
        // tracked map.
        let mut match_margin = margin.map(|limit| {
            let signed = self.reduce_only_positions.get(&trader).unwrap_or(FixedPoint::ZERO);
            let acct = self.account_margins.get(&trader);
            let px = match acct {
                Some(a) if a.px > FixedPoint::ZERO => a.px,
                Some(_) => self.last_trade_price.unwrap_or(FixedPoint::ZERO),
                None => FixedPoint::ZERO,
            };
            MatchMargin {
                limit,
                lot: self.lot_size,
                size0: if signed < FixedPoint::ZERO { -signed } else { signed },
                px,
                allowance0: reduce_only_allowance(signed, params.is_buy),
                budget: limit.budget + acct.map_or(FixedPoint::ZERO, |a| a.free),
                charged: FixedPoint::ZERO,
                exhausted: false,
                taker_only: self.account_margins.taker_only.contains(&trader),
            }
        });
        if params.time_in_force == TimeInForce::FOK {
            let free = self.reduce_only_positions.get(&trader).map_or(FixedPoint::ZERO, |p| {
                reduce_only_allowance(p, params.is_buy)
            });
            // F1 (s517 #4): the pre-check skips makers matching would
            // cancel — on CLONES, so it never mutates the book.
            let mut acc_c = makers.map(|_| self.account_margins.clone());
            let mut ro_c = makers.map(|_| self.reduce_only_positions.clone());
            let pre = match (makers, acc_c.as_mut(), ro_c.as_mut()) {
                (Some(src), Some(acc), Some(ro)) => Some((src, acc, ro)),
                _ => None,
            };
            let fits = match self.can_fill_completely(side, params.price, quantity, trader, free, pre) {
                None => false,
                // F1: a purely closing FOK always fits; otherwise the
                // complete fill's need (closing releases IM) must fit.
                Some(notional) => match_margin.as_ref().is_none_or(|m| {
                    quantity <= free
                        || notional.is_some_and(|n| {
                            let closing = quantity.min(free);
                            let before = m.size0.checked_mul(m.px).ok();
                            let after = (m.size0 - closing)
                                .checked_mul(m.px)
                                .ok()
                                .and_then(|x| x.checked_add(n).ok());
                            matches!((before, after), (Some(b), Some(a))
                                if m.fits(crate::margin::im_delta(m.limit.tiers.as_deref(), b, a)))
                        })
                }),
            };
            if !fits {
                return PlaceResult::rejected(order_id);
            }
        }

        // Execute matching
        let (fills, self_trade_cancels, mut reduce_only_cuts, margin_cancels) =
            self.execute_match(&mut order, match_margin.as_mut(), makers);
        let margin_exhausted = match_margin.as_ref().is_some_and(|m| m.exhausted);

        if let Some(last_fill) = fills.last() {
            self.last_trade_price = Some(last_fill.price);
        }

        // Reduce-only makers removed during matching (STP or cap) leave the index.
        for o in &self_trade_cancels {
            if o.reduce_only {
                self.reduce_only_index.remove(&(o.trader, o.id));
            }
        }
        for c in reduce_only_cuts.iter().chain(&margin_cancels) {
            self.reduce_only_index.remove(&(c.trader, c.order_id));
        }
        // ...and so do fully filled ones (F6: a stale entry kept
        // `has_reduce_only_orders()` true, so every later placement in this
        // market loaded policing positions for nothing).
        for f in &fills {
            if !self.order_index.contains_key(&f.maker_order_id) {
                self.reduce_only_index.remove(&(f.maker, f.maker_order_id));
            }
        }

        // s515 (reduce-only, resting orders): every policed trader whose
        // position these fills moved — and the placing trader, whose new
        // order competes with its older ones — gets its resting reduce-only
        // orders re-fitted to the position (shrunk / cancelled).
        let mut taker_ro_left = None;
        if !self.reduce_only_positions.is_empty() {
            let mut touched: BTreeSet<Address> = BTreeSet::new();
            touched.insert(trader);
            for f in &fills {
                touched.insert(f.maker);
                touched.insert(f.taker);
            }
            for t in touched {
                let left = self.sweep_reduce_only(t, &mut reduce_only_cuts);
                if t == trader {
                    taker_ro_left = left;
                }
            }
        }

        // Determine outcome
        let mut rested_qty = FixedPoint::ZERO;
        let status = if order.remaining_qty == FixedPoint::ZERO {
            OrderStatus::Filled
        } else if is_market
            || margin_exhausted
            || params.time_in_force == TimeInForce::IOC
            || params.time_in_force == TimeInForce::FOK
        {
            // s515 review 4: a taker that ran out of margin mid-match is
            // cancelled like an IOC remainder — it never rests.
            OrderStatus::Cancelled
        } else {
            // s515: a resting reduce-only remainder ranks behind the
            // trader's older reduce-only orders — it keeps only the position
            // budget they left.
            if let (true, Some(left)) = (order.reduce_only, taker_ro_left) {
                order.remaining_qty = order.remaining_qty.min(left);
            }
            if order.remaining_qty == FixedPoint::ZERO {
                if fills.is_empty() {
                    OrderStatus::Rejected
                } else {
                    OrderStatus::Cancelled
                }
            } else {
                // GTC / PostOnly: rest remainder on book
                rested_qty = order.remaining_qty;
                self.insert_order(order);
                if fills.is_empty() {
                    OrderStatus::Resting
                } else {
                    OrderStatus::PartiallyFilled
                }
            }
        };

        // F1 (s517): what this taker committed comes off the sender's
        // running free margin. The executor releases its reservation except
        // what the resting remainder keeps (`reserve(hold, rested)`, the
        // same formula), and the fills moved the position's IM (the need
        // WITHOUT the hold: `left_before = free_now` rests nothing that
        // opens). So the account keeps `free + reservation − kept − ΔIM`.
        if let Some(m) = match_margin.as_ref() {
            if let Some(a) = self.account_margins.get(&trader) {
                let free_now = self
                    .reduce_only_positions
                    .get(&trader)
                    .map_or(FixedPoint::ZERO, |p| reduce_only_allowance(p, params.is_buy));
                // Every fill's need was computed checked; on overflow count
                // the whole budget as spent.
                let spent = m
                    .limit
                    .need(m, FixedPoint::ZERO, FixedPoint::ZERO, free_now, free_now)
                    .and_then(|d| {
                        let kept = match m.limit.hold_price {
                            Some(hp) => crate::margin::order_initial_margin(
                                m.limit.tiers.as_deref(),
                                hp.checked_mul(rested_qty).ok()?,
                            ),
                            None => FixedPoint::ZERO,
                        };
                        d.checked_add(kept).ok()
                    })
                    .unwrap_or(m.budget);
                // B2 (s87): a forgiven rounding unit never reaches the free.
                self.account_margins
                    .set_free(&trader, a.free - m.over_reservation(spent));
            }
        }

        // Stops fired by these fills go back to the caller (s515).
        let triggered_stops = if fills.is_empty() {
            Vec::new()
        } else {
            self.trigger_stops()
        };

        PlaceResult {
            order_id,
            status,
            fills,
            self_trade_cancels,
            rested_qty,
            reduce_only_cuts,
            triggered_stops,
            margin_cancels,
        }
    }

    /// s515: re-fit `trader`'s resting reduce-only orders to its current
    /// (policed) position, oldest order first: orders on the reducing side
    /// keep up to the remaining position budget, everything else (flat, or
    /// the increasing side after a flip) is cancelled. Records a cut for
    /// every quantity removed; returns the budget left over, or `None` if
    /// `trader` is not policed.
    fn sweep_reduce_only(
        &mut self,
        trader: Address,
        cuts: &mut Vec<ReduceOnlyCut>,
    ) -> Option<FixedPoint> {
        let pos = self.reduce_only_positions.get(&trader)?;
        let reduces_buy = pos < FixedPoint::ZERO;
        let mut budget = if reduces_buy { -pos } else { pos };
        let ids: Vec<OrderId> = self
            .reduce_only_index
            .range((trader, OrderId::MIN)..=(trader, OrderId::MAX))
            .map(|&(_, id)| id)
            .collect();
        for id in ids {
            let Some((is_buy, remaining, price)) = self
                .get_order(id)
                .map(|o| (o.side == Side::Buy, o.remaining_qty, o.price))
            else {
                self.reduce_only_index.remove(&(trader, id));
                continue;
            };
            let keep = if budget > FixedPoint::ZERO && is_buy == reduces_buy {
                remaining.min(budget)
            } else {
                FixedPoint::ZERO
            };
            budget -= keep;
            if keep == remaining {
                continue;
            }
            // cancel_order drops the index entry; the in-place modify keeps
            // time priority (qty decrease only).
            let applied = if keep == FixedPoint::ZERO {
                self.cancel_order(id).is_ok()
            } else {
                self.modify_order(id, None, Some(keep)).is_ok()
            };
            if applied {
                cuts.push(ReduceOnlyCut {
                    order_id: id,
                    trader,
                    price,
                    qty: remaining - keep,
                });
            }
        }
        Some(budget)
    }

    /// s515: traders owning (possibly stale) resting reduce-only orders in
    /// this book, ascending — the executor loads their positions into
    /// [`ReduceOnlyPositions`] before placing.
    pub fn reduce_only_traders(&self) -> Vec<Address> {
        let mut out: Vec<Address> = self.reduce_only_index.iter().map(|&(t, _)| t).collect();
        out.dedup();
        out
    }

    /// s515: does this book hold any (possibly stale) resting reduce-only
    /// order? Cheap gate for loading [`ReduceOnlyPositions`].
    pub fn has_reduce_only_orders(&self) -> bool {
        !self.reduce_only_index.is_empty()
    }

    /// s515: install the positions to police for the next placement(s).
    /// Must be cleared ([`Self::clear_reduce_only_positions`]) once the
    /// caller's placements are done — a stale map would police against
    /// out-of-date positions.
    pub fn set_reduce_only_positions(&mut self, positions: ReduceOnlyPositions) {
        self.reduce_only_positions = positions;
    }

    pub fn clear_reduce_only_positions(&mut self) {
        self.reduce_only_positions = ReduceOnlyPositions::default();
    }

    /// F1 (s517): install the account margins for the next placement(s);
    /// cleared ([`Self::clear_account_margins`]) with the reduce-only map.
    pub fn set_account_margins(&mut self, a: AccountMargins) {
        self.account_margins = a;
    }

    pub fn clear_account_margins(&mut self) {
        self.account_margins = AccountMargins::default();
    }

    pub fn account_margins(&self) -> &AccountMargins {
        &self.account_margins
    }

    /// Drop every reduce-only index entry of `trader` (cancel-all paths).
    fn forget_reduce_only_trader(&mut self, trader: &Address) {
        let keys: Vec<(Address, OrderId)> = self
            .reduce_only_index
            .range((*trader, OrderId::MIN)..=(*trader, OrderId::MAX))
            .copied()
            .collect();
        for k in keys {
            self.reduce_only_index.remove(&k);
        }
    }

    /// Cancel an order by ID. Returns the cancelled order.
    pub fn cancel_order(&mut self, order_id: OrderId) -> Result<Order, CoreError> {
        let loc = self
            .order_index
            .remove(&order_id)
            .ok_or(CoreError::OrderNotFound(order_id))?;

        let book = match loc.side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        let queue = book
            .get_mut(&loc.price)
            .ok_or(CoreError::OrderNotFound(order_id))?;

        let pos = Self::queue_position(queue, &self.order_seq, order_id)
            .ok_or(CoreError::OrderNotFound(order_id))?;

        let order = queue.remove(pos).unwrap();

        if queue.is_empty() {
            book.remove(&loc.price);
        }
        if order.reduce_only {
            self.reduce_only_index.remove(&(order.trader, order_id));
        }

        if let Some(ids) = self.trader_orders.get_mut(&order.trader) {
            ids.remove(order_id);
            if ids.is_empty() {
                self.trader_orders.remove(&order.trader);
            }
        }

        let seq = self.order_seq.remove(&order_id);
        self.row_journal.insert(order_id);
        self.level_journal
            .insert((crate::book_rows::side_tag(order.side), order.price.raw()));
        // L3: mid-queue removal invalidates any cached level-hash prefix.
        Self::bump_level_epoch(
            self.level_hash_cache.is_some(),
            &mut self.level_epoch,
            crate::book_rows::side_tag(order.side),
            order.price.raw(),
        );
        // Mode 3: the removed frame's chunk is dirty.
        Self::mark_chunk_dirty(
            self.level_hash_chunked,
            &mut self.dirty_chunks,
            crate::book_rows::side_tag(order.side),
            order.price.raw(),
            seq,
        );
        Ok(order)
    }

    /// C4 (s517): remove `trader`'s PENDING STOPS (same mutation as
    /// `cancel_all`'s `retain`: the survivors keep their relative order, so
    /// `pending_stops` stays id-ascending) and return each removed stop's
    /// reservation inputs `(price, quantity)` — the price its triggered order
    /// would carry (the limit, else the cap; see `trigger_stops`). The caller
    /// releases their reservations; `cancel_all` alone does not report stops.
    pub fn take_pending_stops(&mut self, trader: &Address) -> Vec<(FixedPoint, FixedPoint)> {
        if !self.pending_stops.iter().any(|s| s.trader == *trader) {
            return Vec::new();
        }
        let (taken, kept): (Vec<StopOrder>, Vec<StopOrder>) =
            std::mem::take(&mut self.pending_stops)
                .into_iter()
                .partition(|s| s.trader == *trader);
        self.pending_stops = kept;
        taken
            .into_iter()
            .map(|s| (s.limit_price.unwrap_or(s.price_cap), s.quantity))
            .collect()
    }

    /// Cancel all orders for a trader, pending stops included. Returns the
    /// cancelled resting orders.
    pub fn cancel_all(&mut self, trader: Address, _market_id: Option<MarketId>) -> Vec<Order> {
        // Stops hold open-order slots, so they go even when the trader has
        // no resting order.
        self.pending_stops.retain(|s| s.trader != trader);
        self.forget_reduce_only_trader(&trader);
        let order_ids = match self.trader_orders.remove(&trader) {
            Some(ids) => ids.into_vec(),
            None => return vec![],
        };

        if let Some(cancelled) = self.try_cancel_all_batch(&order_ids) {
            return cancelled;
        }

        let mut cancelled = Vec::with_capacity(order_ids.len());
        let cache_on = self.level_hash_cache.is_some();
        let chunked_on = self.level_hash_chunked;
        for order_id in order_ids {
            if let Some(loc) = self.order_index.remove(&order_id) {
                let book = match loc.side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                if let Some(queue) = book.get_mut(&loc.price) {
                    if let Some(pos) = Self::queue_position(queue, &self.order_seq, order_id) {
                        cancelled.push(queue.remove(pos).unwrap());
                        let seq = self.order_seq.remove(&order_id);
                        self.row_journal.insert(order_id);
                        self.level_journal
                            .insert((crate::book_rows::side_tag(loc.side), loc.price.raw()));
                        // L3: removal invalidates the cached level-hash prefix.
                        Self::bump_level_epoch(
                            cache_on,
                            &mut self.level_epoch,
                            crate::book_rows::side_tag(loc.side),
                            loc.price.raw(),
                        );
                        // Mode 3: the removed frame's chunk is dirty.
                        Self::mark_chunk_dirty(
                            chunked_on,
                            &mut self.dirty_chunks,
                            crate::book_rows::side_tag(loc.side),
                            loc.price.raw(),
                            seq,
                        );
                    }
                    if queue.is_empty() {
                        book.remove(&loc.price);
                    }
                }
            }
        }

        cancelled
    }

    /// Modify an order (cancel-and-replace).
    /// Qty decrease only: keeps time priority. Price change or qty increase: loses priority.
    pub fn modify_order(
        &mut self,
        order_id: OrderId,
        new_price: Option<FixedPoint>,
        new_qty: Option<FixedPoint>,
    ) -> Result<Order, CoreError> {
        // Qty-only decrease: modify in place (keeps time priority)
        if new_price.is_none() {
            if let Some(new_q) = new_qty {
                let loc = self
                    .order_index
                    .get(&order_id)
                    .ok_or(CoreError::OrderNotFound(order_id))?
                    .clone();

                let cache_on = self.level_hash_cache.is_some();
                let chunked_on = self.level_hash_chunked;
                let book = match loc.side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                if let Some(queue) = book.get_mut(&loc.price) {
                    let pos = Self::queue_position(queue, &self.order_seq, order_id);
                    if let Some(order) = pos.and_then(|p| queue.get_mut(p)) {
                        if new_q > FixedPoint::ZERO && new_q < order.remaining_qty {
                            order.remaining_qty = new_q;
                            let out = order.clone();
                            // In-place change: same seq (priority kept), row rewritten.
                            self.row_journal.insert(order_id);
                            self.level_journal
                                .insert((crate::book_rows::side_tag(loc.side), loc.price.raw()));
                            // L3: in-place qty mutation changes an existing
                            // frame's bytes — invalidate the cached prefix.
                            Self::bump_level_epoch(
                                cache_on,
                                &mut self.level_epoch,
                                crate::book_rows::side_tag(loc.side),
                                loc.price.raw(),
                            );
                            // Mode 3: the rewritten frame's chunk is dirty
                            // (seq kept ⇒ same chunk).
                            Self::mark_chunk_dirty(
                                chunked_on,
                                &mut self.dirty_chunks,
                                crate::book_rows::side_tag(loc.side),
                                loc.price.raw(),
                                self.order_seq.get(&order_id).copied(),
                            );
                            return Ok(out);
                        }
                    }
                }
            }
        }

        // Cancel old, re-insert with updated fields (loses time priority)
        let old = self.cancel_order(order_id)?;

        let mut replacement = old;
        if let Some(p) = new_price {
            replacement.price = p;
        }
        if let Some(q) = new_qty {
            replacement.remaining_qty = q;
            // FIX 21 (ECON-FIND-28): Update original_qty when quantity increases
            if q > replacement.original_qty {
                replacement.original_qty = q;
            }
        }
        replacement.id = order_id;

        self.insert_order(replacement.clone());

        Ok(replacement)
    }

    /// Manually evaluate pending stop orders against the last trade price.
    /// Fired stops are removed and returned for the caller to place (s515).
    pub fn check_stops(&mut self) -> Vec<TriggeredStop> {
        self.trigger_stops()
    }

    // ========================================================================
    // Query Methods
    // ========================================================================

    /// Get the next order ID counter value.
    pub fn next_order_id(&self) -> OrderId {
        self.next_id
    }

    /// Set the next order ID counter (for global ID coordination across markets).
    pub fn set_next_order_id(&mut self, id: OrderId) {
        self.next_id = id;
    }

    /// Best (highest) bid price.
    pub fn best_bid(&self) -> Option<FixedPoint> {
        self.bids.keys().next_back().copied()
    }

    /// Best (lowest) ask price.
    pub fn best_ask(&self) -> Option<FixedPoint> {
        self.asks.keys().next().copied()
    }

    /// Spread = best_ask - best_bid.
    pub fn spread(&self) -> Option<FixedPoint> {
        match (self.best_ask(), self.best_bid()) {
            (Some(ask), Some(bid)) => Some(ask - bid),
            _ => None,
        }
    }

    /// Look up an order by ID.
    pub fn get_order(&self, order_id: OrderId) -> Option<&Order> {
        let loc = self.order_index.get(&order_id)?;
        let book = match loc.side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        let queue = book.get(&loc.price)?;
        let pos = Self::queue_position(queue, &self.order_seq, order_id)?;
        queue.get(pos)
    }

    /// Position of `order_id` in its level queue.
    ///
    /// Level queues are seq-ascending by construction (`insert_order` appends
    /// with the monotone `next_seq`, `insert_loaded_order` asserts ascending,
    /// in-place modifies keep the seq, matching pops the FRONT), so the order
    /// is found by binary search on seq — O(log depth) `order_seq` probes
    /// instead of a front-to-back scan. `take_row_ops` re-encodes EVERY
    /// journaled row through `get_order`; on levels thousands deep with tens
    /// of thousands of appended rows per block that scan was O(rows × depth)
    /// per save. Pure lookup: never changes which order is found. Falls back
    /// to the linear scan if the seq probe does not land on `order_id`
    /// (cannot happen while the invariant holds; asserted in debug).
    #[inline]
    fn queue_position(
        queue: &VecDeque<Order>,
        order_seq: &HashMap<OrderId, u64>,
        order_id: OrderId,
    ) -> Option<usize> {
        if let Some(&seq) = order_seq.get(&order_id) {
            let pos = queue.partition_point(|o| {
                order_seq.get(&o.id).is_some_and(|&s| s < seq)
            });
            let hit = queue.get(pos).is_some_and(|o| o.id == order_id);
            debug_assert!(
                hit,
                "queue_position: seq probe missed order {order_id} (level queue not seq-ascending?)"
            );
            if hit {
                return Some(pos);
            }
        }
        queue.iter().position(|o| o.id == order_id)
    }

    /// Total resting orders on the book.
    pub fn order_count(&self) -> usize {
        self.order_index.len()
    }

    /// Number of bid price levels.
    pub fn bid_levels(&self) -> usize {
        self.bids.len()
    }

    /// Number of ask price levels.
    pub fn ask_levels(&self) -> usize {
        self.asks.len()
    }

    /// Last trade price.
    pub fn last_trade_price(&self) -> Option<FixedPoint> {
        self.last_trade_price
    }

    /// All resting orders belonging to a specific trader.
    pub fn orders_for_trader(&self, trader: &Address) -> Vec<&Order> {
        match self.trader_orders.get(trader) {
            Some(ids) => ids.iter().filter_map(|id| self.get_order(*id)).collect(),
            None => vec![],
        }
    }

    /// Orders of `trader` that hold an open-order slot: resting orders plus
    /// pending stops (stops are rare, so a scan is fine).
    pub fn open_order_count(&self, trader: &Address) -> usize {
        self.trader_orders.get(trader).map_or(0, TraderOrders::len)
            + self.pending_stops.iter().filter(|s| s.trader == *trader).count()
    }

    /// Adds every indexed trader's `open_order_count` to `counts[idx[trader]]`.
    /// Walks the smaller of this book's traders and `idx` (an empty book costs
    /// nothing), plus one pass over the pending stops (not one per trader).
    pub fn add_open_order_counts(&self, idx: &HashMap<Address, usize>, counts: &mut [usize]) {
        if self.trader_orders.len() < idx.len() {
            for (trader, ids) in &self.trader_orders {
                if let Some(&i) = idx.get(trader) {
                    counts[i] += ids.len();
                }
            }
        } else {
            for (trader, &i) in idx {
                counts[i] += self.trader_orders.get(trader).map_or(0, TraderOrders::len);
            }
        }
        for stop in &self.pending_stops {
            if let Some(&i) = idx.get(&stop.trader) {
                counts[i] += 1;
            }
        }
    }

    /// Number of pending stop orders.
    pub fn pending_stop_count(&self) -> usize {
        self.pending_stops.len()
    }

    /// Aggregated bid depth, best (highest price) first:
    /// `(price, total remaining quantity, resting order count)` per level.
    /// Read-only view for RPC (S444: `torus_getOrderBook` reads the PROD
    /// `OrderBook` blob, not the test-only `OrderBookSnapshot`).
    pub fn bid_depth(&self) -> Vec<(FixedPoint, FixedPoint, usize)> {
        self.bids
            .iter()
            .rev()
            .map(|(price, q)| {
                let total = q
                    .iter()
                    .fold(FixedPoint::ZERO, |acc, o| acc + o.remaining_qty);
                (*price, total, q.len())
            })
            .collect()
    }

    /// Aggregated ask depth, best (lowest price) first — see [`Self::bid_depth`].
    pub fn ask_depth(&self) -> Vec<(FixedPoint, FixedPoint, usize)> {
        self.asks
            .iter()
            .map(|(price, q)| {
                let total = q
                    .iter()
                    .fold(FixedPoint::ZERO, |acc, o| acc + o.remaining_qty);
                (*price, total, q.len())
            })
            .collect()
    }

    /// Verify internal book invariants. Panics if any invariant is violated.
    /// Used by fuzz tests and determinism tests.
    pub fn verify_invariants(&self) {
        // Invariant 1: no crossed orders
        if let (Some(bid), Some(ask)) = (self.best_bid(), self.best_ask()) {
            assert!(bid < ask, "Crossed book: best_bid={bid} >= best_ask={ask}");
        }
        // Invariant 2: order_count matches actual orders in bids + asks
        let bid_orders: usize = self.bids.values().map(|q| q.len()).sum();
        let ask_orders: usize = self.asks.values().map(|q| q.len()).sum();
        assert_eq!(
            self.order_index.len(),
            bid_orders + ask_orders,
            "order_index({}) != bids({bid_orders}) + asks({ask_orders})",
            self.order_index.len()
        );
        // Invariant 3: all resting quantities > 0
        for queue in self.bids.values().chain(self.asks.values()) {
            for order in queue {
                assert!(
                    order.remaining_qty > FixedPoint::ZERO,
                    "Order {} has non-positive remaining qty",
                    order.id
                );
            }
        }
        // Invariant 4: no empty price levels
        for queue in self.bids.values().chain(self.asks.values()) {
            assert!(!queue.is_empty(), "Empty price level in book");
        }
    }

    // ========================================================================
    // Internal: Matching
    // ========================================================================

    /// Core matching: taker vs opposite side of the book.
    ///
    /// s515: the taker's price is a hard bound for EVERY order type — for a
    /// market order it is the slippage cap (the old `!is_market` bypass let a
    /// market order sweep the whole book at any price). Returns
    /// `(fills, self_trade_cancels, reduce_only_cuts)`.
    ///
    /// s515 review 4: with a `margin` limit every fill is first checked
    /// against it ([`TakerMarginLimit`]); matching stops at the first fill
    /// that does not fit (after taking the part of it that does).
    #[allow(clippy::type_complexity)]
    fn execute_match(
        &mut self,
        taker: &mut Order,
        mut margin: Option<&mut MatchMargin<'_>>,
        makers: Option<&dyn MakerAccountSource>,
    ) -> (Vec<Fill>, Vec<Order>, Vec<ReduceOnlyCut>, Vec<ReduceOnlyCut>) {
        let mut fills = Vec::new();
        let mut self_trade_cancels = Vec::new();
        let mut reduce_only_cuts = Vec::new();
        let mut margin_cancels = Vec::new();
        let market_id = self.market_id;
        let cache_on = self.level_hash_cache.is_some();
        let chunked_on = self.level_hash_chunked;

        match taker.side {
            Side::Buy => {
                while taker.remaining_qty > FixedPoint::ZERO
                    && !margin.as_ref().is_some_and(|m| m.exhausted)
                {
                    let mut level = match self.asks.first_entry() {
                        Some(entry) => entry,
                        None => break,
                    };
                    let best_ask = *level.key();
                    if best_ask > taker.price {
                        break;
                    }
                    let queue = level.get_mut();
                    Self::match_at_level(
                        taker,
                        queue,
                        best_ask,
                        Side::Sell,
                        &mut fills,
                        &mut self_trade_cancels,
                        &mut reduce_only_cuts,
                        &mut self.reduce_only_positions,
                        &mut self.order_index,
                        &mut self.trader_orders,
                        &mut self.order_seq,
                        &mut self.row_journal,
                        &mut self.level_journal,
                        &mut self.level_epoch,
                        cache_on,
                        &mut self.dirty_chunks,
                        chunked_on,
                        margin.as_deref_mut(),
                        &mut self.account_margins,
                        makers,
                        market_id,
                        &mut margin_cancels,
                    );
                    if level.get().is_empty() {
                        level.remove_entry();
                    }
                }
            }
            Side::Sell => {
                while taker.remaining_qty > FixedPoint::ZERO
                    && !margin.as_ref().is_some_and(|m| m.exhausted)
                {
                    let mut level = match self.bids.last_entry() {
                        Some(entry) => entry,
                        None => break,
                    };
                    let best_bid = *level.key();
                    if best_bid < taker.price {
                        break;
                    }
                    let queue = level.get_mut();
                    Self::match_at_level(
                        taker,
                        queue,
                        best_bid,
                        Side::Buy,
                        &mut fills,
                        &mut self_trade_cancels,
                        &mut reduce_only_cuts,
                        &mut self.reduce_only_positions,
                        &mut self.order_index,
                        &mut self.trader_orders,
                        &mut self.order_seq,
                        &mut self.row_journal,
                        &mut self.level_journal,
                        &mut self.level_epoch,
                        cache_on,
                        &mut self.dirty_chunks,
                        chunked_on,
                        margin.as_deref_mut(),
                        &mut self.account_margins,
                        makers,
                        market_id,
                        &mut margin_cancels,
                    );
                    if level.get().is_empty() {
                        level.remove_entry();
                    }
                }
            }
        }

        (fills, self_trade_cancels, reduce_only_cuts, margin_cancels)
    }

    /// Match taker against orders at a single price level.
    /// Static method to satisfy the borrow checker (operates on disjoint fields).
    #[allow(clippy::too_many_arguments)]
    fn match_at_level(
        taker: &mut Order,
        queue: &mut VecDeque<Order>,
        price: FixedPoint,
        maker_side: Side,
        fills: &mut Vec<Fill>,
        self_trade_cancels: &mut Vec<Order>,
        reduce_only_cuts: &mut Vec<ReduceOnlyCut>,
        ro_positions: &mut ReduceOnlyPositions,
        order_index: &mut HashMap<OrderId, OrderLocation>,
        trader_orders: &mut HashMap<Address, TraderOrders>,
        order_seq: &mut HashMap<OrderId, u64>,
        row_journal: &mut BTreeSet<OrderId>,
        level_journal: &mut BTreeSet<(u8, i128)>,
        level_epoch: &mut HashMap<(u8, i128), u64>,
        cache_on: bool,
        dirty_chunks: &mut BTreeSet<(u8, i128, u64)>,
        chunked_on: bool,
        mut margin: Option<&mut MatchMargin<'_>>,
        accounts: &mut AccountMargins,
        makers: Option<&dyn MakerAccountSource>,
        market_id: MarketId,
        margin_cancels: &mut Vec<ReduceOnlyCut>,
    ) {
        let tag = crate::book_rows::side_tag(maker_side);
        let raw_price = price.raw();
        // Every path below mutates this maker level (self-trade pop, partial
        // fill, full fill) — journal it once up front. Every such mutation
        // touches the FRONT of the queue, so the cached level-hash prefix is
        // invalidated at the same guard (L3). Mode 3 marks the touched
        // maker's chunk per iteration below (its seq is known there).
        if taker.remaining_qty > FixedPoint::ZERO && !queue.is_empty() {
            level_journal.insert((crate::book_rows::side_tag(maker_side), price.raw()));
            Self::bump_level_epoch(
                cache_on,
                level_epoch,
                crate::book_rows::side_tag(maker_side),
                price.raw(),
            );
        }
        while taker.remaining_qty > FixedPoint::ZERO && !queue.is_empty() {
            let maker = queue.front().unwrap();

            // Self-trade prevention: cancel the resting (maker) order
            if maker.trader == taker.trader {
                let cancelled = queue.pop_front().unwrap();
                order_index.remove(&cancelled.id);
                if let Some(ids) = trader_orders.get_mut(&cancelled.trader) {
                    ids.remove(cancelled.id);
                }
                let seq = order_seq.remove(&cancelled.id);
                Self::mark_chunk_dirty(chunked_on, dirty_chunks, tag, raw_price, seq);
                row_journal.insert(cancelled.id);
                // A5: hand the whole cancelled order back so the executor can
                // release its remaining order-margin reservation.
                self_trade_cancels.push(cancelled);
                continue;
            }

            // s515 (reduce-only, match time): a policed reduce-only maker
            // fills at most its owner's CURRENT position (which this very
            // sweep may already have moved through the owner's other
            // orders); with nothing left to reduce it is cut from the book.
            let mut maker_fillable = maker.remaining_qty;
            if maker.reduce_only {
                if let Some(pos) = ro_positions.get(&maker.trader) {
                    let allowed = reduce_only_allowance(pos, maker.side == Side::Buy);
                    if allowed <= FixedPoint::ZERO {
                        let cut = queue.pop_front().unwrap();
                        order_index.remove(&cut.id);
                        if let Some(ids) = trader_orders.get_mut(&cut.trader) {
                            ids.remove(cut.id);
                        }
                        let seq = order_seq.remove(&cut.id);
                        Self::mark_chunk_dirty(chunked_on, dirty_chunks, tag, raw_price, seq);
                        row_journal.insert(cut.id);
                        reduce_only_cuts.push(ReduceOnlyCut {
                            order_id: cut.id,
                            trader: cut.trader,
                            price: cut.price,
                            qty: cut.remaining_qty,
                        });
                        continue;
                    }
                    maker_fillable = maker_fillable.min(allowed);
                }
            }

            // Capture maker info before mutable borrow
            let maker_id = maker.id;
            let maker_addr = maker.trader;
            let maker_side = maker.side;
            let maker_remaining = maker.remaining_qty;

            let mut fill_qty = taker.remaining_qty.min(maker_fillable);
            // s515 review 4: the taker's match-time margin check. Review 5:
            // the part of the fill that reduces the taker's current position
            // (policed map, advanced through its own fills) is free.
            // F1 (s517): at the position-size tier, closing releasing IM,
            // against the reservation + the sender's running free margin.
            // The taker state is committed only after the maker passed.
            let mut taker_cut = None;
            if let Some(m) = margin.as_deref_mut() {
                let free = ro_positions.get(&taker.trader).map_or(FixedPoint::ZERO, |p| {
                    reduce_only_allowance(p, taker.side == Side::Buy)
                });
                let lim = m.limit;
                let fits = lim.affordable(m, price, fill_qty, free, taker.remaining_qty);
                let exhausted = fits < fill_qty;
                fill_qty = fits;
                if fill_qty <= FixedPoint::ZERO {
                    m.exhausted = true;
                    break;
                }
                taker_cut = Some((exhausted, free));
            }
            // F1 (s517 #4, HL `marginCanceled`): the maker is checked on the
            // actual fill; one that cannot afford it is cancelled whole and
            // the taker moves on to the next maker.
            if let Some(src) = makers {
                if !maker_fill_fits(
                    accounts,
                    ro_positions,
                    src,
                    market_id,
                    maker_addr,
                    maker_side == Side::Buy,
                    price,
                    fill_qty,
                    maker_remaining,
                ) {
                    let cut = queue.pop_front().unwrap();
                    order_index.remove(&cut.id);
                    if let Some(ids) = trader_orders.get_mut(&cut.trader) {
                        ids.remove(cut.id);
                    }
                    let seq = order_seq.remove(&cut.id);
                    Self::mark_chunk_dirty(chunked_on, dirty_chunks, tag, raw_price, seq);
                    row_journal.insert(cut.id);
                    margin_cancels.push(ReduceOnlyCut {
                        order_id: cut.id,
                        trader: cut.trader,
                        price: cut.price,
                        qty: cut.remaining_qty,
                    });
                    continue;
                }
            }
            if let (Some(m), Some((exhausted, free))) = (margin.as_deref_mut(), taker_cut) {
                m.exhausted |= exhausted;
                // Cannot overflow: `affordable` returned a purely closing
                // quantity (charges nothing) or one whose notional it
                // computed checked.
                m.charged += price * (fill_qty - fill_qty.min(free));
            }
            if !ro_positions.is_empty() {
                ro_positions.apply_fill(&taker.trader, taker.side == Side::Buy, fill_qty);
                ro_positions.apply_fill(&maker_addr, maker_side == Side::Buy, fill_qty);
            }

            fills.push(Fill {
                maker_order_id: maker_id,
                taker_order_id: taker.id,
                price,
                quantity: fill_qty,
                maker: maker_addr,
                taker: taker.trader,
                maker_side,
                timestamp: taker.timestamp,
            });

            taker.remaining_qty -= fill_qty;

            let maker = queue.front_mut().unwrap();
            maker.remaining_qty -= fill_qty;

            // Maker row changed either way: partial fill rewrites it,
            // full fill deletes it. Mode 3: either way its frame's chunk is
            // dirty (partial = bytes changed, full = frame gone).
            row_journal.insert(maker_id);
            Self::mark_chunk_dirty(
                chunked_on,
                dirty_chunks,
                tag,
                raw_price,
                order_seq.get(&maker_id).copied(),
            );
            if maker.remaining_qty == FixedPoint::ZERO {
                let filled = queue.pop_front().unwrap();
                order_index.remove(&filled.id);
                if let Some(ids) = trader_orders.get_mut(&filled.trader) {
                    ids.remove(filled.id);
                }
                order_seq.remove(&filled.id);
            }
            if margin.as_ref().is_some_and(|m| m.exhausted) {
                break;
            }
        }
    }

    /// L3: bump a level's prefix-invalidation epoch. Called from EVERY
    /// non-append queue mutation site (front pop, mid-queue removal, in-place
    /// qty change). Gated on `cache_on` so the disabled state pays zero cost;
    /// entries are never removed for the book's lifetime (a re-created level
    /// must not see a reset epoch). Appends (`insert_order`) do NOT bump —
    /// they are the cacheable case.
    #[inline]
    fn bump_level_epoch(
        cache_on: bool,
        level_epoch: &mut HashMap<(u8, i128), u64>,
        tag: u8,
        raw_price: i128,
    ) {
        if cache_on {
            *level_epoch.entry((tag, raw_price)).or_insert(0) += 1;
        }
    }

    /// Mode 3: record that the chunk holding `seq` on level `(tag, raw_price)`
    /// changed. Called from EVERY queue-mutation site (append, front pop,
    /// mid-queue removal, in-place qty change) — the same surface that
    /// journals the level. Gated on `chunked_on` so modes 0/1/2 pay nothing.
    /// `seq == None` cannot happen for a resting order (every resting order
    /// has a seq) — debug-asserted; in release it marks nothing, which is
    /// still safe only because the level is journaled and would be rebuilt
    /// in full if it had no chunk state.
    #[inline]
    fn mark_chunk_dirty(
        chunked_on: bool,
        dirty_chunks: &mut BTreeSet<(u8, i128, u64)>,
        tag: u8,
        raw_price: i128,
        seq: Option<u64>,
    ) {
        if chunked_on {
            debug_assert!(seq.is_some(), "queue-mutated order has no seq");
            if let Some(seq) = seq {
                dirty_chunks.insert((tag, raw_price, seq / crate::book_rows::LEVEL_CHUNK_SEQS));
            }
        }
    }

    /// Insert an order into the book (at the back of its price level queue).
    /// Assigns the order's insertion sequence (queue-priority persistence)
    /// and journals its row + level.
    fn insert_order(&mut self, order: Order) {
        let side = order.side;
        let price = order.price;
        let id = order.id;
        let trader = order.trader;

        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        if order.reduce_only {
            self.reduce_only_index.insert((trader, id));
        }
        book.entry(price).or_default().push_back(order);

        self.order_index.insert(id, OrderLocation { side, price });
        self.trader_orders.entry(trader).or_default().push(id);

        let seq = self.next_seq;
        self.next_seq += 1;
        self.order_seq.insert(id, seq);
        self.row_journal.insert(id);
        self.level_journal
            .insert((crate::book_rows::side_tag(side), price.raw()));
        // Mode 3: the appended frame's chunk is dirty.
        Self::mark_chunk_dirty(
            self.level_hash_chunked,
            &mut self.dirty_chunks,
            crate::book_rows::side_tag(side),
            price.raw(),
            Some(seq),
        );
    }

    /// Would placing an order at `price` cross the spread?
    fn would_cross(&self, side: Side, price: FixedPoint) -> bool {
        match side {
            Side::Buy => self.best_ask().is_some_and(|ask| price >= ask),
            Side::Sell => self.best_bid().is_some_and(|bid| price <= bid),
        }
    }

    /// Read-only pre-check: can a FOK order be completely filled?
    ///
    /// Mirrors `execute_match` exactly: the price bound applies to every
    /// order type (s515: a market order's price is its cap), self-owned
    /// makers are skipped (STP), and a policed reduce-only maker contributes
    /// at most its owner's position as it evolves through the walk.
    ///
    /// `None` = not completely fillable; `Some(notional)` = fillable, with the
    /// complete fill's notional (Σ price × qty, `None` on overflow) for the
    /// FOK margin check (s515 review 4).
    fn can_fill_completely(
        &self,
        side: Side,
        price: FixedPoint,
        qty: FixedPoint,
        trader: Address,
        mut free: FixedPoint,
        mut makers: Option<(&dyn MakerAccountSource, &mut AccountMargins, &mut ReduceOnlyPositions)>,
    ) -> Option<Option<FixedPoint>> {
        let mut remaining = qty;
        let mut notional = Some(FixedPoint::ZERO);
        // Maker positions moved by earlier makers of this walk (policed only).
        let mut walked: BTreeMap<Address, FixedPoint> = BTreeMap::new();
        let levels: Box<dyn Iterator<Item = (&FixedPoint, &VecDeque<Order>)>> = match side {
            Side::Buy => Box::new(self.asks.iter().take_while(|(&p, _)| p <= price)),
            Side::Sell => Box::new(self.bids.iter().rev().take_while(|(&p, _)| p >= price)),
        };
        for (_, queue) in levels {
            for order in queue {
                if order.trader == trader {
                    continue;
                }
                let mut fillable = order.remaining_qty;
                let pos = walked
                    .get(&order.trader)
                    .copied()
                    .or_else(|| self.reduce_only_positions.get(&order.trader));
                if let (true, Some(pos)) = (order.reduce_only, pos) {
                    fillable = fillable.min(reduce_only_allowance(pos, order.side == Side::Buy));
                }
                let take = remaining.min(fillable);
                // F1 (s517 #4): skip a maker matching would cancel (same
                // `maker_fill_fits`, on the caller's clones).
                if let Some((src, acc, ro)) = makers.as_mut() {
                    if take > FixedPoint::ZERO {
                        let is_buy = order.side == Side::Buy;
                        if !maker_fill_fits(
                            acc,
                            ro,
                            *src,
                            self.market_id,
                            order.trader,
                            is_buy,
                            order.price,
                            take,
                            order.remaining_qty,
                        ) {
                            continue;
                        }
                        ro.apply_fill(&order.trader, is_buy, take);
                    }
                }
                if let Some(pos) = pos {
                    let signed = if order.side == Side::Buy { take } else { -take };
                    walked.insert(order.trader, pos + signed);
                }
                // s515 review 5: only the part beyond the taker's closing
                // allowance (`free`, consumed first) counts.
                let closing = take.min(free);
                free -= closing;
                notional = notional.and_then(|n| {
                    order
                        .price
                        .checked_mul(take - closing)
                        .and_then(|x| n.checked_add(x))
                        .ok()
                });
                remaining -= take;
                if remaining <= FixedPoint::ZERO {
                    return Some(notional);
                }
            }
        }
        None
    }

    /// Evaluate pending stop orders against the last trade price.
    ///
    /// s515: fired stops are REMOVED and RETURNED (trigger order), never
    /// placed here. Placing them inline bypassed the executor entirely —
    /// their fills never reached positions or margin, their reservation was
    /// never released, a StopMarket matched with no price cap, and a
    /// reduce-only TP/SL could not be re-checked against the position at
    /// trigger time. The executor now places each returned stop through its
    /// normal path; that placement's own fills re-evaluate the remaining
    /// stops (cascades stay bounded: every stop fires at most once).
    fn trigger_stops(&mut self) -> Vec<TriggeredStop> {
        let Some(current_price) = self.last_trade_price else {
            return Vec::new();
        };
        let mut fired = Vec::new();
        self.pending_stops.retain(|stop| {
            let fire = match stop.side {
                Side::Buy => current_price >= stop.trigger_price,
                Side::Sell => current_price <= stop.trigger_price,
            };
            if fire {
                fired.push(stop.clone());
            }
            !fire
        });
        fired
            .into_iter()
            .map(|stop| {
                let (order_type, price) = match stop.limit_price {
                    None => (OrderType::Market, stop.price_cap),
                    Some(limit) => (OrderType::Limit, limit),
                };
                TriggeredStop {
                    id: stop.id,
                    trader: stop.trader,
                    timestamp: stop.timestamp,
                    params: PlaceOrderParams {
                        market_id: stop.market_id,
                        is_buy: stop.side == Side::Buy,
                        price,
                        quantity: stop.quantity,
                        order_type,
                        time_in_force: stop.time_in_force,
                        reduce_only: stop.reduce_only,
                        client_order_id: stop.client_order_id,
                    },
                }
            })
            .collect()
    }
}

// ============================================================================
// C4 (perf/exec-scaleup): canonical read access + rebuild hooks for
// per-order-row book persistence (torus-bridge, `TORUS_BOOK_ROWS`).
//
// The row store persists each resting order / pending stop as its own KV row
// instead of one whole-book Borsh blob, so it needs (a) level-structured
// read access in the CANONICAL order (the exact order the whole-book Borsh
// serializer walks: bids ascending price then asks ascending price, FIFO
// within a level) and (b) rebuild hooks equivalent to what
// `BorshDeserialize for OrderBook` does internally. Nothing here can express
// a book state the classic (de)serializer couldn't.
// ============================================================================

impl OrderBook {
    /// Bid price levels ascending (the whole-book serializer's walk order),
    /// each with its FIFO queue (front = highest time priority).
    pub fn bid_queues(&self) -> impl Iterator<Item = (&FixedPoint, &VecDeque<Order>)> {
        self.bids.iter()
    }

    /// Ask price levels ascending, each with its FIFO queue.
    pub fn ask_queues(&self) -> impl Iterator<Item = (&FixedPoint, &VecDeque<Order>)> {
        self.asks.iter()
    }

    /// Set the last trade price during a persistence rebuild (the classic
    /// blob carries it in its header).
    pub fn set_last_trade_price(&mut self, ltp: Option<FixedPoint>) {
        self.last_trade_price = ltp;
    }

    /// Pending stop orders as `(id, borsh bytes)` in trigger (Vec) order.
    /// Stop ids are allocated monotonically and stops are only ever appended /
    /// removed (`Vec::retain` preserves relative order), so Vec order ==
    /// ascending id — the row store relies on this to rebuild trigger order
    /// from id-sorted rows. Debug-asserted here.
    pub fn stop_rows(&self) -> Vec<(OrderId, Vec<u8>)> {
        debug_assert!(
            self.pending_stops.windows(2).all(|w| w[0].id < w[1].id),
            "pending_stops must stay id-ascending (rebuild order invariant)"
        );
        self.pending_stops
            .iter()
            .map(|s| {
                (
                    s.id,
                    borsh::to_vec(s).expect("StopOrder borsh serialize cannot fail"),
                )
            })
            .collect()
    }

    /// rank8: the FIFO queue at one price level, if it exists. Read access for
    /// the journal-driven row differ (walks only affected levels).
    pub fn level_queue(&self, side: Side, price: FixedPoint) -> Option<&VecDeque<Order>> {
        match side {
            Side::Buy => self.bids.get(&price),
            Side::Sell => self.asks.get(&price),
        }
    }

    /// Rebuild one pending stop from its row bytes (see [`Self::stop_rows`]).
    /// Callers MUST append in ascending id order. Returns the stop's id.
    pub fn restore_stop_row(&mut self, bytes: &[u8]) -> io::Result<OrderId> {
        let stop = StopOrder::try_from_slice(bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let Some(last) = self.pending_stops.last() {
            if stop.id <= last.id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stop rows must be restored in ascending id order",
                ));
            }
        }
        let id = stop.id;
        self.pending_stops.push(stop);
        Ok(id)
    }
}

// ============================================================================
// Journal-in-book persistence codec (3c)
//
// The row/level KEY layouts live in `crate::book_rows`; this block owns the
// VALUE encodings and journal draining, which need the book's private fields.
//
// STATE-ROOT PREIMAGE: order-row bytes (mode 1) and level-row bytes incl.
// `level_hash` (mode 2) are committed by the native state root. FROZEN once
// deployed.
// ============================================================================

impl OrderBook {
    /// The book-owned monotonic queue-seq allocator (persisted in the meta row).
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Restore the seq allocator during a persistence rebuild.
    pub fn set_next_seq(&mut self, next_seq: u64) {
        self.next_seq = next_seq;
    }

    /// The insertion seq of a resting order (test/verify hook).
    pub fn order_seq_of(&self, order_id: OrderId) -> Option<u64> {
        self.order_seq.get(&order_id).copied()
    }

    /// Encode one resting order's row value: `seq(8 BE) ‖ borsh(Order)` (the
    /// frozen `Order` codec; the id is repeated inside for integrity).
    /// `None` if the order is not resting.
    pub fn encode_order_row(&self, order_id: OrderId) -> Option<Vec<u8>> {
        let seq = *self.order_seq.get(&order_id)?;
        let order = self.get_order(order_id)?;
        Some(Self::encode_order_row_parts(seq, order))
    }

    /// `seq(8 BE) ‖ borsh(Order)` from parts (shared with the level hasher).
    ///
    /// Thin owning wrapper over [`Self::encode_order_row_into`] — ONE encoding
    /// path, so the allocating and the buffer-reusing callers can never drift.
    fn encode_order_row_parts(seq: u64, order: &Order) -> Vec<u8> {
        let mut w = Vec::with_capacity(8 + 112);
        Self::encode_order_row_into(&mut w, seq, order);
        w
    }

    /// The frozen row encoding staged into a CALLER-OWNED buffer:
    /// `seq(8 BE) ‖ borsh(Order)`, byte-for-byte what
    /// [`Self::encode_order_row_parts`] returns — only WHERE the bytes land
    /// differs. `buf` is truncated first, so on return it holds EXACTLY the
    /// row and `buf.len()` is the row length the level hasher must frame with.
    /// Hot-path form: hoist one buffer out of a per-order loop and the
    /// malloc/free pair per resting order disappears (the capacity is reused).
    fn encode_order_row_into(buf: &mut Vec<u8>, seq: u64, order: &Order) {
        buf.clear();
        buf.extend_from_slice(&seq.to_be_bytes());
        order.serialize(buf).expect("vec write");
    }

    /// The trader address bytes of an order-row value (`seq(8) ‖ id(16) ‖
    /// trader(20) ‖ …`) without decoding the row; `None` if it is too short.
    pub fn order_row_trader(bytes: &[u8]) -> Option<&[u8]> {
        bytes.get(24..44)
    }

    /// Decode an order-row value into `(seq, order)`.
    pub fn decode_order_row(bytes: &[u8]) -> io::Result<(u64, Order)> {
        let mut r = bytes;
        let mut s = [0u8; 8];
        r.read_exact(&mut s)?;
        let seq = u64::from_be_bytes(s);
        let order = Order::deserialize_reader(&mut r)?;
        Ok((seq, order))
    }

    /// Insert an order loaded FROM a persisted row: restores its stored seq,
    /// does NOT journal (a load is not a mutation), and marks its row + level
    /// as persisted. Callers must insert in canonical order (bids ascending
    /// (price, seq), then asks) — the loader owns that ordering.
    pub fn insert_loaded_order(&mut self, order: Order, seq: u64) {
        let side = order.side;
        let price = order.price;
        let id = order.id;
        let trader = order.trader;

        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let queue = book.entry(price).or_default();
        // Mode 3 relies on queue order == ascending seq (chunk members are a
        // contiguous queue range found by binary search on seq) — the loader
        // owns that ordering; assert it in debug.
        debug_assert!(
            queue
                .back()
                .and_then(|o| self.order_seq.get(&o.id))
                .is_none_or(|&prev| prev < seq),
            "insert_loaded_order: seq {seq} not ascending within its level"
        );
        if order.reduce_only {
            self.reduce_only_index.insert((trader, id));
        }
        queue.push_back(order);
        self.order_index.insert(id, OrderLocation { side, price });
        self.trader_orders.entry(trader).or_default().push(id);
        self.order_seq.insert(id, seq);
        self.row_exists.insert(id);
        // A loaded order implies its level's persisted row exists (save-path
        // invariant) — mark it so a later emptying save deletes it.
        self.level_exists
            .insert((crate::book_rows::side_tag(side), price.raw()));
        debug_assert!(seq < self.next_seq, "loaded seq {seq} >= meta next_seq");
        // Mode 3: a load is not a mutation (no journal), but if chunk state
        // already exists for this level (test fixtures) it is now stale —
        // mark the chunk so the next drain of the level refreshes it.
        Self::mark_chunk_dirty(
            self.level_hash_chunked,
            &mut self.dirty_chunks,
            crate::book_rows::side_tag(side),
            price.raw(),
            Some(seq),
        );
    }

    /// Drain the row journal into row ops, O(touched since last save):
    /// `(order_id, Some(row_bytes))` = upsert, `(order_id, None)` = delete.
    /// Ids placed AND removed since the last save (no row ever persisted)
    /// are skipped. Deterministic ascending-id order.
    pub fn take_row_ops(&mut self) -> Vec<(OrderId, Option<Vec<u8>>)> {
        let ids = std::mem::take(&mut self.row_journal);
        let mut ops = Vec::with_capacity(ids.len());
        for id in ids {
            if self.order_index.contains_key(&id) {
                let bytes = self
                    .encode_order_row(id)
                    .expect("resting order must encode");
                self.row_exists.insert(id);
                ops.push((id, Some(bytes)));
            } else if self.row_exists.remove(&id) {
                ops.push((id, None));
            }
        }
        ops
    }

    /// Row upserts for EVERY resting order (full write — genesis, tests,
    /// offline rebuild). Resets the journal and marks all rows persisted.
    /// Deterministic ascending-id order.
    pub fn full_row_ops(&mut self) -> Vec<(OrderId, Vec<u8>)> {
        self.row_journal.clear();
        self.row_exists.clear();
        let mut ids: Vec<OrderId> = self.order_index.keys().copied().collect();
        ids.sort_unstable();
        let mut ops = Vec::with_capacity(ids.len());
        for id in ids {
            let bytes = self
                .encode_order_row(id)
                .expect("resting order must encode");
            self.row_exists.insert(id);
            ops.push((id, bytes));
        }
        ops
    }

    /// Number of journaled (touched-since-last-save) rows — test/metrics hook.
    pub fn journaled_rows(&self) -> usize {
        self.row_journal.len()
    }

    /// Number of journaled (touched-since-last-save) price levels — the
    /// mode-2 save's LPT weight hint (`take_level_ops` work is O(touched
    /// levels) journal work + hashing) and a test/metrics hook.
    pub fn journaled_levels(&self) -> usize {
        self.level_journal.len()
    }

    /// The consensus aggregate of one price level (level-row VALUE parts), or
    /// `None` if the level has no resting orders. O(orders in the level):
    /// sums `remaining_qty` and keccaks the framed order rows front→back.
    pub fn level_row_data(&self, tag: u8, raw_price: i128) -> Option<crate::book_rows::LevelRowData> {
        let price = FixedPoint::from_raw(raw_price);
        let side_book = if tag == crate::book_rows::SIDE_TAG_BID {
            &self.bids
        } else {
            &self.asks
        };
        let queue = side_book.get(&price).filter(|q| !q.is_empty())?;
        let mut total = FixedPoint::ZERO;
        let mut preimage: Vec<u8> = Vec::with_capacity(queue.len() * 160);
        // ONE scratch row for the whole level: `encode_order_row_into` clears
        // it per order, so each iteration stages the same bytes a fresh Vec
        // would have held — without the per-order malloc/free.
        let mut scratch: Vec<u8> = Vec::with_capacity(8 + 112);
        for order in queue {
            total += order.remaining_qty;
            let seq = *self
                .order_seq
                .get(&order.id)
                .expect("resting order must have a seq");
            Self::encode_order_row_into(&mut scratch, seq, order);
            preimage.extend_from_slice(&(scratch.len() as u32).to_le_bytes());
            preimage.extend_from_slice(&scratch);
        }
        Some(crate::book_rows::LevelRowData {
            total_qty_raw: total.raw(),
            order_count: queue.len() as u32,
            level_hash: alloy_primitives::keccak256(&preimage).0,
        })
    }

    /// Drain the level journal into level-row ops, O(touched levels) journal
    /// work + O(orders in touched levels) hashing (or O(appended tail) per
    /// level when the L3 sponge cache holds a valid prefix):
    /// `((side_tag, raw_price), Some(data))` = upsert, `(_, None)` = delete.
    /// Levels touched-and-emptied with no persisted row are skipped (mirrors
    /// `take_row_ops`). Deterministic: BTreeSet drain order. Cache-on and
    /// cache-off produce BYTE-IDENTICAL data (the cache only changes how the
    /// same keccak digest is computed — docs/design-levelhash-cache.md).
    pub fn take_level_ops(
        &mut self,
    ) -> Vec<((u8, i128), Option<crate::book_rows::LevelRowData>)> {
        if self.level_hash_chunked {
            return self.take_level_ops_chunked();
        }
        let keys = std::mem::take(&mut self.level_journal);
        // Move the cache out to sidestep the &self/&mut cache split borrow.
        let mut cache = self.level_hash_cache.take();
        let mut ops = Vec::with_capacity(keys.len());
        for key in keys {
            let (tag, raw_price) = key;
            let data = match cache.as_deref_mut() {
                Some(c) => self.level_row_data_cached(tag, raw_price, c),
                None => self.level_row_data(tag, raw_price),
            };
            match data {
                Some(data) => {
                    self.level_exists.insert(key);
                    ops.push((key, Some(data)));
                }
                None => {
                    // Emptied level: drop its cache entry (its epoch entry
                    // stays — see bump_level_epoch).
                    if let Some(c) = cache.as_deref_mut() {
                        c.entries.remove(&key);
                    }
                    if self.level_exists.remove(&key) {
                        ops.push((key, None));
                    }
                }
            }
        }
        self.level_hash_cache = cache;
        ops
    }

    /// L3 cached variant of [`Self::level_row_data`] — byte-identical output,
    /// with **staged seeding** (docs/design-levelhash-cache.md §1.3): three
    /// mutually-exclusive arms keyed on the level's slot state and epoch:
    ///
    /// - **HIT** (`Seeded`, epoch unchanged): clone-extend the stored sponge
    ///   with only the appended tail frames — O(new tail).
    /// - **PROMOTE** (`Probe`, epoch unchanged): the level survived one full
    ///   append-only interval, so invest now — one full front→back absorb that
    ///   both yields this save's digest and seeds the `Seeded` sponge for the
    ///   future O(tail) hits.
    /// - **MISS** (no slot, churning `Probe`, or stale `Seeded`): the frozen
    ///   one-shot path verbatim (`Self::level_row_data`, asm-backed keccak) plus
    ///   a cheap `Probe` record — NO streaming absorb, NO sponge clone. A level
    ///   that invalidates every save pays only the plain cost + one map insert.
    fn level_row_data_cached(
        &self,
        tag: u8,
        raw_price: i128,
        cache: &mut LevelHashCache,
    ) -> Option<crate::book_rows::LevelRowData> {
        use sha3::Digest;
        let price = FixedPoint::from_raw(raw_price);
        let side_book = if tag == crate::book_rows::SIDE_TAG_BID {
            &self.bids
        } else {
            &self.asks
        };
        let queue = side_book.get(&price).filter(|q| !q.is_empty())?;
        let key = (tag, raw_price);
        let epoch = self.level_epoch.get(&key).copied().unwrap_or(0);
        let n = queue.len();
        cache.tick += 1;
        let tick = cache.tick;

        // Classify the slot without holding a borrow across the plain recompute.
        enum Arm {
            Hit,
            /// Carries the probe's recorded frame count for the append-only
            /// invariant check.
            Promote(u32),
            Miss,
        }
        let arm = match cache.entries.get(&key) {
            Some(LevelHashCacheEntry::Seeded { epoch: e, frame_count, .. })
                if *e == epoch && *frame_count as usize <= n =>
            {
                Arm::Hit
            }
            Some(LevelHashCacheEntry::Probe { epoch: e, frame_count, .. }) if *e == epoch => {
                Arm::Promote(*frame_count)
            }
            // No slot, churning probe (epoch moved), or stale seed ⇒ plain path.
            _ => Arm::Miss,
        };

        match arm {
            Arm::Hit => {
                // No invalidating op since the prefix was absorbed ⇒ the first
                // `start` frames are byte-identical to what the sponge holds
                // (design doc §1). Absorb only the appended tail.
                let LevelHashCacheEntry::Seeded {
                    frame_count,
                    absorbed_len,
                    prefix_qty,
                    hasher,
                    last_used,
                    ..
                } = cache.entries.get_mut(&key).expect("slot present")
                else {
                    unreachable!("classified Hit ⇒ Seeded slot")
                };
                let start = *frame_count as usize;
                // One hoisted scratch row for the whole tail (see
                // `encode_order_row_into`) — same bytes, no per-order alloc.
                let mut scratch: Vec<u8> = Vec::with_capacity(8 + 112);
                for order in queue.iter().skip(start) {
                    // Same FixedPoint += order as the one-shot sum.
                    *prefix_qty += order.remaining_qty;
                    let seq = *self
                        .order_seq
                        .get(&order.id)
                        .expect("resting order must have a seq");
                    Self::encode_order_row_into(&mut scratch, seq, order);
                    hasher.update((scratch.len() as u32).to_le_bytes());
                    hasher.update(&scratch);
                    *absorbed_len += 4 + scratch.len() as u64;
                }
                *frame_count = n as u32;
                *last_used = tick;
                let total_qty_raw = prefix_qty.raw();
                let digest: [u8; 32] = hasher.clone().finalize().into();
                cache.hits += 1;
                Some(crate::book_rows::LevelRowData {
                    total_qty_raw,
                    order_count: n as u32,
                    level_hash: digest,
                })
            }
            Arm::Promote(probe_frames) => {
                // Epoch unchanged since the probe ⇒ only appends ran between the
                // two saves ⇒ the queue can only have grown (the probe's prefix
                // is still a prefix). A shrink here would mean an invalidating
                // op bumped nothing — the fatal direction ruled out in §1.2.
                debug_assert!(
                    n >= probe_frames as usize,
                    "append-only interval must not shrink the level \
                     (n={n} < probe frames={probe_frames})"
                );
                // Second consecutive save with no epoch bump ⇒ the interval was
                // append-only. INVEST: one full front→back absorb — the exact
                // byte stream the one-shot path keccaks — that yields this
                // save's digest AND seeds the sponge (replaces the probe slot,
                // so no entry-count growth ⇒ no eviction needed).
                let (total, absorbed, hasher) = self.absorb_level(queue);
                let digest: [u8; 32] = hasher.clone().finalize().into();
                cache.entries.insert(
                    key,
                    LevelHashCacheEntry::Seeded {
                        epoch,
                        frame_count: n as u32,
                        absorbed_len: absorbed,
                        prefix_qty: total,
                        hasher,
                        last_used: tick,
                    },
                );
                cache.seeds += 1;
                Some(crate::book_rows::LevelRowData {
                    total_qty_raw: total.raw(),
                    order_count: n as u32,
                    level_hash: digest,
                })
            }
            Arm::Miss => {
                // Plain one-shot path VERBATIM (asm-backed one-shot keccak) plus
                // a cheap probe: the sponge investment is deferred until this
                // level proves append-only (the next same-epoch save).
                let data = self.level_row_data(tag, raw_price)?;
                cache.insert_bounded(
                    key,
                    LevelHashCacheEntry::Probe {
                        epoch,
                        frame_count: n as u32,
                        last_used: tick,
                    },
                );
                cache.misses += 1;
                Some(data)
            }
        }
    }

    /// Full front→back streaming absorb of a level's framed order rows into a
    /// fresh keccak sponge, returning `(Σ remaining_qty, absorbed byte count,
    /// un-finalized hasher)`. The absorbed byte stream is identical to the
    /// one-shot `level_row_data` preimage (same `encode_order_row_parts` +
    /// u32-LE length framing), so `hasher.clone().finalize()` is byte-identical.
    fn absorb_level(&self, queue: &VecDeque<Order>) -> (FixedPoint, u64, sha3::Keccak256) {
        use sha3::Digest;
        let mut hasher = sha3::Keccak256::new();
        let mut total = FixedPoint::ZERO;
        let mut absorbed: u64 = 0;
        // One hoisted scratch row for the whole level (see
        // `encode_order_row_into`) — same bytes, no per-order alloc.
        let mut scratch: Vec<u8> = Vec::with_capacity(8 + 112);
        for order in queue {
            total += order.remaining_qty;
            let seq = *self
                .order_seq
                .get(&order.id)
                .expect("resting order must have a seq");
            Self::encode_order_row_into(&mut scratch, seq, order);
            hasher.update((scratch.len() as u32).to_le_bytes());
            hasher.update(&scratch);
            absorbed += 4 + scratch.len() as u64;
        }
        (total, absorbed, hasher)
    }

    /// L3: enable (or re-size) the level-hash sponge cache with a byte
    /// budget. `0` disables (drops) it — the default state; disabled =
    /// exact-today one-shot hashing. Node-local: output bytes are identical
    /// either way.
    pub fn ensure_level_hash_cache(&mut self, budget_bytes: usize) {
        if budget_bytes == 0 {
            self.level_hash_cache = None;
            return;
        }
        let max_entries = (budget_bytes / LEVEL_CACHE_ENTRY_COST).max(1);
        match &mut self.level_hash_cache {
            Some(c) => c.max_entries = max_entries,
            None => {
                self.level_hash_cache = Some(Box::new(LevelHashCache {
                    entries: HashMap::new(),
                    max_entries,
                    tick: 0,
                    hits: 0,
                    misses: 0,
                    seeds: 0,
                }))
            }
        }
    }

    /// L3: drop the level-hash cache (returns to exact-today hashing).
    pub fn disable_level_hash_cache(&mut self) {
        self.level_hash_cache = None;
    }

    /// L3 test/metrics hook: `(hits, misses, seeds, live entries)` if enabled.
    /// `hits` = O(tail) sponge extensions, `misses` = plain one-shot fallbacks,
    /// `seeds` = probe→sponge promotions (staged-seeding investments).
    pub fn level_hash_cache_stats(&self) -> Option<(u64, u64, u64, usize)> {
        self.level_hash_cache
            .as_ref()
            .map(|c| (c.hits, c.misses, c.seeds, c.entries.len()))
    }

    /// Modes 0/1 never persist level rows: drop the journaled level keys
    /// without computing any aggregates (no keccak spent).
    pub fn discard_level_ops(&mut self) {
        self.level_journal.clear();
        // Mode 3 bookkeeping is meaningless without level rows; keep it from
        // leaking if a chunked book is ever drained this way.
        self.dirty_chunks.clear();
        self.level_chunks.clear();
    }

    /// Classic mode never persists per-order rows: drop journaled ids without
    /// encoding anything.
    pub fn discard_row_ops(&mut self) {
        self.row_journal.clear();
    }

    /// Level rows for EVERY non-empty level (full write — genesis, tests,
    /// offline rebuild). Resets the level journal and marks all level rows
    /// persisted. Deterministic: bids then asks, price ascending.
    pub fn full_level_ops(&mut self) -> Vec<((u8, i128), crate::book_rows::LevelRowData)> {
        self.level_journal.clear();
        self.level_exists.clear();
        // L3: full writes (genesis / offline rebuild) use the plain path;
        // drop any cached sponge states defensively.
        if let Some(c) = self.level_hash_cache.as_deref_mut() {
            c.entries.clear();
        }
        let keys: Vec<(u8, i128)> = self
            .bids
            .keys()
            .map(|p| (crate::book_rows::SIDE_TAG_BID, p.raw()))
            .chain(
                self.asks
                    .keys()
                    .map(|p| (crate::book_rows::SIDE_TAG_ASK, p.raw())),
            )
            .collect();
        let mut ops = Vec::with_capacity(keys.len());
        if self.level_hash_chunked {
            // Mode 3: rebuild EVERY level's chunk state from its queue (the
            // full write is the reference point for later incremental
            // drains) and emit the chunked digest.
            self.level_chunks.clear();
            self.dirty_chunks.clear();
            for key in keys {
                let data = self
                    .rebuild_level_chunks(key)
                    .expect("non-empty level must aggregate");
                self.level_exists.insert(key);
                ops.push((key, data));
            }
            return ops;
        }
        for key in keys {
            let data = self
                .level_row_data(key.0, key.1)
                .expect("non-empty level must aggregate");
            self.level_exists.insert(key);
            ops.push((key, data));
        }
        ops
    }
}

// ============================================================================
// Chunked level digest (mode 3, `TORUS_BOOK_ROWS=3`) — depth-independent
// level-hash maintenance.
//
// The flat mode-2 digest keccaks EVERY frame of a dirty level (O(depth) per
// touched level; with band-5 flow the top levels hold thousands of orders and
// fills consume the FRONT, so the append-only sponge cache misses). Mode 3
// buckets a level's frames by `chunk_idx = seq / LEVEL_CHUNK_SEQS` (absolute
// per-book seq — monotone, unique, persisted with each row) and commits
//
//   level_hash = keccak(DOMAIN ‖ count ‖ total_qty ‖ Σ_asc (idx ‖ chunk_digest))
//
// (`book_rows::LevelRowData`). Because the queue is seq-ascending, a chunk's
// members are one contiguous queue range (binary search on seq), and because
// the digest is a pure function of level content, only the chunk(s) whose
// members changed need re-hashing: every queue-mutation site marks
// `(level, seq / 64)` dirty (`mark_chunk_dirty` — the same surface that
// journals the level, docs/design-levelhash-cache.md §1.1), and the drain
// re-hashes just those chunks plus the top hash (n_chunks × 40 B). A level
// with no chunk state (first drain after boot / level creation) is built in
// full — the mode-2 cost, once. The from-scratch `level_row_data_chunked` is
// the boot-verify / oracle path and MUST equal the incremental result for
// every content (property-tested in `level_rows_core_tests`).
//
// Chunk state, dirty marks and the mode flag are node-local RAM: never
// serialized, never part of any row. Determinism: every validator runs the
// same mode (marker byte 3 + boot verify fail-stop) and computes the same
// pure function of the same content.
// ============================================================================

impl OrderBook {
    /// Select the level-hash preimage `take_level_ops` / `full_level_ops`
    /// emit: `true` = chunked (mode 3), `false` = flat (mode 2, default).
    /// The executor sets this for its mode BEFORE the first drain. Turning
    /// it off drops all chunk state.
    pub fn set_level_hash_chunked(&mut self, on: bool) {
        if !on {
            self.level_chunks.clear();
            self.dirty_chunks.clear();
        }
        self.level_hash_chunked = on;
    }

    /// Whether the chunked (mode 3) level digest is selected.
    pub fn level_hash_chunked(&self) -> bool {
        self.level_hash_chunked
    }

    /// Test/metrics hook: `(levels with chunk state, total stored chunks,
    /// pending dirty marks)`.
    pub fn level_chunk_stats(&self) -> (usize, usize, usize) {
        (
            self.level_chunks.len(),
            self.level_chunks.values().map(|c| c.len()).sum(),
            self.dirty_chunks.len(),
        )
    }

    /// From-scratch chunked (mode 3) aggregate of one price level — a pure
    /// function of the level's content, `None` if the level has no resting
    /// orders. O(orders in the level). Boot verify / oracle path; the drain
    /// path (`take_level_ops`) reaches the same bytes incrementally.
    pub fn level_row_data_chunked(
        &self,
        tag: u8,
        raw_price: i128,
    ) -> Option<crate::book_rows::LevelRowData> {
        let price = FixedPoint::from_raw(raw_price);
        let side_book = if tag == crate::book_rows::SIDE_TAG_BID {
            &self.bids
        } else {
            &self.asks
        };
        let queue = side_book.get(&price).filter(|q| !q.is_empty())?;
        let mut chunks = BTreeMap::new();
        Self::rebuild_all_chunks(queue, &self.order_seq, &mut chunks);
        Some(Self::chunked_top(&chunks))
    }

    /// Mode-3 drain: for every journaled level, refresh only its dirty chunks
    /// (or build the whole chunk state if the level has none) and re-derive
    /// the top hash. Emptied levels drop their chunk state and emit a delete
    /// iff a row was persisted (mirrors the flat path).
    fn take_level_ops_chunked(
        &mut self,
    ) -> Vec<((u8, i128), Option<crate::book_rows::LevelRowData>)> {
        let keys = std::mem::take(&mut self.level_journal);
        let mut ops = Vec::with_capacity(keys.len());
        let mut dirty_scratch: Vec<u64> = Vec::new();
        for key in keys {
            let (tag, raw_price) = key;
            // Take this level's dirty marks (leave marks of other levels).
            dirty_scratch.clear();
            let lo = (tag, raw_price, 0u64);
            let hi = (tag, raw_price, u64::MAX);
            dirty_scratch.extend(self.dirty_chunks.range(lo..=hi).map(|k| k.2));
            for idx in &dirty_scratch {
                self.dirty_chunks.remove(&(tag, raw_price, *idx));
            }

            let price = FixedPoint::from_raw(raw_price);
            let side_book = if tag == crate::book_rows::SIDE_TAG_BID {
                &self.bids
            } else {
                &self.asks
            };
            let queue = side_book.get(&price).filter(|q| !q.is_empty());
            let data = match queue {
                None => {
                    self.level_chunks.remove(&key);
                    None
                }
                Some(queue) => match self.level_chunks.entry(key) {
                    std::collections::hash_map::Entry::Vacant(v) => {
                        let mut chunks = BTreeMap::new();
                        Self::rebuild_all_chunks(queue, &self.order_seq, &mut chunks);
                        let data = Self::chunked_top(&chunks);
                        v.insert(chunks);
                        Some(data)
                    }
                    std::collections::hash_map::Entry::Occupied(mut o) => {
                        let chunks = o.get_mut();
                        for &idx in &dirty_scratch {
                            Self::rebuild_one_chunk(queue, &self.order_seq, chunks, idx);
                        }
                        Some(Self::chunked_top(chunks))
                    }
                },
            };
            match data {
                Some(data) => {
                    self.level_exists.insert(key);
                    ops.push((key, Some(data)));
                }
                None => {
                    if self.level_exists.remove(&key) {
                        ops.push((key, None));
                    }
                }
            }
        }
        ops
    }

    /// Full (re)build of one level's chunk state from its queue and the
    /// chunked aggregate; `None` if the level is empty (state dropped).
    fn rebuild_level_chunks(&mut self, key: (u8, i128)) -> Option<crate::book_rows::LevelRowData> {
        let (tag, raw_price) = key;
        let price = FixedPoint::from_raw(raw_price);
        let side_book = if tag == crate::book_rows::SIDE_TAG_BID {
            &self.bids
        } else {
            &self.asks
        };
        let Some(queue) = side_book.get(&price).filter(|q| !q.is_empty()) else {
            self.level_chunks.remove(&key);
            return None;
        };
        let chunks = self.level_chunks.entry(key).or_default();
        chunks.clear();
        Self::rebuild_all_chunks(queue, &self.order_seq, chunks);
        Some(Self::chunked_top(chunks))
    }

    /// Hash EVERY chunk of a level from its queue (one front→back walk,
    /// grouping consecutive frames by `seq / LEVEL_CHUNK_SEQS`) into `chunks`
    /// (cleared first).
    fn rebuild_all_chunks(
        queue: &VecDeque<Order>,
        order_seq: &HashMap<OrderId, u64>,
        chunks: &mut BTreeMap<u64, ChunkAgg>,
    ) {
        chunks.clear();
        let mut scratch: Vec<u8> = Vec::with_capacity(8 + 112);
        let mut preimage: Vec<u8> = Vec::with_capacity(
            crate::book_rows::LEVEL_CHUNK_SEQS as usize * 160,
        );
        let mut cur: Option<u64> = None;
        let mut qty = FixedPoint::ZERO;
        let mut count: u32 = 0;
        for order in queue {
            let seq = *order_seq
                .get(&order.id)
                .expect("resting order must have a seq");
            let idx = seq / crate::book_rows::LEVEL_CHUNK_SEQS;
            if cur != Some(idx) {
                if let Some(prev) = cur {
                    chunks.insert(
                        prev,
                        ChunkAgg {
                            digest: alloy_primitives::keccak256(&preimage).0,
                            qty,
                            count,
                        },
                    );
                }
                debug_assert!(
                    cur.is_none_or(|prev| prev < idx),
                    "level queue must be seq-ascending (chunk {idx} after {cur:?})"
                );
                cur = Some(idx);
                preimage.clear();
                qty = FixedPoint::ZERO;
                count = 0;
            }
            qty += order.remaining_qty;
            count += 1;
            Self::encode_order_row_into(&mut scratch, seq, order);
            preimage.extend_from_slice(&(scratch.len() as u32).to_le_bytes());
            preimage.extend_from_slice(&scratch);
        }
        if let Some(prev) = cur {
            chunks.insert(
                prev,
                ChunkAgg {
                    digest: alloy_primitives::keccak256(&preimage).0,
                    qty,
                    count,
                },
            );
        }
    }

    /// Re-hash ONE chunk of a level from its queue: binary-search the
    /// contiguous member range by seq (the queue is seq-ascending), keccak its
    /// frames, and upsert — or remove the chunk if it has no members left.
    /// O(log depth) seq lookups + O(members of the chunk).
    fn rebuild_one_chunk(
        queue: &VecDeque<Order>,
        order_seq: &HashMap<OrderId, u64>,
        chunks: &mut BTreeMap<u64, ChunkAgg>,
        idx: u64,
    ) {
        let seq_of = |o: &Order| -> u64 {
            *order_seq
                .get(&o.id)
                .expect("resting order must have a seq")
        };
        let lo_seq = idx * crate::book_rows::LEVEL_CHUNK_SEQS;
        let hi_seq = lo_seq + crate::book_rows::LEVEL_CHUNK_SEQS; // exclusive
        let start = queue.partition_point(|o| seq_of(o) < lo_seq);
        let end = queue.partition_point(|o| seq_of(o) < hi_seq);
        if start == end {
            chunks.remove(&idx);
            return;
        }
        let mut scratch: Vec<u8> = Vec::with_capacity(8 + 112);
        let mut preimage: Vec<u8> = Vec::with_capacity((end - start) * 160);
        let mut qty = FixedPoint::ZERO;
        for order in queue.range(start..end) {
            qty += order.remaining_qty;
            Self::encode_order_row_into(&mut scratch, seq_of(order), order);
            preimage.extend_from_slice(&(scratch.len() as u32).to_le_bytes());
            preimage.extend_from_slice(&scratch);
        }
        chunks.insert(
            idx,
            ChunkAgg {
                digest: alloy_primitives::keccak256(&preimage).0,
                qty,
                count: (end - start) as u32,
            },
        );
    }

    /// The mode-3 top hash + aggregate over a level's chunk state:
    /// `keccak(DOMAIN ‖ count(u32 BE) ‖ total_qty(i128 BE) ‖ Σ_asc idx(u64 BE) ‖ digest)`.
    /// `count`/`total_qty` are Σ over chunks in ascending idx (== queue) order,
    /// so the qty sum is the same front→back `+=` sequence as the flat path.
    fn chunked_top(chunks: &BTreeMap<u64, ChunkAgg>) -> crate::book_rows::LevelRowData {
        debug_assert!(!chunks.is_empty(), "chunked_top on an empty level");
        let mut total = FixedPoint::ZERO;
        let mut count: u32 = 0;
        for c in chunks.values() {
            total += c.qty;
            count += c.count;
        }
        let mut preimage: Vec<u8> = Vec::with_capacity(8 + 4 + 16 + chunks.len() * 40);
        preimage.extend_from_slice(&crate::book_rows::LEVEL_HASH_CHUNKED_DOMAIN);
        preimage.extend_from_slice(&count.to_be_bytes());
        preimage.extend_from_slice(&total.raw().to_be_bytes());
        for (idx, c) in chunks {
            preimage.extend_from_slice(&idx.to_be_bytes());
            preimage.extend_from_slice(&c.digest);
        }
        crate::book_rows::LevelRowData {
            total_qty_raw: total.raw(),
            order_count: count,
            level_hash: alloy_primitives::keccak256(&preimage).0,
        }
    }
}

// ============================================================================
// FIX 1 (ECON-FIND-02): Borsh Serialization for OrderBook
// ============================================================================

impl BorshSerialize for Order {
    fn serialize<W: IoWrite>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.id.to_be_bytes())?;
        borsh_write_address(&self.trader, w)?;
        w.write_all(&[match self.side {
            Side::Buy => 0u8,
            Side::Sell => 1u8,
        }])?;
        borsh_write_fp(&self.price, w)?;
        borsh_write_fp(&self.remaining_qty, w)?;
        borsh_write_fp(&self.original_qty, w)?;
        // OrderType discriminant + variant data
        match &self.order_type {
            OrderType::Limit => w.write_all(&[0u8])?,
            OrderType::Market => w.write_all(&[1u8])?,
            OrderType::StopMarket { trigger } => {
                w.write_all(&[2u8])?;
                borsh_write_fp(trigger, w)?;
            }
            OrderType::StopLimit { trigger, limit } => {
                w.write_all(&[3u8])?;
                borsh_write_fp(trigger, w)?;
                borsh_write_fp(limit, w)?;
            }
        }
        // TimeInForce
        w.write_all(&[match self.time_in_force {
            TimeInForce::GTC => 0u8,
            TimeInForce::IOC => 1u8,
            TimeInForce::FOK => 2u8,
            TimeInForce::PostOnly => 3u8,
        }])?;
        w.write_all(&self.timestamp.to_be_bytes())?;
        w.write_all(&[u8::from(self.reduce_only)])?;
        // Option<u64>
        match self.client_order_id {
            None => w.write_all(&[0u8])?,
            Some(cid) => {
                w.write_all(&[1u8])?;
                w.write_all(&cid.to_be_bytes())?;
            }
        }
        Ok(())
    }
}

impl BorshDeserialize for Order {
    fn deserialize_reader<R: IoRead>(r: &mut R) -> io::Result<Self> {
        let mut id_buf = [0u8; 16];
        r.read_exact(&mut id_buf)?;
        let id = u128::from_be_bytes(id_buf);

        let trader = borsh_read_address(r)?;

        let mut side_buf = [0u8; 1];
        r.read_exact(&mut side_buf)?;
        let side = if side_buf[0] == 0 {
            Side::Buy
        } else {
            Side::Sell
        };

        let price = borsh_read_fp(r)?;
        let remaining_qty = borsh_read_fp(r)?;
        let original_qty = borsh_read_fp(r)?;

        let mut ot_buf = [0u8; 1];
        r.read_exact(&mut ot_buf)?;
        let order_type = match ot_buf[0] {
            0 => OrderType::Limit,
            1 => OrderType::Market,
            2 => {
                let trigger = borsh_read_fp(r)?;
                OrderType::StopMarket { trigger }
            }
            3 => {
                let trigger = borsh_read_fp(r)?;
                let limit = borsh_read_fp(r)?;
                OrderType::StopLimit { trigger, limit }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid OrderType discriminant",
                ))
            }
        };

        let mut tif_buf = [0u8; 1];
        r.read_exact(&mut tif_buf)?;
        let time_in_force = match tif_buf[0] {
            0 => TimeInForce::GTC,
            1 => TimeInForce::IOC,
            2 => TimeInForce::FOK,
            3 => TimeInForce::PostOnly,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid TimeInForce discriminant",
                ))
            }
        };

        let mut ts_buf = [0u8; 8];
        r.read_exact(&mut ts_buf)?;
        let timestamp = u64::from_be_bytes(ts_buf);

        let mut ro_buf = [0u8; 1];
        r.read_exact(&mut ro_buf)?;
        let reduce_only = ro_buf[0] != 0;

        let mut cid_tag = [0u8; 1];
        r.read_exact(&mut cid_tag)?;
        let client_order_id = if cid_tag[0] == 0 {
            None
        } else {
            let mut cid_buf = [0u8; 8];
            r.read_exact(&mut cid_buf)?;
            Some(u64::from_be_bytes(cid_buf))
        };

        Ok(Order {
            id,
            trader,
            side,
            price,
            remaining_qty,
            original_qty,
            order_type,
            time_in_force,
            timestamp,
            reduce_only,
            client_order_id,
        })
    }
}

impl BorshSerialize for StopOrder {
    fn serialize<W: IoWrite>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.id.to_be_bytes())?;
        borsh_write_address(&self.trader, w)?;
        w.write_all(&self.market_id.to_be_bytes())?;
        w.write_all(&[match self.side {
            Side::Buy => 0u8,
            Side::Sell => 1u8,
        }])?;
        borsh_write_fp(&self.trigger_price, w)?;
        // Tag 0 = market (no cap: legacy), 1 = limit, 2 = market + s515
        // price cap. A cap-less stop keeps the exact pre-s515 bytes.
        match &self.limit_price {
            None if self.price_cap > FixedPoint::ZERO => {
                w.write_all(&[2u8])?;
                borsh_write_fp(&self.price_cap, w)?;
            }
            None => w.write_all(&[0u8])?,
            Some(lp) => {
                w.write_all(&[1u8])?;
                borsh_write_fp(lp, w)?;
            }
        }
        borsh_write_fp(&self.quantity, w)?;
        w.write_all(&[match self.time_in_force {
            TimeInForce::GTC => 0u8,
            TimeInForce::IOC => 1u8,
            TimeInForce::FOK => 2u8,
            TimeInForce::PostOnly => 3u8,
        }])?;
        w.write_all(&self.timestamp.to_be_bytes())?;
        w.write_all(&[u8::from(self.reduce_only)])?;
        match self.client_order_id {
            None => w.write_all(&[0u8])?,
            Some(cid) => {
                w.write_all(&[1u8])?;
                w.write_all(&cid.to_be_bytes())?;
            }
        }
        Ok(())
    }
}

impl BorshDeserialize for StopOrder {
    fn deserialize_reader<R: IoRead>(r: &mut R) -> io::Result<Self> {
        let mut id_buf = [0u8; 16];
        r.read_exact(&mut id_buf)?;
        let id = u128::from_be_bytes(id_buf);

        let trader = borsh_read_address(r)?;

        let mut mid_buf = [0u8; 8];
        r.read_exact(&mut mid_buf)?;
        let market_id = u64::from_be_bytes(mid_buf);

        let mut side_buf = [0u8; 1];
        r.read_exact(&mut side_buf)?;
        let side = if side_buf[0] == 0 {
            Side::Buy
        } else {
            Side::Sell
        };

        let trigger_price = borsh_read_fp(r)?;

        let mut lp_tag = [0u8; 1];
        r.read_exact(&mut lp_tag)?;
        let (limit_price, price_cap) = match lp_tag[0] {
            0 => (None, FixedPoint::ZERO),
            1 => (Some(borsh_read_fp(r)?), FixedPoint::ZERO),
            2 => (None, borsh_read_fp(r)?),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid stop price tag",
                ))
            }
        };

        let quantity = borsh_read_fp(r)?;

        let mut tif_buf = [0u8; 1];
        r.read_exact(&mut tif_buf)?;
        let time_in_force = match tif_buf[0] {
            0 => TimeInForce::GTC,
            1 => TimeInForce::IOC,
            2 => TimeInForce::FOK,
            3 => TimeInForce::PostOnly,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid TimeInForce discriminant",
                ))
            }
        };

        let mut ts_buf = [0u8; 8];
        r.read_exact(&mut ts_buf)?;
        let timestamp = u64::from_be_bytes(ts_buf);

        let mut ro_buf = [0u8; 1];
        r.read_exact(&mut ro_buf)?;
        let reduce_only = ro_buf[0] != 0;

        let mut cid_tag = [0u8; 1];
        r.read_exact(&mut cid_tag)?;
        let client_order_id = if cid_tag[0] == 0 {
            None
        } else {
            let mut cid_buf = [0u8; 8];
            r.read_exact(&mut cid_buf)?;
            Some(u64::from_be_bytes(cid_buf))
        };

        Ok(StopOrder {
            id,
            trader,
            market_id,
            side,
            trigger_price,
            limit_price,
            price_cap,
            quantity,
            time_in_force,
            timestamp,
            reduce_only,
            client_order_id,
        })
    }
}

impl BorshSerialize for OrderBook {
    fn serialize<W: IoWrite>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.market_id.to_be_bytes())?;
        borsh_write_fp(&self.tick_size, w)?;
        borsh_write_fp(&self.lot_size, w)?;
        w.write_all(&self.next_id.to_be_bytes())?;

        // last_trade_price: Option<FixedPoint>
        match &self.last_trade_price {
            None => w.write_all(&[0u8])?,
            Some(ltp) => {
                w.write_all(&[1u8])?;
                borsh_write_fp(ltp, w)?;
            }
        }

        // Collect all resting orders from bids and asks
        let order_count: u32 = (self.bids.values().map(|q| q.len()).sum::<usize>()
            + self.asks.values().map(|q| q.len()).sum::<usize>())
            as u32;
        w.write_all(&order_count.to_be_bytes())?;

        // Write bids (price ascending from BTreeMap iteration, but order within level is FIFO)
        for queue in self.bids.values() {
            for order in queue {
                order.serialize(w)?;
            }
        }
        // Write asks
        for queue in self.asks.values() {
            for order in queue {
                order.serialize(w)?;
            }
        }

        // Write pending stops
        let stop_count = self.pending_stops.len() as u32;
        w.write_all(&stop_count.to_be_bytes())?;
        for stop in &self.pending_stops {
            stop.serialize(w)?;
        }

        Ok(())
    }
}

impl BorshDeserialize for OrderBook {
    fn deserialize_reader<R: IoRead>(r: &mut R) -> io::Result<Self> {
        let mut mid_buf = [0u8; 8];
        r.read_exact(&mut mid_buf)?;
        let market_id = u64::from_be_bytes(mid_buf);

        let tick_size = borsh_read_fp(r)?;
        let lot_size = borsh_read_fp(r)?;

        let mut nid_buf = [0u8; 16];
        r.read_exact(&mut nid_buf)?;
        let next_id = u128::from_be_bytes(nid_buf);

        let mut ltp_tag = [0u8; 1];
        r.read_exact(&mut ltp_tag)?;
        let last_trade_price = if ltp_tag[0] == 0 {
            None
        } else {
            Some(borsh_read_fp(r)?)
        };

        // Read orders and rebuild the book
        let mut oc_buf = [0u8; 4];
        r.read_exact(&mut oc_buf)?;
        let order_count = u32::from_be_bytes(oc_buf) as usize;

        let mut book = OrderBook {
            market_id,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            order_index: HashMap::new(),
            trader_orders: HashMap::new(),
            pending_stops: Vec::new(),
            tick_size,
            lot_size,
            next_id,
            last_trade_price,
            reduce_only_index: BTreeSet::new(),
            reduce_only_positions: ReduceOnlyPositions::default(),
            account_margins: AccountMargins::default(),
            order_seq: HashMap::new(),
            next_seq: 1,
            row_journal: BTreeSet::new(),
            row_exists: HashSet::new(),
            level_journal: BTreeSet::new(),
            level_exists: HashSet::new(),
            level_epoch: HashMap::new(),
            level_hash_cache: None,
            level_hash_chunked: false,
            level_chunks: HashMap::new(),
            dirty_chunks: BTreeSet::new(),
        };

        for _ in 0..order_count {
            let order = Order::deserialize_reader(r)?;
            book.insert_order(order);
        }

        // Read pending stops
        let mut sc_buf = [0u8; 4];
        r.read_exact(&mut sc_buf)?;
        let stop_count = u32::from_be_bytes(sc_buf) as usize;

        for _ in 0..stop_count {
            let stop = StopOrder::deserialize_reader(r)?;
            book.pending_stops.push(stop);
        }

        // Deserializing is a LOAD, not a mutation: the insert_order calls
        // above journaled every order/level — clear that (classic mode never
        // drains the journals; leaving them populated only leaks memory).
        book.row_journal.clear();
        book.level_journal.clear();

        Ok(book)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Helpers
    fn fp(n: i64) -> FixedPoint {
        FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
    }

    fn fp_frac(whole: i64, frac_8: i64) -> FixedPoint {
        FixedPoint::from_raw(whole as i128 * FixedPoint::SCALE + frac_8 as i128)
    }

    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    fn book() -> OrderBook {
        OrderBook::new(1, fp_frac(0, 1), fp_frac(0, 1))
    }

    fn limit_buy(price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    fn limit_sell(price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy: false,
            price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    /// s515: a market order's price is its slippage cap — these sweep
    /// helpers use a cap no test book reaches.
    fn market_buy(qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: fp(1_000_000_000),
            quantity: qty,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    fn market_sell(qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy: false,
            price: FixedPoint::from_raw(1),
            quantity: qty,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    // ====================================================================
    // L3 level-hash cache — crypto-equivalence witnesses
    // ====================================================================

    /// The sponge-cache correctness axioms, checked as executable facts:
    /// (1) streaming sha3::Keccak256 over arbitrary split points equals the
    ///     one-shot alloy_primitives::keccak256 (same function, cross-impl);
    /// (2) a CLONED mid-absorb state, extended and finalized, equals the
    ///     one-shot digest of the full stream (clone+extend ≡ absorb-all),
    ///     and finalizing the clone does not perturb the original state.
    #[test]
    fn keccak_stream_clone_equivalence() {
        use sha3::Digest;
        let mut s = 0x1234_5678_9ABC_DEF0u64;
        let mut xs = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for len in [0usize, 1, 7, 135, 136, 137, 200, 1000, 5000] {
            let data: Vec<u8> = (0..len).map(|_| (xs() & 0xFF) as u8).collect();
            let oneshot = alloy_primitives::keccak256(&data).0;
            // Random split points, including 0 and len.
            for _ in 0..8 {
                let cut = (xs() as usize) % (len + 1);
                let mut h = sha3::Keccak256::new();
                h.update(&data[..cut]);
                // Snapshot mid-absorb (the cached state), then extend both
                // the original and the snapshot with the tail.
                let mut snap = h.clone();
                h.update(&data[cut..]);
                snap.update(&data[cut..]);
                // Finalizing a CLONE of the extended state must not perturb
                // further use of the state itself.
                let d1: [u8; 32] = h.clone().finalize().into();
                let d2: [u8; 32] = snap.finalize().into();
                assert_eq!(d1, oneshot, "streamed != one-shot (len={len} cut={cut})");
                assert_eq!(d2, oneshot, "cloned-state != one-shot (len={len} cut={cut})");
                // The original is still usable and still agrees.
                let d3: [u8; 32] = h.finalize().into();
                assert_eq!(d3, oneshot, "post-clone original diverged");
            }
        }
    }

    /// Staged-seeding smoke: exercises all three arms of the cached path
    /// (miss→probe, probe→promote/seed, seeded→extend hit, stale→miss) and
    /// asserts byte-identity with the frozen one-shot recompute at each stage
    /// (the exhaustive differential lives in tests/level_rows_core_tests.rs and
    /// torus-bridge).
    #[test]
    fn level_hash_cache_smoke_staged_seeding() {
        let mut ob = book();
        ob.ensure_level_hash_cache(1 << 20);
        // Seed 5 resting orders and save: no slot ⇒ MISS (plain one-shot path
        // + a cheap probe; no sponge is built on a level's first save).
        for i in 0..5 {
            ob.place_order(limit_buy(fp(100), fp(1 + i)), addr(1), i as u64);
        }
        let ops1 = ob.take_level_ops();
        assert_eq!(ops1.len(), 1);
        let (h, m, s, _) = ob.level_hash_cache_stats().unwrap();
        assert_eq!((h, m, s), (0, 1, 0), "first save of a level is a plain miss");

        // Append-only save #2: the probe's epoch is unchanged ⇒ the interval
        // was append-only ⇒ PROMOTE (invest one full absorb, seed the sponge).
        ob.place_order(limit_buy(fp(100), fp(9)), addr(2), 10);
        let ops2 = ob.take_level_ops();
        let (h, m, s, _) = ob.level_hash_cache_stats().unwrap();
        assert_eq!((h, m, s), (0, 1, 1), "append-only interval promotes probe→sponge");
        let plain = ob.level_row_data(crate::book_rows::SIDE_TAG_BID, fp(100).raw());
        assert_eq!(ops2[0].1, plain, "promoted digest != one-shot");

        // Append-only save #3: now a Seeded entry with unchanged epoch ⇒ HIT
        // (O(tail) sponge extension), still byte-identical.
        ob.place_order(limit_buy(fp(100), fp(7)), addr(2), 11);
        let ops3 = ob.take_level_ops();
        let (h, m, s, _) = ob.level_hash_cache_stats().unwrap();
        assert_eq!((h, m, s), (1, 1, 1), "second append-only interval extends the sponge");
        let plain = ob.level_row_data(crate::book_rows::SIDE_TAG_BID, fp(100).raw());
        assert_eq!(ops3[0].1, plain, "cached extend digest != one-shot");

        // Invalidate (cancel mid-queue) ⇒ epoch bump ⇒ stale seed ⇒ MISS
        // (plain one-shot + fresh probe), byte-identical.
        let victim = ob
            .level_queue(Side::Buy, fp(100))
            .unwrap()
            .get(2)
            .unwrap()
            .id;
        ob.cancel_order(victim).unwrap();
        let ops4 = ob.take_level_ops();
        let plain = ob.level_row_data(crate::book_rows::SIDE_TAG_BID, fp(100).raw());
        assert_eq!(ops4[0].1, plain, "post-invalidation digest != one-shot");
        let (h, m, s, _) = ob.level_hash_cache_stats().unwrap();
        assert_eq!((h, m, s), (1, 2, 1), "invalidation forces the plain miss path");
    }

    // ====================================================================
    // 2.1.1 — Basic price-time priority matching
    // ====================================================================

    #[test]
    fn basic_buy_sell_match() {
        let mut ob = book();
        // Sell 10 @ 100
        let r1 = ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        assert_eq!(r1.status, OrderStatus::Resting);
        assert_eq!(ob.order_count(), 1);

        // Buy 10 @ 100 → full match
        let r2 = ob.place_order(limit_buy(fp(100), fp(10)), addr(2), 2);
        assert_eq!(r2.status, OrderStatus::Filled);
        assert_eq!(r2.fills.len(), 1);
        assert_eq!(r2.fills[0].price, fp(100));
        assert_eq!(r2.fills[0].quantity, fp(10));
        assert_eq!(r2.fills[0].maker, addr(1));
        assert_eq!(r2.fills[0].taker, addr(2));
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn no_match_resting_order() {
        let mut ob = book();
        // Buy 10 @ 95, Sell 10 @ 100 → no match, both rest
        ob.place_order(limit_buy(fp(95), fp(10)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(10)), addr(2), 2);
        assert_eq!(ob.order_count(), 2);
        assert_eq!(ob.best_bid(), Some(fp(95)));
        assert_eq!(ob.best_ask(), Some(fp(100)));
        assert_eq!(ob.spread(), Some(fp(5)));
    }

    #[test]
    fn partial_fill() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        // Buy 6 @ 100 → partial fill, maker has 4 remaining
        let r = ob.place_order(limit_buy(fp(100), fp(6)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills[0].quantity, fp(6));
        assert_eq!(ob.order_count(), 1);

        let maker = ob.get_order(1).unwrap();
        assert_eq!(maker.remaining_qty, fp(4));
    }

    #[test]
    fn multiple_fills_at_different_levels() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(5)), addr(2), 2);

        // Buy 10 @ 101 → fills both levels
        let r = ob.place_order(limit_buy(fp(101), fp(10)), addr(3), 3);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 2);
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(r.fills[0].quantity, fp(5));
        assert_eq!(r.fills[1].price, fp(101));
        assert_eq!(r.fills[1].quantity, fp(5));
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn buy_partial_rest_on_book() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);

        // Buy 10 @ 100 → fill 5, rest 5 on book
        let r = ob.place_order(limit_buy(fp(100), fp(10)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::PartiallyFilled);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].quantity, fp(5));
        assert_eq!(ob.order_count(), 1);
        assert_eq!(ob.best_bid(), Some(fp(100)));
    }

    // ====================================================================
    // Price priority
    // ====================================================================

    #[test]
    fn price_priority_asks() {
        let mut ob = book();
        // Place asks at 102, 100, 101 (out of order)
        ob.place_order(limit_sell(fp(102), fp(5)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(5)), addr(2), 2);
        ob.place_order(limit_sell(fp(101), fp(5)), addr(3), 3);

        // Buy 5 → should fill at 100 (lowest ask first)
        let r = ob.place_order(limit_buy(fp(102), fp(5)), addr(4), 4);
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(r.fills[0].maker, addr(2));
    }

    #[test]
    fn price_priority_bids() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(98), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(100), fp(5)), addr(2), 2);
        ob.place_order(limit_buy(fp(99), fp(5)), addr(3), 3);

        // Sell 5 → should fill at 100 (highest bid first)
        let r = ob.place_order(limit_sell(fp(98), fp(5)), addr(4), 4);
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(r.fills[0].maker, addr(2));
    }

    // ====================================================================
    // Time priority
    // ====================================================================

    #[test]
    fn time_priority_same_price() {
        let mut ob = book();
        // Two sells at same price, different times
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(5)), addr(2), 2);

        // Buy 5 → should fill against addr(1) (earlier timestamp)
        let r = ob.place_order(limit_buy(fp(100), fp(5)), addr(3), 3);
        assert_eq!(r.fills[0].maker, addr(1));
        // addr(2) still resting
        assert_eq!(ob.order_count(), 1);
        assert_eq!(ob.get_order(2).unwrap().trader, addr(2));
    }

    #[test]
    fn time_priority_fifo_across_fills() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(3)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(3)), addr(2), 2);
        ob.place_order(limit_sell(fp(100), fp(3)), addr(3), 3);

        // Buy 7 → fills addr(1) fully (3), addr(2) fully (3), addr(3) partial (1)
        let r = ob.place_order(limit_buy(fp(100), fp(7)), addr(4), 4);
        assert_eq!(r.fills.len(), 3);
        assert_eq!(r.fills[0].maker, addr(1));
        assert_eq!(r.fills[0].quantity, fp(3));
        assert_eq!(r.fills[1].maker, addr(2));
        assert_eq!(r.fills[1].quantity, fp(3));
        assert_eq!(r.fills[2].maker, addr(3));
        assert_eq!(r.fills[2].quantity, fp(1));
        assert_eq!(ob.get_order(3).unwrap().remaining_qty, fp(2));
    }

    // ====================================================================
    // 2.1.2 — Order types
    // ====================================================================

    #[test]
    fn market_buy_sweeps_book() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(3)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(3)), addr(2), 2);
        ob.place_order(limit_sell(fp(102), fp(3)), addr(3), 3);

        // Market buy 7 → sweeps first two levels fully, partial third
        let r = ob.place_order(market_buy(fp(7)), addr(4), 4);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 3);
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(r.fills[1].price, fp(101));
        assert_eq!(r.fills[2].price, fp(102));
        assert_eq!(r.fills[2].quantity, fp(1));
    }

    #[test]
    fn market_sell_sweeps_bids() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(99), fp(5)), addr(2), 2);

        let r = ob.place_order(market_sell(fp(8)), addr(3), 3);
        assert_eq!(r.fills.len(), 2);
        assert_eq!(r.fills[0].price, fp(100)); // highest bid first
        assert_eq!(r.fills[0].quantity, fp(5));
        assert_eq!(r.fills[1].price, fp(99));
        assert_eq!(r.fills[1].quantity, fp(3));
    }

    #[test]
    fn market_order_remainder_cancelled() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(3)), addr(1), 1);

        // Market buy 10, only 3 available → fill 3, cancel 7
        let r = ob.place_order(market_buy(fp(10)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Cancelled);
        assert_eq!(r.fills[0].quantity, fp(3));
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn market_order_empty_book_rejected() {
        let mut ob = book();
        let r = ob.place_order(market_buy(fp(10)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert!(r.fills.is_empty());
    }

    #[test]
    fn post_only_rests_when_no_cross() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);

        // PostOnly buy @ 99 → no cross, rests
        let params = PlaceOrderParams {
            time_in_force: TimeInForce::PostOnly,
            ..limit_buy(fp(99), fp(5))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Resting);
        assert_eq!(ob.order_count(), 2);
    }

    #[test]
    fn post_only_rejected_when_crosses() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);

        // PostOnly buy @ 100 → would cross, rejected
        let params = PlaceOrderParams {
            time_in_force: TimeInForce::PostOnly,
            ..limit_buy(fp(100), fp(5))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert!(r.fills.is_empty());
        assert_eq!(ob.order_count(), 1); // only the original sell
    }

    #[test]
    fn post_only_sell_rejected_when_crosses() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::PostOnly,
            ..limit_sell(fp(100), fp(5))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    #[test]
    fn stop_market_pending_then_triggered() {
        let mut ob = book();
        // Place a sell at 105 as liquidity
        ob.place_order(limit_sell(fp(105), fp(10)), addr(1), 1);
        // Place a buy at 95 as liquidity
        ob.place_order(limit_buy(fp(95), fp(10)), addr(2), 2);

        // Stop market buy: trigger at 100, slippage cap 110
        let params = PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: fp(110),
            quantity: fp(5),
            order_type: OrderType::StopMarket { trigger: fp(100) },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let r = ob.place_order(params, addr(3), 3);
        assert_eq!(r.status, OrderStatus::PendingTrigger);
        assert_eq!(ob.pending_stop_count(), 1);

        // Trade at 100 to trigger the stop: sell at 95 matches the buy at 95
        // Actually we need a trade that sets last_trade_price >= 100
        // Let's do a buy that matches the sell at 105
        // This trade at 105 triggers the stop buy (trigger=100, 105>=100).
        // s515: the fired stop is handed back (as a market buy capped at
        // 110, keeping its id) for the executor to place — not run inline.
        let r = ob.place_order(limit_buy(fp(105), fp(1)), addr(4), 4);
        assert_eq!(ob.pending_stop_count(), 0);
        assert_eq!(r.triggered_stops.len(), 1);
        let t = &r.triggered_stops[0];
        assert_eq!(t.trader, addr(3));
        assert_eq!(t.params.order_type, OrderType::Market);
        assert_eq!(t.params.price, fp(110));
        assert_eq!(t.params.quantity, fp(5));
        // Nothing beyond the trigger trade consumed the ask.
        assert_eq!(ob.ask_depth(), vec![(fp(105), fp(9), 1)]);
    }

    #[test]
    fn stop_market_without_price_cap_rejected() {
        let mut ob = book();
        let params = PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::ZERO,
            quantity: fp(5),
            order_type: OrderType::StopMarket { trigger: fp(100) },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        assert_eq!(ob.place_order(params, addr(3), 3).status, OrderStatus::Rejected);
        assert_eq!(ob.pending_stop_count(), 0);
    }

    #[test]
    fn market_order_respects_price_cap() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(3)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(3)), addr(2), 2);
        ob.place_order(limit_sell(fp(103), fp(3)), addr(3), 3);
        let mut p = market_buy(fp(9));
        p.price = fp(101);
        let r = ob.place_order(p, addr(4), 4);
        assert_eq!(r.status, OrderStatus::Cancelled);
        let filled: FixedPoint = r.fills.iter().map(|f| f.quantity).fold(FixedPoint::ZERO, |a, b| a + b);
        assert_eq!(filled, fp(6));
        assert!(r.fills.iter().all(|f| f.price <= fp(101)));
        assert_eq!(r.rested_qty, FixedPoint::ZERO);
        assert_eq!(ob.ask_depth(), vec![(fp(103), fp(3), 1)]);

        // Zero cap: rejected outright.
        let mut p = market_buy(fp(1));
        p.price = FixedPoint::ZERO;
        assert_eq!(ob.place_order(p, addr(4), 5).status, OrderStatus::Rejected);
    }

    #[test]
    fn stop_limit_pending_then_triggered() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(105), fp(10)), addr(1), 1);

        // Stop limit sell: trigger at 95, limit at 90
        let params = PlaceOrderParams {
            market_id: 1,
            is_buy: false,
            price: FixedPoint::ZERO,
            quantity: fp(5),
            order_type: OrderType::StopLimit {
                trigger: fp(95),
                limit: fp(90),
            },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::PendingTrigger);
        assert_eq!(ob.pending_stop_count(), 1);
    }

    // ====================================================================
    // 2.1.3 — Time-in-force
    // ====================================================================

    #[test]
    fn gtc_rests_on_book() {
        let mut ob = book();
        let r = ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Resting);
        assert_eq!(ob.order_count(), 1);
    }

    #[test]
    fn ioc_partial_fill_remainder_cancelled() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(3)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::IOC,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Cancelled);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].quantity, fp(3));
        // Remainder (7) cancelled, not on book
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn ioc_no_match_cancelled() {
        let mut ob = book();
        let params = PlaceOrderParams {
            time_in_force: TimeInForce::IOC,
            ..limit_buy(fp(95), fp(10))
        };
        let r = ob.place_order(params, addr(1), 1);
        assert_eq!(r.status, OrderStatus::Cancelled);
        assert!(r.fills.is_empty());
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn fok_full_fill() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].quantity, fp(10));
    }

    #[test]
    fn fok_rejected_insufficient_qty() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert!(r.fills.is_empty());
        // Maker order untouched
        assert_eq!(ob.order_count(), 1);
        assert_eq!(ob.get_order(1).unwrap().remaining_qty, fp(5));
    }

    #[test]
    fn fok_rejected_no_orders() {
        let mut ob = book();
        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(1), 1);
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    #[test]
    fn fok_multi_level_fill() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(5)), addr(2), 2);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(101), fp(10))
        };
        let r = ob.place_order(params, addr(3), 3);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 2);
    }

    // ====================================================================
    // 2.1.4 — Cancel and modify
    // ====================================================================

    #[test]
    fn cancel_existing_order() {
        let mut ob = book();
        let r = ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);
        let order_id = r.order_id;

        let cancelled = ob.cancel_order(order_id).unwrap();
        assert_eq!(cancelled.id, order_id);
        assert_eq!(cancelled.remaining_qty, fp(10));
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn cancel_nonexistent_order() {
        let mut ob = book();
        let result = ob.cancel_order(999);
        assert!(result.is_err());
    }

    #[test]
    fn cancel_all_for_trader() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(99), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 2);
        ob.place_order(limit_sell(fp(105), fp(5)), addr(1), 3);
        ob.place_order(limit_sell(fp(110), fp(5)), addr(2), 4);

        let cancelled = ob.cancel_all(addr(1), None);
        assert_eq!(cancelled.len(), 3);
        assert_eq!(ob.order_count(), 1); // only addr(2)'s order remains
    }

    #[test]
    fn cancel_all_empty() {
        let mut ob = book();
        let cancelled = ob.cancel_all(addr(1), None);
        assert!(cancelled.is_empty());
    }

    fn stop(is_buy: bool, order_type: OrderType) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy,
            // s515: a stop-market needs a positive price cap (worst price).
            price: if is_buy { fp(1_000) } else { fp(1) },
            quantity: fp(1),
            order_type,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    #[test]
    fn open_order_count_counts_resting_and_pending_stops() {
        let mut ob = book();
        let a = addr(1);
        ob.place_order(limit_sell(fp(105), fp(1)), addr(2), 1);
        let first = ob.place_order(limit_buy(fp(90), fp(1)), a, 2).order_id;
        ob.place_order(limit_buy(fp(91), fp(1)), a, 3);
        ob.place_order(limit_buy(fp(92), fp(1)), a, 4);
        ob.place_order(stop(false, OrderType::StopMarket { trigger: fp(80) }), a, 5);
        let stop_limit = OrderType::StopLimit {
            trigger: fp(100),
            limit: fp(101),
        };
        ob.place_order(stop(true, stop_limit), a, 6);
        let ioc = PlaceOrderParams {
            time_in_force: TimeInForce::IOC,
            ..limit_buy(fp(50), fp(1))
        };
        assert_eq!(ob.place_order(ioc, a, 7).status, OrderStatus::Cancelled);
        assert_eq!(ob.open_order_count(&a), 5);
        assert_eq!(ob.open_order_count(&addr(2)), 1);
        assert_eq!(ob.open_order_count(&addr(9)), 0);

        ob.cancel_order(first).unwrap();
        assert_eq!(ob.open_order_count(&a), 4);

        // A trade at 105 triggers the buy stop. s515: the book hands the
        // fired stop back (the executor places it) instead of resting it
        // inline, so it leaves the pending set and holds no slot here.
        let r = ob.place_order(limit_buy(fp(105), fp(1)), addr(3), 8);
        assert_eq!(r.triggered_stops.len(), 1);
        assert_eq!(r.triggered_stops[0].trader, a);
        assert_eq!(ob.pending_stop_count(), 1);
        assert_eq!(ob.orders_for_trader(&a).len(), 2);
        assert_eq!(ob.open_order_count(&a), 3);
    }

    /// `cancel_all` returns a trader's orders in its `trader_orders` order:
    /// arrival order for a live book, decode (level) order after a reload —
    /// and mid-list removals (cancel, maker fill, STP) never reorder it.
    #[test]
    fn cancel_all_output_order_survives_mid_list_removals() {
        let a = addr(1);
        let scenario = |ob: &mut OrderBook, ids: &[OrderId]| {
            for (k, id) in ids.iter().enumerate() {
                if k % 3 == 0 {
                    ob.cancel_order(*id).unwrap();
                }
            }
            // Maker fills on a's best asks, then an STP walk of level 200 by
            // a's own bid (which then rests: a new arrival at the end).
            ob.place_order(market_buy(fp(4)), addr(2), 0);
            ob.place_order(limit_buy(fp(201), fp(1)), a, 0);
            for k in 0..5 {
                ob.place_order(limit_sell(fp(300 + k), fp(1)), a, 0);
            }
        };

        // Live book: arrival order = ascending id.
        let mut ob = book();
        let ids: Vec<OrderId> = (0..100)
            .map(|k| ob.place_order(limit_sell(fp(200 + k % 7), fp(1)), a, 0).order_id)
            .collect();
        scenario(&mut ob, &ids);
        let mut live: Vec<OrderId> = ob.orders_for_trader(&a).iter().map(|o| o.id).collect();
        live.sort_unstable();
        assert!(live.len() > 50);
        let got: Vec<OrderId> = ob.cancel_all(a, None).iter().map(|o| o.id).collect();
        assert_eq!(got, live);

        // Reloaded book: decode order (bids, then asks by level), not ids.
        let mut ob = book();
        let ids: Vec<OrderId> = (0..100)
            .map(|k| ob.place_order(limit_sell(fp(200 + k % 7), fp(1)), a, 0).order_id)
            .collect();
        let mut ob = OrderBook::try_from_slice(&borsh::to_vec(&ob).unwrap()).unwrap();
        let decode_order: Vec<OrderId> = ob
            .bid_queues()
            .chain(ob.ask_queues())
            .flat_map(|(_, q)| q.iter().map(|o| o.id))
            .collect();
        assert_ne!(decode_order, ids, "level order differs from arrival order");
        scenario(&mut ob, &ids);
        let want: Vec<OrderId> = decode_order
            .iter()
            .copied()
            .filter(|id| ob.get_order(*id).is_some())
            .chain(
                (ids[99] + 1..ob.next_order_id())
                    .filter(|id| ob.get_order(*id).is_some_and(|o| o.trader == a)),
            )
            .collect();
        let got: Vec<OrderId> = ob.cancel_all(a, None).iter().map(|o| o.id).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn add_open_order_counts_matches_open_order_count() {
        let mut ob = book();
        for (t, n) in [(1u8, 3usize), (2, 1), (3, 0)] {
            for k in 0..n {
                ob.place_order(limit_buy(fp(90 + k as i64), fp(1)), addr(t), 0);
            }
        }
        for t in [1u8, 3, 3, 4] {
            ob.place_order(stop(true, OrderType::StopMarket { trigger: fp(200) }), addr(t), 0);
        }
        let traders: Vec<Address> = (1..=5).map(addr).collect();
        let idx: HashMap<Address, usize> =
            traders.iter().enumerate().map(|(i, t)| (*t, i)).collect();
        let mut counts = vec![10; traders.len()];
        ob.add_open_order_counts(&idx, &mut counts);
        let want: Vec<usize> = traders.iter().map(|t| 10 + ob.open_order_count(t)).collect();
        assert_eq!(counts, want);
        assert_eq!(counts, [14, 11, 12, 11, 10]);
        // Fewer senders than traders in the book (the other walk).
        let one: HashMap<Address, usize> = [(addr(1), 0)].into();
        let mut counts = vec![0];
        ob.add_open_order_counts(&one, &mut counts);
        assert_eq!(counts, [4]);
        // An empty book adds nothing.
        book().add_open_order_counts(&idx, &mut counts);
        assert_eq!(counts, [4]);
    }

    #[test]
    fn open_order_counts_match_per_sender_sums_for_any_thread_count() {
        let mut books = Vec::new();
        for m in 0..23u8 {
            let mut ob = book();
            for t in 0..(m % 9) {
                for k in 0..=(t + m) % 4 {
                    ob.place_order(limit_buy(fp(50 + k as i64), fp(1)), addr(1 + t), 0);
                }
            }
            for t in 0..m % 3 {
                let trigger = OrderType::StopMarket { trigger: fp(200) };
                ob.place_order(stop(true, trigger), addr(3 + t), 0);
            }
            books.push(ob);
        }
        let refs: Vec<&OrderBook> = books.iter().collect();
        let senders: HashMap<Address, usize> = (1..=12).map(|n| (addr(n), n as usize - 1)).collect();
        let want: Vec<usize> = (1..=12)
            .map(|n| books.iter().map(|b| b.open_order_count(&addr(n))).sum())
            .collect();
        assert!(want.iter().sum::<usize>() > 100);
        for threads in [0, 1, 2, 3, 8, 64] {
            assert_eq!(open_order_counts(&refs, &senders, threads, 0), want, "threads={threads}");
            assert_eq!(open_order_counts(&refs, &senders, threads, 1 << 20), want);
        }
    }

    #[test]
    fn cancel_all_removes_stops_of_trader_without_resting_orders() {
        let mut ob = book();
        let a = addr(1);
        ob.place_order(stop(false, OrderType::StopMarket { trigger: fp(80) }), a, 1);
        ob.place_order(stop(true, OrderType::StopMarket { trigger: fp(120) }), a, 2);
        ob.place_order(stop(true, OrderType::StopMarket { trigger: fp(130) }), addr(2), 3);
        assert_eq!(ob.open_order_count(&a), 2);
        assert!(ob.cancel_all(a, None).is_empty());
        assert_eq!(ob.open_order_count(&a), 0);
        assert_eq!(ob.open_order_count(&addr(2)), 1);
    }

    #[test]
    fn modify_qty_decrease_keeps_priority() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(10)), addr(2), 2);

        // Decrease addr(1)'s qty → should keep time priority (still first)
        ob.modify_order(1, None, Some(fp(5))).unwrap();

        // Buy matches addr(1) first (still has priority)
        let r = ob.place_order(limit_buy(fp(100), fp(5)), addr(3), 3);
        assert_eq!(r.fills[0].maker, addr(1));
        assert_eq!(r.fills[0].quantity, fp(5));
    }

    #[test]
    fn modify_price_loses_priority() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(10)), addr(2), 2);

        // Change addr(1)'s price → loses priority, goes to back
        ob.modify_order(1, Some(fp(100)), None).unwrap();

        // Buy matches addr(2) first (addr(1) lost priority)
        let r = ob.place_order(limit_buy(fp(100), fp(5)), addr(3), 3);
        assert_eq!(r.fills[0].maker, addr(2));
    }

    #[test]
    fn modify_nonexistent_order() {
        let mut ob = book();
        let result = ob.modify_order(999, Some(fp(100)), None);
        assert!(result.is_err());
    }

    #[test]
    fn modify_price_and_qty() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let modified = ob.modify_order(1, Some(fp(99)), Some(fp(8))).unwrap();
        assert_eq!(modified.price, fp(99));
        assert_eq!(modified.remaining_qty, fp(8));
        assert_eq!(ob.best_ask(), Some(fp(99)));
    }

    // ====================================================================
    // 2.1.5 — Comprehensive tests
    // ====================================================================

    // Self-trade prevention
    #[test]
    fn self_trade_prevention_cancel_resting() {
        let mut ob = book();
        // Trader 1 places a sell
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        // Same trader places a buy → self-trade, maker cancelled, buyer rests
        let r = ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 2);
        assert_eq!(r.self_trade_cancels.len(), 1);
        assert!(r.fills.is_empty());
        assert_eq!(r.status, OrderStatus::Resting);
        assert_eq!(ob.order_count(), 1); // maker cancelled, buy rests (GTC)
        assert_eq!(ob.best_bid(), Some(fp(100)));
    }

    #[test]
    fn self_trade_skips_to_next_maker() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1); // same as buyer
        ob.place_order(limit_sell(fp(100), fp(5)), addr(2), 2); // different

        // Trader 1 buys → skips own sell, fills against addr(2)
        let r = ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 3);
        let cancelled_ids: Vec<_> = r.self_trade_cancels.iter().map(|o| o.id).collect();
        assert_eq!(cancelled_ids, vec![1]);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].maker, addr(2));
        assert_eq!(r.status, OrderStatus::Filled);
    }

    // Multi-level sweep
    #[test]
    fn market_buy_sweep_multiple_levels() {
        let mut ob = book();
        for i in 0..5 {
            ob.place_order(
                limit_sell(fp(100 + i as i64), fp(2)),
                addr(i + 1),
                i as u64 + 1,
            );
        }

        let r = ob.place_order(market_buy(fp(10)), addr(10), 10);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 5);
        for (i, fill) in r.fills.iter().enumerate() {
            assert_eq!(fill.price, fp(100 + i as i64));
            assert_eq!(fill.quantity, fp(2));
        }
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn market_sell_sweep_multiple_levels() {
        let mut ob = book();
        for i in 0..5 {
            ob.place_order(
                limit_buy(fp(100 - i as i64), fp(2)),
                addr(i + 1),
                i as u64 + 1,
            );
        }

        let r = ob.place_order(market_sell(fp(10)), addr(10), 10);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 5);
        // Highest bid filled first
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(r.fills[4].price, fp(96));
    }

    // Edge cases
    #[test]
    fn minimum_quantity_fill() {
        let mut ob = book();
        let min = fp_frac(0, 1); // 0.00000001
        ob.place_order(limit_sell(fp(100), min), addr(1), 1);

        let r = ob.place_order(limit_buy(fp(100), min), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills[0].quantity, min);
    }

    #[test]
    fn large_price_values() {
        let mut ob = book();
        let big = FixedPoint::from_raw(i64::MAX as i128 * FixedPoint::SCALE / 100);
        ob.place_order(limit_sell(big, fp(1)), addr(1), 1);

        let r = ob.place_order(limit_buy(big, fp(1)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
    }

    #[test]
    fn sell_matches_best_bid_only() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(98), fp(5)), addr(2), 2);

        // Sell at 99 → matches 100 bid but not 98 bid
        let r = ob.place_order(limit_sell(fp(99), fp(5)), addr(3), 3);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(ob.order_count(), 1);
        assert_eq!(ob.best_bid(), Some(fp(98)));
    }

    // Book state consistency
    #[test]
    fn book_consistent_after_operations() {
        let mut ob = book();

        // Build up the book
        for i in 1..=5 {
            ob.place_order(limit_buy(fp(95 + i as i64), fp(10)), addr(i), i as u64);
        }
        for i in 6..=10 {
            ob.place_order(limit_sell(fp(100 + i as i64), fp(10)), addr(i), i as u64);
        }
        assert_eq!(ob.order_count(), 10);
        assert_eq!(ob.bid_levels(), 5);
        assert_eq!(ob.ask_levels(), 5);

        // Cancel some
        ob.cancel_order(1).unwrap();
        ob.cancel_order(6).unwrap();
        assert_eq!(ob.order_count(), 8);

        // Match some
        ob.place_order(market_sell(fp(25)), addr(11), 11);
        // Filled 25 from bids (best bids first)

        // Verify remaining state is consistent
        let bid_count: usize = ob.bids.values().map(|q| q.len()).sum();
        let ask_count: usize = ob.asks.values().map(|q| q.len()).sum();
        assert_eq!(ob.order_count(), bid_count + ask_count);

        // Every order in index has a valid location
        for (&id, loc) in &ob.order_index {
            let book = match loc.side {
                Side::Buy => &ob.bids,
                Side::Sell => &ob.asks,
            };
            assert!(book.get(&loc.price).unwrap().iter().any(|o| o.id == id));
        }
    }

    // Fill at maker's price
    #[test]
    fn fill_at_maker_price_not_taker() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        // Buy at 105 → fills at 100 (maker's price)
        let r = ob.place_order(limit_buy(fp(105), fp(10)), addr(2), 2);
        assert_eq!(r.fills[0].price, fp(100));
    }

    #[test]
    fn sell_fill_at_maker_price() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);

        // Sell at 95 → fills at 100 (maker's price)
        let r = ob.place_order(limit_sell(fp(95), fp(10)), addr(2), 2);
        assert_eq!(r.fills[0].price, fp(100));
    }

    // Multiple traders cancel-all
    #[test]
    fn cancel_all_leaves_other_traders() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(99), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(100), fp(5)), addr(2), 2);
        ob.place_order(limit_sell(fp(105), fp(5)), addr(1), 3);

        ob.cancel_all(addr(1), None);
        assert_eq!(ob.order_count(), 1);
        // addr(2)'s order still there
        assert!(ob.get_order(2).is_some());
    }

    // IOC full fill
    #[test]
    fn ioc_full_fill() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::IOC,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 1);
    }

    // Order ID allocation
    #[test]
    fn order_ids_sequential() {
        let mut ob = book();
        let r1 = ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);
        let r2 = ob.place_order(limit_buy(fp(99), fp(10)), addr(2), 2);
        let r3 = ob.place_order(limit_buy(fp(98), fp(10)), addr(3), 3);
        assert_eq!(r1.order_id, 1);
        assert_eq!(r2.order_id, 2);
        assert_eq!(r3.order_id, 3);
    }

    // Last trade price tracking
    #[test]
    fn last_trade_price_updates() {
        let mut ob = book();
        assert_eq!(ob.last_trade_price(), None);

        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(100), fp(5)), addr(2), 2);
        assert_eq!(ob.last_trade_price(), Some(fp(100)));

        ob.place_order(limit_sell(fp(101), fp(5)), addr(3), 3);
        ob.place_order(limit_buy(fp(101), fp(5)), addr(4), 4);
        assert_eq!(ob.last_trade_price(), Some(fp(101)));
    }

    // Cancel after partial fill
    #[test]
    fn cancel_partially_filled_order() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        ob.place_order(limit_buy(fp(100), fp(3)), addr(2), 2);

        // Order 1 partially filled (7 remaining)
        let order = ob.get_order(1).unwrap();
        assert_eq!(order.remaining_qty, fp(7));

        // Cancel the remainder
        let cancelled = ob.cancel_order(1).unwrap();
        assert_eq!(cancelled.remaining_qty, fp(7));
        assert_eq!(ob.order_count(), 0);
    }

    // Determinism test
    #[test]
    fn deterministic_matching() {
        fn run_sequence() -> (
            Vec<PlaceResult>,
            usize,
            Option<FixedPoint>,
            Option<FixedPoint>,
        ) {
            let mut ob = book();
            let mut results = Vec::new();

            results.push(ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1));
            results.push(ob.place_order(limit_sell(fp(99), fp(5)), addr(2), 2));
            results.push(ob.place_order(limit_buy(fp(100), fp(8)), addr(3), 3));
            results.push(ob.place_order(limit_buy(fp(98), fp(3)), addr(4), 4));
            results.push(ob.place_order(market_sell(fp(2)), addr(5), 5));

            (results, ob.order_count(), ob.best_bid(), ob.best_ask())
        }

        let (r1, c1, bb1, ba1) = run_sequence();
        let (r2, c2, bb2, ba2) = run_sequence();

        assert_eq!(c1, c2);
        assert_eq!(bb1, bb2);
        assert_eq!(ba1, ba2);
        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.order_id, b.order_id);
            assert_eq!(a.status, b.status);
            assert_eq!(a.fills, b.fills);
        }
    }

    // Mixed buy/sell at same price
    #[test]
    fn crossing_orders_immediate_match() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);

        // Buy at 101 crosses the ask at 100
        let r = ob.place_order(limit_buy(fp(101), fp(5)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills[0].price, fp(100)); // fills at maker's price
    }

    // Query methods
    #[test]
    fn query_best_bid_ask_empty() {
        let ob = book();
        assert_eq!(ob.best_bid(), None);
        assert_eq!(ob.best_ask(), None);
        assert_eq!(ob.spread(), None);
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn query_spread() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(99), fp(5)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(5)), addr(2), 2);
        assert_eq!(ob.spread(), Some(fp(2)));
    }

    // Fractional quantities
    #[test]
    fn fractional_quantity_matching() {
        let mut ob = book();
        let qty = fp_frac(1, 50_000_000); // 1.5
        ob.place_order(limit_sell(fp(100), qty), addr(1), 1);

        let r = ob.place_order(limit_buy(fp(100), qty), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills[0].quantity, qty);
    }

    // Reduce-only flag preserved
    #[test]
    fn reduce_only_preserved() {
        let mut ob = book();
        let params = PlaceOrderParams {
            reduce_only: true,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(1), 1);
        let order = ob.get_order(r.order_id).unwrap();
        assert!(order.reduce_only);
    }

    // Client order ID preserved
    #[test]
    fn client_order_id_preserved() {
        let mut ob = book();
        let params = PlaceOrderParams {
            client_order_id: Some(42),
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(1), 1);
        let order = ob.get_order(r.order_id).unwrap();
        assert_eq!(order.client_order_id, Some(42));
    }

    // Fill maker_side tracking
    #[test]
    fn fill_tracks_maker_side() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let r = ob.place_order(limit_buy(fp(100), fp(10)), addr(2), 2);
        assert_eq!(r.fills[0].maker_side, Side::Sell);

        let mut ob = book();
        ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);
        let r = ob.place_order(limit_sell(fp(100), fp(10)), addr(2), 2);
        assert_eq!(r.fills[0].maker_side, Side::Buy);
    }

    // Empty book after all fills
    #[test]
    fn book_empty_after_full_sweep() {
        let mut ob = book();
        for i in 1..=10 {
            ob.place_order(limit_sell(fp(100 + i as i64), fp(1)), addr(i), i as u64);
        }
        assert_eq!(ob.order_count(), 10);

        ob.place_order(market_buy(fp(10)), addr(20), 20);
        assert_eq!(ob.order_count(), 0);
        assert!(ob.asks.is_empty());
        assert_eq!(ob.ask_levels(), 0);
    }

    // FOK with self-trade: pre-check should skip own orders
    #[test]
    fn fok_skips_self_trade_in_precheck() {
        let mut ob = book();
        // Trader 1 places sell of 5
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        // Trader 2 places sell of 5
        ob.place_order(limit_sell(fp(100), fp(5)), addr(2), 2);

        // Trader 1 FOK buy of 5 → should skip own sell, fill against trader 2
        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(100), fp(5))
        };
        let r = ob.place_order(params, addr(1), 3);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills[0].maker, addr(2));
    }

    // FOK rejected because only self-trade available
    #[test]
    fn fok_rejected_only_self_trade() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(100), fp(10))
        };
        // Same trader → can't fill (self-trade prevention), so FOK rejects
        let r = ob.place_order(params, addr(1), 2);
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    // Many orders at same price level
    #[test]
    fn many_orders_same_level() {
        let mut ob = book();
        for i in 1..=100u8 {
            ob.place_order(limit_sell(fp(100), fp(1)), addr(i), i as u64);
        }
        assert_eq!(ob.order_count(), 100);
        assert_eq!(ob.ask_levels(), 1);

        // Sweep 50
        let r = ob.place_order(market_buy(fp(50)), addr(200), 200);
        assert_eq!(r.fills.len(), 50);
        assert_eq!(ob.order_count(), 50);

        // Fills are in FIFO order
        for (i, fill) in r.fills.iter().enumerate() {
            assert_eq!(fill.maker, addr(i as u8 + 1));
        }
    }

    // Modify to different price level
    #[test]
    fn modify_to_different_price_level() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        assert_eq!(ob.best_ask(), Some(fp(100)));

        ob.modify_order(1, Some(fp(99)), None).unwrap();
        assert_eq!(ob.best_ask(), Some(fp(99)));
        assert_eq!(ob.order_count(), 1);
    }

    // Cancel then re-place
    #[test]
    fn cancel_and_resubmit() {
        let mut ob = book();
        let r = ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);
        ob.cancel_order(r.order_id).unwrap();
        assert_eq!(ob.order_count(), 0);

        // Re-place at different price
        let r2 = ob.place_order(limit_buy(fp(101), fp(10)), addr(1), 2);
        assert_eq!(ob.order_count(), 1);
        assert_eq!(ob.best_bid(), Some(fp(101)));
        assert_ne!(r.order_id, r2.order_id); // new ID
    }

    // ====================================================================
    // 2.1b.2 — Dust order rejection
    // ====================================================================

    #[test]
    fn dust_order_rejected() {
        // lot_size = 1.0, so 0.5 is dust
        let mut ob = OrderBook::new(1, fp_frac(0, 1), fp(1));
        let r = ob.place_order(limit_buy(fp(100), fp_frac(0, 50_000_000)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert!(r.fills.is_empty());
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn dust_order_exact_lot_size_accepted() {
        let mut ob = OrderBook::new(1, fp_frac(0, 1), fp(1));
        let r = ob.place_order(limit_buy(fp(100), fp(1)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Resting);
        assert_eq!(ob.order_count(), 1);
    }

    #[test]
    fn dust_stop_order_rejected() {
        let mut ob = OrderBook::new(1, fp_frac(0, 1), fp(1));
        let params = PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: fp(120), // s515: stop-market slippage cap
            quantity: fp_frac(0, 50_000_000),
            order_type: OrderType::StopMarket { trigger: fp(100) },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let r = ob.place_order(params, addr(1), 1);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert_eq!(ob.pending_stop_count(), 0);
    }

    #[test]
    fn dust_market_order_rejected() {
        let mut ob = OrderBook::new(1, fp_frac(0, 1), fp(1));
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        let r = ob.place_order(market_buy(fp_frac(0, 50_000_000)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    // ====================================================================
    // FIX 7 — Price validation for limit orders
    // ====================================================================

    #[test]
    fn reject_zero_price_limit() {
        let mut ob = book();
        let r = ob.place_order(limit_buy(FixedPoint::ZERO, fp(10)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn reject_negative_price_limit() {
        let mut ob = book();
        let r = ob.place_order(
            limit_buy(FixedPoint::from_raw(-100 * FixedPoint::SCALE), fp(10)),
            addr(1),
            1,
        );
        assert_eq!(r.status, OrderStatus::Rejected);
        assert_eq!(ob.order_count(), 0);
    }

    // ====================================================================
    // FIX 8 — Tick size enforcement
    // ====================================================================

    #[test]
    fn reject_tick_size_violation() {
        // Book with tick_size = 1.0 (fp(1))
        let mut ob = OrderBook::new(1, fp(1), fp_frac(0, 1));
        // Price 100.5 doesn't align to tick_size 1.0
        // raw = 100 * SCALE + SCALE/2 = 10050000000
        // tick_size raw = 1 * SCALE = 100000000
        // 10050000000 % 100000000 = 50000000 != 0 => rejected
        let r = ob.place_order(
            limit_buy(fp_frac(100, FixedPoint::SCALE as i64 / 2), fp(10)),
            addr(1),
            1,
        );
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    #[test]
    fn accept_valid_tick_size() {
        let mut ob = OrderBook::new(1, fp(1), fp_frac(0, 1));
        let r = ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Resting);
    }

    // ====================================================================
    // FIX 9 — Stop order trigger direction validation
    // ====================================================================

    #[test]
    fn reject_buy_stop_below_market() {
        let mut ob = book();
        // Create a last trade price by executing a fill
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(market_buy(fp(5)), addr(2), 2);
        // last_trade_price is now 100

        // Buy stop with trigger at 90 (below current) should be rejected
        let r = ob.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(120), // s515: stop-market slippage cap
                quantity: fp(5),
                order_type: OrderType::StopMarket { trigger: fp(90) },
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            addr(3),
            3,
        );
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    #[test]
    fn accept_buy_stop_above_market() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(market_buy(fp(5)), addr(2), 2);

        // Buy stop with trigger at 110 (above current) should be accepted
        let r = ob.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(120), // s515: stop-market slippage cap
                quantity: fp(5),
                order_type: OrderType::StopMarket { trigger: fp(110) },
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            addr(3),
            3,
        );
        assert_eq!(r.status, OrderStatus::PendingTrigger);
    }

    // ====================================================================
    // FIX 10 — Max orders per trader per market
    // ====================================================================

    #[test]
    fn no_per_market_cap_on_one_traders_orders() {
        // The open-order limit is per user across all markets and enforced
        // by the executor; the book itself has no per-trader cap.
        let mut ob = book();
        for i in 0..250 {
            let price = fp(100) + FixedPoint::from_raw(i as i128 * FixedPoint::SCALE);
            let r = ob.place_order(limit_sell(price, fp(1)), addr(1), i as u64);
            assert_eq!(r.status, OrderStatus::Resting, "order {i} should rest");
        }
        assert_eq!(ob.open_order_count(&addr(1)), 250);
        // An aggressive order from the same trader still trades (STP aside).
        ob.place_order(limit_buy(fp(90), fp(1)), addr(2), 998);
        let r = ob.place_order(market_sell(fp(1)), addr(1), 999);
        assert_eq!(r.status, OrderStatus::Filled);
    }

    // ====================================================================
    // FIX 21 — original_qty updated on modify increase
    // ====================================================================

    #[test]
    fn modify_increase_updates_original_qty() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let modified = ob.modify_order(1, None, Some(fp(20))).unwrap();
        assert_eq!(modified.remaining_qty, fp(20));
        assert_eq!(modified.original_qty, fp(20)); // FIX 21: should be updated

        // Verify on book too
        let on_book = ob.get_order(1).unwrap();
        assert_eq!(on_book.original_qty, fp(20));
    }

    // ====================================================================
    // FIX 1 — Borsh serialization roundtrip
    // ====================================================================

    #[test]
    fn order_book_serialize_deserialize_roundtrip() {
        let mut ob = book();

        // Place some orders
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(5)), addr(2), 2);
        ob.place_order(limit_buy(fp(99), fp(8)), addr(3), 3);
        ob.place_order(limit_buy(fp(98), fp(3)), addr(4), 4);

        let original_count = ob.order_count();
        let original_bid = ob.best_bid();
        let original_ask = ob.best_ask();

        // Serialize
        let data = borsh::to_vec(&ob).unwrap();

        // Deserialize
        let restored: OrderBook = OrderBook::try_from_slice(&data).unwrap();

        assert_eq!(restored.order_count(), original_count);
        assert_eq!(restored.best_bid(), original_bid);
        assert_eq!(restored.best_ask(), original_ask);
        assert_eq!(restored.market_id, ob.market_id);

        // Verify specific orders
        assert!(restored.get_order(1).is_some());
        assert_eq!(restored.get_order(1).unwrap().remaining_qty, fp(10));
        assert!(restored.get_order(3).is_some());
        assert_eq!(restored.get_order(3).unwrap().remaining_qty, fp(8));
    }
}

// ============================================================================
// CONSENSUS PREIMAGE CHARACTERIZATION TESTS
//
// `level_hash` is committed by the native state root: one byte of drift is a
// hard fork. These tests PIN the observable output of the level-hash preimage
// builder against hard-coded constants captured from the pre-refactor encoder,
// so any change to WHAT bytes are produced (as opposed to WHERE they are
// staged) fails here. Fixtures are built from fixed seeds — no clocks, no RNG,
// no HashMap iteration order (the preimage walks the level's VecDeque).
// ============================================================================
#[cfg(test)]
mod level_preimage_characterization {
    use super::*;

    const LEVEL_PRICE_RAW: i128 = 100 * FixedPoint::SCALE;

    fn hex32(b: &[u8; 32]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Deterministic order derived from `i` alone (splitmix-style avalanche —
    /// no RNG crate, no clock). Cycles every `OrderType`, every `TimeInForce`,
    /// both `reduce_only` values and both `client_order_id` shapes.
    fn fixture_order(i: u64) -> Order {
        let mut m = i.wrapping_add(0x9E37_79B9_7F4A_7C15);
        m = (m ^ (m >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        m = (m ^ (m >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        m ^= m >> 31;
        let qty = (m % 1_000_000) as i128 + 1;
        Order {
            id: (i + 1) as OrderId,
            trader: Address::from([(m & 0xff) as u8; 20]),
            side: Side::Buy,
            price: FixedPoint::from_raw(LEVEL_PRICE_RAW),
            remaining_qty: FixedPoint::from_raw(qty),
            original_qty: FixedPoint::from_raw(qty + 7),
            order_type: match m % 4 {
                0 => OrderType::Limit,
                1 => OrderType::Market,
                2 => OrderType::StopMarket {
                    trigger: FixedPoint::from_raw(qty * 3 - 11),
                },
                _ => OrderType::StopLimit {
                    trigger: FixedPoint::from_raw(-(qty * 5) - 1),
                    limit: FixedPoint::from_raw(qty * 7),
                },
            },
            time_in_force: match (m >> 8) % 4 {
                0 => TimeInForce::GTC,
                1 => TimeInForce::IOC,
                2 => TimeInForce::FOK,
                _ => TimeInForce::PostOnly,
            },
            timestamp: m >> 17,
            reduce_only: (m >> 5) & 1 == 1,
            client_order_id: if (m >> 6) & 1 == 1 { Some(m) } else { None },
        }
    }

    /// A book holding exactly `depth` resting orders on ONE bid level, in
    /// insertion order, with pinned seqs (500.., independent of any allocator).
    fn fixture_book(depth: usize) -> OrderBook {
        let mut ob = OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1));
        ob.set_next_seq(depth as u64 + 10_000);
        for i in 0..depth {
            ob.insert_loaded_order(fixture_order(i as u64), 500 + i as u64);
        }
        ob
    }

    /// Independent scratch oracle: the frozen wire shape spelled out by hand,
    /// `u32_LE(row_len) ‖ seq(8 BE) ‖ borsh(Order)` front→back, keccak.
    fn oracle(ob: &OrderBook, depth: usize) -> crate::book_rows::LevelRowData {
        let queue = ob
            .level_queue(Side::Buy, FixedPoint::from_raw(LEVEL_PRICE_RAW))
            .expect("level exists");
        assert_eq!(queue.len(), depth);
        let mut total: i128 = 0;
        let mut preimage: Vec<u8> = Vec::new();
        for o in queue {
            total += o.remaining_qty.raw();
            let seq = ob.order_seq_of(o.id).expect("resting order has a seq");
            let mut row = Vec::new();
            row.extend_from_slice(&seq.to_be_bytes());
            borsh::BorshSerialize::serialize(o, &mut row).unwrap();
            preimage.extend_from_slice(&(row.len() as u32).to_le_bytes());
            preimage.extend_from_slice(&row);
        }
        crate::book_rows::LevelRowData {
            total_qty_raw: total,
            order_count: queue.len() as u32,
            level_hash: alloy_primitives::keccak256(&preimage).0,
        }
    }

    /// A spread of `Order` shapes for the row-codec pin: every TIF, every
    /// order type, both `reduce_only`, all `client_order_id` shapes, and i128
    /// extremes (incl. negatives and MIN/MAX) in the price/qty fields.
    fn shape_spread() -> Vec<(u64, Order)> {
        let mut v = Vec::new();
        let base = Order {
            id: 7,
            trader: Address::from([0xAB; 20]),
            side: Side::Sell,
            price: FixedPoint::from_raw(1),
            remaining_qty: FixedPoint::from_raw(2),
            original_qty: FixedPoint::from_raw(3),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            timestamp: 0,
            reduce_only: false,
            client_order_id: None,
        };
        for (idx, tif) in [
            TimeInForce::GTC,
            TimeInForce::IOC,
            TimeInForce::FOK,
            TimeInForce::PostOnly,
        ]
        .into_iter()
        .enumerate()
        {
            let mut o = base.clone();
            o.time_in_force = tif;
            o.id = 100 + idx as OrderId;
            v.push((idx as u64, o));
        }
        for (idx, ro) in [false, true].into_iter().enumerate() {
            let mut o = base.clone();
            o.reduce_only = ro;
            o.id = 200 + idx as OrderId;
            v.push((u64::MAX - idx as u64, o));
        }
        for (idx, coid) in [None, Some(0u64), Some(u64::MAX)].into_iter().enumerate() {
            let mut o = base.clone();
            o.client_order_id = coid;
            o.id = 300 + idx as OrderId;
            v.push((idx as u64 * 1_000_003, o));
        }
        for (idx, ot) in [
            OrderType::Limit,
            OrderType::Market,
            OrderType::StopMarket {
                trigger: FixedPoint::from_raw(i128::MIN),
            },
            OrderType::StopLimit {
                trigger: FixedPoint::from_raw(i128::MAX),
                limit: FixedPoint::from_raw(-1),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let mut o = base.clone();
            o.order_type = ot;
            o.id = 400 + idx as OrderId;
            v.push((0xDEAD_BEEF_CAFE_0000 + idx as u64, o));
        }
        for (idx, raw) in [i128::MIN, i128::MIN + 1, -1i128, 0, i128::MAX]
            .into_iter()
            .enumerate()
        {
            let mut o = base.clone();
            o.price = FixedPoint::from_raw(raw);
            o.remaining_qty = FixedPoint::from_raw(raw);
            o.original_qty = FixedPoint::from_raw(raw.wrapping_neg());
            o.side = if idx % 2 == 0 { Side::Buy } else { Side::Sell };
            o.timestamp = u64::MAX - idx as u64;
            o.id = 500 + idx as OrderId;
            v.push((idx as u64, o));
        }
        v
    }

    /// PIN 1 — `level_row_data` output at depths 1 / 2 / 100 / 1000, as
    /// `(depth, level_hash hex, total_qty_raw, order_count)`, captured from the
    /// pre-refactor encoder. These are consensus bytes: if this test fails, the
    /// state-root preimage changed.
    const PINNED_LEVELS: [(usize, &str, i128, u32); 4] = [
        (
            1,
            "dda17d0ba0f95ca56ec1cb4c3dc3b8a99d1406fc6b11eaa43851f0f19b880f14",
            607_536,
            1,
        ),
        (
            2,
            "a5773c2b3a041435f7d6995bb82b932ca63d9646358ab3e7ae4522fe5f9cead1",
            1_430_002,
            2,
        ),
        (
            100,
            "6f53fb7754706154ef65538416629cb4e4d70388ee835fee48d4833147f5f485",
            51_292_508,
            100,
        ),
        (
            1000,
            "232826303bfc2d1b335f41a54707104aa246f639fb97856cdfaa643b7f517ecc",
            491_881_418,
            1000,
        ),
    ];

    /// PIN 2 — keccak over the concatenation of every `encode_order_row_parts`
    /// output in `shape_spread()`, each framed by its u32-LE length (so a
    /// length drift is caught as well as a content drift).
    const PINNED_SHAPE_SPREAD: &str =
        "f5d7a4b2fb1f3a56db9d6f62a902e988eff5e51cd9ebf655b78476415b2687a9";

    /// Dump helper for (re-)capturing the pins. Ignored by default; run with
    /// `--ignored --nocapture` to print the current constants.
    #[test]
    #[ignore]
    fn dump_pins() {
        for depth in [1usize, 2, 100, 1000] {
            let ob = fixture_book(depth);
            let d = ob
                .level_row_data(crate::book_rows::SIDE_TAG_BID, LEVEL_PRICE_RAW)
                .expect("level exists");
            println!(
                "PIN_LEVEL ({}, \"{}\", {}, {}),",
                depth,
                hex32(&d.level_hash),
                d.total_qty_raw,
                d.order_count
            );
        }
        let mut cat: Vec<u8> = Vec::new();
        for (seq, o) in shape_spread() {
            let row = OrderBook::encode_order_row_parts(seq, &o);
            cat.extend_from_slice(&(row.len() as u32).to_le_bytes());
            cat.extend_from_slice(&row);
        }
        println!(
            "PIN_SHAPES \"{}\" bytes={}",
            hex32(&alloy_primitives::keccak256(&cat).0),
            cat.len()
        );
    }

    #[test]
    fn level_row_data_matches_pinned_consensus_bytes() {
        for (depth, hash_hex, total, count) in PINNED_LEVELS {
            let ob = fixture_book(depth);
            let d = ob
                .level_row_data(crate::book_rows::SIDE_TAG_BID, LEVEL_PRICE_RAW)
                .expect("level exists");
            assert_eq!(hex32(&d.level_hash), hash_hex, "level_hash drift @depth {depth}");
            assert_eq!(d.total_qty_raw, total, "total_qty_raw drift @depth {depth}");
            assert_eq!(d.order_count, count, "order_count drift @depth {depth}");
            // Cross-check against the hand-written wire-shape oracle.
            assert_eq!(d, oracle(&ob, depth), "oracle mismatch @depth {depth}");
        }
    }

    /// All three cached arms (Miss → Promote → Hit, including a Hit that
    /// absorbs an appended tail) must reproduce the plain one-shot bytes.
    #[test]
    fn cached_arms_match_plain() {
        let tag = crate::book_rows::SIDE_TAG_BID;
        let mut ob = fixture_book(100);
        ob.ensure_level_hash_cache(1 << 20);
        let mut cache = ob.level_hash_cache.take().expect("cache enabled");

        // Miss (no slot yet) — plain path verbatim + probe record.
        let miss = ob.level_row_data_cached(tag, LEVEL_PRICE_RAW, &mut cache).unwrap();
        assert_eq!(miss, ob.level_row_data(tag, LEVEL_PRICE_RAW).unwrap());
        assert_eq!(cache.misses, 1);

        // Promote (probe, same epoch) — full front→back streaming absorb.
        let promote = ob.level_row_data_cached(tag, LEVEL_PRICE_RAW, &mut cache).unwrap();
        assert_eq!(promote, ob.level_row_data(tag, LEVEL_PRICE_RAW).unwrap());
        assert_eq!(cache.seeds, 1);

        // Hit with NO new tail.
        let hit0 = ob.level_row_data_cached(tag, LEVEL_PRICE_RAW, &mut cache).unwrap();
        assert_eq!(hit0, ob.level_row_data(tag, LEVEL_PRICE_RAW).unwrap());

        // Hit WITH an appended tail (appends do not bump the level epoch).
        for i in 100..137u64 {
            ob.insert_loaded_order(fixture_order(i), 500 + i);
        }
        let hit1 = ob.level_row_data_cached(tag, LEVEL_PRICE_RAW, &mut cache).unwrap();
        assert_eq!(hit1, ob.level_row_data(tag, LEVEL_PRICE_RAW).unwrap());
        assert_eq!(hit1.order_count, 137);
        assert_eq!(cache.hits, 2);
    }

    #[test]
    fn encode_order_row_parts_shape_spread_is_pinned() {
        let mut cat: Vec<u8> = Vec::new();
        for (seq, o) in shape_spread() {
            let row = OrderBook::encode_order_row_parts(seq, &o);
            // Structural invariant: seq is the first 8 bytes, big-endian...
            assert_eq!(&row[..8], &seq.to_be_bytes());
            // ...and the remainder is exactly borsh(Order).
            let mut expect = Vec::new();
            borsh::BorshSerialize::serialize(&o, &mut expect).unwrap();
            assert_eq!(&row[8..], &expect[..]);
            cat.extend_from_slice(&(row.len() as u32).to_le_bytes());
            cat.extend_from_slice(&row);
        }
        assert_eq!(
            hex32(&alloy_primitives::keccak256(&cat).0),
            PINNED_SHAPE_SPREAD,
            "order-row codec drift"
        );
    }

    /// The one bug the buffer-reuse refactor could introduce: a missing or
    /// mis-ordered `clear()`, which would leave stale bytes in front of the
    /// row AND inflate `buf.len()` — i.e. corrupt both the payload and the
    /// u32-LE framing length. A DIRTY buffer must produce exactly what a fresh
    /// one does, byte-for-byte and length-for-length, for every shape.
    #[test]
    fn order_row_trader_reads_the_trader_without_decoding() {
        for (seq, o) in shape_spread() {
            let row = OrderBook::encode_order_row_parts(seq, &o);
            assert_eq!(OrderBook::order_row_trader(&row), Some(o.trader.as_slice()));
            let (_, decoded) = OrderBook::decode_order_row(&row).unwrap();
            assert_eq!(decoded.trader, o.trader);
        }
        assert_eq!(OrderBook::order_row_trader(&[0u8; 43]), None);
    }

    #[test]
    fn encode_order_row_into_is_dirty_buffer_proof() {
        // Junk shapes: shorter than a row, exactly a row, far longer than a
        // row, and a buffer with a big spare capacity but zero length.
        let junks: [Vec<u8>; 5] = [
            vec![],
            vec![0xFF; 3],
            vec![0x5A; 120],
            vec![0xA5; 4096],
            Vec::with_capacity(8192),
        ];
        for (seq, o) in shape_spread() {
            let fresh = OrderBook::encode_order_row_parts(seq, &o);
            for junk in &junks {
                let mut dirty = junk.clone();
                OrderBook::encode_order_row_into(&mut dirty, seq, &o);
                assert_eq!(dirty.len(), fresh.len(), "framing length drift on dirty buf");
                assert_eq!(dirty, fresh, "byte drift on dirty buf (seq={seq})");
            }
            // Reuse ACROSS orders (the actual hot-path pattern): the buffer
            // still holds the previous, possibly longer, row.
            let mut reused = Vec::new();
            for (seq2, o2) in shape_spread() {
                OrderBook::encode_order_row_into(&mut reused, seq2, &o2);
            }
            OrderBook::encode_order_row_into(&mut reused, seq, &o);
            assert_eq!(reused, fresh, "byte drift on cross-order reuse");
        }
    }

    // ------------------------------------------------------------------
    // Mode 3 (chunked digest) — independent oracle + pinned vectors.
    // ------------------------------------------------------------------

    /// Hand-spelled mode-3 oracle: bucket the level's frames by
    /// `seq / 64` in queue order, keccak each bucket, then keccak
    /// `DOMAIN ‖ count(u32 BE) ‖ total(i128 BE) ‖ Σ_asc idx(u64 BE) ‖ digest`.
    /// Shares NO code with the implementation (own framing, own grouping).
    fn chunked_oracle(ob: &OrderBook, side: Side, raw_price: i128) -> Option<crate::book_rows::LevelRowData> {
        let queue = ob.level_queue(side, FixedPoint::from_raw(raw_price))?;
        if queue.is_empty() {
            return None;
        }
        let mut buckets: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut total: i128 = 0;
        for o in queue {
            total += o.remaining_qty.raw();
            let seq = ob.order_seq_of(o.id).expect("resting order has a seq");
            let mut row = Vec::new();
            row.extend_from_slice(&seq.to_be_bytes());
            borsh::BorshSerialize::serialize(o, &mut row).unwrap();
            let b = buckets.entry(seq / 64).or_default();
            b.extend_from_slice(&(row.len() as u32).to_le_bytes());
            b.extend_from_slice(&row);
        }
        let mut top: Vec<u8> = b"TORUSLV2".to_vec();
        top.extend_from_slice(&(queue.len() as u32).to_be_bytes());
        top.extend_from_slice(&total.to_be_bytes());
        for (idx, bytes) in &buckets {
            top.extend_from_slice(&idx.to_be_bytes());
            top.extend_from_slice(&alloy_primitives::keccak256(bytes).0);
        }
        Some(crate::book_rows::LevelRowData {
            total_qty_raw: total,
            order_count: queue.len() as u32,
            level_hash: alloy_primitives::keccak256(&top).0,
        })
    }

    /// PIN 3 — mode-3 `level_row_data_chunked` output at depths 1 / 2 / 100 /
    /// 1000 on the same fixture as PIN 1 (seqs 500.. ⇒ chunk boundaries at
    /// 512, 576, …, so depth 100 spans 3 chunks and depth 1000 spans 17).
    /// Consensus bytes for `TORUS_BOOK_ROWS=3`.
    const PINNED_CHUNKED_LEVELS: [(usize, &str, i128, u32); 4] = [
        (
            1,
            "92057f3fe1a90d2c43999af96afc36b42aa622345f3d79aaafeae8814dcc1855",
            607_536,
            1,
        ),
        (
            2,
            "1ae40f8b2c58bdfa7e898dc6fd1c523f7829906bfb286beec16b98b75a3d3939",
            1_430_002,
            2,
        ),
        (
            100,
            "4d6803930eba3b71d8c3e728eabc7380ecc8a0a2bef1371a1571dc8ae7c5a7d1",
            51_292_508,
            100,
        ),
        (
            1000,
            "d4785c1dabd2df08f8589170f542279efeba37c5677caed2b62058dd1ff7be0a",
            491_881_418,
            1000,
        ),
    ];

    #[test]
    #[ignore]
    fn dump_chunked_pins() {
        for depth in [1usize, 2, 100, 1000] {
            let ob = fixture_book(depth);
            let d = ob
                .level_row_data_chunked(crate::book_rows::SIDE_TAG_BID, LEVEL_PRICE_RAW)
                .expect("level exists");
            println!(
                "PIN_CHUNKED ({}, \"{}\", {}, {}),",
                depth,
                hex32(&d.level_hash),
                d.total_qty_raw,
                d.order_count
            );
        }
    }

    #[test]
    fn level_row_data_chunked_matches_oracle_and_pins() {
        for (depth, hash_hex, total, count) in PINNED_CHUNKED_LEVELS {
            let ob = fixture_book(depth);
            let d = ob
                .level_row_data_chunked(crate::book_rows::SIDE_TAG_BID, LEVEL_PRICE_RAW)
                .expect("level exists");
            assert_eq!(
                Some(d),
                chunked_oracle(&ob, Side::Buy, LEVEL_PRICE_RAW),
                "chunked oracle mismatch @depth {depth}"
            );
            // Aggregate parts are mode-independent.
            let flat = ob
                .level_row_data(crate::book_rows::SIDE_TAG_BID, LEVEL_PRICE_RAW)
                .unwrap();
            assert_eq!(d.total_qty_raw, flat.total_qty_raw);
            assert_eq!(d.order_count, flat.order_count);
            assert_ne!(d.level_hash, flat.level_hash, "modes must not collide");
            assert_eq!(hex32(&d.level_hash), hash_hex, "chunked level_hash drift @depth {depth}");
            assert_eq!(d.total_qty_raw, total);
            assert_eq!(d.order_count, count);
        }
    }

    /// The incremental drain (`take_level_ops` with the chunked digest
    /// selected) must reproduce the from-scratch chunked digest AND the
    /// hand oracle after every class of mutation: build, front pops (fills),
    /// mid removal (cancel), in-place qty change, tail append, cancel_all,
    /// and re-creation of an emptied level.
    #[test]
    fn chunked_incremental_drain_matches_scratch() {
        let tag = crate::book_rows::SIDE_TAG_BID;
        let mut ob = fixture_book(300);
        ob.set_level_hash_chunked(true);
        // First drain: level journal is empty (loads do not journal) — force
        // via a real mutation: append one order through insert_order.
        let check = |ob: &mut OrderBook, what: &str| {
            let ops = ob.take_level_ops();
            let got = ops
                .iter()
                .find(|(k, _)| *k == (tag, LEVEL_PRICE_RAW))
                .map(|(_, d)| *d)
                .unwrap_or_else(|| panic!("{what}: level not drained"));
            let scratch = ob.level_row_data_chunked(tag, LEVEL_PRICE_RAW);
            let oracle = chunked_oracle(ob, Side::Buy, LEVEL_PRICE_RAW);
            assert_eq!(got, scratch, "{what}: incremental != scratch");
            assert_eq!(got, oracle, "{what}: incremental != oracle");
        };
        let mut o = fixture_order(300);
        o.id = 100_000;
        ob.insert_order(o);
        check(&mut ob, "initial build + append");
        // Nothing dirty ⇒ no op for this level.
        assert!(ob.take_level_ops().is_empty());

        // Front pops: cross with a sell that eats 5 makers (fills at front).
        let taker = PlaceOrderParams {
            market_id: 1,
            is_buy: false,
            price: FixedPoint::from_raw(LEVEL_PRICE_RAW),
            quantity: {
                let q = ob.level_queue(Side::Buy, FixedPoint::from_raw(LEVEL_PRICE_RAW)).unwrap();
                let mut sum = FixedPoint::ZERO;
                for m in q.iter().take(5) {
                    sum += m.remaining_qty;
                }
                sum + FixedPoint::from_raw(1) // + partial on the 6th
            },
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::IOC,
            reduce_only: false,
            client_order_id: None,
        };
        let r = ob.place_order(taker, Address::from([0xEE; 20]), 7);
        assert!(r.fills.len() + r.self_trade_cancels.len() >= 6, "taker must consume the front");
        check(&mut ob, "front pops + partial");
        let (levels, chunks, dirty) = ob.level_chunk_stats();
        assert_eq!((levels, dirty), (1, 0));
        assert!(chunks >= 5, "300 orders over 64-seq chunks ⇒ >= 5 chunks");

        // Mid removal + in-place decrease + tail append in one interval.
        let mid_id = ob
            .level_queue(Side::Buy, FixedPoint::from_raw(LEVEL_PRICE_RAW))
            .unwrap()[150]
            .id;
        ob.cancel_order(mid_id).unwrap();
        let some_id = ob
            .level_queue(Side::Buy, FixedPoint::from_raw(LEVEL_PRICE_RAW))
            .unwrap()[40]
            .id;
        ob.modify_order(some_id, None, Some(FixedPoint::from_raw(1))).unwrap();
        let mut o2 = fixture_order(301);
        o2.id = 100_001;
        ob.insert_order(o2);
        check(&mut ob, "mid cancel + in-place modify + append");

        // Only the touched chunks were re-hashed: assert by dirty-count
        // bookkeeping — after drain nothing is pending.
        assert_eq!(ob.level_chunk_stats().2, 0);

        // Cancel-all of the taker-side traders empties nothing here; empty the
        // level completely via cancel_all per trader, then re-create it.
        let traders: Vec<Address> = ob
            .level_queue(Side::Buy, FixedPoint::from_raw(LEVEL_PRICE_RAW))
            .unwrap()
            .iter()
            .map(|o| o.trader)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        for t in traders {
            ob.cancel_all(t, None);
        }
        assert!(ob.level_queue(Side::Buy, FixedPoint::from_raw(LEVEL_PRICE_RAW)).is_none());
        let ops = ob.take_level_ops();
        assert!(
            ops.iter().any(|(k, d)| *k == (tag, LEVEL_PRICE_RAW) && d.is_none()),
            "emptied level must emit a delete"
        );
        assert_eq!(ob.level_chunk_stats(), (0, 0, 0));
        let mut o3 = fixture_order(302);
        o3.id = 100_002;
        ob.insert_order(o3);
        check(&mut ob, "re-created level");
    }
}

// ============================================================================
// Queue lookup by id (binary search on seq) — correctness + timing probe
// ============================================================================
//
// Every level queue is seq-ascending (appends take `next_seq`, loads assert
// ascending, in-place modifies keep the seq, matches pop the FRONT), so an
// order can be located in its level in O(log depth) seq probes instead of a
// front-to-back scan. `take_row_ops` re-encodes EVERY journaled row through
// `get_order` — with tens of thousands of appended rows per block on levels
// thousands deep, the scan was the O(depth) term left in save_books after the
// chunked digest. Pure lookup change: bytes/semantics untouched.
#[cfg(test)]
mod queue_lookup_tests {
    use super::*;

    fn fp(n: i64) -> FixedPoint {
        FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
    }
    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }
    /// Distinct trader per `i` modulo 4096 (keeps each trader's resting
    /// orders far below the per-user open-order limit).
    fn trader(i: usize) -> Address {
        let mut b = [0u8; 20];
        b[0] = 0xAA;
        b[18] = ((i >> 8) & 0x0F) as u8;
        b[19] = (i & 0xFF) as u8;
        Address::from(b)
    }
    fn params(is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy,
            price: fp(price),
            quantity: fp(qty),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    /// Deep level, ids looked up at every position, then interleaved cancels
    /// (mid), fills (front), in-place qty modify, cancel_all and a
    /// loaded-order book: `get_order` / cancel / modify must always resolve
    /// the right order (and never a neighbour) — the lookup must agree with a
    /// brute-force scan at every step.
    #[test]
    fn id_lookup_agrees_with_linear_scan_under_mutation() {
        let mut ob = OrderBook::new(1, fp(1), fp(1));
        let mut ids: Vec<OrderId> = Vec::new();
        for i in 0..2_000usize {
            let r = ob.place_order(params(true, 100 - (i % 3) as i64, 5), trader(i % 90), i as u64);
            assert_eq!(r.status, OrderStatus::Resting);
            ids.push(r.order_id);
        }
        let brute = |ob: &OrderBook, id: OrderId| -> Option<Order> {
            ob.bid_queues()
                .chain(ob.ask_queues())
                .flat_map(|(_, q)| q.iter())
                .find(|o| o.id == id)
                .cloned()
        };
        let check_all = |ob: &OrderBook, ids: &[OrderId]| {
            for &id in ids {
                assert_eq!(ob.get_order(id).cloned(), brute(ob, id), "id {id}");
            }
        };
        check_all(&ob, &ids);

        // Mid-queue cancels (every 5th), then verify + the cancelled are gone.
        let mut alive: Vec<OrderId> = Vec::new();
        for (i, &id) in ids.iter().enumerate() {
            if i % 5 == 3 {
                let o = ob.cancel_order(id).expect("cancel");
                assert_eq!(o.id, id);
                assert!(ob.get_order(id).is_none());
                assert!(brute(&ob, id).is_none());
            } else {
                alive.push(id);
            }
        }
        check_all(&ob, &alive);

        // Front fills: a sell sweeps part of the top bid level.
        let r = ob.place_order(params(false, 100, 300), addr(9), 5_000);
        assert!(!r.fills.is_empty());
        alive.retain(|&id| brute(&ob, id).is_some());
        check_all(&ob, &alive);
        assert!(ob.get_order(999_999).is_none());

        // In-place qty decrease keeps the position; the row is found.
        let victim = alive[alive.len() / 2];
        let o = ob.modify_order(victim, None, Some(fp(1))).expect("modify");
        assert_eq!(o.id, victim);
        assert_eq!(ob.get_order(victim).unwrap().remaining_qty, fp(1));
        check_all(&ob, &alive);

        // cancel_all for one trader.
        let gone = ob.cancel_all(trader(4), None);
        assert!(!gone.is_empty());
        for o in &gone {
            assert!(ob.get_order(o.id).is_none());
        }
        alive.retain(|&id| brute(&ob, id).is_some());
        check_all(&ob, &alive);

        // Loaded book (persisted seqs restored) resolves ids too — including
        // ids that are NOT ascending with seq (loader gets seq-ordered rows).
        let mut loaded = OrderBook::new(1, fp(1), fp(1));
        let mut rows: Vec<(u64, Order)> = ob
            .bid_queues()
            .flat_map(|(_, q)| q.iter())
            .map(|o| (ob.order_seq_of(o.id).unwrap(), o.clone()))
            .collect();
        rows.sort_by_key(|(s, o)| (o.price.raw(), *s));
        // Restore meta.next_seq first, as the real loaders do (native_executor
        // book load, book_reader): loaded seqs must be below it.
        loaded.set_next_seq(ob.next_seq());
        for (seq, o) in rows {
            loaded.insert_loaded_order(o, seq);
        }
        for &id in &alive {
            assert_eq!(loaded.get_order(id).cloned(), brute(&loaded, id), "loaded id {id}");
        }
        // And a Classic (borsh) round trip re-assigns seqs in queue order.
        let bytes = borsh::to_vec(&ob).unwrap();
        let rt: OrderBook = borsh::from_slice(&bytes).unwrap();
        for &id in &alive {
            assert_eq!(rt.get_order(id).cloned(), brute(&rt, id), "borsh id {id}");
        }
    }

    /// Timing probe (run with `--ignored --nocapture`): a 5 000-deep level,
    /// 2 000 rows appended and journaled in one "block", then `take_row_ops`.
    /// Before the seq binary search this was O(rows × depth) (~10 M order
    /// compares); after, O(rows × log depth).
    #[test]
    #[ignore]
    fn take_row_ops_timing_probe_deep_level() {
        let mut ob = OrderBook::new(1, fp(1), fp(1));
        for i in 0..5_000u64 {
            let r = ob.place_order(params(true, 100, 5), trader(i as usize), i);
            assert_eq!(r.status, OrderStatus::Resting);
        }
        let _ = ob.take_row_ops();
        for i in 0..2_000u64 {
            let r = ob.place_order(params(true, 100, 5), trader(2_048 + i as usize), 10_000 + i);
            assert_eq!(r.status, OrderStatus::Resting);
        }
        let t = std::time::Instant::now();
        let ops = ob.take_row_ops();
        let el = t.elapsed();
        assert_eq!(ops.len(), 2_000);
        eprintln!(
            "take_row_ops: {} rows on a {}-deep level: {:?} ({:.2} µs/row)",
            ops.len(),
            ob.order_count(),
            el,
            el.as_secs_f64() * 1e6 / ops.len() as f64
        );
    }
}

/// s515 review 4: the book side of the match-time margin check.
#[cfg(test)]
mod taker_margin_limit_tests {
    use super::*;

    fn fp(n: i64) -> FixedPoint {
        FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
    }

    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    fn order(is_buy: bool, price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy,
            price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    fn market_sell(qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            order_type: OrderType::Market,
            time_in_force: TimeInForce::IOC,
            ..order(false, fp(1), qty)
        }
    }

    /// Default 20x (no tiers).
    fn limit(budget: FixedPoint, hold_price: Option<FixedPoint>) -> TakerMarginLimit {
        TakerMarginLimit {
            budget,
            tiers: None,
            hold_price,
        }
    }

    fn filled(r: &PlaceResult) -> FixedPoint {
        r.fills.iter().fold(FixedPoint::ZERO, |a, f| a + f.quantity)
    }

    /// Bid 100 x 4, lot 0.5: each unit at 100 costs 5; a budget of 12 fits
    /// 2.4 units, cut down to the lot multiple 2.0 (2.5 would cost 12.5).
    #[test]
    fn cut_to_the_largest_lot_multiple_that_fits() {
        let mut b = OrderBook::new(1, fp(1), FixedPoint::from_raw(FixedPoint::SCALE / 2));
        b.place_order(order(true, fp(100), fp(4)), addr(1), 1);
        let r = b.place_order_with_margin(market_sell(fp(4)), addr(2), 2, Some(&limit(fp(12), None)));
        assert_eq!(filled(&r), fp(2));
        assert_eq!(r.status, OrderStatus::Cancelled);
        assert_eq!(r.rested_qty, FixedPoint::ZERO);
        assert_eq!(b.best_bid(), Some(fp(100)));
        assert_eq!(b.orders_for_trader(&addr(1))[0].remaining_qty, fp(2));
    }

    /// Bids 100 x 2 and 90 x 2, budget 12: the 100 level fits (10), not even
    /// one unit at 90 does (10 + 4.5 > 12) — matching stops, the 90 level is
    /// untouched (no zero-quantity fill).
    #[test]
    fn stops_before_a_level_it_cannot_afford() {
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, fp(100), fp(2)), addr(1), 1);
        b.place_order(order(true, fp(90), fp(2)), addr(3), 1);
        let r = b.place_order_with_margin(market_sell(fp(4)), addr(2), 2, Some(&limit(fp(12), None)));
        assert_eq!(r.fills.len(), 1);
        assert_eq!(filled(&r), fp(2));
        assert_eq!(r.status, OrderStatus::Cancelled);
        assert_eq!(b.orders_for_trader(&addr(3))[0].remaining_qty, fp(2));
    }

    /// GTC sell 4 @50 against bid 100 x 1, budget 16: filling 1 costs 5 plus
    /// the 7.5 hold of the 3 left — it fits, is not exhausted, and the rest
    /// RESTS normally. Against bid 100 x 4 it stops after 2 and cancels.
    #[test]
    fn resting_remainder_hold_counts_and_rests_when_margin_suffices() {
        let lim = limit(fp(16), Some(fp(50)));
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, fp(100), fp(1)), addr(1), 1);
        let r = b.place_order_with_margin(order(false, fp(50), fp(4)), addr(2), 2, Some(&lim));
        assert_eq!(filled(&r), fp(1));
        assert_eq!(r.status, OrderStatus::PartiallyFilled);
        assert_eq!(r.rested_qty, fp(3));

        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, fp(100), fp(4)), addr(1), 1);
        let r = b.place_order_with_margin(order(false, fp(50), fp(4)), addr(2), 2, Some(&lim));
        assert_eq!(filled(&r), fp(2));
        assert_eq!(r.status, OrderStatus::Cancelled);
        assert_eq!(r.rested_qty, FixedPoint::ZERO);
        assert!(b.orders_for_trader(&addr(2)).is_empty());
    }

    /// FOK: all-or-nothing within the margin, too.
    #[test]
    fn fok_rejected_whole_when_its_complete_fill_does_not_fit() {
        let mut p = order(false, fp(50), fp(4));
        p.time_in_force = TimeInForce::FOK;
        for (budget, fills) in [(fp(19), 0), (fp(20), 4)] {
            let mut b = OrderBook::new(1, fp(1), fp(1));
            b.place_order(order(true, fp(100), fp(4)), addr(1), 1);
            let r = b.place_order_with_margin(p.clone(), addr(2), 2, Some(&limit(budget, None)));
            assert_eq!(filled(&r), fp(fills), "budget {budget}");
        }
    }

    /// A fill whose notional overflows i128 is never affordable — no panic;
    /// the taker takes what it can compute and stops.
    #[test]
    fn overflowing_fill_notional_stops_without_panic() {
        let huge = FixedPoint::from_raw(10i128.pow(25) * FixedPoint::SCALE);
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, huge, fp(1_000_000)), addr(1), 1);
        let lim = limit(FixedPoint::MAX, None);
        let r = b.place_order_with_margin(market_sell(fp(1_000_000)), addr(2), 2, Some(&lim));
        assert!(filled(&r) > FixedPoint::ZERO && filled(&r) < fp(1_000_000));
        assert_eq!(r.status, OrderStatus::Cancelled);
    }

    /// Book with bid 100 x 30 (addr 1) and the taker (addr 2) at signed
    /// position `taker_pos` (policed via the book's position map).
    fn book_with_taker_pos(taker_pos: FixedPoint) -> OrderBook {
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, fp(100), fp(30)), addr(1), 1);
        let mut pos = ReduceOnlyPositions::new();
        pos.insert(addr(2), taker_pos);
        b.set_reduce_only_positions(pos);
        b
    }

    /// F2 (s515 review 5): the part of a fill that reduces the taker's
    /// opposite-side position needs no margin (Hyperliquid). Long 20, sell 30
    /// at 100: 20 free, the 10 that open a short cost 5 each — budget 50 fits
    /// all 30, budget 49 fits 20 + 9.
    #[test]
    fn closing_part_of_the_fills_is_free_the_opening_rest_is_charged() {
        for (budget, fills) in [(fp(50), 30), (fp(49), 29)] {
            let mut b = book_with_taker_pos(fp(20));
            let r = b.place_order_with_margin(market_sell(fp(30)), addr(2), 2, Some(&limit(budget, None)));
            assert_eq!(filled(&r), fp(fills), "budget {budget}");
        }
        // Budget 0: closes the whole long, opens nothing.
        let mut b = book_with_taker_pos(fp(20));
        let r = b.place_order_with_margin(market_sell(fp(30)), addr(2), 2, Some(&limit(FixedPoint::ZERO, None)));
        assert_eq!(filled(&r), fp(20));
        assert_eq!(r.status, OrderStatus::Cancelled);
    }

    /// Only the OPPOSITE side is free: a short taker selling pays for every
    /// unit, and a taker absent from the position map is charged as before.
    #[test]
    fn increasing_or_unknown_position_is_charged_in_full() {
        let mut b = book_with_taker_pos(-fp(20));
        let r = b.place_order_with_margin(market_sell(fp(30)), addr(2), 2, Some(&limit(fp(49), None)));
        assert_eq!(filled(&r), fp(9));
        let mut b = book_with_taker_pos(fp(20));
        b.clear_reduce_only_positions();
        let r = b.place_order_with_margin(market_sell(fp(30)), addr(2), 2, Some(&limit(fp(49), None)));
        assert_eq!(filled(&r), fp(9));
    }

    /// FOK: the free closing part does not count against its budget either.
    #[test]
    fn fok_closing_part_is_free() {
        let mut p = market_sell(fp(30));
        p.order_type = OrderType::Limit;
        p.price = fp(100);
        p.time_in_force = TimeInForce::FOK;
        for (budget, fills) in [(fp(50), 30), (fp(49), 0)] {
            let mut b = book_with_taker_pos(fp(20));
            let r = b.place_order_with_margin(p.clone(), addr(2), 2, Some(&limit(budget, None)));
            assert_eq!(filled(&r), fp(fills), "budget {budget}");
        }
    }

    /// GTC: closing fills are free, the resting remainder keeps its hold.
    /// GTC sell 30 @50 against bid 100 x 30, budget 75 = its placement
    /// reservation IM(50 x 30). Flat: one fill at 100 already exceeds it
    /// (IM(100 + 50 x 29) = 77.5), nothing fills. Long 10: the 10 closing
    /// fills are free (the hold drops to IM(50 x 20) = 50); each further unit
    /// adds 100 filled and drops 50 held, IM(50 x 20 + 50k) <= 75 -> k <= 10:
    /// 20 fill and the rest is cancelled.
    #[test]
    fn gtc_closing_fills_are_free_and_the_hold_still_counts() {
        for (pos, fills) in [(FixedPoint::ZERO, 0), (fp(10), 20)] {
            let mut b = book_with_taker_pos(pos);
            let r = b.place_order_with_margin(
                order(false, fp(50), fp(30)),
                addr(2),
                2,
                Some(&limit(fp(75), Some(fp(50)))),
            );
            assert_eq!(filled(&r), fp(fills), "pos {pos}");
            assert_eq!(r.status, OrderStatus::Cancelled, "pos {pos}");
        }
    }

    /// F4 (s515 review 5): fills and the resting hold are charged as ONE
    /// notional at ITS tier (monotone in the fill size, so the binary search
    /// is exact; charging them apart undercharged across a tier boundary).
    /// Tiers: <= 100 at 10x, above at 5x. GTC sell 3 @50 vs bid 100 x 3,
    /// budget 30 = IM(150): one fill makes the order 100 filled + 100 held
    /// = IM(200) = 40 > 30 (apart: 10 + 10 = 20 would have fit).
    #[test]
    fn fills_and_hold_are_charged_at_their_combined_tier() {
        let tiers: std::sync::Arc<[crate::margin::MarginTier]> = std::sync::Arc::from(vec![
            crate::margin::MarginTier { max_notional: fp(100), max_leverage: 10 },
            crate::margin::MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 },
        ]);
        let lim = TakerMarginLimit { budget: fp(30), tiers: Some(tiers), hold_price: Some(fp(50)) };
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, fp(100), fp(3)), addr(1), 1);
        let r = b.place_order_with_margin(order(false, fp(50), fp(3)), addr(2), 2, Some(&lim));
        assert_eq!(filled(&r), FixedPoint::ZERO);
        assert_eq!(r.status, OrderStatus::Cancelled);
    }
    fn fp_c(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * (FixedPoint::SCALE / 100))
    }

    fn tiers_20_then_5() -> std::sync::Arc<[crate::margin::MarginTier]> {
        std::sync::Arc::from(vec![
            crate::margin::MarginTier { max_notional: fp(1_000), max_leverage: 20 },
            crate::margin::MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 },
        ])
    }

    fn with_account(
        b: &mut OrderBook,
        t: Address,
        free: FixedPoint,
        px: FixedPoint,
        tiers: Option<std::sync::Arc<[crate::margin::MarginTier]>>,
    ) {
        let mut am = AccountMargins::new(tiers);
        am.insert(t, free, px);
        b.set_account_margins(am);
    }

    fn market_buy(qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            order_type: OrderType::Market,
            time_in_force: TimeInForce::IOC,
            ..order(true, fp(1_000), qty)
        }
    }

    /// F1 (s517): an increase is charged at the POSITION's tier. Long 10
    /// valued at 100 (IM 50 at 20x), asks 100 x 5, market buy 5 with
    /// reservation 25: unit k makes the position 1,000 + 100k at 5x → +170 …
    /// +250. Free 225 (budget 250) fits all 5; free 224 fits 4 (+230).
    /// Per-order margin (IM(100k) <= 25) filled 5 in both.
    #[test]
    fn f1_increase_is_charged_at_the_position_tier() {
        for (free, fills) in [(fp(225), 5), (fp(224), 4)] {
            let mut b = OrderBook::new(1, fp(1), fp(1));
            b.place_order(order(false, fp(100), fp(5)), addr(1), 1);
            let mut pos = ReduceOnlyPositions::new();
            pos.insert(addr(2), fp(10));
            b.set_reduce_only_positions(pos);
            with_account(&mut b, addr(2), free, fp(100), Some(tiers_20_then_5()));
            let lim = TakerMarginLimit { budget: fp(25), tiers: Some(tiers_20_then_5()), hold_price: None };
            let r = b.place_order_with_margin(market_buy(fp(5)), addr(2), 2, Some(&lim));
            assert_eq!(filled(&r), fp(fills), "free {free}");
        }
    }

    /// F1 (s517): closing releases the position's IM for the flip. Long 20
    /// valued at 100 (IM 100); budget = 0.5 reservation + free −50.5 = −50:
    /// sell 30 — 20 close, 10 open short (IM 50): 50 − 100 = −50 fits → 30.
    /// Free −51.5: 29 (45 − 100). Per-order margin (budget 0.5) filled only 20.
    #[test]
    fn f1_closing_releases_position_im_for_the_flip() {
        for (free, fills) in [(fp_c(-5050), 30), (fp_c(-5150), 29)] {
            let mut b = book_with_taker_pos(fp(20));
            with_account(&mut b, addr(2), free, fp(100), None);
            let r = b.place_order_with_margin(market_sell(fp(30)), addr(2), 2, Some(&limit(fp_c(50), None)));
            assert_eq!(filled(&r), fp(fills), "free {free}");
        }
    }

    /// F1 (s517): one sender's takers in one book share a RUNNING free margin
    /// (the batch's exclusive pool). Free 98: sell A (reservation 1) fills 19
    /// (IM 95 <= 99) and leaves 4; sell B (reservation 1) is short 19 at the
    /// book's last price 100 and fills 1 more (5 <= 1 + 4). Free ends at 0.
    #[test]
    fn f1_running_free_is_shared_by_one_senders_takers() {
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, fp(100), fp(40)), addr(1), 1);
        let mut pos = ReduceOnlyPositions::new();
        pos.insert(addr(2), FixedPoint::ZERO);
        b.set_reduce_only_positions(pos);
        with_account(&mut b, addr(2), fp(98), FixedPoint::ZERO, None);
        let r1 = b.place_order_with_margin(market_sell(fp(20)), addr(2), 2, Some(&limit(fp(1), None)));
        let r2 = b.place_order_with_margin(market_sell(fp(20)), addr(2), 3, Some(&limit(fp(1), None)));
        assert_eq!((filled(&r1), filled(&r2)), (fp(19), fp(1)));
        assert_eq!(b.account_margins().get(&addr(2)).unwrap().free, FixedPoint::ZERO);
    }

    /// F1 (s517): a purely closing fill always fits, even with the account
    /// under water (HL: reducing needs no margin).
    #[test]
    fn f1_closing_fills_fit_an_under_margined_account() {
        let mut b = book_with_taker_pos(fp(20));
        with_account(&mut b, addr(2), -fp(500), fp(100), None);
        let r = b.place_order_with_margin(market_sell(fp(20)), addr(2), 2, Some(&limit(FixedPoint::ZERO, None)));
        assert_eq!(filled(&r), fp(20));
    }

    /// F1 (s517) FOK: the complete fill is judged by the same need (closing
    /// releases IM): free −50.5 → the flip fits (−50 <= −50); free −51.5 →
    /// rejected whole even though the fill lowers the need (it opens 10 short).
    #[test]
    fn f1_fok_uses_the_account_need() {
        let mut p = market_sell(fp(30));
        p.order_type = OrderType::Limit;
        p.price = fp(100);
        p.time_in_force = TimeInForce::FOK;
        for (free, fills) in [(fp_c(-5050), 30), (fp_c(-5150), 0)] {
            let mut b = book_with_taker_pos(fp(20));
            with_account(&mut b, addr(2), free, fp(100), None);
            let r = b.place_order_with_margin(p.clone(), addr(2), 2, Some(&limit(fp_c(50), None)));
            assert_eq!(filled(&r), fp(fills), "free {free}");
        }
    }

    /// F1 (Correction s517, T4 write-back): a resting remainder KEEPS its
    /// reservation (`reserve(hold, rested)`), so it must not flow back into
    /// the running free margin. Flat, free 100, GTC sell 10 @100 against no
    /// bids (reservation 50): it rests whole, free stays 100 (was 150).
    #[test]
    fn f1_resting_remainder_keeps_its_reservation_out_of_running_free() {
        let mut b = OrderBook::new(1, fp(1), fp(1));
        let mut pos = ReduceOnlyPositions::new();
        pos.insert(addr(2), FixedPoint::ZERO);
        b.set_reduce_only_positions(pos);
        with_account(&mut b, addr(2), fp(100), FixedPoint::ZERO, None);
        let r = b.place_order_with_margin(order(false, fp(100), fp(10)), addr(2), 2, Some(&limit(fp(50), Some(fp(100)))));
        assert_eq!(r.rested_qty, fp(10));
        assert_eq!(b.account_margins().get(&addr(2)).unwrap().free, fp(100));
    }

    struct Src(Vec<(Address, MakerAccount)>);
    impl MakerAccountSource for Src {
        fn maker_account(&self, maker: &Address, _market_id: MarketId) -> MakerAccount {
            self.0.iter().find(|(a, _)| a == maker).map(|(_, m)| *m).unwrap_or(MakerAccount {
                free: fp(1_000_000),
                signed_pos: FixedPoint::ZERO,
                px: FixedPoint::ZERO,
            })
        }
    }

    fn acct(free: FixedPoint, signed_pos: FixedPoint, px: FixedPoint) -> MakerAccount {
        MakerAccount { free, signed_pos, px }
    }

    /// F1 (s517 #4): bid 10 @100 of an under-water maker (free −95) cannot
    /// take the fill (IM 50 − its share 50 = 0 > −95): it is cancelled whole
    /// (`margin_cancels`, so its reservation is released) and the taker
    /// fills the next bid. With free 0 it fills.
    #[test]
    fn f1_under_margined_maker_is_cancelled_and_the_taker_moves_on() {
        for (free, cancelled) in [(-fp(95), true), (FixedPoint::ZERO, false)] {
            let mut b = OrderBook::new(1, fp(1), fp(1));
            let id = b.place_order(order(true, fp(100), fp(10)), addr(1), 1).order_id;
            b.place_order(order(true, fp(99), fp(10)), addr(3), 1);
            let src = Src(vec![(addr(1), acct(free, FixedPoint::ZERO, FixedPoint::ZERO))]);
            let r = b.place_order_with_accounts(market_sell(fp(10)), addr(2), 2, None, Some(&src));
            assert_eq!(filled(&r), fp(10));
            let maker = if cancelled { addr(3) } else { addr(1) };
            assert!(r.fills.iter().all(|f| f.maker == maker), "free {free}");
            if cancelled {
                assert_eq!(
                    r.margin_cancels,
                    vec![ReduceOnlyCut { order_id: id, trader: addr(1), price: fp(100), qty: fp(10) }]
                );
                assert!(b.orders_for_trader(&addr(1)).is_empty());
            } else {
                assert!(r.margin_cancels.is_empty());
            }
        }
    }

    /// F1 (s517 #4): a maker fill that only CLOSES its position always fits.
    #[test]
    fn f1_closing_maker_fill_is_never_cancelled() {
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, fp(100), fp(10)), addr(1), 1);
        let src = Src(vec![(addr(1), acct(-fp(1_000), -fp(10), fp(100)))]);
        let r = b.place_order_with_accounts(market_sell(fp(10)), addr(2), 2, None, Some(&src));
        assert_eq!(filled(&r), fp(10));
        assert!(r.margin_cancels.is_empty());
    }

    /// F1 (s517 #4) FOK: the pre-check skips the maker matching would
    /// cancel. FOK sell 10 @99 fills 10 from addr 3 (addr 1 cancelled); FOK
    /// sell 20 @99 is rejected and changes nothing (addr 1 still rests).
    #[test]
    fn f1_fok_precheck_mirrors_maker_cancels() {
        for (qty, fills) in [(10, 10), (20, 0)] {
            let mut b = OrderBook::new(1, fp(1), fp(1));
            b.place_order(order(true, fp(100), fp(10)), addr(1), 1);
            b.place_order(order(true, fp(99), fp(10)), addr(3), 1);
            let src = Src(vec![(addr(1), acct(-fp(95), FixedPoint::ZERO, FixedPoint::ZERO))]);
            let mut p = order(false, fp(99), fp(qty));
            p.time_in_force = TimeInForce::FOK;
            let r = b.place_order_with_accounts(p, addr(2), 2, None, Some(&src));
            assert_eq!(filled(&r), fp(fills), "qty {qty}");
            assert_eq!(b.orders_for_trader(&addr(1)).is_empty(), qty == 10, "qty {qty}");
        }
    }

    /// Review fix 2 (s517): floor rounding alone must not cancel a maker.
    /// Default 20x (no tier change). Maker long 1 valued at 100 + 19 raw
    /// (notional ≡ 19 mod 20 raw), bid 1 @(100 + 1 raw) filled whole:
    /// floor((B + x)/20) − floor(B/20) = floor(x/20) + 1 while its share is
    /// floor(x/20) — a +1 raw delta that used to cancel it at free 0.
    #[test]
    fn f1_maker_fill_rounding_does_not_cancel_at_free_zero() {
        let one = FixedPoint::from_raw(1);
        let mut b = OrderBook::new(1, one, one);
        let px = FixedPoint::from_raw(fp(100).raw() + 1);
        b.place_order(order(true, px, fp(1)), addr(1), 1);
        let src = Src(vec![(addr(1), acct(FixedPoint::ZERO, fp(1), FixedPoint::from_raw(fp(100).raw() + 19)))]);
        let r = b.place_order_with_accounts(market_sell(fp(1)), addr(2), 2, None, Some(&src));
        assert_eq!(filled(&r), fp(1));
        assert!(r.margin_cancels.is_empty());
    }

    /// B2 (s87): the bench's at-limit case. T short 1 valued at 100 + 19
    /// raw (notional ≡ 19 mod 20 raw) sells q = 1 + 1 raw @101 into a bid at
    /// exactly 101: x = 101 q ≡ 1 mod 20 raw, so its need IM(A + x) − IM(A)
    /// is floor(x/20) + 1 raw while its budget (its reservation) is
    /// floor(x/20). `(taker_only, tif, account free)` → filled: a taker-only
    /// budget (D2 non-pool market) gets the makers' +1 raw rounding
    /// allowance (GTC and the FOK pre-check); a shared account at free 0 and
    /// a taker-only one already below 0 do not.
    #[test]
    fn b2_taker_only_rounding_does_not_cancel_at_the_limit() {
        let one = FixedPoint::from_raw(1);
        let q = fp(1) + one;
        let px = fp(101);
        let reserve = FixedPoint::from_raw((px * q).raw() / 20);
        for (taker_only, tif, free, fills) in [
            (true, TimeInForce::GTC, FixedPoint::ZERO, true),
            (true, TimeInForce::FOK, FixedPoint::ZERO, true),
            (true, TimeInForce::GTC, -one, false),
            (false, TimeInForce::GTC, FixedPoint::ZERO, false),
            (false, TimeInForce::FOK, FixedPoint::ZERO, false),
        ] {
            let mut b = OrderBook::new(1, fp(1), fp(1));
            b.place_order(order(true, px, q), addr(1), 1);
            let mut pos = ReduceOnlyPositions::new();
            pos.insert(addr(2), -fp(1));
            b.set_reduce_only_positions(pos);
            let mut am = AccountMargins::new(None);
            let entry = FixedPoint::from_raw(fp(100).raw() + 19);
            if taker_only {
                am.insert_taker_only(addr(2), entry);
                am.set_free(&addr(2), free);
            } else {
                am.insert(addr(2), free, entry);
            }
            b.set_account_margins(am);
            let p = PlaceOrderParams { time_in_force: tif, ..order(false, px, q) };
            let hold = (tif == TimeInForce::GTC).then_some(px);
            let r = b.place_order_with_margin(p, addr(2), 2, Some(&limit(reserve, hold)));
            let what = format!("taker_only={taker_only} {tif:?} free={free}");
            assert_eq!(filled(&r), if fills { q } else { FixedPoint::ZERO }, "{what}");
            if fills {
                // The forgiven raw unit does not leak into the running free.
                assert_eq!(b.account_margins().get(&addr(2)).unwrap().free, free, "{what}");
            }
        }
    }

    /// B2 (s87): the allowance is per taker, like the makers' per fill, and
    /// the forgiven unit never leaks into the running free: a second such
    /// sell of the same sender in the same taker-only book fills too.
    #[test]
    fn b2_taker_only_rounding_allowance_is_per_taker() {
        let one = FixedPoint::from_raw(1);
        let px = fp(101);
        let entry = FixedPoint::from_raw(fp(100).raw() + 19);
        let im = |n: FixedPoint| crate::margin::order_initial_margin(None, n);
        let (q1, q2) = (fp(1) + one, fp(1) + one + one);
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, px, q1 + q2), addr(1), 1);
        let mut pos = ReduceOnlyPositions::new();
        pos.insert(addr(2), -fp(1));
        b.set_reduce_only_positions(pos);
        let mut am = AccountMargins::new(None);
        am.insert_taker_only(addr(2), entry);
        b.set_account_margins(am);
        for (id, size0, q) in [(2u64, fp(1), q1), (3, fp(2) + one, q2)] {
            // Precondition: each sell's need is exactly its reservation + 1 raw.
            let (a, x) = (size0 * entry, px * q);
            assert_eq!(im(a + x) - im(a), im(x) + one, "sell {id}");
            let r = b.place_order_with_margin(order(false, px, q), addr(2), id, Some(&limit(im(x), Some(px))));
            assert_eq!(filled(&r), q, "sell {id}");
            assert_eq!(b.account_margins().get(&addr(2)).unwrap().free, FixedPoint::ZERO, "sell {id}");
        }
    }
}
