//! Position tracking and PnL computation (task 2.2.5).
//!
//! Stores positions in CF_NATIVE_POSITIONS and native balances in CF_NATIVE_BALANCES.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};

use alloy_primitives::U256;
use borsh::{BorshDeserialize, BorshSerialize};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::{StateBackend, StateDb};
use torus_types::{Address, FixedPoint, MarketId};

use crate::error::CoreError;

// ============================================================================
// Borsh helpers for FixedPoint and Address
// ============================================================================

pub(crate) fn borsh_write_fp<W: Write>(val: &FixedPoint, w: &mut W) -> io::Result<()> {
    w.write_all(&val.raw().to_be_bytes())
}

pub(crate) fn borsh_read_fp<R: Read>(r: &mut R) -> io::Result<FixedPoint> {
    let mut buf = [0u8; 16];
    r.read_exact(&mut buf)?;
    Ok(FixedPoint::from_raw(i128::from_be_bytes(buf)))
}

pub(crate) fn borsh_write_address<W: Write>(addr: &Address, w: &mut W) -> io::Result<()> {
    w.write_all(addr.as_slice())
}

pub(crate) fn borsh_read_address<R: Read>(r: &mut R) -> io::Result<Address> {
    let mut buf = [0u8; 20];
    r.read_exact(&mut buf)?;
    Ok(Address::new(buf))
}

// ============================================================================
// Types
// ============================================================================

/// Margin mode for a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarginType {
    Cross = 0,
    Isolated = 1,
}

/// A trader's position in a specific perpetual market.
#[derive(Clone, Debug)]
pub struct Position {
    pub trader: Address,
    pub market_id: MarketId,
    pub is_long: bool,
    pub size: FixedPoint,
    /// Average entry price (decisions and display). The fill price on an
    /// open or a flip, `cost_basis / size` (truncated) after an increase,
    /// unchanged by a partial close. Money is computed from `cost_basis`.
    pub entry_price: FixedPoint,
    /// s100 item 2 (design A): the exact cost of the open size, Σ of the
    /// fills' notionals (`price × qty`, one rounding per fill) less the
    /// pro-rata cost of what was closed. UPnL = mark × size − cost_basis
    /// (long), cost_basis − mark × size (short). Only `fill_transition`
    /// writes it, together with `entry_price`.
    pub cost_basis: FixedPoint,
    pub realized_pnl: FixedPoint,
    /// Margin allocated to this position (>0 for isolated, 0 for cross).
    pub isolated_margin: FixedPoint,
    pub margin_type: MarginType,
}

impl Position {
    /// Unrealized PnL at a given mark price: `mark × size − cost_basis` for a
    /// long, `cost_basis − mark × size` for a short (one rounding: the
    /// product).
    pub fn unrealized_pnl(&self, mark_price: FixedPoint) -> FixedPoint {
        let value = mark_price * self.size;
        if self.is_long {
            value - self.cost_basis
        } else {
            self.cost_basis - value
        }
    }

    /// Notional value at mark price.
    pub fn notional(&self, mark_price: FixedPoint) -> FixedPoint {
        self.size * mark_price
    }
}

/// AUDIT FIX ECON-FIND-31: Schema version byte prepended to Position serialization.
/// BREAKING CHANGE (acceptable pre-launch): existing serialized Positions are
/// incompatible and must be re-created.
/// s100 item 2: v2 adds `cost_basis` after `entry_price` (111 bytes). A v1
/// row has no cost basis and is refused (fresh genesis), never guessed.
const POSITION_SCHEMA_VERSION: u8 = 2;

/// Serialized size of a v2 [`Position`].
pub const POSITION_V2_LEN: usize = 111;

impl BorshSerialize for Position {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&[POSITION_SCHEMA_VERSION])?;
        borsh_write_address(&self.trader, w)?;
        w.write_all(&self.market_id.to_be_bytes())?;
        w.write_all(&[u8::from(self.is_long)])?;
        borsh_write_fp(&self.size, w)?;
        borsh_write_fp(&self.entry_price, w)?;
        borsh_write_fp(&self.cost_basis, w)?;
        borsh_write_fp(&self.realized_pnl, w)?;
        borsh_write_fp(&self.isolated_margin, w)?;
        w.write_all(&[self.margin_type as u8])?;
        Ok(())
    }
}

