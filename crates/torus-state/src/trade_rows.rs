//! Packed trade-history rows (s77): one row per (market, block, chunk) in
//! `CF_NATIVE_TRADES` and one per (trader, block) in `CF_NATIVE_USER_TRADES`,
//! instead of three rows per fill. Both CFs are node-local (outside the native
//! consensus root) and read only by the RPC trade-history methods.
//!
//! | CF | key (big-endian) | value (little-endian) |
//! |---|---|---|
//! | trades | `market u64 \| block u64 \| chunk u16` | `ver u8 \| timestamp u64 \| n u16 \| n × {trade_index u32, price i128, qty i128, taker_side u8}` |
//! | user trades | `address[20] \| !block u64` | `ver u8 \| timestamp u64 \| n u32 \| n × {trade_index u32, market u64, price i128, qty i128, taker_side u8, role u8}` |
//!
//! A market's fills are in `trade_index` order; chunk `k` holds its fills
//! `[1024k, 1024k + 1024)`. A trader's row holds every maker (role 0) and taker
//! (role 1) entry of theirs in the block, in `trade_index` order. Rows are built
//! once per block from all of its fills, so the same fills always give the same
//! bytes and a crash-replay rewrite overwrites the same keys.

use alloy_primitives::Address;

use crate::bg_writer::PackedCfBatch;
use crate::cf::{CF_CONSENSUS_META, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES};
use crate::db::StateDb;
use crate::error::StateError;

/// Key in `CF_CONSENSUS_META`: on-disk format of the two trade-history CFs.
pub const META_TRADE_HISTORY_FORMAT: &[u8] = b"trade_history_format";
/// Packed rows (this module). Absent = the old three-rows-per-fill format.
pub const TRADE_HISTORY_FORMAT: u8 = 2;

/// Value format version byte of both row kinds.
pub const TRADE_ROW_VERSION: u8 = 1;
/// Maximum fills in one `CF_NATIVE_TRADES` row.
pub const TRADE_CHUNK_FILLS: usize = 1024;
pub const TRADE_KEY_LEN: usize = 18;
pub const USER_TRADE_KEY_LEN: usize = 28;

const TRADE_HEADER: usize = 1 + 8 + 2;
const TRADE_ENTRY: usize = 4 + 16 + 16 + 1;
const USER_HEADER: usize = 1 + 8 + 4;
const USER_ENTRY: usize = 4 + 8 + 16 + 16 + 1 + 1;

/// One fill as the executor records it; `encode_block` turns a block's fills
/// into rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TradeFill {
    pub trade_index: u32,
    pub market: u64,
    pub maker: Address,
    pub taker: Address,
    pub price_raw: i128,
    pub qty_raw: i128,
    /// 0 = taker bought, 1 = taker sold.
    pub taker_side: u8,
    // s80, stream-only: `encode_block` ignores the fields below (rows unchanged).
    pub maker_order_id: u128,
    pub taker_order_id: u128,
    /// Signed position size before the fill (long > 0, short < 0, none = 0).
    pub maker_start_raw: i128,
    pub taker_start_raw: i128,
    /// Realized PnL of the fill's close part (0 = no close part).
    pub maker_pnl_raw: i128,
    pub taker_pnl_raw: i128,
}

/// One executed block's fills, shared by the trade writer and the stream sink
/// (s80).
#[derive(Debug)]
pub struct BlockFills {
    pub height: u64,
    pub timestamp: u64,
    pub fills: Vec<TradeFill>,
}

/// Stream consumer of executed blocks' fills (s80). Called on the execution
/// thread; must not block.
pub trait FillSink: Send + Sync {
    /// Whether anyone currently wants fills (checked once per block before
    /// execution). False = the block records no fills for the sink (with
    /// trade history off it records none at all).
    fn wants_fills(&self) -> bool;
    /// One executed block's fills (only blocks with fills), in height order.
    fn send_fills(&self, block: std::sync::Arc<BlockFills>);
}

/// One entry of a `CF_NATIVE_TRADES` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketTrade {
    pub trade_index: u32,
    pub price_raw: i128,
    pub qty_raw: i128,
    pub taker_side: u8,
}

