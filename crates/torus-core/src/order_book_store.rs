//! Per-order-row order-book persistence in `CF_NATIVE_ORDER_BOOKS`.
//!
//! Deep-book storage round (swarm findings #7 + #8): the CF used to hold ONE
//! monolithic borsh-`OrderBook` value per market, making every save/load and
//! every incremental-trie touch O(all resting orders in the book). This
//! module replaces that with:
//!
//! - a **header row** per market — `market_id(8 BE) ‖ 0x00` → book metadata
//!   (tick/lot, `next_id`, `last_trade_price`, `next_seq`, pending stops);
//! - an **order row** per resting order — `market_id(8 BE) ‖ 0x01 ‖
//!   order_id(16 BE)` → `seq(8 BE) ‖ Order` (the frozen `Order` codec).
//!
//! Saving a block writes the header + one row per order TOUCHED this block
//! (placed / filled / cancelled / modified), and deletes are point deletes —
//! O(touched), not O(depth). The incremental native trie sees exactly those
//! small dirty keys, collapsing its per-block cost the same way.
//!
//! **STATE-ROOT PREIMAGE**: `CF_NATIVE_ORDER_BOOKS` is one of the 6
//! `NATIVE_ROOT_CFS`, so this keying IS the consensus preimage. The layout
//! is FROZEN once deployed; every node in a fleet must agree on it
//! (coordinated deploy; fresh-chain devnet-safe).
//!
//! **Determinism**: BE keys make iteration order lexicographic == numeric.
//! Reload rebuilds each book in the exact order the old monolithic codec
//! did (bids by price ascending, then asks, intra-level by insertion `seq`),
//! so matching order, price-time priority and `trader_orders` grouping are
//! byte-identical in behavior to a pre-round reload. The `seq` is explicit
//! because intra-level queue order is NOT ascending order-id (`modify_order`
//! requeues with the same id).
//!
//! **Legacy**: any 8-byte key in the CF is a pre-round monolithic value
//! (`OrderBook` or `OrderBookSnapshot` blob). Loading fails LOUDLY
//! ([`CoreError::LegacyOrderBookValue`]) — fresh chains / offline migration
//! only; there is deliberately NO live-migration path.

use std::collections::HashMap;

use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
use torus_state::StateBackend;
use torus_types::{FixedPoint, MarketId, OrderId, Side};

use crate::error::CoreError;
use crate::order_book::{Order, OrderBook};

/// Key-tag byte for the per-market header row.
pub const ROW_TAG_HEADER: u8 = 0x00;
/// Key-tag byte for per-order rows.
pub const ROW_TAG_ORDER: u8 = 0x01;
/// Key-tag byte for price-level aggregate rows (0x0800 top-N gas round).
pub const ROW_TAG_LEVEL: u8 = 0x02;

/// Side byte inside a level-row key. Bids sort before asks.
pub const SIDE_TAG_BID: u8 = 0x00;
pub const SIDE_TAG_ASK: u8 = 0x01;

/// Canonical side byte for level-row keys.
pub const fn side_tag(side: Side) -> u8 {
    match side {
        Side::Buy => SIDE_TAG_BID,
        Side::Sell => SIDE_TAG_ASK,
    }
}

/// Order-preserving price encoding for level-row keys: sign-flipped BE i128
/// (total order for signed prices), bitwise-NOT for bids — so forward
/// lexicographic iteration walks BOTH sides best-first (bids high→low,
/// asks low→high).
fn price_enc(tag: u8, raw_price: i128) -> [u8; 16] {
    let mut b = ((raw_price as u128) ^ (1u128 << 127)).to_be_bytes();
    if tag == SIDE_TAG_BID {
        for byte in &mut b {
            *byte = !*byte;
        }
    }
    b
}

/// Level row key from raw journal parts: `market_id(8 BE) ‖ 0x02 ‖ side(1) ‖
/// price_enc(16)` (26 bytes — length-disjoint from 8/9/25 layouts).
fn level_row_key_tagged(market_id: MarketId, tag: u8, raw_price: i128) -> [u8; 26] {
    let mut k = [0u8; 26];
    k[..8].copy_from_slice(&market_id.to_be_bytes());
    k[8] = ROW_TAG_LEVEL;
    k[9] = tag;
    k[10..].copy_from_slice(&price_enc(tag, raw_price));
    k
}