impl BorshDeserialize for Position {
    fn deserialize_reader<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut ver = [0u8; 1];
        r.read_exact(&mut ver)?;
        if ver[0] != POSITION_SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported Position schema version: {} (expected {})",
                    ver[0], POSITION_SCHEMA_VERSION
                ),
            ));
        }
        let trader = borsh_read_address(r)?;
        let mut mb = [0u8; 8];
        r.read_exact(&mut mb)?;
        let market_id = u64::from_be_bytes(mb);
        let mut lb = [0u8; 1];
        r.read_exact(&mut lb)?;
        let is_long = lb[0] != 0;
        let size = borsh_read_fp(r)?;
        let entry_price = borsh_read_fp(r)?;
        let cost_basis = borsh_read_fp(r)?;
        let realized_pnl = borsh_read_fp(r)?;
        let isolated_margin = borsh_read_fp(r)?;
        let mut mt = [0u8; 1];
        r.read_exact(&mut mt)?;
        let margin_type = if mt[0] == 0 {
            MarginType::Cross
        } else {
            MarginType::Isolated
        };
        Ok(Self {
            trader,
            market_id,
            is_long,
            size,
            entry_price,
            cost_basis,
            realized_pnl,
            isolated_margin,
            margin_type,
        })
    }
}

/// Native trading balance (separate from EVM balance).
#[derive(Clone, Debug)]
pub struct NativeBalance {
    pub available: FixedPoint,
    pub order_margin: FixedPoint,
}

impl Default for NativeBalance {
    fn default() -> Self {
        Self {
            available: FixedPoint::ZERO,
            order_margin: FixedPoint::ZERO,
        }
    }
}

/// AUDIT FIX ECON-FIND-31: Schema version byte prepended to NativeBalance serialization.
/// BREAKING CHANGE (acceptable pre-launch): existing serialized NativeBalances are
/// incompatible and must be re-created.
const NATIVE_BALANCE_SCHEMA_VERSION: u8 = 1;

impl BorshSerialize for NativeBalance {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&[NATIVE_BALANCE_SCHEMA_VERSION])?;
        borsh_write_fp(&self.available, w)?;
        borsh_write_fp(&self.order_margin, w)?;
        Ok(())
    }
}

impl BorshDeserialize for NativeBalance {
    fn deserialize_reader<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut ver = [0u8; 1];
        r.read_exact(&mut ver)?;
        if ver[0] != NATIVE_BALANCE_SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported NativeBalance schema version: {} (expected {})",
                    ver[0], NATIVE_BALANCE_SCHEMA_VERSION
                ),
            ));
        }
        let available = borsh_read_fp(r)?;
        let order_margin = borsh_read_fp(r)?;
        Ok(Self {
            available,
            order_margin,
        })
    }
}

// ============================================================================
// Keys
// ============================================================================

/// Position key: trader(20) + market_id(8) = 28 bytes.
pub fn position_key(trader: &Address, market_id: MarketId) -> [u8; 28] {
    let mut key = [0u8; 28];
    key[..20].copy_from_slice(trader.as_slice());
    key[20..28].copy_from_slice(&market_id.to_be_bytes());
    key
}

/// Cumulative-volume key: `b"cvlm"`(4) + trader(20) = 24 bytes, in
/// `CF_NATIVE_BALANCES` next to the 20-byte balance rows (same precedent as
/// the insurance-fund row). Value: raw `FixedPoint`, 16 bytes BE.
pub fn cum_volume_key(trader: &Address) -> [u8; 24] {
    let mut key = [0u8; 24];
    key[..4].copy_from_slice(b"cvlm");
    key[4..].copy_from_slice(trader.as_slice());
    key
}

// ============================================================================
// Open-order limit (Hyperliquid model)
// ============================================================================

/// Open orders every user may hold, summed over all markets.
pub const OPEN_ORDER_BASE_LIMIT: u32 = 1000;
/// One more open order per this much lifetime traded notional (quote units).
pub const OPEN_ORDER_VOLUME_STEP: i128 = 5_000_000;
/// Hard cap on the open-order limit.
pub const OPEN_ORDER_MAX_LIMIT: u32 = 5000;

/// `min(1000 + floor(cum_volume / 5M), 5000)`.
pub fn open_order_limit(cum_volume: FixedPoint) -> u32 {
    let steps = cum_volume.raw().max(0) / (OPEN_ORDER_VOLUME_STEP * FixedPoint::SCALE);
    let extra = steps.min(i128::from(OPEN_ORDER_MAX_LIMIT - OPEN_ORDER_BASE_LIMIT));
    OPEN_ORDER_BASE_LIMIT + extra as u32
}

