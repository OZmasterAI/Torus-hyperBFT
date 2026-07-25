//! Layout-aware READ paths for the persisted order books.
//!
//! `cf_native_order_books` holds one of THREE layouts, chosen at genesis by
//! `TORUS_BOOK_ROWS` (see `torus_bridge::native_executor::BookMode`):
//!
//! | mode | root CF `cf_native_order_books`            | node-local `cf_book_order_rows` |
//! |------|-------------------------------------------|---------------------------------|
//! | 0    | whole-book borsh blob, 8-byte key         | —                               |
//! | 1    | meta + per-order + stop rows              | —                               |
//! | 2    | meta + stop + price-LEVEL rows            | per-order rows                  |
//!
//! Every reader (`torus_getOrderBook`, `torus_getOpenOrders`,
//! `torus_getMarkPrice`, precompile `0x0800`) used to assume layout 0 and so
//! returned an EMPTY book — with no error — on a mode-1/2 node. This module is
//! the single place that knows how to read all three.
//!
//! Design rules (all deliberate):
//!   * **Never read the env var.** A reader must serve whatever is actually on
//!     disk; the process flag belongs to the writer. The layout comes from the
//!     `__book_mode__` marker row, else from content sniffing on key shape —
//!     the same discipline the executor's loaders use.
//!   * **Never silently empty.** A layout that cannot be decoded is an ERROR.
//!     Only a genuinely absent market yields an empty book.
//!   * **Read-only.** Nothing here writes, mutates, or touches the consensus
//!     state root.
//!   * Mode-2 DEPTH is served from the ROOT-CF level rows (the consensus
//!     aggregate: qty ‖ count ‖ level_hash), so it does not depend on the
//!     node-local store at all. Per-trader ORDERS need the node-local rows —
//!     the root CF does not carry order identity beyond the level hash.

use std::collections::BTreeMap;

use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS};
use torus_state::{StateBackend, StateError};
use torus_types::{Address, FixedPoint, MarketId, Side};

use crate::book_rows::{
    book_meta_key, price_dec, side_from_tag, BookMetaRow, LevelRowData, LEVEL_ROW_VALUE_LEN,
    ROW_TAG_LEVEL, ROW_TAG_META, ROW_TAG_ORDER, ROW_TAG_STOP,
};
use crate::error::CoreError;
use crate::order_book::{Order, OrderBook};

/// Node-local mode marker row in `CF_NATIVE_MARKETS` (written by the executor
/// on its first save; key length != 8 so every market reader skips it).
pub const BOOK_MODE_MARKER_KEY: &[u8] = b"__book_mode__";

/// The on-disk book layout, as observed — never as configured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookLayout {
    /// Whole-book borsh blob under an 8-byte market key.
    Classic,
    /// Per-order rows in the root CF (`TORUS_BOOK_ROWS=1`).
    OrderRows,
    /// Level rows in the root CF + per-order rows in the node-local store
    /// (`TORUS_BOOK_ROWS=2`).
    LevelAuthority,
}

impl BookLayout {
    /// Marker-byte discriminant (mirrors `BookMode::marker_byte`).
    pub fn from_marker_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(BookLayout::Classic),
            1 => Some(BookLayout::OrderRows),
            2 => Some(BookLayout::LevelAuthority),
            _ => None,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            BookLayout::Classic => "classic whole-book blobs",
            BookLayout::OrderRows => "per-order rows (TORUS_BOOK_ROWS=1)",
            BookLayout::LevelAuthority => "level authority (TORUS_BOOK_ROWS=2)",
        }
    }

    /// True for the two row layouts (1 and 2).
    pub fn is_rows(self) -> bool {
        !matches!(self, BookLayout::Classic)
    }
}

/// One aggregated price level, best-first within its side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthLevel {
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    pub order_count: u32,
}

/// Aggregated book depth: bids highest-first, asks lowest-first (the same
/// order as `OrderBook::bid_depth` / `ask_depth`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookDepth {
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
}

impl BookDepth {
    pub fn is_empty(&self) -> bool {
        self.bids.is_empty() && self.asks.is_empty()
    }
}

fn layout_err(msg: impl Into<String>) -> CoreError {
    CoreError::BookLayout(msg.into())
}

/// Raw `(key, value)` rows straight off a column family.
type RawRows = Vec<(Vec<u8>, Vec<u8>)>;