/// Level row key: `market_id(8 BE) ‖ 0x02 ‖ side(1) ‖ price_enc(16)`.
pub fn level_row_key(market_id: MarketId, side: Side, price: FixedPoint) -> [u8; 26] {
    level_row_key_tagged(market_id, side_tag(side), price.raw())
}

/// Header row key: `market_id(8 BE) ‖ 0x00` (9 bytes — length-disjoint from
/// both legacy keys (8) and order rows (25), so misreads are impossible).
pub fn header_key(market_id: MarketId) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[..8].copy_from_slice(&market_id.to_be_bytes());
    k[8] = ROW_TAG_HEADER;
    k
}

/// Order row key: `market_id(8 BE) ‖ 0x01 ‖ order_id(16 BE)` (25 bytes).
pub fn order_row_key(market_id: MarketId, order_id: OrderId) -> [u8; 25] {
    let mut k = [0u8; 25];
    k[..8].copy_from_slice(&market_id.to_be_bytes());
    k[8] = ROW_TAG_ORDER;
    k[9..].copy_from_slice(&order_id.to_be_bytes());
    k
}

fn key_hex(key: &[u8]) -> String {
    let mut s = String::with_capacity(2 + key.len() * 2);
    s.push_str("0x");
    for b in key {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Classified CF row.
enum Row {
    Header(MarketId, Vec<u8>),
    Order(MarketId, OrderId, Vec<u8>),
    /// Price-level aggregate row — derived read-side data; the book is
    /// assembled from order rows only, so loads skip these.
    Level,
}

/// Classify a raw CF entry, failing LOUDLY on legacy monolithic keys and on
/// unrecognized layouts.
fn classify(key: &[u8], value: Vec<u8>) -> Result<Row, CoreError> {
    match key.len() {
        8 => Err(CoreError::LegacyOrderBookValue {
            key_hex: key_hex(key),
        }),
        9 if key[8] == ROW_TAG_HEADER => Ok(Row::Header(
            u64::from_be_bytes(key[..8].try_into().unwrap()),
            value,
        )),
        25 if key[8] == ROW_TAG_ORDER => Ok(Row::Order(
            u64::from_be_bytes(key[..8].try_into().unwrap()),
            u128::from_be_bytes(key[9..25].try_into().unwrap()),
            value,
        )),
        26 if key[8] == ROW_TAG_LEVEL => Ok(Row::Level),
        _ => Err(CoreError::CorruptOrderBookRow {
            key_hex: key_hex(key),
        }),
    }
}

/// Rebuild one book from its header + `(seq, order)` rows, replicating the
/// old monolithic deserialize order exactly: bids by price ascending, then
/// asks by price ascending, intra-level by `seq` ascending (queue order).
fn assemble(
    market_id: MarketId,
    header: Vec<u8>,
    mut orders: Vec<(u64, Order)>,
) -> Result<OrderBook, CoreError> {
    let mut book = OrderBook::from_header_bytes(market_id, &header)
        .map_err(|e| CoreError::Borsh(format!("order-book header (market {market_id}): {e}")))?;
    orders.sort_by(|(sa, a), (sb, b)| {
        let rank = |o: &Order| u8::from(o.side == Side::Sell); // bids first
        (rank(a), a.price, *sa).cmp(&(rank(b), b.price, *sb))
    });
    for (seq, order) in orders {
        book.insert_loaded_order(order, seq);
    }
    Ok(book)
}

/// Load one market's book. `Ok(None)` if the market has no header row.
pub fn load_book<B: StateBackend>(
    state: &B,
    market_id: MarketId,
) -> Result<Option<OrderBook>, CoreError> {
    let prefix = market_id.to_be_bytes();
    let entries = state.iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&prefix))?;
    let mut header: Option<Vec<u8>> = None;
    let mut orders: Vec<(u64, Order)> = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        match classify(&key, value)? {
            Row::Header(_, bytes) => header = Some(bytes),
            Row::Order(_, order_id, bytes) => {
                let (seq, order) = OrderBook::decode_order_row(&bytes).map_err(|e| {
                    CoreError::Borsh(format!("order row {} decode: {e}", key_hex(&key)))
                })?;
                if order.id != order_id {
                    return Err(CoreError::CorruptOrderBookRow {
                        key_hex: key_hex(&key),
                    });
                }
                orders.push((seq, order));
            }
            Row::Level => {}
        }
    }
    match header {
        Some(h) => Ok(Some(assemble(market_id, h, orders)?)),
        None if orders.is_empty() => Ok(None),
        None => Err(CoreError::CorruptOrderBookRow {
            key_hex: format!("orphan order rows for market {market_id} (no header)"),
        }),
    }
}