/// One entry of a `CF_NATIVE_USER_TRADES` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserTrade {
    pub trade_index: u32,
    pub market: u64,
    pub price_raw: i128,
    pub qty_raw: i128,
    pub taker_side: u8,
    /// 0 = maker, 1 = taker.
    pub role: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradeRowError {
    Version(u8),
    Length(usize),
}

impl std::fmt::Display for TradeRowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version(v) => write!(f, "trade row: unknown version {v}"),
            Self::Length(n) => write!(f, "trade row: bad length {n}"),
        }
    }
}

impl std::error::Error for TradeRowError {}

pub fn trade_key(market: u64, block: u64, chunk: u16) -> [u8; TRADE_KEY_LEN] {
    let mut key = [0u8; TRADE_KEY_LEN];
    key[..8].copy_from_slice(&market.to_be_bytes());
    key[8..16].copy_from_slice(&block.to_be_bytes());
    key[16..].copy_from_slice(&chunk.to_be_bytes());
    key
}

/// `(market, block, chunk)` of a `CF_NATIVE_TRADES` key.
pub fn parse_trade_key(key: &[u8]) -> Option<(u64, u64, u16)> {
    let key: &[u8; TRADE_KEY_LEN] = key.try_into().ok()?;
    Some((
        u64::from_be_bytes(key[..8].try_into().unwrap()),
        u64::from_be_bytes(key[8..16].try_into().unwrap()),
        u16::from_be_bytes(key[16..].try_into().unwrap()),
    ))
}

/// Newest block first under forward iteration (`!block`).
pub fn user_trade_key(trader: &Address, block: u64) -> [u8; USER_TRADE_KEY_LEN] {
    let mut key = [0u8; USER_TRADE_KEY_LEN];
    key[..20].copy_from_slice(trader.as_slice());
    key[20..].copy_from_slice(&(!block).to_be_bytes());
    key
}

/// `(trader, block)` of a `CF_NATIVE_USER_TRADES` key.
pub fn parse_user_trade_key(key: &[u8]) -> Option<(Address, u64)> {
    let key: &[u8; USER_TRADE_KEY_LEN] = key.try_into().ok()?;
    Some((
        Address::from_slice(&key[..20]),
        !u64::from_be_bytes(key[20..].try_into().unwrap()),
    ))
}

/// Append the rows of one block to `out`: market rows in key order, then
/// trader rows in key order. `fills` must be in `trade_index` order.
pub fn encode_block(block: u64, timestamp: u64, fills: &[TradeFill], out: &mut PackedCfBatch) {
    debug_assert!(fills.windows(2).all(|w| w[0].trade_index < w[1].trade_index));
    let mut value = Vec::new();

    // Market rows: stable sort keeps trade_index order within a market.
    let mut by_market: Vec<u32> = (0..fills.len() as u32).collect();
    by_market.sort_by_key(|&i| fills[i as usize].market);
    for market_fills in by_market.chunk_by(|&a, &b| fills[a as usize].market == fills[b as usize].market) {
        let market = fills[market_fills[0] as usize].market;
        for (chunk, part) in market_fills.chunks(TRADE_CHUNK_FILLS).enumerate() {
            value.clear();
            value.push(TRADE_ROW_VERSION);
            value.extend_from_slice(&timestamp.to_le_bytes());
            value.extend_from_slice(&(part.len() as u16).to_le_bytes());
            for &i in part {
                let f = &fills[i as usize];
                value.extend_from_slice(&f.trade_index.to_le_bytes());
                value.extend_from_slice(&f.price_raw.to_le_bytes());
                value.extend_from_slice(&f.qty_raw.to_le_bytes());
                value.push(f.taker_side);
            }
            out.push(CF_NATIVE_TRADES, &trade_key(market, block, chunk as u16), &value);
        }
    }

    // Trader rows: (fill, role) entries, maker before taker within a fill;
    // stable sort by address keeps that order within a trader.
    let mut by_trader: Vec<(u32, u8)> = Vec::with_capacity(fills.len() * 2);
    for i in 0..fills.len() as u32 {
        by_trader.push((i, 0));
        by_trader.push((i, 1));
    }
    let trader = |&(i, role): &(u32, u8)| {
        let f = &fills[i as usize];
        if role == 0 { f.maker } else { f.taker }
    };
    by_trader.sort_by_key(trader);
    for entries in by_trader.chunk_by(|a, b| trader(a) == trader(b)) {
        value.clear();
        value.push(TRADE_ROW_VERSION);
        value.extend_from_slice(&timestamp.to_le_bytes());
        value.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for &(i, role) in entries {
            let f = &fills[i as usize];
            value.extend_from_slice(&f.trade_index.to_le_bytes());
            value.extend_from_slice(&f.market.to_le_bytes());
            value.extend_from_slice(&f.price_raw.to_le_bytes());
            value.extend_from_slice(&f.qty_raw.to_le_bytes());
            value.push(f.taker_side);
            value.push(role);
        }
        out.push(CF_NATIVE_USER_TRADES, &user_trade_key(&trader(&entries[0]), block), &value);
    }
}