/// `iterate_cf` that tolerates a column family an older DB never created
/// (treated as empty) but surfaces every other storage failure.
fn iterate_optional_cf<S: StateBackend>(
    state: &S,
    cf: &'static str,
    prefix: Option<&[u8]>,
) -> Result<RawRows, CoreError> {
    match state.iterate_cf(cf, prefix) {
        Ok(rows) => Ok(rows),
        Err(StateError::MissingColumnFamily(_)) => Ok(Vec::new()),
        Err(e) => Err(CoreError::State(e)),
    }
}

/// Classify a single `cf_native_order_books` key.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyShape {
    ClassicBlob,
    Meta,
    OrderRow,
    StopRow,
    LevelRow,
}

fn key_shape(key: &[u8]) -> Option<KeyShape> {
    match key.len() {
        8 => Some(KeyShape::ClassicBlob),
        9 if key[8] == ROW_TAG_META => Some(KeyShape::Meta),
        25 if key[8] == ROW_TAG_ORDER => Some(KeyShape::OrderRow),
        25 if key[8] == ROW_TAG_STOP => Some(KeyShape::StopRow),
        26 if key[8] == ROW_TAG_LEVEL => Some(KeyShape::LevelRow),
        _ => None,
    }
}

/// The layout the DB is ACTUALLY written in.
///
/// 1. `__book_mode__` marker row (authoritative; written on the executor's
///    first save under every mode). A marker byte that is not a known mode is
///    an error — a reader must not guess.
/// 2. Pre-marker DBs: sniff key shapes across `cf_native_order_books`.
///    Mixed layouts, or a key shape belonging to no layout, are errors.
///    Meta/stop rows alone (a market with no resting orders) are disambiguated
///    by the node-local order store; if that is empty too, both row layouts
///    read back identically, so `OrderRows` is returned.
pub fn detect_layout<S: StateBackend>(state: &S) -> Result<BookLayout, CoreError> {
    if let Some(bytes) = state.get_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY)? {
        return match (bytes.len() == 1)
            .then(|| BookLayout::from_marker_byte(bytes[0]))
            .flatten()
        {
            Some(layout) => Ok(layout),
            None => Err(layout_err(format!(
                "__book_mode__ marker row is not a known mode discriminant ({bytes:02x?}) \
                 — refusing to guess the on-disk book layout"
            ))),
        };
    }

    // Pre-marker DB (or a DB that has never saved a book): sniff.
    let mut saw_classic = false;
    let mut saw_order_row = false;
    let mut saw_level_row = false;
    let mut saw_meta = false;
    for (key, _) in iterate_optional_cf(state, CF_NATIVE_ORDER_BOOKS, None)? {
        match key_shape(&key) {
            Some(KeyShape::ClassicBlob) => saw_classic = true,
            Some(KeyShape::OrderRow) => saw_order_row = true,
            Some(KeyShape::LevelRow) => saw_level_row = true,
            Some(KeyShape::Meta) => saw_meta = true,
            Some(KeyShape::StopRow) => {}
            None => {
                return Err(layout_err(format!(
                    "unrecognized cf_native_order_books key (len {}) — the book CF holds \
                     a layout this build cannot read",
                    key.len()
                )))
            }
        }
    }

    let store_nonempty = !iterate_optional_cf(state, CF_BOOK_ORDER_ROWS, None)?.is_empty();

    if saw_classic && (saw_order_row || saw_level_row || saw_meta) {
        return Err(layout_err(
            "cf_native_order_books mixes classic whole-book blobs with row-layout keys \
             — the book CF is corrupt (a layout change needs a fresh genesis)",
        ));
    }
    if saw_order_row && saw_level_row {
        return Err(layout_err(
            "cf_native_order_books mixes per-order rows (mode 1) with level rows (mode 2) \
             — the book CF is corrupt (a layout change needs a fresh genesis)",
        ));
    }
    if saw_classic {
        return Ok(BookLayout::Classic);
    }
    if saw_level_row {
        return Ok(BookLayout::LevelAuthority);
    }
    if saw_order_row {
        return Ok(BookLayout::OrderRows);
    }
    if store_nonempty {
        if saw_meta {
            // Meta rows + a populated node-local store = mode 2 with every
            // level currently... impossible (orders imply levels), so this is
            // a stale/half-written CF.
            return Err(layout_err(
                "cf_book_order_rows holds order rows but cf_native_order_books has no \
                 level rows — split-brain between the node-local order store and the \
                 consensus root (corrupt DB)",
            ));
        }
        return Err(layout_err(
            "cf_book_order_rows is non-empty but cf_native_order_books has no book rows \
             at all — split-brain between the node-local order store and the consensus \
             root (corrupt DB)",
        ));
    }
    // Only meta/stop rows, or nothing at all: both row layouts (and, for an
    // empty CF, the classic layout) read back identically here.
    Ok(if saw_meta {
        BookLayout::OrderRows
    } else {
        BookLayout::Classic
    })
}

