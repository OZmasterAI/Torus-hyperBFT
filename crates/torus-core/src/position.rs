//! Position tracking and PnL computation (task 2.2.5).
//!
//! Stores positions in CF_NATIVE_POSITIONS and native balances in CF_NATIVE_BALANCES.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};

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
    pub entry_price: FixedPoint,
    pub realized_pnl: FixedPoint,
    /// Margin allocated to this position (>0 for isolated, 0 for cross).
    pub isolated_margin: FixedPoint,
    pub margin_type: MarginType,
}

impl Position {
    /// Unrealized PnL at a given mark price.
    pub fn unrealized_pnl(&self, mark_price: FixedPoint) -> FixedPoint {
        let diff = if self.is_long {
            mark_price - self.entry_price
        } else {
            self.entry_price - mark_price
        };
        diff * self.size
    }

    /// Notional value at mark price.
    pub fn notional(&self, mark_price: FixedPoint) -> FixedPoint {
        self.size * mark_price
    }
}

/// AUDIT FIX ECON-FIND-31: Schema version byte prepended to Position serialization.
/// BREAKING CHANGE (acceptable pre-launch): existing serialized Positions are
/// incompatible and must be re-created.
const POSITION_SCHEMA_VERSION: u8 = 1;

impl BorshSerialize for Position {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&[POSITION_SCHEMA_VERSION])?;
        borsh_write_address(&self.trader, w)?;
        w.write_all(&self.market_id.to_be_bytes())?;
        w.write_all(&[u8::from(self.is_long)])?;
        borsh_write_fp(&self.size, w)?;
        borsh_write_fp(&self.entry_price, w)?;
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
        let data = borsh::to_vec(pos).map_err(|e| CoreError::Borsh(e.to_string()))?;
        self.state.put_cf_raw(CF_NATIVE_POSITIONS, &key, &data)?;
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
        let data = borsh::to_vec(bal).map_err(|e| CoreError::Borsh(e.to_string()))?;
        self.state
            .put_cf_raw(CF_NATIVE_BALANCES, trader.as_slice(), &data)?;
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
    ) -> Result<(), CoreError> {
        let existing = self.get_position(trader, market_id)?;
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
        Ok(())
    }

    /// C1: `apply_fill`, but the position read-modify-write goes through a
    /// write-back [`PositionCache`] and NO balance is touched — the realized
    /// PnL (if the fill had a close component) is returned for the caller to
    /// credit through whatever balance authority it maintains (the executor's
    /// BalanceCache). `Some(pnl)` is emitted exactly when the classic path
    /// would have called its internal PnL credit — including `Some(ZERO)` for
    /// a flat close, which still materializes the trader's balance row.
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
        let existing = cache.load(self, trader, market_id)?;
        let (new_pos, pnl) =
            fill_transition(existing, trader, market_id, is_buy, fill_qty, fill_price, margin_type);
        match new_pos {
            Some(pos) => cache.set(pos),
            None => cache.remove(trader, market_id),
        }
        Ok(pnl)
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
    match existing {
        None => (
            // Open new position
            Some(Position {
                trader: *trader,
                market_id,
                is_long: is_buy,
                size: fill_qty,
                entry_price: fill_price,
                realized_pnl: FixedPoint::ZERO,
                isolated_margin: FixedPoint::ZERO,
                margin_type,
            }),
            None,
        ),
        Some(mut pos) => {
            let same_dir = (is_buy && pos.is_long) || (!is_buy && !pos.is_long);

            if same_dir {
                // Increase: weighted average entry
                let total_cost = pos.entry_price * pos.size + fill_price * fill_qty;
                let new_size = pos.size + fill_qty;
                if new_size > FixedPoint::ZERO {
                    pos.entry_price = total_cost / new_size;
                }
                pos.size = new_size;
                (Some(pos), None)
            } else if fill_qty < pos.size {
                // Partial close
                let pnl_per = if pos.is_long {
                    fill_price - pos.entry_price
                } else {
                    pos.entry_price - fill_price
                };
                pos.realized_pnl += pnl_per * fill_qty;
                pos.size -= fill_qty;
                (Some(pos), Some(pnl_per * fill_qty))
            } else if fill_qty == pos.size {
                // Full close
                let pnl_per = if pos.is_long {
                    fill_price - pos.entry_price
                } else {
                    pos.entry_price - fill_price
                };
                (None, Some(pnl_per * fill_qty))
            } else {
                // Flip: close + open opposite
                let pnl_per = if pos.is_long {
                    fill_price - pos.entry_price
                } else {
                    pos.entry_price - fill_price
                };
                let close_pnl = pnl_per * pos.size;
                let remainder = fill_qty - pos.size;
                (
                    Some(Position {
                        trader: *trader,
                        market_id,
                        is_long: is_buy,
                        size: remainder,
                        entry_price: fill_price,
                        realized_pnl: FixedPoint::ZERO,
                        isolated_margin: FixedPoint::ZERO,
                        margin_type,
                    }),
                    Some(close_pnl),
                )
            }
        }
    }
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