/// On startup, before replay: if the trade-history CFs are not in the current
/// format, delete every row of both (node-local history, not consensus state)
/// and write the marker, atomically. Returns whether it wiped.
pub fn ensure_trade_history_format(db: &StateDb) -> Result<bool, StateError> {
    if db.get_cf_raw(CF_CONSENSUS_META, META_TRADE_HISTORY_FORMAT)?.as_deref()
        == Some(&[TRADE_HISTORY_FORMAT][..])
    {
        return Ok(false);
    }
    // Every key is at most 32 bytes, so all sort below 64 × 0xFF.
    let mut batch = rocksdb::WriteBatch::default();
    for cf in [CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES] {
        batch.delete_range_cf(db.cf_handle(cf)?, &[][..], &[0xFFu8; 64][..]);
    }
    batch.put_cf(db.cf_handle(CF_CONSENSUS_META)?, META_TRADE_HISTORY_FORMAT, [TRADE_HISTORY_FORMAT]);
    db.write_with(batch, &rocksdb::WriteOptions::default())?;
    Ok(true)
}

/// Check version and length; returns `(timestamp, entries)`.
fn split_row(value: &[u8], header: usize, entry: usize) -> Result<(u64, &[u8]), TradeRowError> {
    let bad_len = TradeRowError::Length(value.len());
    if value.len() < header {
        return Err(bad_len);
    }
    if value[0] != TRADE_ROW_VERSION {
        return Err(TradeRowError::Version(value[0]));
    }
    let timestamp = u64::from_le_bytes(value[1..9].try_into().unwrap());
    let n = match header - 9 {
        2 => u16::from_le_bytes(value[9..11].try_into().unwrap()) as usize,
        _ => u32::from_le_bytes(value[9..13].try_into().unwrap()) as usize,
    };
    let body = &value[header..];
    if n.checked_mul(entry) != Some(body.len()) {
        return Err(bad_len);
    }
    Ok((timestamp, body))
}

fn i128_at(b: &[u8], at: usize) -> i128 {
    i128::from_le_bytes(b[at..at + 16].try_into().unwrap())
}

/// Decode a `CF_NATIVE_TRADES` value into `(timestamp, fills)`.
pub fn decode_trade_row(value: &[u8]) -> Result<(u64, Vec<MarketTrade>), TradeRowError> {
    let (timestamp, body) = split_row(value, TRADE_HEADER, TRADE_ENTRY)?;
    let fills = body
        .chunks_exact(TRADE_ENTRY)
        .map(|e| MarketTrade {
            trade_index: u32::from_le_bytes(e[..4].try_into().unwrap()),
            price_raw: i128_at(e, 4),
            qty_raw: i128_at(e, 20),
            taker_side: e[36],
        })
        .collect();
    Ok((timestamp, fills))
}