/// All `cf_native_order_books` rows for one market (prefix scan on the 8-byte
/// market id — covers the classic blob key and every tagged row).
fn market_rows<S: StateBackend>(state: &S, market_id: MarketId) -> Result<RawRows, CoreError> {
    iterate_optional_cf(
        state,
        CF_NATIVE_ORDER_BOOKS,
        Some(&market_id.to_be_bytes()),
    )
}

/// The raw classic whole-book blob, if the market has one. Callers own the
/// decode so each keeps its established classic semantics.
pub fn classic_blob<S: StateBackend>(
    state: &S,
    market_id: MarketId,
) -> Result<Option<Vec<u8>>, CoreError> {
    Ok(state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &market_id.to_be_bytes())?)
}

/// The market's meta row (modes 1/2), or `None` when the market has no book.
/// A market with rows but no meta row is an ERROR — its book cannot be
/// reconstructed and pretending it is empty is the bug this module fixes.
fn read_meta<S: StateBackend>(
    state: &S,
    market_id: MarketId,
    rows: &[(Vec<u8>, Vec<u8>)],
) -> Result<Option<BookMetaRow>, CoreError> {
    let mut meta = None;
    let mut has_other_rows = false;
    for (key, value) in rows {
        match key_shape(key) {
            Some(KeyShape::Meta) => {
                meta = Some(BookMetaRow::decode(value).map_err(|e| {
                    layout_err(format!("market {market_id}: {e}"))
                })?);
            }
            Some(KeyShape::ClassicBlob) => {
                return Err(layout_err(format!(
                    "market {market_id}: cf_native_order_books holds a classic whole-book \
                     blob under a row layout — the book CF is corrupt (a layout change \
                     needs a fresh genesis)"
                )))
            }
            Some(_) => has_other_rows = true,
            None => {
                return Err(layout_err(format!(
                    "market {market_id}: unrecognized cf_native_order_books key (len {})",
                    key.len()
                )))
            }
        }
    }
    if meta.is_none() && has_other_rows {
        return Err(layout_err(format!(
            "market {market_id} has order/stop/level rows but no meta row (corrupt row \
             store) — refusing to serve a partial book"
        )));
    }
    let _ = state;
    Ok(meta)
}

/// Aggregated depth for one market under `layout`.
///
/// * Classic — decodes the whole-book blob.
/// * Mode 1 — aggregates the market's per-order rows.
/// * Mode 2 — reads the ROOT-CF level rows (consensus aggregates); the
///   node-local order store is not consulted.
///
/// An absent market is an empty book (legitimate). Anything undecodable is an
/// error.
pub fn read_book_depth<S: StateBackend>(
    state: &S,
    market_id: MarketId,
    layout: BookLayout,
) -> Result<BookDepth, CoreError> {
    if layout == BookLayout::Classic {
        return match classic_blob(state, market_id)? {
            None => Ok(BookDepth::default()),
            Some(blob) => {
                let book = decode_classic_book(&blob, market_id)?;
                Ok(book_depth(&book))
            }
        };
    }

    let rows = market_rows(state, market_id)?;
    let meta = read_meta(state, market_id, &rows)?;
    if meta.is_none() {
        return Ok(BookDepth::default());
    }

    match layout {
        BookLayout::LevelAuthority => {
            // Level rows: forward key order is best-first per side, bids
            // before asks (see `book_rows::price_enc`).
            let mut depth = BookDepth::default();
            for (key, value) in &rows {
                if key_shape(key) != Some(KeyShape::LevelRow) {
                    continue;
                }
                if value.len() != LEVEL_ROW_VALUE_LEN {
                    return Err(layout_err(format!(
                        "market {market_id}: level row value len {} != {LEVEL_ROW_VALUE_LEN} \
                         (corrupt level row)",
                        value.len()
                    )));
                }
                let data = LevelRowData::decode(value)
                    .map_err(|e| layout_err(format!("market {market_id}: {e}")))?;
                let tag = key[9];
                let side = side_from_tag(tag).ok_or_else(|| {
                    layout_err(format!(
                        "market {market_id}: level row has unknown side tag {tag:#04x}"
                    ))
                })?;
                let level = DepthLevel {
                    price: FixedPoint::from_raw(price_dec(
                        tag,
                        &key[10..26].try_into().expect("26-byte level key"),
                    )),
                    quantity: FixedPoint::from_raw(data.total_qty_raw),
                    order_count: data.order_count,
                };
                match side {
                    Side::Buy => depth.bids.push(level),
                    Side::Sell => depth.asks.push(level),
                }
            }
            Ok(depth)
        }
        BookLayout::OrderRows => {
            let mut bids: BTreeMap<i128, (i128, u32)> = BTreeMap::new();
            let mut asks: BTreeMap<i128, (i128, u32)> = BTreeMap::new();
            for (key, value) in &rows {
                if key_shape(key) != Some(KeyShape::OrderRow) {
                    continue;
                }
                let (_seq, order) = decode_order_row(value, market_id)?;
                let side_map = match order.side {
                    Side::Buy => &mut bids,
                    Side::Sell => &mut asks,
                };
                let e = side_map.entry(order.price.raw()).or_insert((0, 0));
                e.0 += order.remaining_qty.raw();
                e.1 += 1;
            }
            let to_levels = |m: BTreeMap<i128, (i128, u32)>| -> Vec<DepthLevel> {
                m.into_iter()
                    .map(|(price, (qty, n))| DepthLevel {
                        price: FixedPoint::from_raw(price),
                        quantity: FixedPoint::from_raw(qty),
                        order_count: n,
                    })
                    .collect()
            };
            let mut bid_levels = to_levels(bids);
            bid_levels.reverse(); // best (highest) bid first
            Ok(BookDepth {
                bids: bid_levels,
                asks: to_levels(asks),
            })
        }
        BookLayout::Classic => unreachable!("handled above"),
    }
}

