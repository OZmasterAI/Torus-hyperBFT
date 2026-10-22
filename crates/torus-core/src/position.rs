//! Position tracking and PnL computation (task 2.2.5).
//!
//! Stores positions in CF_NATIVE_POSITIONS and native balances in CF_NATIVE_BALANCES.

use std::collections::hash_map::Entry;
use std::io::{self, Read, Write};

use alloy_primitives::map::{HashMap, HashSet};
use alloy_primitives::U256;
use borsh::{BorshDeserialize, BorshSerialize};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::{OwnPendingRow, StateBackend, StateDb};
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
    /// Funding engine (P0.0 `funding_engine`): the funding state, `Some`
    /// once a fill touched the position while its market had a funding row
    /// (a v3 row). `None` (a v2 row): entry index 0, nothing paid yet.
    pub funding: Option<PositionFunding>,
}

/// Funding engine: a position's lazy-settlement state. Packed (alignment
/// 1): `Option<PositionFunding>` is 33 bytes and fits `Position` in 144
/// bytes (unpacked, the i128 alignment made it 160; every resident position
/// record pays it, `users_size_of_pins`). Fields are read by value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C, packed)]
pub struct PositionFunding {
    /// The market's cumulative index this position last settled at
    /// (`crate::funding::INDEX_SCALE`).
    pub entry_index: i128,
    /// Funding settled in cash since the position opened (positive = paid).
    pub paid: FixedPoint,
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

    /// Funding engine: what this position owes from its entry index to the
    /// market's `index` (positive = it pays; `None` = no funding: 0), rounded
    /// against the holder ([`crate::funding::owed`]).
    pub fn funding_owed(&self, index: Option<i128>) -> FixedPoint {
        let Some(index) = index else {
            return FixedPoint::ZERO;
        };
        let entry = self.funding.map_or(0, |f| f.entry_index);
        crate::funding::owed(self.is_long, self.size, entry, index)
    }

    /// Funding engine: funding since the position opened (settled + owed at
    /// `index`; positive = paid).
    pub fn funding_since_open(&self, index: Option<i128>) -> FixedPoint {
        self.funding.map_or(FixedPoint::ZERO, |f| f.paid) + self.funding_owed(index)
    }

