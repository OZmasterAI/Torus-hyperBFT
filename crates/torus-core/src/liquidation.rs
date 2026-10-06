//! Item 3: Hyperliquid-style liquidation — the pure parts and the state
//! primitives (`docs/plans/liquidation.md`). The block step that drives them
//! lives in the bridge (`liquidation_step.rs`).
//!
//! No liquidation penalty, no insurance fund, no socialized loss (decision 6):
//! size only ever moves through a fill between two accounts ([`transfer`]), so
//! Σ long size == Σ short size per market after every step, and collateral
//! only ever moves between accounts (value is conserved).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use alloy_primitives::aliases::U1024;
use torus_state::cf::{CF_NATIVE_LIQUIDATION, CF_NATIVE_POSITIONS};
use torus_state::StateBackend;
use torus_types::{Address, FixedPoint, MarketId};

use crate::error::CoreError;
use crate::margin::{effective_max_leverage, AccountView, MarginTier, DEFAULT_ORDER_MAX_LEVERAGE};
use crate::position::{MarginType, Position, PositionManager};

// ============================================================================
// Constants
// ============================================================================

/// Item 3 (decision 7): the liquidator vault — a fixed protocol account (no
/// known key: no signed action can come from it). Deposits: later branch.
pub const LIQUIDATOR_VAULT: Address = Address::new(*b"torus-liquidator-vlt");
/// adl-budget P2 (owner s96): the ADL escrows — protocol accounts (no known
/// key) that take a bankrupt account's positions at their ADL price in the
/// bankruptcy block, one per side so opposite obligations never net. Never
/// classified, never ADL candidates, excluded from funding (adl-budget §8).
pub const ADL_ESCROW_LONG: Address = Address::new(*b"torus-adl-escrow-lng");
pub const ADL_ESCROW_SHORT: Address = Address::new(*b"torus-adl-escrow-sht");

/// The escrow that takes a bankrupt position of side `is_long`.
pub fn adl_escrow(is_long: bool) -> Address {
    if is_long {
        ADL_ESCROW_LONG
    } else {
        ADL_ESCROW_SHORT
    }
}

pub fn is_adl_escrow(a: &Address) -> bool {
    *a == ADL_ESCROW_LONG || *a == ADL_ESCROW_SHORT
}

/// HL: positions above this notional (at the mark) are liquidated in chunks.
/// Raw units (`FixedPoint::from_raw` is not `const`).
pub const CHUNK_NOTIONAL_THRESHOLD_RAW: i128 = 100_000 * FixedPoint::SCALE;
/// HL: 20% of the position per chunk = size / 5.
pub const CHUNK_DIVISOR: i128 = 5;
/// HL: seconds of block time after a chunk during which every stage-1 order
/// of the account is for the entire position (s88 parity fix).
pub const CHUNK_COOLDOWN_SECS: u64 = 30;
/// D5 (decided, user s517): accounts valued per block.
pub const LIQ_SCAN_PER_BLOCK: usize = 2_048;
/// D5 (decided, user s517): accounts acted on per block.
pub const LIQ_ACT_PER_BLOCK: usize = 64;
/// adl-budget Q3: the drain's work units per block — traders examined by a
/// ranking + rows visited + candidates read (+ edge rows). Chosen (A8) so an
/// HL-sized event (a few hundred account-markets) closes the escrows in its
/// own block; one constant. A8 (measured, `ubench_adl` HL mode: N = 5,000
/// traders, 3 accounts x 100 markets, one side): U_hl = 500,800 units;
/// W = max(1.25 U_hl, 500,900) rounded up to 10,000 (adl-budget.md §9).
pub const ADL_WORK_PER_BLOCK: u64 = 630_000;
/// Rows per seek of the bounded walks.
const SCAN_PAGE: usize = 1_024;
/// `CF_NATIVE_LIQUIDATION` tags (0x01 unused / reserved: no account index, C1).
pub const COOLDOWN_TAG: u8 = 0x02;
pub const PREV_MARK_TAG: u8 = 0x03;
pub const CURSOR_KEY: [u8; 1] = [0x04];
/// Review M2 (s517): `0x06 ‖ trader` — the account was still under MM after
/// its last liquidation action (keeps the step due; 0x05 is reserved).
pub const PENDING_TAG: u8 = 0x06;
/// P2 (s96): `0x07 ‖ height(8) ‖ market(8) ‖ side(1: 1 long) ‖ trader(20)` ->
/// `size raw (16, BE) ‖ price raw (16, BE)`: what the escrow of `is_long`
/// still owes for `trader`'s position taken at block `height` at `price`
/// (per account: H1's clamp).
pub const ADL_OBLIGATION_TAG: u8 = 0x07;