/// The market's last trade price.
///
/// Modes 1/2 read the meta row's `ltp_tag` / `ltp_raw` suffix — the value has
/// always been on disk; the readers just never looked there.
pub fn read_last_trade_price<S: StateBackend>(
    state: &S,
    market_id: MarketId,
    layout: BookLayout,
) -> Result<Option<FixedPoint>, CoreError> {
    if layout == BookLayout::Classic {
        return match classic_blob(state, market_id)? {
            None => Ok(None),
            Some(blob) => Ok(decode_classic_book(&blob, market_id)?.last_trade_price()),
        };
    }
    match state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &book_meta_key(market_id))? {
        None => Ok(None),
        Some(value) => Ok(BookMetaRow::decode(&value)
            .map_err(|e| layout_err(format!("market {market_id}: {e}")))?
            .last_trade_price),
    }
}

/// Resting orders belonging to `trader`, as `(market_id, order)`.
///
/// `market` restricts the scan to one market; `limit` caps the result (the RPC
/// handlers cap at 500). Ordering is deterministic: ascending
/// `(market_id, order_id)`.
///
/// Mode 2 reads the NODE-LOCAL `cf_book_order_rows`: the root CF commits only
/// level aggregates, so order identity lives outside consensus. A market whose
/// root CF commits levels while the node-local store has no rows for it is a
/// stale/lost store — an ERROR, never an empty list.
pub fn read_open_orders<S: StateBackend>(
    state: &S,
    trader: &Address,
    market: Option<MarketId>,
    layout: BookLayout,
    limit: usize,
) -> Result<Vec<(MarketId, Order)>, CoreError> {
    let mut out: Vec<(MarketId, Order)> = Vec::new();

    match layout {
        BookLayout::Classic => {
            let books: Vec<(MarketId, Vec<u8>)> = match market {
                Some(mid) => classic_blob(state, mid)?
                    .map(|b| vec![(mid, b)])
                    .unwrap_or_default(),
                None => iterate_optional_cf(state, CF_NATIVE_ORDER_BOOKS, None)?
                    .into_iter()
                    .filter(|(k, _)| k.len() == 8)
                    .map(|(k, v)| (u64::from_be_bytes(k[..8].try_into().unwrap()), v))
                    .collect(),
            };
            for (mid, blob) in books {
                let book = decode_classic_book(&blob, mid)?;
                for order in book.orders_for_trader(trader) {
                    out.push((mid, order.clone()));
                }
            }
        }
        BookLayout::OrderRows => {
            let rows = match market {
                Some(mid) => {
                    // Fail loud on a market whose rows exist without a meta row.
                    let rows = market_rows(state, mid)?;
                    read_meta(state, mid, &rows)?;
                    rows
                }
                None => iterate_optional_cf(state, CF_NATIVE_ORDER_BOOKS, None)?,
            };
            for (key, value) in &rows {
                if key_shape(key) != Some(KeyShape::OrderRow) {
                    continue;
                }
                let mid = u64::from_be_bytes(key[..8].try_into().unwrap());
                let (_seq, order) = decode_order_row(value, mid)?;
                check_row_id(key, &order, mid, CF_NATIVE_ORDER_BOOKS)?;
                if order.trader == *trader {
                    out.push((mid, order));
                }
            }
        }
        BookLayout::LevelAuthority => {
            let store = match market {
                Some(mid) => {
                    let rows = market_rows(state, mid)?;
                    let has_levels = rows
                        .iter()
                        .any(|(k, _)| key_shape(k) == Some(KeyShape::LevelRow));
                    read_meta(state, mid, &rows)?;
                    let store = iterate_optional_cf(
                        state,
                        CF_BOOK_ORDER_ROWS,
                        Some(&mid.to_be_bytes()),
                    )?;
                    if has_levels && store.is_empty() {
                        return Err(layout_err(format!(
                            "market {mid}: cf_native_order_books commits level rows but the \
                             node-local cf_book_order_rows store holds no orders for it — \
                             the order store is stale or lost (refusing to report an empty \
                             order list)"
                        )));
                    }
                    store
                }
                None => {
                    let root = iterate_optional_cf(state, CF_NATIVE_ORDER_BOOKS, None)?;
                    let store = iterate_optional_cf(state, CF_BOOK_ORDER_ROWS, None)?;
                    let mut markets_with_levels: Vec<MarketId> = root
                        .iter()
                        .filter(|(k, _)| key_shape(k) == Some(KeyShape::LevelRow))
                        .map(|(k, _)| u64::from_be_bytes(k[..8].try_into().unwrap()))
                        .collect();
                    markets_with_levels.sort_unstable();
                    markets_with_levels.dedup();
                    let mut markets_in_store: Vec<MarketId> = store
                        .iter()
                        .filter(|(k, _)| k.len() == 25 && k[8] == ROW_TAG_ORDER)
                        .map(|(k, _)| u64::from_be_bytes(k[..8].try_into().unwrap()))
                        .collect();
                    markets_in_store.sort_unstable();
                    markets_in_store.dedup();
                    if let Some(missing) = markets_with_levels
                        .iter()
                        .find(|mid| !markets_in_store.contains(mid))
                    {
                        return Err(layout_err(format!(
                            "market {missing}: cf_native_order_books commits level rows but \
                             the node-local cf_book_order_rows store holds no orders for it \
                             — the order store is stale or lost (refusing to report an empty \
                             order list)"
                        )));
                    }
                    store
                }
            };
            for (key, value) in &store {
                if key.len() != 25 || key[8] != ROW_TAG_ORDER {
                    return Err(layout_err(format!(
                        "unrecognized cf_book_order_rows key (len {}) — corrupt node-local \
                         order store",
                        key.len()
                    )));
                }
                let mid = u64::from_be_bytes(key[..8].try_into().unwrap());
                let (_seq, order) = decode_order_row(value, mid)?;
                check_row_id(key, &order, mid, CF_BOOK_ORDER_ROWS)?;
                if order.trader == *trader {
                    out.push((mid, order));
                }
            }
        }
    }

    out.sort_by_key(|(mid, o)| (*mid, o.id));
    out.truncate(limit);
    Ok(out)
}