    /// Serialized size: [`POSITION_V2_LEN`] without funding state, else
    /// [`POSITION_V3_LEN`].
    pub fn encoded_len(&self) -> usize {
        if self.funding.is_some() {
            POSITION_V3_LEN
        } else {
            POSITION_V2_LEN
        }
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

/// Funding engine: a v3 row is a v2 row (version byte 3) followed by the
/// entry index (i128 BE) and the funding paid (raw `FixedPoint`, 16 BE).
/// Written only for a position with funding state, so with the feature off
/// every row stays v2, byte for byte.
const POSITION_SCHEMA_V3: u8 = 3;

/// Serialized size of a v3 [`Position`].
pub const POSITION_V3_LEN: usize = POSITION_V2_LEN + 32;

impl BorshSerialize for Position {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let version = if self.funding.is_some() { POSITION_SCHEMA_V3 } else { POSITION_SCHEMA_VERSION };
        w.write_all(&[version])?;
        borsh_write_address(&self.trader, w)?;
        w.write_all(&self.market_id.to_be_bytes())?;
        w.write_all(&[u8::from(self.is_long)])?;
        borsh_write_fp(&self.size, w)?;
        borsh_write_fp(&self.entry_price, w)?;
        borsh_write_fp(&self.cost_basis, w)?;
        borsh_write_fp(&self.realized_pnl, w)?;
        borsh_write_fp(&self.isolated_margin, w)?;
        w.write_all(&[self.margin_type as u8])?;
        if let Some(f) = &self.funding {
            w.write_all(&f.entry_index.to_be_bytes())?;
            let paid = f.paid;
            borsh_write_fp(&paid, w)?;
        }
        Ok(())
    }
}

impl BorshDeserialize for Position {
    fn deserialize_reader<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut ver = [0u8; 1];
        r.read_exact(&mut ver)?;
        if ver[0] != POSITION_SCHEMA_VERSION && ver[0] != POSITION_SCHEMA_V3 {
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
        let funding = if ver[0] == POSITION_SCHEMA_V3 {
            let mut ib = [0u8; 16];
            r.read_exact(&mut ib)?;
            let paid = borsh_read_fp(r)?;
            Some(PositionFunding { entry_index: i128::from_be_bytes(ib), paid })
        } else {
            None
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
            funding,
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
// Typed rows (item 6 Phase 4 step 4)
// ============================================================================

/// Item 6 Phase 4 step 4: a decoded copy of the resident rows R's positions
/// and balances rows, per trader (the executor's record set). Each method
/// answers `Some(row)` (`Some(None)`: R holds no such row) exactly as
/// decoding R's bytes would, or `None` when it cannot (an opaque trader:
/// read the bytes). Only consulted for a key with no own pending row in the
/// block ([`OwnPendingRow::Resident`]); see the `_typed` reads of
/// [`PositionManager`].
pub trait TypedRows: Send + Sync {
    /// `trader`'s position in `market_id` (the row at [`position_key`]).
    fn position(&self, trader: &Address, market_id: MarketId) -> Option<Option<Position>>;
    /// `trader`'s balance row (`None` inside: no row; the read defaults it).
    fn balance(&self, trader: &Address) -> Option<Option<NativeBalance>>;
    /// `trader`'s `cvlm` row (`None` inside: no row; the read reads zero).
    fn cum_volume(&self, trader: &Address) -> Option<Option<FixedPoint>>;
}

fn decode_position(data: &[u8]) -> Result<Position, CoreError> {
    Position::try_from_slice(data).map_err(|e| CoreError::Borsh(e.to_string()))
}

/// Pre-testnet "aliased opaque position rows" (roadmap, found s128): the
/// stored position row `data` at `key`, decoded and refused unless `key` is
/// [`position_key`] of the row's own trader and market. The only writer of
/// the CF ([`PositionManager::put_position`]) keys every row that way, and
/// nothing else writes it (genesis writes no position rows, there are no
/// migrations or state imports), so a row naming another key is corrupt
/// state, refused like a row that does not decode: [`CoreError::Borsh`], a
/// local fault, so the executor fail-stops the block (R02). Every position
/// load goes through here; before, such a row was decoded and a fill on it
/// filed under the key its fields name, so two markets' settle caches could
/// hold one key and the parallel settle's merge dropped an update.
pub fn decode_position_row(key: &[u8], data: &[u8]) -> Result<Position, CoreError> {
    let pos = decode_position(data)?;
    if key != position_key(&pos.trader, pos.market_id) {
        return Err(CoreError::Borsh(format!(
            "position row at key 0x{} names trader {} market {}",
            alloy_primitives::hex::encode(key),
            pos.trader,
            pos.market_id
        )));
    }
    Ok(pos)
}

fn decode_native_balance(data: &[u8]) -> Result<NativeBalance, CoreError> {
    NativeBalance::try_from_slice(data).map_err(|e| CoreError::Borsh(e.to_string()))
}

fn decode_cum_volume(data: &[u8]) -> Result<FixedPoint, CoreError> {
    let raw: [u8; 16] = data
        .try_into()
        .map_err(|_| CoreError::Borsh(format!("cum_volume row has {} bytes, expected 16", data.len())))?;
    Ok(FixedPoint::from_raw(i128::from_be_bytes(raw)))
}

// ============================================================================
// PositionManager — CRUD for positions and native balances
// ============================================================================

/// Funding engine: the block's cumulative funding index per market
/// (`crate::funding::INDEX_SCALE`), fixed for the whole block.
pub type FundingIndices = HashMap<MarketId, i128>;

#[derive(Clone)]
pub struct PositionManager<T: StateBackend = StateDb> {
    state: T,
    /// Funding engine: the block's index table, set by the block-start
    /// funding step while the feature is active (`None`: no funding, no
    /// funding reads). A market outside the table reads its row.
    funding: Option<std::sync::Arc<FundingIndices>>,
}

impl<T: StateBackend> PositionManager<T> {
    pub fn new(state: T) -> Self {
        Self { state, funding: None }
    }

    pub fn state(&self) -> &T {
        &self.state
    }

    /// Funding engine: install (or clear) the block's index table. Every
    /// fill through this manager (and its clones made afterwards) settles
    /// the position's funding against it.
    pub fn set_funding(&mut self, table: Option<std::sync::Arc<FundingIndices>>) {
        self.funding = table;
    }

    /// Funding engine: the block's index table (`None`: funding off).
    pub fn funding_table(&self) -> Option<&std::sync::Arc<FundingIndices>> {
        self.funding.as_ref()
    }

    /// Funding engine: `market_id`'s cumulative index this block. `None`
    /// while funding is off (no read) or when the market has no funding row.
    pub fn funding_index(&self, market_id: MarketId) -> Result<Option<i128>, CoreError> {
        let Some(table) = &self.funding else {
            return Ok(None);
        };
        match table.get(&market_id) {
            Some(index) => Ok(Some(*index)),
            None => crate::funding::read_index(&self.state, market_id),
        }
    }

    // ---- Position CRUD ----

    pub fn get_position(
        &self,
        trader: &Address,
        market_id: MarketId,
    ) -> Result<Option<Position>, CoreError> {
        let key = position_key(trader, market_id);
        match self.state.get_cf_raw(CF_NATIVE_POSITIONS, &key)? {
            Some(data) => Ok(Some(decode_position_row(&key, &data)?)),
            None => Ok(None),
        }
    }

    /// Item 6 Phase 4 step 4: [`Self::get_position`], with the block's own
    /// pending row first (bytes, decoded as there), else `typed`'s record
    /// when the backend reads R for the key; any other case (no `typed`, the
    /// backend cannot tell, `typed` declines) reads `get_cf_raw`. Same
    /// result, same errors.
    pub fn get_position_typed(
        &self,
        typed: Option<&dyn TypedRows>,
        trader: &Address,
        market_id: MarketId,
    ) -> Result<Option<Position>, CoreError> {
        let Some(typed) = typed else {
            return self.get_position(trader, market_id);
        };
        let key = position_key(trader, market_id);
        match self.state.own_pending_row(CF_NATIVE_POSITIONS, &key) {
            OwnPendingRow::Pending(row) => row.as_deref().map(|r| decode_position_row(&key, r)).transpose(),
            OwnPendingRow::Resident => match typed.position(trader, market_id) {
                Some(pos) => Ok(pos),
                None => self.get_position(trader, market_id),
            },
            OwnPendingRow::Unknown => self.get_position(trader, market_id),
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
        for (key, value) in entries {
            out.push(decode_position_row(&key, &value)?);
        }
        Ok(out)
    }

    // ---- Native balance CRUD ----

    pub fn get_native_balance(&self, trader: &Address) -> Result<NativeBalance, CoreError> {
        match self
            .state
            .get_cf_raw(CF_NATIVE_BALANCES, trader.as_slice())?
        {
            Some(data) => decode_native_balance(&data),
            None => Ok(NativeBalance::default()),
        }
    }

    /// Item 6 Phase 4 step 4: [`Self::get_native_balance`] through `typed`
    /// (see [`Self::get_position_typed`]).
    pub fn get_native_balance_typed(
        &self,
        typed: Option<&dyn TypedRows>,
        trader: &Address,
    ) -> Result<NativeBalance, CoreError> {
        let Some(typed) = typed else {
            return self.get_native_balance(trader);
        };
        let row = match self.state.own_pending_row(CF_NATIVE_BALANCES, trader.as_slice()) {
            OwnPendingRow::Pending(row) => row.as_deref().map(decode_native_balance).transpose()?,
            OwnPendingRow::Resident => match typed.balance(trader) {
                Some(bal) => bal,
                None => return self.get_native_balance(trader),
            },
            OwnPendingRow::Unknown => return self.get_native_balance(trader),
        };
        Ok(row.unwrap_or_default())
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
            Some(data) => decode_cum_volume(&data),
            None => Ok(FixedPoint::ZERO),
        }
    }

    /// Item 6 Phase 4 step 4: [`Self::get_cum_volume`] through `typed` (see
    /// [`Self::get_position_typed`]).
    pub fn get_cum_volume_typed(
        &self,
        typed: Option<&dyn TypedRows>,
        trader: &Address,
    ) -> Result<FixedPoint, CoreError> {
        let Some(typed) = typed else {
            return self.get_cum_volume(trader);
        };
        let row = match self.state.own_pending_row(CF_NATIVE_BALANCES, &cum_volume_key(trader)) {
            OwnPendingRow::Pending(row) => row.as_deref().map(decode_cum_volume).transpose()?,
            OwnPendingRow::Resident => match typed.cum_volume(trader) {
                Some(volume) => volume,
                None => return self.get_cum_volume(trader),
            },
            OwnPendingRow::Unknown => return self.get_cum_volume(trader),
        };
        Ok(row.unwrap_or(FixedPoint::ZERO))
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
        let index = self.funding_index(market_id)?;
        let existing = self.get_position(trader, market_id)?;
        let start_size = signed_size(&existing);
        let (new_pos, pnl, funding) = fill_transition(
            existing, trader, market_id, is_buy, fill_qty, fill_price, margin_type, index,
        );
        match new_pos {
            Some(pos) => self.put_position(&pos)?,
            // `fill_transition` yields None only on a full close of an
            // existing position, so the delete always targets a live row.
            None => self.delete_position(trader, market_id)?,
        }
        let effect = FillEffect { start_size, closed_pnl: pnl, funding };
        if let Some(cash) = effect.cash() {
            self.credit_realized_pnl(trader, cash)?;
        }
        Ok(effect)
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
        .map(|e| e.cash())
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
        self.apply_fill_cached_effect_typed(
            cache, None, trader, market_id, is_buy, fill_qty, fill_price, margin_type,
        )
    }

    /// Item 6 Phase 4 step 4: [`apply_fill_cached_effect`], a cache miss
    /// read through `typed` ([`Self::get_position_typed`]). Same effect,
    /// same cache writes.
    ///
    /// [`apply_fill_cached_effect`]: Self::apply_fill_cached_effect
    #[allow(clippy::too_many_arguments)]
    pub fn apply_fill_cached_effect_typed(
        &self,
        cache: &mut PositionCache,
        typed: Option<&dyn TypedRows>,
        trader: &Address,
        market_id: MarketId,
        is_buy: bool,
        fill_qty: FixedPoint,
        fill_price: FixedPoint,
        margin_type: MarginType,
    ) -> Result<FillEffect, CoreError> {
        let index = self.funding_index(market_id)?;
        cache.update_in_place(self, typed, trader, market_id, |existing| {
            let start_size = signed_size(&existing);
            let (new_pos, pnl, funding) = fill_transition(
                existing,
                trader,
                market_id,
                is_buy,
                fill_qty,
                fill_price,
                margin_type,
                index,
            );
            (
                new_pos,
                FillEffect {
                    start_size,
                    closed_pnl: pnl,
                    funding,
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
    /// Funding engine: funding the position settled at this fill (positive
    /// = paid), before the fill applied. 0 without funding.
    pub funding: FixedPoint,
}

// Hand-written: `FixedPoint` has no `Default`.
impl Default for FillEffect {
    fn default() -> Self {
        Self { start_size: FixedPoint::ZERO, closed_pnl: None, funding: FixedPoint::ZERO }
    }
}

impl FillEffect {
    /// The trader's cash change of this fill: the realized PnL less the
    /// funding settled. `None` = no balance write (no close part and no
    /// funding), exactly the cases where the pre-funding paths wrote none.
    pub fn cash(&self) -> Option<FixedPoint> {
        match self.closed_pnl {
            Some(pnl) => Some(pnl - self.funding),
            None if self.funding != FixedPoint::ZERO => Some(FixedPoint::ZERO - self.funding),
            None => None,
        }
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
#[allow(clippy::too_many_arguments)]
fn fill_transition(
    existing: Option<Position>,
    trader: &Address,
    market_id: MarketId,
    is_buy: bool,
    fill_qty: FixedPoint,
    fill_price: FixedPoint,
    margin_type: MarginType,
    funding_index: Option<i128>,
) -> (Option<Position>, Option<FixedPoint>, FixedPoint) {
    // Funding engine: settle the position at the market's index first (its
    // owed amount is paid in cash by the caller, `FillEffect::cash`), then
    // fill. A position opened (or the open part of a flip) starts at the
    // index with nothing paid. No index (feature off, or no funding row
    // yet): exactly the pre-funding transition.
    let Some(index) = funding_index else {
        let (new_pos, pnl) =
            fill_transition_core(existing, trader, market_id, is_buy, fill_qty, fill_price, margin_type);
        return (new_pos, pnl, FixedPoint::ZERO);
    };
    let mut owed = FixedPoint::ZERO;
    let existing = existing.map(|mut pos| {
        owed = pos.funding_owed(Some(index));
        let paid = pos.funding.map_or(FixedPoint::ZERO, |f| f.paid);
        pos.funding = Some(PositionFunding { entry_index: index, paid: paid + owed });
        pos
    });
    let (new_pos, pnl) =
        fill_transition_core(existing, trader, market_id, is_buy, fill_qty, fill_price, margin_type);
    let fresh = PositionFunding { entry_index: index, paid: FixedPoint::ZERO };
    let new_pos = new_pos.map(|mut pos| {
        // A fresh open (no row before, or the open part of a flip) has no
        // funding state from `fill_transition_core`.
        if pos.funding.is_none() {
            pos.funding = Some(fresh);
        }
        pos
    });
    (new_pos, pnl, owed)
}

/// The pre-funding transition (cost basis, entry price, realized PnL).
fn fill_transition_core(
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
        funding: None,
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

/// How [`PositionCache::flush_with_runs`] wrote its caches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunsFlush {
    /// The caches' key sets were provably disjoint: one sorted pass over
    /// every cache's dirty rows, nothing merged.
    Unmerged,
    /// Not provably disjoint: the runs were `merge_disjoint`-ed into the
    /// batch cache in order and `flush_all` ran (the flag-off sequence).
    Merged,
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
    /// 8k L2.2: lowest and highest market id of any key ever put in `map`
    /// (clean reads, misses, tombstones and dirty rows alike); `None` while
    /// `map` is empty. `map` never drops a key, so this always covers its
    /// key set: two caches whose ranges do not intersect hold disjoint keys
    /// ([`Self::runs_disjoint`]). Two compares per new key.
    markets: Option<(MarketId, MarketId)>,
}

/// Widen `markets` (a [`PositionCache`]'s key-market range) to cover `m`.
#[inline]
fn cover_market(markets: &mut Option<(MarketId, MarketId)>, m: MarketId) {
    *markets = Some(match *markets {
        Some((lo, hi)) => (lo.min(m), hi.max(m)),
        None => (m, m),
    });
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
        self.load_typed(positions, None, trader, market_id)
    }

    /// Item 6 Phase 4 step 4: [`Self::load`], a miss read through `typed`
    /// ([`PositionManager::get_position_typed`]).
    pub fn load_typed<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
        typed: Option<&dyn TypedRows>,
        trader: &Address,
        market_id: MarketId,
    ) -> Result<Option<Position>, CoreError> {
        let key = (*trader, market_id);
        if let Some(entry) = self.map.get(&key) {
            return Ok(entry.clone());
        }
        let pos = positions.get_position_typed(typed, trader, market_id)?;
        self.map.insert(key, pos.clone());
        cover_market(&mut self.markets, market_id);
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
        typed: Option<&dyn TypedRows>,
        trader: &Address,
        market_id: MarketId,
        f: impl FnOnce(Option<Position>) -> (Option<Position>, R),
    ) -> Result<R, CoreError> {
        let key = (*trader, market_id);
        let slot = match self.map.entry(key) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let row = positions.get_position_typed(typed, trader, market_id)?;
                cover_market(&mut self.markets, market_id);
                e.insert(row)
            }
        };
        // Every entry sits under its own fields' key: a load refuses a row
        // naming another key ([`decode_position_row`]) and `set` keys from
        // the fields. Checked anyway (two compares): an entry that broke it
        // would put a foreign key in this cache.
        if let Some(p) = slot.as_ref().filter(|p| (p.trader, p.market_id) != key) {
            return Err(CoreError::Borsh(format!(
                "position cache entry ({trader}, {market_id}) names trader {} market {}",
                p.trader, p.market_id
            )));
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
        cover_market(&mut self.markets, key.1);
        self.map.insert(key, Some(pos));
        self.dirty.insert(key);
    }

    /// Tombstone a fully-closed position (flushes as a delete).
    pub fn remove(&mut self, trader: &Address, market_id: MarketId) {
        let key = (*trader, market_id);
        cover_market(&mut self.markets, market_id);
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
    ///
    /// Pre-testnet: a stored row whose fields name another market (an
    /// "aliased opaque" row) used to file a fill under that market's key, so
    /// two market caches could share a key and `extend` kept the later
    /// cache's entry, dropping an update. Loads now refuse such rows
    /// ([`decode_position_row`]), so per-market caches filled by loads and
    /// fills are disjoint; only an explicit [`Self::set`] / [`Self::remove`]
    /// of a foreign market's key can still overlap them, and `extend` keeps
    /// the later entry.
    pub fn merge_disjoint(&mut self, other: PositionCache) {
        self.map.extend(other.map);
        self.dirty.extend(other.dirty);
        if let Some((lo, hi)) = other.markets {
            cover_market(&mut self.markets, lo);
            cover_market(&mut self.markets, hi);
        }
    }

    /// 8k L2.2: proof that this cache and every run hold pairwise disjoint
    /// key sets, from the key-market ranges alone (O(runs), no key walk):
    /// the non-empty runs' ranges are strictly ascending (each run's lowest
    /// market above the previous run's highest) and this cache's range
    /// meets none of them. Pass B's per-market caches satisfy it (loads
    /// refuse rows naming another key, [`decode_position_row`]); a foreign
    /// market's key put in one by `set` / `remove` fails it. `false` = not proven
    /// (conservative: disjoint caches can fail it, overlapping ones never
    /// pass).
    pub fn runs_disjoint(&self, runs: &[PositionCache]) -> bool {
        let mut prev_hi: Option<MarketId> = None;
        for (lo, hi) in runs.iter().filter_map(|r| r.markets) {
            if prev_hi.is_some_and(|p| lo <= p) {
                return false;
            }
            if let Some((blo, bhi)) = self.markets {
                if blo <= hi && lo <= bhi {
                    return false;
                }
            }
            prev_hi = Some(hi);
        }
        true
    }

    /// 8k L2.1: lowest and highest market id of any key ever put in this
    /// cache (clean entries included); `None` while it is empty. A plan cache
    /// whose range is exactly `(m, m)` holds only market `m`'s keys.
    pub fn market_range(&self) -> Option<(MarketId, MarketId)> {
        self.markets
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

    /// 8k L2.2 (`TORUS_SETTLE_NO_MERGE=1`): [`flush_all`] of this cache with
    /// `runs` (pass B's per-market caches) merged in, in order, without
    /// merging them when that is provably the same.
    ///
    /// Fast path ([`RunsFlush::Unmerged`]), when [`runs_disjoint`] proves
    /// every cache's key set (clean entries included) disjoint from the
    /// others': gathers `(key, row)` for every dirty key of this cache and
    /// of each run, sorts by `(trader, market_id)` (the keys are unique, so
    /// the order is total and the sort deterministic) and writes in that
    /// order, then clears every dirty set. That is exactly the write sequence
    /// of [`merge_disjoint`] of every run followed by [`flush_all`]: the same
    /// rows, in the same global key order. What it skips: moving every entry
    /// of every run (clean read hits included) into one map, and the second
    /// lookup per dirty key.
    ///
    /// Otherwise ([`RunsFlush::Merged`]; a `set` / `remove` put one market's
    /// key in another market's cache, or the ranges merely touch): it IS
    /// merge + flush_all, run by run in
    /// slice order (`std::mem::take` leaves each run empty), so a clean
    /// entry of a later run replaces an earlier run's dirty row exactly as
    /// the flag-off path does.
    ///
    /// [`flush_all`]: Self::flush_all
    /// [`merge_disjoint`]: Self::merge_disjoint
    /// [`runs_disjoint`]: Self::runs_disjoint
    pub fn flush_with_runs<T: StateBackend>(
        &mut self,
        runs: &mut [PositionCache],
        positions: &PositionManager<T>,
    ) -> Result<RunsFlush, CoreError> {
        if !self.runs_disjoint(runs) {
            for run in runs.iter_mut() {
                self.merge_disjoint(std::mem::take(run));
            }
            self.flush_all(positions)?;
            return Ok(RunsFlush::Merged);
        }
        let total = self.dirty.len() + runs.iter().map(|r| r.dirty.len()).sum::<usize>();
        let mut rows: Vec<((Address, MarketId), &Option<Position>)> = Vec::with_capacity(total);
        for cache in std::iter::once(&*self).chain(runs.iter()) {
            rows.extend(
                cache
                    .dirty
                    .iter()
                    .filter_map(|key| cache.map.get(key).map(|row| (*key, row))),
            );
        }
        rows.sort_unstable_by_key(|row| row.0);
        debug_assert!(
            rows.windows(2).all(|w| w[0].0 < w[1].0),
            "flush_with_runs: runs_disjoint passed but two caches hold the same key"
        );
        for (key, row) in rows {
            match row {
                Some(pos) => positions.put_position(pos)?,
                None => positions.delete_position(&key.0, key.1)?,
            }
        }
        self.dirty.clear();
        for run in runs.iter_mut() {
            run.dirty.clear();
        }
        Ok(RunsFlush::Unmerged)
    }

    /// Item 6 Phase 2 step 0.4: the per-row flush, frozen as the reference
    /// for P2-2's batch overlay writes: one `put_position` /
    /// `delete_position` per dirty key in key order (`flush_all` as of
    /// `d3ba3c0a`). Test-only, no runtime flag (D16); other crates' tests
    /// reach it through the `test-reference-paths` feature (plan 9.8).
    #[cfg(any(test, feature = "test-reference-paths"))]
    pub fn flush_all_per_row<T: StateBackend>(
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
        let (pos, pnl, funding) = fill_transition(
            pos,
            &Address::new([7; 20]),
            3,
            is_buy,
            raw(q),
            raw(p),
            MarginType::Cross,
            None,
        );
        assert_eq!(funding, FixedPoint::ZERO, "no funding without an index");
        (pos, pnl)
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
            funding: None,
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

    /// Item 6 Phase 2 step 0.4: `flush_all` leaves the overlay exactly as
    /// the per-row reference does: the same pending writes and tombstones,
    /// the same reads, and (through a checkpoint opened before the flush) the
    /// same undo journal. Random sets / removes over rows the DB already
    /// holds, rows written earlier in the block, and new rows; several
    /// flushes per cache.
    #[test]
    fn flush_all_matches_the_per_row_reference() {
        use torus_state::cf::CF_NATIVE_POSITIONS;
        use torus_state::{NativeStateOverlay, StateDb};
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let pos = |t: u8, m: MarketId, size: i128, entry: i128| Position {
            trader: Address::new([t; 20]),
            market_id: m,
            is_long: size > 0,
            size: raw(size.abs() * S),
            entry_price: raw(entry * S),
            // One fill: cost_basis = size x entry.
            cost_basis: raw(size.abs() * entry * S),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
            funding: None,
        };
        let base = PositionManager::new(db.clone());
        for t in 1..=6u8 {
            base.put_position(&pos(t, 1, t as i128, 100)).unwrap();
        }
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: u64| {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) % n
        };
        let overlays = [
            NativeStateOverlay::new(db.clone()),
            NativeStateOverlay::new(db.clone()),
        ];
        let managers = overlays.each_ref().map(|o| PositionManager::new(o.clone()));
        // Written earlier in the block, before the checkpoint.
        for m in &managers {
            m.put_position(&pos(2, 2, -3, 90)).unwrap();
        }
        let mut caches = [PositionCache::new(), PositionCache::new()];
        let keys = |o: &NativeStateOverlay| -> Vec<Option<Vec<u8>>> {
            (1..=9u8)
                .flat_map(|t| (1..=3).map(move |m| position_key(&Address::new([t; 20]), m)))
                .map(|k| o.get_cf_raw(CF_NATIVE_POSITIONS, &k).unwrap())
                .collect()
        };
        for round in 0..6 {
            for o in &overlays {
                o.checkpoint();
            }
            for _ in 0..40 {
                let (t, m) = (1 + next(9) as u8, 1 + next(3));
                let op = next(4);
                for (c, mgr) in caches.iter_mut().zip(&managers) {
                    match op {
                        0 => c.remove(&Address::new([t; 20]), m),
                        1 => {
                            c.load(mgr, &Address::new([t; 20]), m).unwrap();
                        }
                        _ => c.set(pos(t, m, 1 + (round * 7 + t as i128) % 5, 95 + op as i128)),
                    }
                }
            }
            let [c0, c1] = &mut caches;
            c0.flush_all(&managers[0]).unwrap();
            c1.flush_all_per_row(&managers[1]).unwrap();
            assert_eq!(
                overlays[0].own_pending_delta(),
                overlays[1].own_pending_delta(),
                "round {round}"
            );
            assert_eq!(
                keys(&overlays[0]),
                keys(&overlays[1]),
                "round {round}: reads"
            );
            if round % 2 == 1 {
                for o in &overlays {
                    o.revert_to_checkpoint();
                }
                assert_eq!(
                    overlays[0].own_pending_delta(),
                    overlays[1].own_pending_delta(),
                    "round {round}: reverted"
                );
                assert_eq!(
                    keys(&overlays[0]),
                    keys(&overlays[1]),
                    "round {round}: reverted reads"
                );
            } else {
                for o in &overlays {
                    o.commit_checkpoint();
                }
            }
        }
        assert!(!overlays[0].own_pending_delta().is_empty());
    }
}

#[cfg(test)]
mod exec_hasher_tests {
    use super::*;
    use std::hash::BuildHasher;

    /// Item 6 Phase 2 P2-5: the execution maps' hasher (alloy's default
    /// `map-foldhash`) is seeded per map, so keys a trader chooses cannot be
    /// aimed at one bucket. Fails if feature unification ever gives alloy's
    /// map an unseeded hasher (e.g. `map-fxhash` without `map-foldhash`).
    #[test]
    fn exec_map_hasher_is_seeded_per_map() {
        let a = HashMap::<(Address, MarketId), ()>::default();
        let b = HashMap::<(Address, MarketId), ()>::default();
        let key = (Address::ZERO, 1);
        assert_ne!(a.hasher().hash_one(key), b.hasher().hash_one(key));
    }
}