// ============================================================================
// Pure parts
// ============================================================================

/// The class of an account (design *Classification*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// `AV >= MM`.
    Healthy,
    /// `2/3 MM <= AV < MM`: reduce-only market orders into the book.
    Stage1,
    /// `0 <= AV < 2/3 MM`: positions + collateral to the vault at the mark.
    Backstop,
    /// `AV < 0`: auto-deleverage against ranked counterparties.
    Adl,
}

/// Decisions 2, 4, 5: classify on exact raw units. `None` on overflow (the
/// caller skips the account — never a panic).
pub fn classify(v: &AccountView) -> Option<Health> {
    let av = v
        .available
        .checked_add(v.order_margin)
        .ok()?
        .checked_add(v.upnl)
        .ok()?;
    let mm = v.maintenance;
    Some(if av >= mm {
        Health::Healthy
    } else if av < FixedPoint::ZERO {
        Health::Adl
    } else if av.raw().checked_mul(3)? < mm.raw().checked_mul(2)? {
        Health::Backstop
    } else {
        Health::Stage1
    })
}

/// D2: the stage-1 order size and whether it is a chunk. Notional at the mark
/// above 100,000 (or overflowing) ⇒ 20% of the size (`raw / 5`), unless that
/// is below the book's lot (then the whole size).
pub fn stage1_qty(size: FixedPoint, mark: FixedPoint, lot: FixedPoint) -> (FixedPoint, bool) {
    let big = size
        .checked_mul(mark)
        .map_or(true, |n| n.raw() > CHUNK_NOTIONAL_THRESHOLD_RAW);
    if !big {
        return (size, false);
    }
    let q = FixedPoint::from_raw(size.raw() / CHUNK_DIVISOR);
    if q < lot {
        (size, false)
    } else {
        (q, true)
    }
}

/// D1: the price cap of a liquidation order = `mark ∓ mark / (2 × lev)`, `lev`
/// = the max leverage of the position's tier (the position's MM rate).
/// Saturating at the i128 bounds (no panic on any input).
pub fn slippage_cap(
    tiers: Option<&[MarginTier]>,
    mark: FixedPoint,
    notional: FixedPoint,
    is_buy: bool,
) -> FixedPoint {
    let lev = tiers
        .map_or(DEFAULT_ORDER_MAX_LEVERAGE, |t| effective_max_leverage(t, notional))
        .max(1);
    let d = mark.raw() / (2 * i128::from(lev));
    FixedPoint::from_raw(if is_buy {
        mark.raw().saturating_add(d)
    } else {
        mark.raw().saturating_sub(d)
    })
}

/// `ceil(a / b)` for `b > 0` (Rust `/` truncates toward zero, which is
/// already the ceiling for a negative quotient).
fn ceil_div(a: i128, b: i128) -> i128 {
    let q = a / b;
    if a % b != 0 && a > 0 {
        q + 1
    } else {
        q
    }
}

/// Review H1 (user decision s517): the BANKRUPTCY price of a position — the
/// close price at which its account ends at exactly 0, given `rest` =
/// collateral (available + order margin) + the UPnL of the account's OTHER
/// positions. Long: `entry − rest / size`; short: `entry + rest / size`.
/// Exact integer math, rounded AGAINST the bankrupt trader (a long's price
/// down, a short's up: `ceil(rest × SCALE / size)` off the entry), so a close
/// at this price never leaves the account positive (realized PnL truncates
/// toward zero, i.e. up for a loss, and still stays `<= −rest`). `None` on
/// overflow or a non-positive size.
pub fn bankruptcy_price(
    rest: FixedPoint,
    is_long: bool,
    size: FixedPoint,
    entry: FixedPoint,
) -> Option<FixedPoint> {
    if size.raw() <= 0 {
        return None;
    }
    let per = ceil_div(rest.raw().checked_mul(FixedPoint::SCALE)?, size.raw());
    let p = if is_long {
        entry.raw().checked_sub(per)?
    } else {
        entry.raw().checked_add(per)?
    };
    Some(FixedPoint::from_raw(p))
}