/// Rebuild one market's full `OrderBook` from the row layouts (mode 1: root CF
/// order rows; mode 2: node-local order rows). Read-only. `None` when the
/// market has no book at all.
///
/// Insertion follows the executor's canonical order — bids ascending
/// `(price, seq)`, then asks — so the rebuilt book is queue-identical to the
/// executor's.
pub fn rebuild_book<S: StateBackend>(
    state: &S,
    market_id: MarketId,
    layout: BookLayout,
) -> Result<Option<OrderBook>, CoreError> {
    if layout == BookLayout::Classic {
        return match classic_blob(state, market_id)? {
            None => Ok(None),
            Some(blob) => Ok(Some(decode_classic_book(&blob, market_id)?)),
        };
    }

    let rows = market_rows(state, market_id)?;
    let Some(meta) = read_meta(state, market_id, &rows)? else {
        return Ok(None);
    };

    let order_rows: RawRows = match layout {
        BookLayout::OrderRows => rows
            .iter()
            .filter(|(k, _)| key_shape(k) == Some(KeyShape::OrderRow))
            .cloned()
            .collect(),
        BookLayout::LevelAuthority => {
            let store =
                iterate_optional_cf(state, CF_BOOK_ORDER_ROWS, Some(&market_id.to_be_bytes()))?;
            let has_levels = rows
                .iter()
                .any(|(k, _)| key_shape(k) == Some(KeyShape::LevelRow));
            if has_levels && store.is_empty() {
                return Err(layout_err(format!(
                    "market {market_id}: cf_native_order_books commits level rows but the \
                     node-local cf_book_order_rows store holds no orders for it — the order \
                     store is stale or lost (refusing to serve an empty book)"
                )));
            }
            store
        }
        BookLayout::Classic => unreachable!("handled above"),
    };

    let mut orders: Vec<(u64, Order)> = Vec::with_capacity(order_rows.len());
    for (key, value) in &order_rows {
        let (seq, order) = decode_order_row(value, market_id)?;
        check_row_id(key, &order, market_id, CF_NATIVE_ORDER_BOOKS)?;
        if seq >= meta.next_seq {
            return Err(layout_err(format!(
                "market {market_id}: order row seq {seq} >= meta next_seq {} (corrupt row \
                 store)",
                meta.next_seq
            )));
        }
        orders.push((seq, order));
    }

    let mut book = OrderBook::new(market_id, meta.tick_size, meta.lot_size);
    book.set_next_order_id(meta.next_id);
    book.set_last_trade_price(meta.last_trade_price);
    book.set_next_seq(meta.next_seq);

    // Canonical insertion order (mirrors the executor's `rebuild_book`).
    orders.sort_by(|a, b| {
        let rank = |o: &Order| u8::from(o.side == Side::Sell);
        (rank(&a.1), a.1.price, a.0).cmp(&(rank(&b.1), b.1.price, b.0))
    });
    for (seq, order) in orders {
        book.insert_loaded_order(order, seq);
    }

    let mut stops: Vec<(u128, Vec<u8>)> = rows
        .iter()
        .filter(|(k, _)| key_shape(k) == Some(KeyShape::StopRow))
        .map(|(k, v)| (u128::from_be_bytes(k[9..25].try_into().unwrap()), v.clone()))
        .collect();
    stops.sort_by_key(|(id, _)| *id);
    for (stop_id, bytes) in stops {
        match book.restore_stop_row(&bytes) {
            Ok(id) if id == stop_id => {}
            Ok(id) => {
                return Err(layout_err(format!(
                    "market {market_id}: stop row key id {stop_id} != payload id {id} \
                     (corrupt row store)"
                )))
            }
            Err(e) => return Err(layout_err(format!("market {market_id}: {e}"))),
        }
    }
    Ok(Some(book))
}

