//! Position tracking and PnL computation (task 2.2.5).
//!
//! Stores positions in CF_NATIVE_POSITIONS and native balances in CF_NATIVE_BALANCES.

use std::io::{self, Read, Write};

use borsh::{BorshDeserialize, BorshSerialize};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::StateDb;
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

impl BorshSerialize for Position {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
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

impl BorshSerialize for NativeBalance {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        borsh_write_fp(&self.available, w)?;
        borsh_write_fp(&self.order_margin, w)?;
        Ok(())
    }
}

impl BorshDeserialize for NativeBalance {
    fn deserialize_reader<R: Read>(r: &mut R) -> io::Result<Self> {
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
pub struct PositionManager {
    state_db: StateDb,
}

impl PositionManager {
    pub fn new(state_db: StateDb) -> Self {
        Self { state_db }
    }

    pub fn state_db(&self) -> &StateDb {
        &self.state_db
    }

    // ---- Position CRUD ----

    pub fn get_position(
        &self,
        trader: &Address,
        market_id: MarketId,
    ) -> Result<Option<Position>, CoreError> {
        let key = position_key(trader, market_id);
        match self.state_db.get_cf_raw(CF_NATIVE_POSITIONS, &key)? {
            Some(data) => Ok(Some(
                Position::try_from_slice(&data)
                    .map_err(|e| CoreError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    pub fn put_position(&self, pos: &Position) -> Result<(), CoreError> {
        let key = position_key(&pos.trader, pos.market_id);
        let data = borsh::to_vec(pos).map_err(|e| CoreError::Borsh(e.to_string()))?;
        self.state_db
            .put_cf_raw(CF_NATIVE_POSITIONS, &key, &data)?;
        Ok(())
    }

    pub fn delete_position(
        &self,
        trader: &Address,
        market_id: MarketId,
    ) -> Result<(), CoreError> {
        let key = position_key(trader, market_id);
        self.state_db.delete_cf_raw(CF_NATIVE_POSITIONS, &key)?;
        Ok(())
    }

    /// All positions for a trader (prefix scan).
    pub fn positions_for_trader(&self, trader: &Address) -> Result<Vec<Position>, CoreError> {
        let db = self.state_db.inner();
        let cf = db
            .cf_handle(CF_NATIVE_POSITIONS)
            .ok_or(CoreError::MissingCf(CF_NATIVE_POSITIONS))?;
        let prefix = trader.as_slice();
        let iter = db.prefix_iterator_cf(cf, prefix);
        let mut out = Vec::new();
        for item in iter {
            let (key, value) =
                item.map_err(|e| CoreError::State(torus_state::StateError::RocksDb(e)))?;
            if !key.starts_with(prefix) {
                break;
            }
            out.push(
                Position::try_from_slice(&value)
                    .map_err(|e| CoreError::Borsh(e.to_string()))?,
            );
        }
        Ok(out)
    }

    // ---- Native balance CRUD ----

    pub fn get_native_balance(&self, trader: &Address) -> Result<NativeBalance, CoreError> {
        match self
            .state_db
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
        self.state_db
            .put_cf_raw(CF_NATIVE_BALANCES, trader.as_slice(), &data)?;
        Ok(())
    }

    // ---- Fill application ----

    /// Update position after a fill. Handles open/increase/reduce/close/flip.
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

        match existing {
            None => {
                // Open new position
                self.put_position(&Position {
                    trader: *trader,
                    market_id,
                    is_long: is_buy,
                    size: fill_qty,
                    entry_price: fill_price,
                    realized_pnl: FixedPoint::ZERO,
                    isolated_margin: FixedPoint::ZERO,
                    margin_type,
                })?;
            }
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
                    self.put_position(&pos)?;
                } else if fill_qty < pos.size {
                    // Partial close
                    let pnl_per = if pos.is_long {
                        fill_price - pos.entry_price
                    } else {
                        pos.entry_price - fill_price
                    };
                    pos.realized_pnl = pos.realized_pnl + pnl_per * fill_qty;
                    pos.size = pos.size - fill_qty;
                    self.put_position(&pos)?;
                    self.credit_realized_pnl(trader, pnl_per * fill_qty)?;
                } else if fill_qty == pos.size {
                    // Full close
                    let pnl_per = if pos.is_long {
                        fill_price - pos.entry_price
                    } else {
                        pos.entry_price - fill_price
                    };
                    let pnl = pnl_per * fill_qty;
                    self.delete_position(trader, market_id)?;
                    self.credit_realized_pnl(trader, pnl)?;
                } else {
                    // Flip: close + open opposite
                    let pnl_per = if pos.is_long {
                        fill_price - pos.entry_price
                    } else {
                        pos.entry_price - fill_price
                    };
                    let close_pnl = pnl_per * pos.size;
                    self.credit_realized_pnl(trader, close_pnl)?;

                    let remainder = fill_qty - pos.size;
                    self.put_position(&Position {
                        trader: *trader,
                        market_id,
                        is_long: is_buy,
                        size: remainder,
                        entry_price: fill_price,
                        realized_pnl: FixedPoint::ZERO,
                        isolated_margin: FixedPoint::ZERO,
                        margin_type,
                    })?;
                }
            }
        }
        Ok(())
    }

    /// Credit (or debit if negative) realized PnL to native balance.
    fn credit_realized_pnl(
        &self,
        trader: &Address,
        pnl: FixedPoint,
    ) -> Result<(), CoreError> {
        let mut bal = self.get_native_balance(trader)?;
        bal.available = bal.available + pnl;
        self.put_native_balance(trader, &bal)?;
        Ok(())
    }
}