/// Review H1: the ADL close price — `px` (the previous mark, else the mark)
/// CLAMPED to the bankruptcy price on the side unfavourable to the bankrupt
/// account (a long sells at no more than it, a short buys at no less), so
/// the account never keeps value its counterparties paid for; when `px` is
/// already worse, the account ends negative and the deficit goes to the
/// vault. Without a usable bankruptcy price (overflow, `<= 0`) the mark
/// stands in for it.
pub fn adl_price(
    px: FixedPoint,
    bankruptcy: Option<FixedPoint>,
    mark: FixedPoint,
    is_long: bool,
) -> FixedPoint {
    let b = bankruptcy.filter(|b| b.raw() > 0).unwrap_or(mark);
    if is_long {
        px.min(b)
    } else {
        px.max(b)
    }
}

/// An ADL counterparty: an opposite-side position in the ADL market.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdlCandidate {
    pub trader: Address,
    pub is_long: bool,
    pub size: FixedPoint,
    pub entry_price: FixedPoint,
    /// The counterparty's account value (ranking only — C7).
    pub account_value: FixedPoint,
}

/// `|x|` of a raw value clamped at 0, widened.
fn wide(x: i128) -> U1024 {
    U1024::from(x.max(0) as u128)
}

/// Decision 5 (HL): rank = (mark/entry for a long, entry/mark for a short) ×
/// (notional at the mark / account value), DESCENDING, exact — compared by
/// cross-multiplication in U1024 (each side is a product of <= 5 values of
/// <= 127 bits: never overflows). Candidates with AV <= 0 (or a zero
/// denominator) rank last; ties by address ascending. Stable and total.
pub fn adl_rank(mark: FixedPoint, mut cands: Vec<AdlCandidate>) -> Vec<AdlCandidate> {
    // (num, den) with den == 0 meaning "rank last".
    let key = |c: &AdlCandidate| -> (U1024, U1024) {
        let notional = wide(c.size.raw()) * wide(mark.raw());
        let (px_num, px_den) = if c.is_long {
            (mark.raw(), c.entry_price.raw())
        } else {
            (c.entry_price.raw(), mark.raw())
        };
        let den = if c.account_value.raw() <= 0 {
            U1024::ZERO
        } else {
            wide(px_den) * wide(c.account_value.raw())
        };
        (wide(px_num) * notional, den)
    };
    let mut keyed: Vec<((U1024, U1024), AdlCandidate)> =
        cands.drain(..).map(|c| (key(&c), c)).collect();
    keyed.sort_by(|((an, ad), a), ((bn, bd), b)| {
        let by_rank = match (ad.is_zero(), bd.is_zero()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            // descending a = an/ad: b·… vs a·…
            (false, false) => (*bn * *ad).cmp(&(*an * *bd)),
        };
        by_rank.then_with(|| a.trader.cmp(&b.trader))
    });
    keyed.into_iter().map(|(_, c)| c).collect()
}

// ============================================================================
// Transfer primitives (the only ways liquidation moves size / collateral)
// ============================================================================

/// One fill between two accounts: `from` closes `qty` of its position in `m`
/// at `price`, `to` takes the same side. Σ long == Σ short is preserved.
pub fn transfer<T: StateBackend>(
    pm: &PositionManager<T>,
    from: &Address,
    to: &Address,
    m: MarketId,
    qty: FixedPoint,
    price: FixedPoint,
) -> Result<(), CoreError> {
    let from_long = pm
        .get_position(from, m)?
        .ok_or_else(|| CoreError::InvalidInput(format!("liquidation: {from} has no position in {m}")))?
        .is_long;
    pm.apply_fill(from, m, !from_long, qty, price, MarginType::Cross)?;
    pm.apply_fill(to, m, from_long, qty, price, MarginType::Cross)?;
    Ok(())
}