// ============================================================================
// PositionManager — CRUD for positions and native balances
// ============================================================================

#[derive(Clone)]
pub struct PositionManager<T: StateBackend = StateDb> {
    state: T,
}

impl<T: StateBackend> PositionManager<T> {
    pub fn new(state: T) -> Self {
        Self { state }
    }

    pub fn state(&self) -> &T {
        &self.state
    }

    // ---- Position CRUD ----

    pub fn get_position(
        &self,
        trader: &Address,
        market_id: MarketId,
    ) -> Result<Option<Position>, CoreError> {
        let key = position_key(trader, market_id);
        match self.state.get_cf_raw(CF_NATIVE_POSITIONS, &key)? {
            Some(data) => Ok(Some(
                Position::try_from_slice(&data).map_err(|e| CoreError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    pub fn put_position(&self, pos: &Position) -> Result<(), CoreError> {
        let key = position_key(&pos.trader, pos.market_id);
        // Capacity hint matches the fixed v2 layout; Vec can still grow if the
        // codec changes. Avoid retaining borsh::to_vec's 1 KiB starter buffer.
        let mut data = Vec::with_capacity(POSITION_V2_LEN);
        pos.serialize(&mut data).map_err(|e| CoreError::Borsh(e.to_string()))?;
        self.state.put_cf_raw_owned(CF_NATIVE_POSITIONS, &key, data)?;
        Ok(())
    }

    pub fn delete_position(&self, trader: &Address, market_id: MarketId) -> Result<(), CoreError> {
        let key = position_key(trader, market_id);
        self.state.delete_cf_raw(CF_NATIVE_POSITIONS, &key)?;
        Ok(())
    }

    /// All positions for a trader (prefix scan).
    pub fn positions_for_trader(&self, trader: &Address) -> Result<Vec<Position>, CoreError> {
        let entries = self
            .state
            .iterate_cf(CF_NATIVE_POSITIONS, Some(trader.as_slice()))?;
        let mut out = Vec::with_capacity(entries.len());
        for (_key, value) in entries {
            out.push(
                Position::try_from_slice(&value).map_err(|e| CoreError::Borsh(e.to_string()))?,
            );
        }
        Ok(out)
    }

    // ---- Native balance CRUD ----

    pub fn get_native_balance(&self, trader: &Address) -> Result<NativeBalance, CoreError> {
        match self
            .state
            .get_cf_raw(CF_NATIVE_BALANCES, trader.as_slice())?
        {
            Some(data) => Ok(NativeBalance::try_from_slice(&data)
                .map_err(|e| CoreError::Borsh(e.to_string()))?),
            None => Ok(NativeBalance::default()),
        }
    }

    pub fn put_native_balance(
        &self,
        trader: &Address,
        bal: &NativeBalance,
    ) -> Result<(), CoreError> {
        let mut data = Vec::with_capacity(33); // fixed v1 layout, growable hint
        bal.serialize(&mut data).map_err(|e| CoreError::Borsh(e.to_string()))?;
        self.state
            .put_cf_raw_owned(CF_NATIVE_BALANCES, trader.as_slice(), data)?;
        Ok(())
    }

    /// Lifetime traded notional (maker + taker) of `trader`; zero if absent.
    pub fn get_cum_volume(&self, trader: &Address) -> Result<FixedPoint, CoreError> {
        match self
            .state
            .get_cf_raw(CF_NATIVE_BALANCES, &cum_volume_key(trader))?
        {
            Some(data) => {
                let raw: [u8; 16] = data.as_slice().try_into().map_err(|_| {
                    CoreError::Borsh(format!("cum_volume row has {} bytes, expected 16", data.len()))
                })?;
                Ok(FixedPoint::from_raw(i128::from_be_bytes(raw)))
            }
            None => Ok(FixedPoint::ZERO),
        }
    }

    pub fn put_cum_volume(&self, trader: &Address, volume: FixedPoint) -> Result<(), CoreError> {
        self.state.put_cf_raw(
            CF_NATIVE_BALANCES,
            &cum_volume_key(trader),
            &volume.raw().to_be_bytes(),
        )?;
        Ok(())
    }

    // ---- Fill application ----

    /// Update position after a fill. Handles open/increase/reduce/close/flip.
    ///
    /// Reads and writes the backend directly (one position read-modify-write
    /// plus, on any close component, one balance read-modify-write). Batch
    /// callers that settle many fills should use [`apply_fill_cached`]
    /// (C1) — identical transition semantics via [`fill_transition`], but the
    /// row round-trips hit an in-memory [`PositionCache`] instead.
    ///
    /// [`apply_fill_cached`]: Self::apply_fill_cached
    pub fn apply_fill(
        &self,
        trader: &Address,
        market_id: MarketId,
        is_buy: bool,
        fill_qty: FixedPoint,
        fill_price: FixedPoint,
        margin_type: MarginType,
    ) -> Result<FillEffect, CoreError> {
        let existing = self.get_position(trader, market_id)?;
        let start_size = signed_size(&existing);
        let (new_pos, pnl) =
            fill_transition(existing, trader, market_id, is_buy, fill_qty, fill_price, margin_type);
        match new_pos {
            Some(pos) => self.put_position(&pos)?,
            // `fill_transition` yields None only on a full close of an
            // existing position, so the delete always targets a live row.
            None => self.delete_position(trader, market_id)?,
        }
        if let Some(pnl) = pnl {
            self.credit_realized_pnl(trader, pnl)?;
        }
        Ok(FillEffect { start_size, closed_pnl: pnl })
    }

    /// C1: `apply_fill`, but the position read-modify-write goes through a
    /// write-back [`PositionCache`] and NO balance is touched — the realized
    /// PnL (if the fill had a close component) is returned for the caller to
    /// credit through whatever balance authority it maintains (the executor's
    /// BalanceCache). `Some(pnl)` is emitted exactly when the classic path
    /// would have called its internal PnL credit — including `Some(ZERO)` for
    /// a flat close, which still materializes the trader's balance row.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_fill_cached(
        &self,
        cache: &mut PositionCache,
        trader: &Address,
        market_id: MarketId,
        is_buy: bool,
        fill_qty: FixedPoint,
        fill_price: FixedPoint,
        margin_type: MarginType,
    ) -> Result<Option<FixedPoint>, CoreError> {
        self.apply_fill_cached_effect(
            cache, trader, market_id, is_buy, fill_qty, fill_price, margin_type,
        )
        .map(|e| e.closed_pnl)
    }

    /// [`apply_fill_cached`], also reporting the trader's signed position
    /// size before the fill (s80 userFills `startPosition`). Same transition,
    /// same cache writes, and no balance is touched.
    ///
    /// [`apply_fill_cached`]: Self::apply_fill_cached
    #[allow(clippy::too_many_arguments)]
    pub fn apply_fill_cached_effect(
        &self,
        cache: &mut PositionCache,
        trader: &Address,
        market_id: MarketId,
        is_buy: bool,
        fill_qty: FixedPoint,
        fill_price: FixedPoint,
        margin_type: MarginType,
    ) -> Result<FillEffect, CoreError> {
        cache.update_in_place(self, trader, market_id, |existing| {
            let start_size = signed_size(&existing);
            let (new_pos, pnl) = fill_transition(
                existing,
                trader,
                market_id,
                is_buy,
                fill_qty,
                fill_price,
                margin_type,
            );
            (
                new_pos,
                FillEffect {
                    start_size,
                    closed_pnl: pnl,
                },
            )
        })
    }

    /// Credit (or debit if negative) realized PnL to native balance.
    fn credit_realized_pnl(&self, trader: &Address, pnl: FixedPoint) -> Result<(), CoreError> {
        let mut bal = self.get_native_balance(trader)?;
        bal.available += pnl;
        self.put_native_balance(trader, &bal)?;
        Ok(())
    }
}

// ============================================================================
// Fill transition (pure) + PositionCache (C1)
// ============================================================================

/// What one fill did to one trader's position (s80 userFills stream).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FillEffect {
    /// Signed position size before the fill (long > 0, short < 0, none = 0).
    pub start_size: FixedPoint,
    /// Realized PnL of the fill's close component (None = no close part).
    pub closed_pnl: Option<FixedPoint>,
}

// Hand-written: `FixedPoint` has no `Default`.
impl Default for FillEffect {
    fn default() -> Self {
        Self { start_size: FixedPoint::ZERO, closed_pnl: None }
    }
}

/// Signed size of an optional position (long > 0, short < 0, none = 0).
fn signed_size(p: &Option<Position>) -> FixedPoint {
    match p {
        Some(p) if p.is_long => p.size,
        Some(p) => FixedPoint::ZERO - p.size,
        None => FixedPoint::ZERO,
    }
}

/// Pure fill transition shared by [`PositionManager::apply_fill`] and
/// [`PositionManager::apply_fill_cached`] so the two paths cannot drift.
///
/// Returns `(new_position, pnl_event)`:
/// - `new_position`: `Some(pos)` to store, `None` when the fill fully closed
///   an existing position (row must be deleted). A `None` input (no existing
///   position) always opens, so `None` output implies the input was `Some`.
/// - `pnl_event`: `Some(pnl)` iff the fill had a close component (partial
///   close, full close, or flip) — exactly the cases where the classic path
///   credited realized PnL to the trader's native balance. `Some(ZERO)` is
///   meaningful: the credit of zero still creates/rewrites the balance row.
fn fill_transition(
    existing: Option<Position>,
    trader: &Address,
    market_id: MarketId,
    is_buy: bool,
    fill_qty: FixedPoint,
    fill_price: FixedPoint,
    margin_type: MarginType,
) -> (Option<Position>, Option<FixedPoint>) {
    // s100 item 2 (design A): ONE rounding per fill. Both sides of a fill
    // call with the same price and qty, so they book the same product and
    // the value sum (balances + Σ UPnL at one price) is conserved exactly.
    let notional = fill_price * fill_qty;
    let open = |size: FixedPoint, cost_basis: FixedPoint| Position {
        trader: *trader,
        market_id,
        is_long: is_buy,
        size,
        entry_price: fill_price,
        cost_basis,
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type,
    };
    match existing {
        None => (Some(open(fill_qty, notional)), None),
        Some(mut pos) => {
            let same_dir = (is_buy && pos.is_long) || (!is_buy && !pos.is_long);
            // PnL of closing cost `removed` for `value` (the close's notional).
            let is_long = pos.is_long;
            let close_pnl = |value: FixedPoint, removed: FixedPoint| {
                if is_long {
                    value - removed
                } else {
                    removed - value
                }
            };

            if same_dir {
                // Increase: the basis adds the notional; entry = basis / size.
                let new_size = pos.size + fill_qty;
                pos.cost_basis += notional;
                if new_size > FixedPoint::ZERO {
                    pos.entry_price = pos.cost_basis / new_size;
                }
                pos.size = new_size;
                (Some(pos), None)
            } else if fill_qty < pos.size {
                // Partial close: remove the closed part's pro-rata cost.
                let removed = pro_rata(pos.cost_basis, fill_qty, pos.size);
                let pnl = close_pnl(notional, removed);
                pos.cost_basis -= removed;
                pos.realized_pnl += pnl;
                pos.size -= fill_qty;
                (Some(pos), Some(pnl))
            } else if fill_qty == pos.size {
                // Full close: the whole basis.
                (None, Some(close_pnl(notional, pos.cost_basis)))
            } else {
                // Flip: split the notional by subtraction, so the close and
                // open parts add up to exactly the counterparty's notional.
                let remainder = fill_qty - pos.size;
                let open_part = fill_price * remainder;
                let pnl = close_pnl(notional - open_part, pos.cost_basis);
                (Some(open(remainder, open_part)), Some(pnl))
            }
        }
    }
}

/// `basis × q / size`, truncated toward zero, in one rounding. `q < size`
/// (a partial close) keeps `|result| < |basis|`, so it fits.
fn pro_rata(basis: FixedPoint, q: FixedPoint, size: FixedPoint) -> FixedPoint {
    if let Some(p) = basis.raw().checked_mul(q.raw()) {
        return FixedPoint::from_raw(p / size.raw());
    }
    let wide = |x: i128| U256::from(x.unsigned_abs());
    let m = (wide(basis.raw()) * wide(q.raw()) / wide(size.raw())).to::<u128>();
    // The caller's q < size keeps m < |basis| <= 2^127: it fits.
    let m = i128::try_from(m).expect("pro_rata: q < size keeps the share below |basis|");
    let negative = (basis.raw() < 0) ^ (q.raw() < 0) ^ (size.raw() < 0);
    FixedPoint::from_raw(if negative { -m } else { m })
}

/// C1: per-batch write-back cache for `Position` rows, mirroring the
/// executor's BalanceCache pattern.
///
/// Phase-4 settlement does two position read-modify-writes per fill; through
/// the `NativeStateOverlay` each get/put takes an `RwLock` and allocates
/// `(String, Vec<u8>)` map keys plus a Borsh round-trip. This cache keeps the
/// working set in a typed in-memory map for the duration of a batch: first
/// reads fall through to the backend, subsequent read-modify-writes are pure
/// map operations, and each *unique* dirty row is written to the backend once
/// by [`flush_all`].
///
/// Entries are `Option<Position>`: `Some` is a live row, `None` is either a
/// cached miss or a tombstone from a full close (`remove`). Tombstoned dirty
/// entries flush as deletes — including open-then-close within one batch,
/// which matches the classic path's put-then-delete (both end in an overlay
/// tombstone for the key).
///
/// Determinism: final backend state is order-independent anyway (distinct
/// keys, last-write-wins), but [`flush_all`] additionally sorts dirty keys by
/// `(trader, market_id)` — exactly the byte order of [`position_key`], since
/// both encode big-endian — so every node issues the identical write
/// sequence.
///
/// Coherence: the cache is only valid while it is the *single* authority for
/// position rows — every in-batch position read/write must go through it, and
/// `flush_all` must run before anything else (next batch, book persistence,
/// block-end flush) reads positions from the backend.
///
/// [`flush_all`]: Self::flush_all
#[derive(Default)]
pub struct PositionCache {
    map: HashMap<(Address, MarketId), Option<Position>>,
    dirty: HashSet<(Address, MarketId)>,
}

impl PositionCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read-through load: cache hit, else fall through to the backend and
    /// memoize (a miss caches `None` so repeat misses stay in-memory too).
    pub fn load<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
        trader: &Address,
        market_id: MarketId,
    ) -> Result<Option<Position>, CoreError> {
        let key = (*trader, market_id);
        if let Some(entry) = self.map.get(&key) {
            return Ok(entry.clone());
        }
        let pos = positions.get_position(trader, market_id)?;
        self.map.insert(key, pos.clone());
        Ok(pos)
    }

    /// The fill path's read-modify-write: [`load`], then `f`, then [`set`]
    /// (`Some`) or [`remove`] (`None`) — with one map lookup and no copy of
    /// the cached row. `f` gets the row by value and its result is written
    /// back to the same slot and marked dirty before returning. A backend
    /// read error on a miss returns before anything is cached, as in
    /// `load`. A panic in `f` leaves the slot empty; no caller keeps using a
    /// cache after a panic (parallel settle drops a panicked market's cache,
    /// and the sequential path unwinds out of the batch that owns it).
    ///
    /// [`load`]: Self::load
    /// [`set`]: Self::set
    /// [`remove`]: Self::remove
    fn update_in_place<T: StateBackend, R>(
        &mut self,
        positions: &PositionManager<T>,
        trader: &Address,
        market_id: MarketId,
        f: impl FnOnce(Option<Position>) -> (Option<Position>, R),
    ) -> Result<R, CoreError> {
        let key = (*trader, market_id);
        let slot = match self.map.entry(key) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(positions.get_position(trader, market_id)?),
        };
        if slot
            .as_ref()
            .is_some_and(|p| (p.trader, p.market_id) != key)
        {
            // A stored row whose fields name another key (`put_position`
            // never writes one): `set` files the result under the fields'
            // key, so keep the exact load + set sequence.
            let (new_pos, out) = f(slot.clone());
            match new_pos {
                Some(pos) => self.set(pos),
                None => self.remove(trader, market_id),
            }
            return Ok(out);
        }
        let (new_pos, out) = f(slot.take());
        // `fill_transition` opens with (trader, market_id) or keeps the
        // row's own fields, which match the key here: same slot as `set`.
        debug_assert!(new_pos
            .as_ref()
            .is_none_or(|p| (p.trader, p.market_id) == key));
        *slot = new_pos;
        self.dirty.insert(key);
        Ok(out)
    }