/// Load EVERY book in the CF (exec startup / per-block context build).
pub fn load_all_books<B: StateBackend>(
    state: &B,
) -> Result<HashMap<MarketId, OrderBook>, CoreError> {
    let entries = state.iterate_cf(CF_NATIVE_ORDER_BOOKS, None)?;
    #[allow(clippy::type_complexity)]
    let mut grouped: HashMap<MarketId, (Option<Vec<u8>>, Vec<(u64, Order)>)> = HashMap::new();
    for (key, value) in entries {
        match classify(&key, value)? {
            Row::Header(mid, bytes) => grouped.entry(mid).or_default().0 = Some(bytes),
            Row::Order(mid, order_id, bytes) => {
                let (seq, order) = OrderBook::decode_order_row(&bytes).map_err(|e| {
                    CoreError::Borsh(format!("order row {} decode: {e}", key_hex(&key)))
                })?;
                if order.id != order_id {
                    return Err(CoreError::CorruptOrderBookRow {
                        key_hex: key_hex(&key),
                    });
                }
                grouped.entry(mid).or_default().1.push((seq, order));
            }
            Row::Level => {}
        }
    }
    let mut books = HashMap::with_capacity(grouped.len());
    for (mid, (header, orders)) in grouped {
        let Some(h) = header else {
            return Err(CoreError::CorruptOrderBookRow {
                key_hex: format!("orphan order rows for market {mid} (no header)"),
            });
        };
        books.insert(mid, assemble(mid, h, orders)?);
    }
    Ok(books)
}

/// `(bids, asks)` as `(price, total_qty)` pairs, best-first per side.
pub type LevelPairs = (
    Vec<(FixedPoint, FixedPoint)>,
    Vec<(FixedPoint, FixedPoint)>,
);

/// Best `n` price levels per side via the aggregate level rows — O(n) point
/// reads per side (bounded scan, no full-book load), best-first on both
/// sides (bids high→low, asks low→high). Returns `(bids, asks)` as
/// `(price, total_qty)` pairs. Absent market / empty book → empty vecs; a
/// LEGACY monolithic value fails loudly like every other load path.
pub fn load_top_levels<B: StateBackend>(
    state: &B,
    market_id: MarketId,
    n: usize,
) -> Result<LevelPairs, CoreError> {
    let read_side = |tag: u8| -> Result<Vec<(FixedPoint, FixedPoint)>, CoreError> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let mut prefix = [0u8; 10];
        prefix[..8].copy_from_slice(&market_id.to_be_bytes());
        prefix[8] = ROW_TAG_LEVEL;
        prefix[9] = tag;
        let entries = state.iterate_cf_bounded(CF_NATIVE_ORDER_BOOKS, Some(&prefix), n)?;
        let mut levels = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            if key.len() != 26 || value.len() != 16 {
                return Err(CoreError::CorruptOrderBookRow {
                    key_hex: key_hex(&key),
                });
            }
            let mut enc: [u8; 16] = key[10..26].try_into().unwrap();
            if tag == SIDE_TAG_BID {
                for b in &mut enc {
                    *b = !*b;
                }
            }
            let price = (u128::from_be_bytes(enc) ^ (1u128 << 127)) as i128;
            let qty = i128::from_be_bytes(value.as_slice().try_into().unwrap());
            levels.push((FixedPoint::from_raw(price), FixedPoint::from_raw(qty)));
        }
        Ok(levels)
    };
    let bids = read_side(SIDE_TAG_BID)?;
    let asks = read_side(SIDE_TAG_ASK)?;
    if bids.is_empty() && asks.is_empty() {
        // Loud legacy check: a pre-round monolithic value at the bare 8-byte
        // key MUST not be silently reported as an empty book.
        if state
            .get_cf_raw(CF_NATIVE_ORDER_BOOKS, &market_id.to_be_bytes())?
            .is_some()
        {
            return Err(CoreError::LegacyOrderBookValue {
                key_hex: key_hex(&market_id.to_be_bytes()),
            });
        }
    }
    Ok((bids, asks))
}