/// Move `from`'s whole collateral (`available + order_margin`, any sign) to
/// `to`'s available: afterwards `from.available + from.order_margin == 0`.
/// Returns the amount moved.
pub fn move_collateral<T: StateBackend>(
    pm: &PositionManager<T>,
    from: &Address,
    to: &Address,
) -> Result<FixedPoint, CoreError> {
    let of = |_| CoreError::Overflow("liquidation collateral overflows i128".into());
    let mut fb = pm.get_native_balance(from)?;
    let c = fb.available.checked_add(fb.order_margin).map_err(of)?;
    if c == FixedPoint::ZERO {
        return Ok(c);
    }
    let mut tb = pm.get_native_balance(to)?;
    fb.available = fb.available.checked_sub(c).map_err(of)?;
    tb.available = tb.available.checked_add(c).map_err(of)?;
    pm.put_native_balance(from, &fb)?;
    pm.put_native_balance(to, &tb)?;
    Ok(c)
}

/// Decision 4 (backstop): every MARKED position (ascending market) moves to
/// `vault` at its mark, then the remaining collateral. Review H2 (user
/// decision s517): a position whose market has no mark stays with the trader.
pub fn backstop<T: StateBackend>(
    pm: &PositionManager<T>,
    trader: &Address,
    vault: &Address,
    mark: impl Fn(MarketId) -> Option<FixedPoint>,
) -> Result<(), CoreError> {
    for p in pm.positions_for_trader(trader)? {
        let Some(px) = mark(p.market_id) else { continue };
        transfer(pm, trader, vault, p.market_id, p.size, px)?;
    }
    move_collateral(pm, trader, vault)?;
    Ok(())
}

/// Decision 5 (ADL): close at most `qty` of `u`'s position in `m` at `price`
/// against `ranked` in order, `q = min(remaining, candidate size)` each (a
/// candidate whose position vanished or changed side is skipped). Returns
/// `(closes, next, read)`: the closes in order, `(counterparty, size)` (the
/// caller logs them); `next` = how many leading candidates are used up
/// (vanished, flipped or fully closed: adl-budget Q2, the drain's cache
/// position); `read` = how many candidates were read (Q3 work units).
pub fn adl_close<T: StateBackend>(
    pm: &PositionManager<T>,
    u: &Address,
    m: MarketId,
    price: FixedPoint,
    qty: FixedPoint,
    ranked: &[AdlCandidate],
) -> Result<(Vec<(Address, FixedPoint)>, usize, usize), CoreError> {
    let mut closes = Vec::new();
    let (mut next, mut read) = (0usize, 0usize);
    let Some(up) = pm.get_position(u, m)? else {
        return Ok((closes, next, read));
    };
    let mut remaining = up.size.min(qty);
    for c in ranked {
        if remaining <= FixedPoint::ZERO {
            break;
        }
        read += 1;
        // Used up: no later row of the block needs this candidate again.
        let used_up = c.trader == *u
            || match pm.get_position(&c.trader, m)? {
                Some(cp) if cp.is_long != up.is_long && cp.size > FixedPoint::ZERO => {
                    let q = remaining.min(cp.size);
                    transfer(pm, u, &c.trader, m, q, price)?;
                    closes.push((c.trader, q));
                    remaining -= q;
                    q == cp.size
                }
                _ => true,
            };
        if used_up && next + 1 == read {
            next += 1;
        }
    }
    Ok((closes, next, read))
}

/// P2 edge (real holders exhausted): escrow long sells `q` at `p_long`, escrow
/// short buys `q` at `p_short`. Two prices realize (p_long - p_short) x q more
/// than one shared price would; the vault pays it, so value is conserved.
/// Returns the vault's change (+ = credited). Never flips or opens an escrow
/// position (18c review): q <= 0, or an escrow not holding at least q on its
/// own side, is an error and nothing is written.
pub fn cross_close<T: StateBackend>(
    pm: &PositionManager<T>,
    m: MarketId,
    q: FixedPoint,
    p_long: FixedPoint,
    p_short: FixedPoint,
    vault: &Address,
) -> Result<FixedPoint, CoreError> {
    let of = |_| CoreError::Overflow("adl cross close overflows i128".into());
    for (e, is_long) in [(ADL_ESCROW_LONG, true), (ADL_ESCROW_SHORT, false)] {
        let holds = pm.get_position(&e, m)?.is_some_and(|p| p.is_long == is_long && p.size >= q);
        if q <= FixedPoint::ZERO || !holds {
            return Err(CoreError::InvalidInput(format!("adl cross close: {e} cannot close {q:?} in {m}")));
        }
    }
    pm.apply_fill(&ADL_ESCROW_LONG, m, false, q, p_long, MarginType::Cross)?;
    pm.apply_fill(&ADL_ESCROW_SHORT, m, true, q, p_short, MarginType::Cross)?;
    let paid = p_short.checked_sub(p_long).map_err(of)?.checked_mul(q).map_err(of)?;
    let mut vb = pm.get_native_balance(vault)?;
    vb.available = vb.available.checked_add(paid).map_err(of)?;
    pm.put_native_balance(vault, &vb)?;
    Ok(paid)
}