    /// Store an updated position and mark it dirty (no backend write yet).
    pub fn set(&mut self, pos: Position) {
        let key = (pos.trader, pos.market_id);
        self.map.insert(key, Some(pos));
        self.dirty.insert(key);
    }

    /// Tombstone a fully-closed position (flushes as a delete).
    pub fn remove(&mut self, trader: &Address, market_id: MarketId) {
        let key = (*trader, market_id);
        self.map.insert(key, None);
        self.dirty.insert(key);
    }

    /// C3 (parallel settle): absorb another cache — entries and dirty marks.
    ///
    /// Intended for merging PER-MARKET caches into the batch cache: position
    /// keys are `(trader, market_id)`, so caches built from different markets
    /// have provably disjoint key sets and the merged map is identical to
    /// what one shared cache would hold after sequential settlement — in any
    /// merge order. Callers merge in sorted market order anyway.
    pub fn merge_disjoint(&mut self, other: PositionCache) {
        self.map.extend(other.map);
        self.dirty.extend(other.dirty);
    }

    /// Number of cached entries (live rows, misses and tombstones).
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Reserve room for `additional` more entries (and dirty marks), so a
    /// run of [`merge_disjoint`] calls does not rehash as the map grows.
    /// Capacity only: no entry or iteration order that matters changes
    /// (`flush_all` sorts its keys).
    ///
    /// [`merge_disjoint`]: Self::merge_disjoint
    pub fn reserve(&mut self, additional: usize) {
        self.map.reserve(additional);
        self.dirty.reserve(additional);
    }