/// Aggregated depth of an in-memory book, in the reader's level type.
pub fn book_depth(book: &OrderBook) -> BookDepth {
    let to_levels = |d: Vec<(FixedPoint, FixedPoint, usize)>| -> Vec<DepthLevel> {
        d.into_iter()
            .map(|(price, quantity, n)| DepthLevel {
                price,
                quantity,
                order_count: n as u32,
            })
            .collect()
    };
    BookDepth {
        bids: to_levels(book.bid_depth()),
        asks: to_levels(book.ask_depth()),
    }
}

fn decode_classic_book(blob: &[u8], market_id: MarketId) -> Result<OrderBook, CoreError> {
    use borsh::BorshDeserialize;
    OrderBook::try_from_slice(blob).map_err(|e| {
        layout_err(format!(
            "market {market_id}: cf_native_order_books value is not a classic whole-book \
             blob: {e}"
        ))
    })
}

fn decode_order_row(value: &[u8], market_id: MarketId) -> Result<(u64, Order), CoreError> {
    OrderBook::decode_order_row(value)
        .map_err(|e| layout_err(format!("market {market_id}: book order row: {e}")))
}

/// The order id is in the key AND inside the payload — disagreement means the
/// row store is corrupt.
fn check_row_id(
    key: &[u8],
    order: &Order,
    market_id: MarketId,
    cf: &str,
) -> Result<(), CoreError> {
    let key_id = u128::from_be_bytes(key[9..25].try_into().expect("25-byte order row key"));
    if key_id != order.id {
        return Err(layout_err(format!(
            "market {market_id}: {cf} row key id {key_id} != payload id {} (corrupt row \
             store)",
            order.id
        )));
    }
    Ok(())
}