/// Decode a `CF_NATIVE_USER_TRADES` value into `(timestamp, entries)`.
pub fn decode_user_row(value: &[u8]) -> Result<(u64, Vec<UserTrade>), TradeRowError> {
    let (timestamp, body) = split_row(value, USER_HEADER, USER_ENTRY)?;
    let entries = body
        .chunks_exact(USER_ENTRY)
        .map(|e| UserTrade {
            trade_index: u32::from_le_bytes(e[..4].try_into().unwrap()),
            market: u64::from_le_bytes(e[4..12].try_into().unwrap()),
            price_raw: i128_at(e, 12),
            qty_raw: i128_at(e, 28),
            taker_side: e[44],
            role: e[45],
        })
        .collect();
    Ok((timestamp, entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::repeat_byte(b)
    }

    fn fill(i: u32, market: u64, maker: u8, taker: u8) -> TradeFill {
        TradeFill {
            trade_index: i,
            market,
            maker: addr(maker),
            taker: addr(taker),
            price_raw: 1_000 + i as i128,
            qty_raw: -(i as i128) - 7,
            taker_side: (i % 2) as u8,
            // Stream-only fields: non-zero so the row tests show they are ignored.
            maker_order_id: 10 + i as u128,
            taker_order_id: 20 + i as u128,
            maker_start_raw: -3,
            taker_start_raw: 4,
            maker_pnl_raw: 5,
            taker_pnl_raw: -6,
        }
    }

    fn encode(block: u64, ts: u64, fills: &[TradeFill]) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
        let mut out = PackedCfBatch::default();
        encode_block(block, ts, fills, &mut out);
        out.into_raw()
    }

    #[test]
    fn round_trip_both_row_kinds() {
        // Two markets interleaved; trader 3 is maker in one fill, taker in another.
        let fills = [fill(0, 9, 1, 3), fill(1, 2, 3, 4), fill(2, 9, 1, 4)];
        let rows = encode(77, 1_700, &fills);

        let market_rows: Vec<_> = rows.iter().filter(|r| r.0 == CF_NATIVE_TRADES).collect();
        let keys: Vec<_> = market_rows.iter().map(|r| parse_trade_key(&r.1).unwrap()).collect();
        assert_eq!(keys, vec![(2, 77, 0), (9, 77, 0)]);
        let (ts, m9) = decode_trade_row(&market_rows[1].2).unwrap();
        assert_eq!(ts, 1_700);
        assert_eq!(
            m9,
            vec![
                MarketTrade { trade_index: 0, price_raw: 1_000, qty_raw: -7, taker_side: 0 },
                MarketTrade { trade_index: 2, price_raw: 1_002, qty_raw: -9, taker_side: 0 },
            ]
        );

        let user_rows: Vec<_> = rows.iter().filter(|r| r.0 == CF_NATIVE_USER_TRADES).collect();
        let users: Vec<_> = user_rows.iter().map(|r| parse_user_trade_key(&r.1).unwrap()).collect();
        assert_eq!(users, vec![(addr(1), 77), (addr(3), 77), (addr(4), 77)]);
        let (ts, t3) = decode_user_row(&user_rows[1].2).unwrap();
        assert_eq!(ts, 1_700);
        assert_eq!(
            t3,
            vec![
                UserTrade { trade_index: 0, market: 9, price_raw: 1_000, qty_raw: -7, taker_side: 0, role: 1 },
                UserTrade { trade_index: 1, market: 2, price_raw: 1_001, qty_raw: -8, taker_side: 1, role: 0 },
            ]
        );
        // Row sizes are the fixed layouts: 11 + 37n and 13 + 46n.
        assert_eq!(market_rows[1].2.len(), 11 + 2 * 37);
        assert_eq!(user_rows[1].2.len(), 13 + 2 * 46);
    }

    #[test]
    fn market_rows_chunk_at_1024_fills() {
        let fills: Vec<_> = (0..2_500).map(|i| fill(i, 5, 1, 2)).collect();
        let rows = encode(3, 0, &fills);
        let chunks: Vec<_> = rows
            .iter()
            .filter(|r| r.0 == CF_NATIVE_TRADES)
            .map(|r| {
                let (_, _, chunk) = parse_trade_key(&r.1).unwrap();
                let (_, f) = decode_trade_row(&r.2).unwrap();
                (chunk, f.len(), f[0].trade_index)
            })
            .collect();
        assert_eq!(chunks, vec![(0, 1024, 0), (1, 1024, 1024), (2, 452, 2048)]);
    }

    #[test]
    fn self_trade_gives_maker_then_taker_entry() {
        let rows = encode(1, 0, &[fill(0, 1, 6, 6)]);
        let user: Vec<_> = rows.iter().filter(|r| r.0 == CF_NATIVE_USER_TRADES).collect();
        assert_eq!(user.len(), 1);
        let roles: Vec<_> = decode_user_row(&user[0].2).unwrap().1.iter().map(|e| e.role).collect();
        assert_eq!(roles, vec![0, 1]);
    }

    #[test]
    fn same_fills_same_bytes_and_empty_block_writes_nothing() {
        let fills: Vec<_> = (0..40).map(|i| fill(i, (i % 3) as u64, (i % 5) as u8, (i % 7) as u8 + 10)).collect();
        assert_eq!(encode(9, 4, &fills), encode(9, 4, &fills));
        assert!(encode(9, 4, &[]).is_empty());
    }

    #[test]
    fn user_keys_order_newest_block_first() {
        assert!(user_trade_key(&addr(1), 10) < user_trade_key(&addr(1), 9));
        assert!(trade_key(1, 9, 1) < trade_key(1, 10, 0));
    }

    #[test]
    fn format_marker_wipes_old_rows_once() {
        use crate::StateBackend;
        let dir = tempfile::tempdir().unwrap();
        let db = crate::StateDb::open(dir.path()).unwrap();
        // Old per-fill rows (20- and 32-byte keys), including an all-0xFF key.
        db.put_cf_raw(CF_NATIVE_TRADES, &[7; 20], &[1; 65]).unwrap();
        db.put_cf_raw(CF_NATIVE_USER_TRADES, &[0xFF; 32], &[1; 74]).unwrap();
        db.put_cf_raw(CF_CONSENSUS_META, b"other", b"kept").unwrap();

        assert!(ensure_trade_history_format(&db).unwrap(), "old format: wiped");
        for cf in [CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES] {
            assert!(db.iterate_cf(cf, None).unwrap().is_empty(), "{cf} wiped");
        }
        assert_eq!(
            db.get_cf_raw(CF_CONSENSUS_META, META_TRADE_HISTORY_FORMAT).unwrap(),
            Some(vec![TRADE_HISTORY_FORMAT])
        );
        assert_eq!(db.get_cf_raw(CF_CONSENSUS_META, b"other").unwrap(), Some(b"kept".to_vec()));

        // New rows survive the next start.
        let mut rows = PackedCfBatch::default();
        encode_block(1, 0, &[fill(0, 1, 2, 3)], &mut rows);
        for (cf, k, v) in rows.iter() {
            db.put_cf_raw(cf, k, v).unwrap();
        }
        assert!(!ensure_trade_history_format(&db).unwrap(), "current format: untouched");
        assert_eq!(db.iterate_cf(CF_NATIVE_TRADES, None).unwrap().len(), 1);
        assert_eq!(db.iterate_cf(CF_NATIVE_USER_TRADES, None).unwrap().len(), 2);
    }

    #[test]
    fn decode_rejects_bad_rows_without_panicking() {
        let rows = encode(1, 0, &[fill(0, 1, 2, 3)]);
        let (m, u) = (&rows[0].2, &rows[1].2);
        assert!(decode_trade_row(m).is_ok() && decode_user_row(u).is_ok());
        for bad in [&m[..m.len() - 1], &m[..5], &[][..]] {
            assert!(matches!(decode_trade_row(bad), Err(TradeRowError::Length(_))));
        }
        let mut wrong_ver = m.clone();
        wrong_ver[0] = 2;
        assert_eq!(decode_trade_row(&wrong_ver), Err(TradeRowError::Version(2)));
        assert!(decode_user_row(&u[..u.len() - 3]).is_err());
        // A market row is not a valid user row and vice versa.
        assert!(decode_user_row(m).is_err());
        assert!(decode_trade_row(u).is_err());
        assert!(parse_trade_key(&[0; 20]).is_none());
        assert!(parse_user_trade_key(&[0; 32]).is_none());
    }
}