    /// Write every dirty row to the backend once, in sorted key order
    /// (deterministic write sequence; see type-level docs). Clean entries
    /// (read-only hits/misses) are untouched. Clears the dirty set.
    pub fn flush_all<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
    ) -> Result<(), CoreError> {
        let mut keys: Vec<(Address, MarketId)> = self.dirty.iter().copied().collect();
        keys.sort();
        for key in keys {
            match self.map.get(&key) {
                Some(Some(pos)) => positions.put_position(pos)?,
                Some(None) => positions.delete_position(&key.0, key.1)?,
                None => {}
            }
        }
        self.dirty.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i128 = FixedPoint::SCALE;

    fn raw(r: i128) -> FixedPoint {
        FixedPoint::from_raw(r)
    }

    /// `p × q`, truncated: the one rounding of a fill.
    fn n(p: i128, q: i128) -> i128 {
        p * q / S
    }

    fn fill(
        pos: Option<Position>,
        is_buy: bool,
        q: i128,
        p: i128,
    ) -> (Option<Position>, Option<FixedPoint>) {
        fill_transition(
            pos,
            &Address::new([7; 20]),
            3,
            is_buy,
            raw(q),
            raw(p),
            MarginType::Cross,
        )
    }

    /// (is_long, size, entry, basis, realized) in raw units.
    fn parts(p: &Position) -> (bool, i128, i128, i128, i128) {
        (
            p.is_long,
            p.size.raw(),
            p.entry_price.raw(),
            p.cost_basis.raw(),
            p.realized_pnl.raw(),
        )
    }

    const P1: i128 = 1_000 * S + 7; // 1,000.00000007
    const Q1: i128 = 33_333_333; // 0.33333333
    const P2: i128 = 1_013 * S + 99_999_991;
    const Q2: i128 = 77_777_777;

    #[test]
    fn open_books_the_notional_at_the_fill_price() {
        let (pos, pnl) = fill(None, true, Q1, P1);
        assert_eq!(parts(&pos.unwrap()), (true, Q1, P1, n(P1, Q1), 0));
        assert_eq!(pnl, None);
    }

    #[test]
    fn increase_adds_the_notional_and_reaverages_the_entry() {
        let pos = fill(None, false, Q1, P1).0;
        let (pos, pnl) = fill(pos, false, Q2, P2);
        let basis = n(P1, Q1) + n(P2, Q2);
        assert_eq!(
            parts(&pos.unwrap()),
            (false, Q1 + Q2, basis * S / (Q1 + Q2), basis, 0)
        );
        assert_eq!(pnl, None);
    }

    #[test]
    fn partial_close_removes_the_pro_rata_basis() {
        for is_long in [true, false] {
            let pos = fill(fill(None, is_long, Q1, P1).0, is_long, Q2, P2)
                .0
                .unwrap();
            let (size, entry, basis) =
                (pos.size.raw(), pos.entry_price.raw(), pos.cost_basis.raw());
            let (q, p) = (12_345_679, 990 * S + 31);
            let removed = basis * q / size;
            let pnl = if is_long {
                n(p, q) - removed
            } else {
                removed - n(p, q)
            };
            let (pos, got) = fill(Some(pos), !is_long, q, p);
            assert_eq!(
                parts(&pos.unwrap()),
                (is_long, size - q, entry, basis - removed, pnl),
                "{is_long}"
            );
            assert_eq!(got, Some(raw(pnl)));
        }
    }

    #[test]
    fn full_close_realizes_notional_against_the_whole_basis() {
        for is_long in [true, false] {
            let pos = fill(fill(None, is_long, Q1, P1).0, is_long, Q2, P2).0;
            let basis = n(P1, Q1) + n(P2, Q2);
            let p = 1_001 * S + 3;
            let (pos, pnl) = fill(pos, !is_long, Q1 + Q2, p);
            assert!(pos.is_none(), "{is_long}: row deleted");
            let want = if is_long {
                n(p, Q1 + Q2) - basis
            } else {
                basis - n(p, Q1 + Q2)
            };
            assert_eq!(pnl, Some(raw(want)), "{is_long}");
        }
    }

    /// The close and open parts add up to exactly the fill's one notional.
    #[test]
    fn flip_splits_the_notional_by_subtraction() {
        for is_long in [true, false] {
            let pos = fill(None, is_long, Q1, P1).0;
            let (q, p) = (Q1 + Q2, 1_020 * S + 55_555_555);
            let open_part = n(p, Q2);
            let close_part = n(p, q) - open_part;
            let want = if is_long {
                close_part - n(P1, Q1)
            } else {
                n(P1, Q1) - close_part
            };
            let (pos, pnl) = fill(pos, !is_long, q, p);
            assert_eq!(
                parts(&pos.unwrap()),
                (!is_long, Q2, p, open_part, 0),
                "{is_long}"
            );
            assert_eq!(pnl, Some(raw(want)), "{is_long}");
        }
    }

    /// Value moved by one fill: the buyer gives exactly the notional, the
    /// seller gets it, whatever each side's branch (here: a flip vs an open).
    #[test]
    fn both_sides_of_a_fill_move_the_same_notional() {
        // value at price 0 = cash + (−basis long, +basis short)
        let v = |pos: &Option<Position>, cash: FixedPoint| match pos {
            Some(p) if p.is_long => cash - p.cost_basis,
            Some(p) => cash + p.cost_basis,
            None => cash,
        };
        let (q, p) = (Q1 + Q2, 1_020 * S + 55_555_555);
        let short = fill(None, false, Q1, P1).0;
        let before = v(&short, FixedPoint::ZERO);
        let (flipped, pnl) = fill(short, true, q, p);
        let (opened, _) = fill(None, false, q, p);
        assert_eq!(v(&flipped, pnl.unwrap()) - before, raw(-n(p, q)), "buyer");
        assert_eq!(v(&opened, FixedPoint::ZERO), raw(n(p, q)), "seller");
    }

    #[test]
    fn pro_rata_is_one_rounding_also_past_i128() {
        assert_eq!(pro_rata(raw(10), raw(1), raw(3)), raw(3));
        let big = 10i128.pow(30) + 1;
        assert!(
            big.checked_mul(10i128.pow(10)).is_none(),
            "takes the wide path"
        );
        assert_eq!(
            pro_rata(raw(big), raw(10i128.pow(10)), raw(3 * 10i128.pow(10))),
            raw(big / 3)
        );
        assert_eq!(
            pro_rata(raw(-big), raw(10i128.pow(10)), raw(3 * 10i128.pow(10))),
            raw(-big / 3),
            "toward zero"
        );
    }

    #[test]
    fn v2_layout_roundtrips_and_v1_is_refused() {
        let pos = Position {
            trader: Address::new([9; 20]),
            market_id: 0x0102_0304_0506_0708,
            is_long: false,
            size: raw(11),
            entry_price: raw(22),
            cost_basis: raw(-33),
            realized_pnl: raw(44),
            isolated_margin: raw(55),
            margin_type: MarginType::Isolated,
        };
        let bytes = borsh::to_vec(&pos).unwrap();
        assert_eq!((bytes.len(), bytes[0]), (POSITION_V2_LEN, 2));
        assert_eq!(
            &bytes[62..78],
            &(-33i128).to_be_bytes(),
            "cost_basis right after entry_price"
        );
        let back = Position::try_from_slice(&bytes).unwrap();
        assert_eq!(
            (
                back.trader,
                back.market_id,
                parts(&back),
                back.isolated_margin,
                back.margin_type
            ),
            (
                pos.trader,
                pos.market_id,
                parts(&pos),
                pos.isolated_margin,
                pos.margin_type
            )
        );
        // v1: no cost basis (95 bytes). Refused, never decoded with a guess.
        let v1: Vec<u8> = [&[1u8][..], &bytes[1..62], &bytes[78..]].concat();
        assert_eq!(v1.len(), 95);
        let err = Position::try_from_slice(&v1).unwrap_err().to_string();
        assert!(
            err.contains("unsupported Position schema version: 1"),
            "{err}"
        );
        assert!(
            Position::try_from_slice(&bytes[..POSITION_V2_LEN - 1]).is_err(),
            "truncated"
        );
    }
}