/// Q1 (s96): ADL counterparties = every position on side `want_long` of
/// `traders` (ascending, each once; the escrows skipped), `get` reading a
/// trader's position in the ADL market, `av` valuing a holder (ranking only,
/// C7). The ranking's work units are `traders.len()` (Q3).
pub fn adl_candidates(
    traders: &[Address],
    want_long: bool,
    mut get: impl FnMut(&Address) -> Result<Option<Position>, CoreError>,
    mut av: impl FnMut(&Address) -> Result<FixedPoint, CoreError>,
) -> Result<Vec<AdlCandidate>, CoreError> {
    let mut out = Vec::new();
    for t in traders.iter().filter(|t| !is_adl_escrow(t)) {
        let Some(p) = get(t)? else { continue };
        if p.is_long != want_long || p.size <= FixedPoint::ZERO {
            continue;
        }
        out.push(AdlCandidate {
            trader: *t,
            is_long: p.is_long,
            size: p.size,
            entry_price: p.entry_price,
            account_value: av(t)?,
        });
    }
    Ok(out)
}

/// D9: a FLAT account's negative collateral moves to `vault` (conserved, never
/// written off). Returns the moved amount (ZERO when nothing moved: the
/// account holds positions, its collateral is >= 0, or it is the vault).
pub fn settle_flat_deficit<T: StateBackend>(
    pm: &PositionManager<T>,
    t: &Address,
    vault: &Address,
) -> Result<FixedPoint, CoreError> {
    if t == vault || !pm.positions_for_trader(t)?.is_empty() {
        return Ok(FixedPoint::ZERO);
    }
    let b = pm.get_native_balance(t)?;
    let c = b
        .available
        .checked_add(b.order_margin)
        .map_err(|_| CoreError::Overflow("liquidation collateral overflows i128".into()))?;
    if c >= FixedPoint::ZERO {
        return Ok(FixedPoint::ZERO);
    }
    move_collateral(pm, t, vault)
}

// ============================================================================
// Candidate walk (C1) and CF_NATIVE_LIQUIDATION rows
// ============================================================================

/// The smallest key above every `t ‖ market` key and at most the next
/// trader's first key: `t ‖ ff×8 ‖ 00`.
fn past_trader(t: &Address) -> Vec<u8> {
    [t.as_slice(), &[0xff; 8], &[0x00]].concat()
}

/// C1: candidates = distinct traders of `CF_NATIVE_POSITIONS` (keys `trader ‖
/// market`, sorted by trader), ascending, strictly after `after`, at most
/// `limit`. Review H3: one bounded seek per trader (`iterate_cf_from` with
/// limit 1, then skip past the trader's rows) — never a whole-CF load.
pub fn traders_after<T: StateBackend>(
    state: &T,
    after: Option<Address>,
    limit: usize,
) -> Result<Vec<Address>, CoreError> {
    let mut out: Vec<Address> = Vec::new();
    let mut start = after.as_ref().map_or_else(Vec::new, past_trader);
    while out.len() < limit {
        let Some((k, _)) = state.iterate_cf_from(CF_NATIVE_POSITIONS, &start, 1)?.pop() else {
            break;
        };
        if k.len() != 28 {
            start = [k.as_slice(), &[0u8]].concat();
            continue;
        }
        let t = Address::from_slice(&k[..20]);
        out.push(t);
        start = past_trader(&t);
    }
    Ok(out)
}

fn malformed(what: &str) -> CoreError {
    CoreError::InvalidInput(format!("malformed liquidation row: {what}"))
}

fn cooldown_key(t: &Address) -> [u8; 21] {
    let mut k = [0u8; 21];
    k[0] = COOLDOWN_TAG;
    k[1..].copy_from_slice(t.as_slice());
    k
}

fn prev_mark_key(m: MarketId) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[0] = PREV_MARK_TAG;
    k[1..].copy_from_slice(&m.to_be_bytes());
    k
}