/// Read ONLY a market's `last_trade_price` (header decode; no rows touched).
pub fn load_last_trade_price<B: StateBackend>(
    state: &B,
    market_id: MarketId,
) -> Result<Option<FixedPoint>, CoreError> {
    match state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &header_key(market_id))? {
        Some(bytes) => {
            let shell = OrderBook::from_header_bytes(market_id, &bytes)
                .map_err(|e| CoreError::Borsh(format!("order-book header: {e}")))?;
            Ok(shell.last_trade_price())
        }
        None => {
            // Loud legacy check: a pre-round monolithic value would sit at the
            // bare 8-byte key and MUST not be silently reported as "no book".
            if state
                .get_cf_raw(CF_NATIVE_ORDER_BOOKS, &market_id.to_be_bytes())?
                .is_some()
            {
                return Err(CoreError::LegacyOrderBookValue {
                    key_hex: key_hex(&market_id.to_be_bytes()),
                });
            }
            Ok(None)
        }
    }
}

/// Stats returned by the save paths (telemetry / test hooks).
/// `rows_*` count ORDER rows; level aggregate rows are counted separately
/// (`levels_*`). `bytes_written` covers header + order rows + level rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveStats {
    pub rows_written: usize,
    pub rows_deleted: usize,
    pub bytes_written: u64,
    pub levels_written: usize,
    pub levels_deleted: usize,
}

/// Persist a book's delta since the last save: the header plus one
/// upsert/delete per TOUCHED order. O(touched this block), not O(depth).
pub fn save_book_delta<B: StateBackend>(
    state: &B,
    book: &mut OrderBook,
) -> Result<SaveStats, CoreError> {
    let market_id = book.market_id;
    let mut stats = SaveStats::default();

    let header = book.encode_header();
    stats.bytes_written += header.len() as u64;
    state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &header_key(market_id), &header)?;

    for (order_id, op) in book.take_row_ops() {
        let key = order_row_key(market_id, order_id);
        match op {
            Some(bytes) => {
                stats.rows_written += 1;
                stats.bytes_written += bytes.len() as u64;
                state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &bytes)?;
            }
            None => {
                stats.rows_deleted += 1;
                state.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &key)?;
            }
        }
    }

    // Price-level aggregate rows: one upsert/delete per level TOUCHED this
    // block. Same O(touched) shape as the order rows above.
    for ((tag, raw_price), op) in book.take_level_ops() {
        let key = level_row_key_tagged(market_id, tag, raw_price);
        match op {
            Some(qty) => {
                stats.levels_written += 1;
                stats.bytes_written += 16;
                state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &qty.raw().to_be_bytes())?;
            }
            None => {
                stats.levels_deleted += 1;
                state.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &key)?;
            }
        }
    }
    Ok(stats)
}

/// Persist a book IN FULL: header + a row for every resting order, deleting
/// any stale rows already in the CF for this market. For genesis seeding,
/// tests and the offline migration — the block path uses [`save_book_delta`].
pub fn save_book_full<B: StateBackend>(
    state: &B,
    book: &mut OrderBook,
) -> Result<SaveStats, CoreError> {
    let market_id = book.market_id;
    let mut stats = SaveStats::default();

    // Reconcile: delete every existing key under this market's prefix that
    // the fresh write below does not overwrite.
    let prefix = market_id.to_be_bytes();
    let existing = state.iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&prefix))?;
    let ops = book.full_row_ops();
    let level_ops = book.full_level_ops();
    let keep: std::collections::HashSet<Vec<u8>> = ops
        .iter()
        .map(|(id, _)| order_row_key(market_id, *id).to_vec())
        .chain(
            level_ops
                .iter()
                .map(|((tag, raw), _)| level_row_key_tagged(market_id, *tag, *raw).to_vec()),
        )
        .chain(std::iter::once(header_key(market_id).to_vec()))
        .collect();
    for (key, _) in existing {
        if !keep.contains(&key) {
            stats.rows_deleted += 1;
            state.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &key)?;
        }
    }

    let header = book.encode_header();
    stats.bytes_written += header.len() as u64;
    state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &header_key(market_id), &header)?;
    for (order_id, bytes) in ops {
        stats.rows_written += 1;
        stats.bytes_written += bytes.len() as u64;
        state.put_cf_raw(
            CF_NATIVE_ORDER_BOOKS,
            &order_row_key(market_id, order_id),
            &bytes,
        )?;
    }
    for ((tag, raw_price), qty) in level_ops {
        stats.levels_written += 1;
        stats.bytes_written += 16;
        state.put_cf_raw(
            CF_NATIVE_ORDER_BOOKS,
            &level_row_key_tagged(market_id, tag, raw_price),
            &qty.raw().to_be_bytes(),
        )?;
    }
    Ok(stats)
}