fn pending_key(t: &Address) -> [u8; 21] {
    let mut k = [0u8; 21];
    k[0] = PENDING_TAG;
    k[1..].copy_from_slice(t.as_slice());
    k
}

/// Review M2: mark (`true`) / clear (`false`) `t` as still under MM after its
/// liquidation action — writes only when the row changes; returns whether
/// it did.
pub fn set_pending<T: StateBackend>(state: &T, t: &Address, on: bool) -> Result<bool, CoreError> {
    let k = pending_key(t);
    let exists = state.get_cf_raw(CF_NATIVE_LIQUIDATION, &k)?.is_some();
    match (on, exists) {
        (true, false) => state.put_cf_raw(CF_NATIVE_LIQUIDATION, &k, &[1])?,
        (false, true) => state.delete_cf_raw(CF_NATIVE_LIQUIDATION, &k)?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// Telemetry (node-local, read-only): the number of rows of `tag` (the
/// pending rows `0x06`, the ADL obligation rows `0x07`) — paged prefix seeks,
/// nothing collected.
pub fn tag_count<T: StateBackend>(state: &T, tag: u8) -> Result<u64, CoreError> {
    let (mut n, mut start) = (0u64, vec![tag]);
    loop {
        let page = state.iterate_cf_prefix_from(CF_NATIVE_LIQUIDATION, &[tag], &start, SCAN_PAGE)?;
        n += page.len() as u64;
        match page.last() {
            Some((k, _)) if page.len() == SCAN_PAGE => start = [k.as_slice(), &[0u8]].concat(),
            _ => return Ok(n),
        }
    }
}

/// Telemetry (node-local, read-only): how many of `traders` (ascending) hold
/// a pending row — one paged seek over their key range, the rows of other
/// traders in it skipped.
pub fn pending_among<T: StateBackend>(state: &T, traders: &[Address]) -> Result<u64, CoreError> {
    let (Some(first), Some(last)) = (traders.first(), traders.last()) else {
        return Ok(0);
    };
    let (end, mut start) = (pending_key(last), pending_key(first).to_vec());
    let mut n = 0u64;
    loop {
        let page = state.iterate_cf_prefix_from(CF_NATIVE_LIQUIDATION, &[PENDING_TAG], &start, SCAN_PAGE)?;
        for (k, _) in &page {
            if k.as_slice() > end.as_slice() {
                return Ok(n);
            }
            if k.len() == 21 && traders.binary_search(&Address::from_slice(&k[1..])).is_ok() {
                n += 1;
            }
        }
        match page.last() {
            Some((k, _)) if page.len() == SCAN_PAGE => start = [k.as_slice(), &[0u8]].concat(),
            _ => return Ok(n),
        }
    }
}

/// D3: whether `t` chunked less than [`CHUNK_COOLDOWN_SECS`] of block time ago.
pub fn in_cooldown<T: StateBackend>(state: &T, t: &Address, now: u64) -> Result<bool, CoreError> {
    match state.get_cf_raw(CF_NATIVE_LIQUIDATION, &cooldown_key(t))? {
        None => Ok(false),
        Some(v) => {
            let ts = u64::from_be_bytes(v.as_slice().try_into().map_err(|_| malformed("cooldown"))?);
            Ok(now.saturating_sub(ts) < CHUNK_COOLDOWN_SECS)
        }
    }
}

/// D3: `t` chunked at block time `now`.
pub fn set_cooldown<T: StateBackend>(state: &T, t: &Address, now: u64) -> Result<(), CoreError> {
    state.put_cf_raw(CF_NATIVE_LIQUIDATION, &cooldown_key(t), &now.to_be_bytes())?;
    Ok(())
}

/// Delete `t`'s cooldown row if it exists (no write otherwise).
pub fn clear_cooldown<T: StateBackend>(state: &T, t: &Address) -> Result<(), CoreError> {
    let k = cooldown_key(t);
    if state.get_cf_raw(CF_NATIVE_LIQUIDATION, &k)?.is_some() {
        state.delete_cf_raw(CF_NATIVE_LIQUIDATION, &k)?;
    }
    Ok(())
}

/// Rule H (owner s96, changes D10): `0x03 ‖ m` -> last(16) ‖ [prev(16)]: the
/// market's last usable mark and the mark before it that DIFFERED from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkRow {
    pub last: FixedPoint,
    pub prev: Option<FixedPoint>,
}

/// The rows of `markets` and each marked market's pre-clamp ADL base this
/// step: the old `last` when the mark changed, else `prev` (absent: the
/// caller uses the mark, D10's fallback).
pub fn adl_bases<T: StateBackend>(
    state: &T,
    markets: &[MarketId],
    marks: &BTreeMap<MarketId, FixedPoint>,
) -> Result<(BTreeMap<MarketId, FixedPoint>, BTreeMap<MarketId, MarkRow>), CoreError> {
    let raw = |b: &[u8]| -> Result<FixedPoint, CoreError> {
        Ok(FixedPoint::from_raw(i128::from_be_bytes(b.try_into().map_err(|_| malformed("prev mark"))?)))
    };
    let (mut bases, mut rows) = (BTreeMap::new(), BTreeMap::new());
    for &m in markets {
        let Some(v) = state.get_cf_raw(CF_NATIVE_LIQUIDATION, &prev_mark_key(m))? else { continue };
        let row = match v.len() {
            16 => MarkRow { last: raw(&v)?, prev: None },
            32 => MarkRow { last: raw(&v[..16])?, prev: Some(raw(&v[16..])?) },
            _ => return Err(malformed("prev mark")),
        };
        if let Some(mark) = marks.get(&m) {
            if let Some(b) = if *mark != row.last { Some(row.last) } else { row.prev } {
                bases.insert(m, b);
            }
        }
        rows.insert(m, row);
    }
    Ok((bases, rows))
}

/// Rule H: a listed market with a usable mark that differs from its row's
/// `last` (or has no row) gets (last = mark, prev = old last); an unchanged
/// mark writes nothing; a listed market without a usable mark loses its row
/// (review H1: never a base from before an oracle outage). Returns the writes.
pub fn put_mark_rows<T: StateBackend>(
    state: &T,
    listed: &[MarketId],
    marks: &BTreeMap<MarketId, FixedPoint>,
    rows: &BTreeMap<MarketId, MarkRow>,
) -> Result<usize, CoreError> {
    let mut writes = 0;
    for m in listed {
        let k = prev_mark_key(*m);
        match (marks.get(m), rows.get(m)) {
            (Some(p), Some(r)) if r.last == *p => {}
            (Some(p), r) => {
                let mut v = p.raw().to_be_bytes().to_vec();
                if let Some(r) = r {
                    v.extend_from_slice(&r.last.raw().to_be_bytes());
                }
                state.put_cf_raw(CF_NATIVE_LIQUIDATION, &k, &v)?;
                writes += 1;
            }
            (None, Some(_)) => {
                state.delete_cf_raw(CF_NATIVE_LIQUIDATION, &k)?;
                writes += 1;
            }
            (None, None) => {}
        }
    }
    Ok(writes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Obligation {
    pub height: u64,
    pub market: MarketId,
    pub is_long: bool,
    pub trader: Address,
    pub size: FixedPoint,
    pub price: FixedPoint,
}

impl Obligation {
    /// The row key (see [`ADL_OBLIGATION_TAG`]). Unique per obligation: an
    /// account leaves liquidation flat in its bankruptcy block, so a
    /// re-bankruptcy of the same trader gets a new (later) height — never a
    /// key collision ([`put_obligation`] rejects one).
    pub fn key(&self) -> [u8; 38] {
        let mut k = [0u8; 38];
        k[0] = ADL_OBLIGATION_TAG;
        k[1..9].copy_from_slice(&self.height.to_be_bytes());
        k[9..17].copy_from_slice(&self.market.to_be_bytes());
        k[17] = u8::from(self.is_long);
        k[18..].copy_from_slice(self.trader.as_slice());
        k
    }
}

/// Write `o` as a new row (size > 0) or delete its row (size 0). A new row
/// over an existing key is an error (never an overwrite: see
/// [`Obligation::key`]); so are a negative size (it never vanishes
/// silently) and a price <= 0 (18c review).
pub fn put_obligation<T: StateBackend>(state: &T, o: &Obligation) -> Result<(), CoreError> {
    let k = o.key();
    if o.size < FixedPoint::ZERO || o.price <= FixedPoint::ZERO {
        return Err(CoreError::InvalidInput(format!("adl obligation: negative size or price <= 0: {o:?}")));
    }
    if o.size == FixedPoint::ZERO {
        state.delete_cf_raw(CF_NATIVE_LIQUIDATION, &k)?;
        return Ok(());
    }
    if state.get_cf_raw(CF_NATIVE_LIQUIDATION, &k)?.is_some() {
        return Err(CoreError::InvalidInput(format!("adl obligation row exists: {o:?}")));
    }
    let v = [o.size.raw().to_be_bytes(), o.price.raw().to_be_bytes()].concat();
    state.put_cf_raw(CF_NATIVE_LIQUIDATION, &k, &v)?;
    Ok(())
}

/// adl-budget A6: overwrite the EXISTING row of `o`'s key with `o`'s size and
/// price (the drain's remainder, the pairing partner), or delete it at size
/// 0. A missing row, a negative size or a price <= 0 is an error.
pub fn update_obligation<T: StateBackend>(state: &T, o: &Obligation) -> Result<(), CoreError> {
    let k = o.key();
    if o.size < FixedPoint::ZERO || o.price <= FixedPoint::ZERO {
        return Err(CoreError::InvalidInput(format!("adl obligation: negative size or price <= 0: {o:?}")));
    }
    if state.get_cf_raw(CF_NATIVE_LIQUIDATION, &k)?.is_none() {
        return Err(CoreError::InvalidInput(format!("adl obligation row missing: {o:?}")));
    }
    if o.size == FixedPoint::ZERO {
        state.delete_cf_raw(CF_NATIVE_LIQUIDATION, &k)?;
        return Ok(());
    }
    let v = [o.size.raw().to_be_bytes(), o.price.raw().to_be_bytes()].concat();
    state.put_cf_raw(CF_NATIVE_LIQUIDATION, &k, &v)?;
    Ok(())
}

/// The first obligation at or after `start` (the bare tag, or a key + 0x00).
/// A malformed row (lengths, side byte, size or price <= 0) is an error.
pub fn next_obligation<T: StateBackend>(state: &T, start: &[u8]) -> Result<Option<Obligation>, CoreError> {
    let Some((k, v)) = state.iterate_cf_prefix_from(CF_NATIVE_LIQUIDATION, &[ADL_OBLIGATION_TAG], start, 1)?.pop()
    else {
        return Ok(None);
    };
    if k.len() != 38 || v.len() != 32 || k[17] > 1 {
        return Err(malformed("adl obligation"));
    }
    let raw = |b: &[u8]| i128::from_be_bytes(b.try_into().expect("16 bytes"));
    let (size, price) = (FixedPoint::from_raw(raw(&v[..16])), FixedPoint::from_raw(raw(&v[16..])));
    // Review L1: the writers never store size <= 0 or price <= 0.
    if size <= FixedPoint::ZERO || price <= FixedPoint::ZERO {
        return Err(malformed("adl obligation (size or price <= 0)"));
    }
    Ok(Some(Obligation {
        height: u64::from_be_bytes(k[1..9].try_into().expect("8 bytes")),
        market: MarketId::from_be_bytes(k[9..17].try_into().expect("8 bytes")),
        is_long: k[17] == 1,
        trader: Address::from_slice(&k[18..]),
        size,
        price,
    }))
}

/// D5: the round-robin scan cursor (the last trader scanned by a cut pass).
pub fn cursor<T: StateBackend>(state: &T) -> Result<Option<Address>, CoreError> {
    match state.get_cf_raw(CF_NATIVE_LIQUIDATION, &CURSOR_KEY)? {
        None => Ok(None),
        Some(v) if v.len() == 20 => Ok(Some(Address::from_slice(&v))),
        Some(_) => Err(malformed("cursor")),
    }
}

/// D5: set (`Some`) or delete (`None`) the cursor — no write when unchanged.
pub fn put_cursor<T: StateBackend>(state: &T, c: Option<Address>) -> Result<(), CoreError> {
    if cursor(state)? == c {
        return Ok(());
    }
    match c {
        Some(a) => state.put_cf_raw(CF_NATIVE_LIQUIDATION, &CURSOR_KEY, a.as_slice())?,
        None => state.delete_cf_raw(CF_NATIVE_LIQUIDATION, &CURSOR_KEY)?,
    }
    Ok(())
}
