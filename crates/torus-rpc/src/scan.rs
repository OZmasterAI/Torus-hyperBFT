//! R24: bounded work for public read RPCs.
//!
//! A response-byte cap (`TORUS_RPC_MAX_RESPONSE_MB`) does not bound the work a
//! read does BEFORE it answers: a small `torus_getOpenInterest` reply used to
//! walk every position row. Every read here either seeks to the rows it needs
//! (a market prefix, the best N book levels) or stops after
//! `TORUS_RPC_MAX_SCAN_ROWS` rows with [`RpcError::ScanLimitExceeded`]
//! (`-32005`), instead of finishing the scan first.
//!
//! A "row" is one key the RocksDB iterator returns (or one seek), whether or
//! not it is kept: filtered rows cost the same to read.

use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

use borsh::BorshDeserialize;
use torus_core::book_reader::{
    self, BookDepth, BookLayout, CloidOrder, LayoutSniff, MarketRowsCheck, BOOK_MODE_MARKER_KEY,
};
use torus_core::book_rows::{ROW_TAG_LEVEL, ROW_TAG_ORDER, ROW_TAG_STOP};
use torus_core::error::CoreError;
use torus_core::order_book::{Order, OrderBook};
use torus_core::position::{decode_position_row, Position};
use torus_economics::governance::{Proposal, ProposalStatus};
use torus_economics::types::ValidatorState;
use torus_economics::EconomicsError;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_GOVERNANCE_PROPOSALS, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_STAKING_VALIDATORS,
};
use torus_state::StateDb;
use torus_types::{Address, Cloid, FixedPoint, MarketId};

use crate::error::RpcError;
use crate::RpcState;

/// Default `TORUS_RPC_MAX_SCAN_ROWS`: rows one public read may examine.
pub(crate) const DEFAULT_MAX_SCAN_ROWS: usize = 250_000;
/// Env var: rows one public read may examine (R24).
pub(crate) const ENV_MAX_SCAN_ROWS: &str = "TORUS_RPC_MAX_SCAN_ROWS";
/// `torus_getOrderBook` levels per side: the default and the most a `depth`
/// parameter may ask for.
pub(crate) const ORDER_BOOK_MAX_LEVELS: usize = 1000;

/// Resolve `TORUS_RPC_MAX_SCAN_ROWS` from a raw env value. Pure (takes the raw
/// string) so it is testable without env races. Empty / unset / unparsable /
/// zero fall back to [`DEFAULT_MAX_SCAN_ROWS`].
pub(crate) fn resolve_max_scan_rows(raw: Option<&str>) -> usize {
    raw.map(str::trim)
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_SCAN_ROWS)
}

/// The rows one read may still examine. Dropping it adds the rows it counted
/// to the server's `scan_rows_examined` total (error paths included).
pub(crate) struct ScanBudget {
    max: usize,
    examined: usize,
    sink: Option<Arc<AtomicU64>>,
}

impl ScanBudget {
    /// A budget of `max` rows, not reported anywhere.
    #[cfg(test)]
    pub(crate) fn new(max: usize) -> Self {
        Self {
            max,
            examined: 0,
            sink: None,
        }
    }

    /// Count one examined row; `ScanLimitExceeded` once more than `max` rows
    /// were examined (so a read touches at most `max + 1` rows).
    pub(crate) fn step(&mut self) -> Result<(), RpcError> {
        self.examined += 1;
        if self.examined > self.max {
            return Err(RpcError::ScanLimitExceeded { limit: self.max });
        }
        Ok(())
    }

    /// No limit, not reported (a library caller outside a request).
    pub(crate) fn unlimited() -> Self {
        Self {
            max: usize::MAX,
            examined: 0,
            sink: None,
        }
    }

    /// Rows examined so far.
    #[cfg(test)]
    pub(crate) fn examined(&self) -> usize {
        self.examined
    }
}

impl Drop for ScanBudget {
    fn drop(&mut self) {
        if let Some(sink) = &self.sink {
            sink.fetch_add(self.examined as u64, Relaxed);
        }
    }
}

impl RpcState {
    /// A fresh per-request budget of `max_scan_rows`, reported to
    /// `scan_rows_examined` when dropped.
    pub(crate) fn scan_budget(&self) -> ScanBudget {
        ScanBudget {
            max: self.max_scan_rows,
            examined: 0,
            sink: Some(self.scan_rows_examined.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Bounded readers
// ---------------------------------------------------------------------------

/// A value that holds many records under one key (a classic whole-book blob,
/// a packed user-trade row) is charged one row per started
/// `CLASSIC_BLOB_BYTES_PER_ROW` bytes (about one order), before it is decoded:
/// its size is the work, and it cannot be read in part.
pub(crate) const CLASSIC_BLOB_BYTES_PER_ROW: usize = 128;

fn rocks(e: rocksdb::Error) -> RpcError {
    RpcError::Internal(format!("rocksdb: {e}"))
}

fn internal(msg: impl Into<String>) -> RpcError {
    RpcError::Internal(msg.into())
}

/// A book reader error, as the unbounded readers report it.
fn core(e: CoreError) -> RpcError {
    RpcError::Internal(e.to_string())
}

/// Rows charged for a packed value (classic blob, packed user-trade row) of
/// `len` bytes.
pub(crate) fn packed_rows(len: usize) -> usize {
    len.div_ceil(CLASSIC_BLOB_BYTES_PER_ROW).max(1)
}

impl ScanBudget {
    /// Charge `n` rows at once (a classic blob, before decoding it). Refused
    /// without charging past the limit when fewer than `n` rows are left.
    pub(crate) fn step_n(&mut self, n: usize) -> Result<(), RpcError> {
        if n > self.max.saturating_sub(self.examined) {
            self.examined = self.max + 1;
            return Err(RpcError::ScanLimitExceeded { limit: self.max });
        }
        self.examined += n;
        Ok(())
    }
}

type Flow = ControlFlow<()>;
const GO: Flow = ControlFlow::Continue(());
const STOP: Flow = ControlFlow::Break(());

/// A walk run to its end or stopped early: either way, done.
fn done(walk: Result<Flow, RpcError>) -> Result<(), RpcError> {
    walk.map(drop)
}

/// One RocksDB snapshot shared by the reads of one answer (`None` = each
/// read sees the latest commit).
pub(crate) type Snap<'a, 's> = Option<&'a rocksdb::Snapshot<'s>>;

/// s139: the native applied height (`META_NATIVE_APPLIED_HEIGHT`, the eth
/// head's source) as of `snap`; 0 when absent.
pub(crate) fn applied_height_at(state: &StateDb, snap: &rocksdb::Snapshot<'_>) -> Result<u64, RpcError> {
    let Ok(handle) = state.cf_handle(torus_state::cf::CF_CONSENSUS_META) else {
        return Ok(0);
    };
    let row = snap
        .get_cf(handle, torus_state::cf::META_NATIVE_APPLIED_HEIGHT)
        .map_err(rocks)?;
    Ok(row
        .and_then(|v| <[u8; 8]>::try_from(v.as_slice()).ok())
        .map_or(0, u64::from_be_bytes))
}

/// The rows of `cf` under `prefix` (empty = the whole CF), in key order, each
/// charged to `b` BEFORE `visit` sees it; `visit` may stop early. A missing
/// CF (older DB) has no rows. RocksDB gets an upper bound at the prefix end.
/// `snap` = read as of that snapshot.
fn for_each_prefixed(
    state: &StateDb,
    snap: Snap<'_, '_>,
    cf: &str,
    prefix: &[u8],
    b: &mut ScanBudget,
    mut visit: impl FnMut(&[u8], &[u8], &mut ScanBudget) -> Result<Flow, RpcError>,
) -> Result<Flow, RpcError> {
    let Ok(handle) = state.cf_handle(cf) else {
        return Ok(GO);
    };
    let db = state.inner();
    let mut ro = torus_state::db::prefix_read_opts(prefix);
    if let Some(snap) = snap {
        ro.set_snapshot(snap);
    }
    let mut it = db.raw_iterator_cf_opt(handle, ro);
    it.seek(prefix);
    while let (Some(key), Some(value)) = (it.key(), it.value()) {
        if !key.starts_with(prefix) {
            break;
        }
        b.step()?;
        if visit(key, value, b)?.is_break() {
            return Ok(STOP);
        }
        it.next();
    }
    it.status().map_err(rocks)?;
    Ok(GO)
}

/// The first key of `cf` under `prefix` (one seek, charged).
fn first_key(
    state: &StateDb,
    cf: &str,
    prefix: &[u8],
    b: &mut ScanBudget,
) -> Result<Option<Vec<u8>>, RpcError> {
    Ok(first_row(state, None, cf, prefix, b)?.map(|(k, _)| k))
}

/// One `(key, value)` row.
type KeyValue = (Vec<u8>, Vec<u8>);

/// The first row of `cf` under `prefix` (one seek, charged).
fn first_row(
    state: &StateDb,
    snap: Snap<'_, '_>,
    cf: &str,
    prefix: &[u8],
    b: &mut ScanBudget,
) -> Result<Option<KeyValue>, RpcError> {
    let mut first = None;
    done(for_each_prefixed(state, snap, cf, prefix, b, |k, v, _| {
        first = Some((k.to_vec(), v.to_vec()));
        Ok(STOP)
    }))?;
    if first.is_none() {
        // An empty prefix still cost a seek.
        b.step()?;
    }
    Ok(first)
}

/// The book layout on disk (`book_reader::detect_layout`), charged to `b`:
/// the `__book_mode__` marker is one read; a DB without the marker is
/// sniffed key by key over the whole book CF (each key charged), then one
/// seek of the node-local order store. No state is written (follow-up: a
/// persisted marker on marker-less DBs).
pub(crate) fn detect_layout(state: &StateDb, b: &mut ScanBudget) -> Result<BookLayout, RpcError> {
    b.step()?;
    if let Some(bytes) = state.get_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY)? {
        return book_reader::layout_from_marker(&bytes).map_err(core);
    }
    let mut sniff = LayoutSniff::default();
    done(for_each_prefixed(
        state,
        None,
        CF_NATIVE_ORDER_BOOKS,
        &[],
        b,
        |k, _, _| {
            sniff.observe(k).map_err(core)?;
            Ok(GO)
        },
    ))?;
    let store_nonempty = first_key(state, CF_BOOK_ORDER_ROWS, &[], b)?.is_some();
    sniff.finish(store_nonempty).map_err(core)
}

/// Every market with a row in the book root CF, ascending: one seek per
/// market (its rows are skipped, not read).
fn for_each_book_market(
    state: &StateDb,
    b: &mut ScanBudget,
    mut visit: impl FnMut(MarketId, &mut ScanBudget) -> Result<Flow, RpcError>,
) -> Result<Flow, RpcError> {
    let Ok(handle) = state.cf_handle(CF_NATIVE_ORDER_BOOKS) else {
        return Ok(GO);
    };
    let db = state.inner();
    let mut it = db.raw_iterator_cf(handle);
    it.seek_to_first();
    while let Some(key) = it.key() {
        b.step()?;
        let Some(m) = key.get(..8) else {
            it.next();
            continue;
        };
        let m = u64::from_be_bytes(m.try_into().expect("8 bytes"));
        if visit(m, b)?.is_break() {
            return Ok(STOP);
        }
        match m.checked_add(1) {
            Some(next) => it.seek(next.to_be_bytes()),
            None => break,
        }
    }
    it.status().map_err(rocks)?;
    Ok(GO)
}

/// `market ‖ 0x01 ‖ id` (the order row shape `book_reader` reads).
fn is_order_row(key: &[u8]) -> bool {
    key.len() == 25 && key[8] == ROW_TAG_ORDER
}

/// `market ‖ 0x03 ‖ side ‖ price` (the level row shape `book_reader` reads).
fn is_level_row(key: &[u8]) -> bool {
    key.len() == 26 && key[8] == ROW_TAG_LEVEL
}

fn market_of(key: &[u8]) -> MarketId {
    u64::from_be_bytes(key[..8].try_into().expect("8-byte market prefix"))
}

fn market_prefix(mid: MarketId, tag: u8) -> [u8; 9] {
    let mut p = [0u8; 9];
    p[..8].copy_from_slice(&mid.to_be_bytes());
    p[8] = tag;
    p
}

/// A `BookCorrupt` error, as `book_reader` reports it.
fn corrupt(msg: String) -> RpcError {
    core(CoreError::BookCorrupt(msg))
}

/// An order row of `cf` (`book_reader`'s decode and key-id checks, and
/// errors).
fn checked_order(key: &[u8], row: &[u8], cf: &str) -> Result<Order, RpcError> {
    let mid = market_of(key);
    let (_, order) = OrderBook::decode_order_row(row)
        .map_err(|e| corrupt(format!("market {mid}: book order row: {e}")))?;
    let key_id = u128::from_be_bytes(key[9..25].try_into().expect("25-byte order row key"));
    if key_id != order.id {
        return Err(corrupt(format!(
            "market {mid}: {cf} row key id {key_id} != payload id {} (corrupt row store)",
            order.id
        )));
    }
    Ok(order)
}

/// A row of the mode-2 node-local order store: any key that is not an
/// order row is an error (`book_reader::read_open_orders`).
fn store_order(key: &[u8], row: &[u8]) -> Result<Order, RpcError> {
    if !is_order_row(key) {
        return Err(corrupt(format!(
            "unrecognized cf_book_order_rows key (len {}) — corrupt node-local order store",
            key.len()
        )));
    }
    checked_order(key, row, CF_BOOK_ORDER_ROWS)
}

fn stale_store(mid: MarketId) -> RpcError {
    corrupt(format!(
        "market {mid}: cf_native_order_books commits level rows but the node-local \
         cf_book_order_rows store holds no orders for it — the order store is stale or \
         lost (refusing to report an empty order list)"
    ))
}

/// How a classic whole-book blob that does not decode is reported: main's
/// readers differ, and each method keeps its own text.
#[derive(Clone, Copy)]
enum ClassicDecodeErr {
    /// `torus_getOpenOrders` / `torus_getUserLimits`: "borsh decode order
    /// book: …".
    Borsh,
    /// `book_reader` (`torus_getOrderByCloid`): `BookCorrupt`.
    Corrupt,
}

/// Every classic whole-book blob (`market` = one), charged by size before
/// it is decoded.
fn for_each_classic_book(
    state: &StateDb,
    market: Option<MarketId>,
    how: ClassicDecodeErr,
    b: &mut ScanBudget,
    mut visit: impl FnMut(MarketId, OrderBook) -> Result<Flow, RpcError>,
) -> Result<Flow, RpcError> {
    let decode = |mid: MarketId, blob: &[u8]| {
        OrderBook::try_from_slice(blob).map_err(|e| match how {
            ClassicDecodeErr::Borsh => internal(format!("borsh decode order book: {e}")),
            ClassicDecodeErr::Corrupt => corrupt(format!(
                "market {mid}: cf_native_order_books value is not a classic whole-book blob: {e}"
            )),
        })
    };
    match market {
        Some(mid) => {
            b.step()?;
            match state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &mid.to_be_bytes())? {
                None => Ok(GO),
                Some(blob) => {
                    b.step_n(packed_rows(blob.len()))?;
                    visit(mid, decode(mid, &blob)?)
                }
            }
        }
        None => for_each_prefixed(state, None, CF_NATIVE_ORDER_BOOKS, &[], b, |k, v, b| {
            let Ok(key) = <[u8; 8]>::try_from(k) else {
                return Ok(GO);
            };
            let mid = u64::from_be_bytes(key);
            b.step_n(packed_rows(v.len()))?;
            visit(mid, decode(mid, v)?)
        }),
    }
}

/// `trader`'s resting orders (`market` = one market), as main returns them.
///
/// Classic: `torus_getOpenOrders`'s blob walk (each blob charged by size;
/// stops once `limit` orders are found). Row layouts:
/// `book_reader::read_open_orders`, row for row: every order row read is
/// decoded and checked (whoever's it is), with a market every row of the
/// market is validated (an unknown key, a bad meta row, rows without one),
/// then the trader's orders are sorted by `(market, id)` and cut to
/// `limit`. Only the work is bounded: all markets seek each market's order
/// rows (mode 1) or its first level row (mode 2) instead of reading the
/// whole root CF.
pub(crate) fn trader_orders(
    state: &StateDb,
    trader: &Address,
    market: Option<MarketId>,
    layout: BookLayout,
    limit: usize,
    b: &mut ScanBudget,
) -> Result<Vec<(MarketId, Order)>, RpcError> {
    let mut out: Vec<(MarketId, Order)> = Vec::new();
    match (layout, market) {
        (BookLayout::Classic, _) => {
            done(for_each_classic_book(
                state,
                market,
                ClassicDecodeErr::Borsh,
                b,
                |mid, book| {
                    for o in book.orders_for_trader(trader) {
                        if out.len() >= limit {
                            break;
                        }
                        out.push((mid, o.clone()));
                    }
                    Ok(if out.len() >= limit { STOP } else { GO })
                },
            ))?;
            return Ok(out);
        }
        (BookLayout::OrderRows, Some(mid)) => {
            // The market's meta check runs over every row first: its error
            // wins over an order row's, as in `read_open_orders`.
            let mut check = MarketRowsCheck::new(mid);
            let mut order_err = None;
            done(for_each_prefixed(
                state,
                None,
                CF_NATIVE_ORDER_BOOKS,
                &mid.to_be_bytes(),
                b,
                |k, v, _| {
                    check.observe(k, v).map_err(core)?;
                    if order_err.is_none() && is_order_row(k) {
                        match checked_order(k, v, CF_NATIVE_ORDER_BOOKS) {
                            Ok(o) if o.trader == *trader => out.push((mid, o)),
                            Ok(_) => {}
                            Err(e) => order_err = Some(e),
                        }
                    }
                    Ok(GO)
                },
            ))?;
            check.finish().map_err(core)?;
            if let Some(e) = order_err {
                return Err(e);
            }
        }
        (BookLayout::OrderRows, None) => {
            done(for_each_book_market(state, b, |mid, b| {
                for_each_prefixed(
                    state,
                    None,
                    CF_NATIVE_ORDER_BOOKS,
                    &market_prefix(mid, ROW_TAG_ORDER),
                    b,
                    |k, v, _| {
                        if is_order_row(k) {
                            let o = checked_order(k, v, CF_NATIVE_ORDER_BOOKS)?;
                            if o.trader == *trader {
                                out.push((mid, o));
                            }
                        }
                        Ok(GO)
                    },
                )
            }))?;
        }
        (BookLayout::LevelAuthority, Some(mid)) => {
            let mut check = MarketRowsCheck::new(mid);
            let mut has_levels = false;
            done(for_each_prefixed(
                state,
                None,
                CF_NATIVE_ORDER_BOOKS,
                &mid.to_be_bytes(),
                b,
                |k, v, _| {
                    check.observe(k, v).map_err(core)?;
                    has_levels |= is_level_row(k);
                    Ok(GO)
                },
            ))?;
            check.finish().map_err(core)?;
            let mut saw_row = false;
            done(for_each_prefixed(
                state,
                None,
                CF_BOOK_ORDER_ROWS,
                &mid.to_be_bytes(),
                b,
                |k, v, _| {
                    saw_row = true;
                    let o = store_order(k, v)?;
                    if o.trader == *trader {
                        out.push((mid, o));
                    }
                    Ok(GO)
                },
            ))?;
            if has_levels && !saw_row {
                return Err(stale_store(mid));
            }
        }
        (BookLayout::LevelAuthority, None) => {
            // Markets committing a level row (one seek each), then the
            // whole node-local store: a market with levels and no store
            // row is the stale-store error, which wins over a corrupt store
            // row, as in `read_open_orders`.
            let mut with_levels = Vec::new();
            done(for_each_book_market(state, b, |mid, b| {
                let mut found = false;
                done(for_each_prefixed(
                    state,
                    None,
                    CF_NATIVE_ORDER_BOOKS,
                    &market_prefix(mid, ROW_TAG_LEVEL),
                    b,
                    |k, _, _| {
                        found = is_level_row(k);
                        Ok(if found { STOP } else { GO })
                    },
                ))?;
                if found {
                    with_levels.push(mid);
                }
                Ok(GO)
            }))?;
            let mut in_store = BTreeSet::new();
            let mut row_err = None;
            done(for_each_prefixed(
                state,
                None,
                CF_BOOK_ORDER_ROWS,
                &[],
                b,
                |k, v, _| {
                    if is_order_row(k) {
                        in_store.insert(market_of(k));
                    }
                    if row_err.is_none() {
                        match store_order(k, v) {
                            Ok(o) if o.trader == *trader => out.push((market_of(k), o)),
                            Ok(_) => {}
                            Err(e) => row_err = Some(e),
                        }
                    }
                    Ok(GO)
                },
            ))?;
            if let Some(&mid) = with_levels.iter().find(|m| !in_store.contains(*m)) {
                return Err(stale_store(mid));
            }
            if let Some(e) = row_err {
                return Err(e);
            }
        }
    }
    out.sort_by_key(|(mid, o)| (*mid, o.id));
    out.truncate(limit);
    Ok(out)
}

/// `torus_getUserLimits`: `trader`'s open orders over all markets (resting +
/// pending stops), counted as main's handler counted them: classic blobs
/// (`OrderBook::open_order_count`); under the row layouts every order row of
/// the order store is visited and only the trader's own are decoded, then
/// each market's stop rows (one seek per market).
pub(crate) fn count_open_orders(
    state: &StateDb,
    trader: &Address,
    layout: BookLayout,
    b: &mut ScanBudget,
) -> Result<usize, RpcError> {
    let mut count = 0;
    if layout == BookLayout::Classic {
        done(for_each_classic_book(
            state,
            None,
            ClassicDecodeErr::Borsh,
            b,
            |_, book| {
                count += book.open_order_count(trader);
                Ok(GO)
            },
        ))?;
        return Ok(count);
    }
    let store = match layout {
        BookLayout::OrderRows => CF_NATIVE_ORDER_BOOKS,
        _ => CF_BOOK_ORDER_ROWS,
    };
    done(for_each_prefixed(
        state,
        None,
        store,
        &[],
        b,
        |k, row, _| {
            if is_order_row(k) && OrderBook::order_row_trader(row) == Some(trader.as_slice()) {
                let (_, order) = OrderBook::decode_order_row(row)
                    .map_err(|e| internal(format!("book order row: {e}")))?;
                count += usize::from(order.trader == *trader);
            }
            Ok(GO)
        },
    ))?;
    done(for_each_book_market(state, b, |mid, b| {
        for_each_prefixed(
            state,
            None,
            CF_NATIVE_ORDER_BOOKS,
            &market_prefix(mid, ROW_TAG_STOP),
            b,
            |k, row, _| {
                if k.len() == 25 {
                    let mut one = OrderBook::new(mid, FixedPoint::ONE, FixedPoint::ONE);
                    one.restore_stop_row(row)
                        .map_err(|e| internal(format!("market {mid}: {e}")))?;
                    count += one.open_order_count(trader);
                }
                Ok(GO)
            },
        )
    }))?;
    Ok(count)
}

/// `torus_getOrderByCloid`: `trader`'s open order with `cloid`, as
/// `book_reader::find_by_cloid` finds it. Pending stops first, stopping at
/// a stop hit (a commit between the two passes can only move a cloid from
/// a stop to a resting order, so it is never missed); then every resting
/// order is read and validated ([`trader_orders`]) before a resting hit is
/// returned. A classic blob holds both kinds of one market: one pass, a
/// stop hit returns at once, a resting hit once every blob decoded.
pub(crate) fn find_by_cloid(
    state: &StateDb,
    trader: &Address,
    cloid: Cloid,
    layout: BookLayout,
    b: &mut ScanBudget,
) -> Result<Option<CloidOrder>, RpcError> {
    if layout == BookLayout::Classic {
        let mut stop = None;
        let mut resting: Option<(MarketId, Order)> = None;
        done(for_each_classic_book(
            state,
            None,
            ClassicDecodeErr::Corrupt,
            b,
            |mid, book| {
                if let Some(s) = book
                    .pending_stops()
                    .iter()
                    .find(|s| s.trader == *trader && s.client_order_id == Some(cloid))
                {
                    stop = Some(CloidOrder::Stop(mid, s.clone()));
                    return Ok(STOP);
                }
                if resting.is_none() {
                    resting = book
                        .orders_for_trader(trader)
                        .into_iter()
                        .filter(|o| o.client_order_id == Some(cloid))
                        .min_by_key(|o| o.id)
                        .map(|o| (mid, o.clone()));
                }
                Ok(GO)
            },
        ))?;
        return Ok(stop.or(resting.map(|(mid, o)| CloidOrder::Resting(mid, o))));
    }
    let mut found = None;
    done(for_each_book_market(state, b, |mid, b| {
        for_each_prefixed(
            state,
            None,
            CF_NATIVE_ORDER_BOOKS,
            &market_prefix(mid, ROW_TAG_STOP),
            b,
            |k, row, _| {
                if k.len() != 25 {
                    return Ok(GO);
                }
                let stop = OrderBook::decode_stop_row(row)
                    .map_err(|e| corrupt(format!("market {mid}: stop row does not decode: {e}")))?;
                if stop.trader == *trader && stop.client_order_id == Some(cloid) {
                    found = Some(CloidOrder::Stop(mid, stop));
                    return Ok(STOP);
                }
                Ok(GO)
            },
        )
    }))?;
    if found.is_some() {
        return Ok(found);
    }
    Ok(trader_orders(state, trader, None, layout, usize::MAX, b)?
        .into_iter()
        .find(|(_, o)| o.client_order_id == Some(cloid))
        .map(|(mid, o)| CloidOrder::Resting(mid, o)))
}

/// `torus_getOrderBook` under a row layout: at most `depth` levels per side,
/// best first. Every row of the market is read (charged) and handed to
/// `book_reader::depth_from_market_rows`, so the answer and every error
/// (an unknown key, a bad meta or level row anywhere in the market) are
/// main's; `depth` bounds the reply. The rows come from ONE snapshot, so a
/// commit that moves both sides is seen whole or not at all (never a
/// crossed book).
///
/// s139: `snap` = read as of that snapshot (the `l2Book` stream reads the
/// applied height and the book under one); `None` = a fresh one.
pub(crate) fn row_book_depth(
    state: &StateDb,
    snap: Snap<'_, '_>,
    mid: MarketId,
    layout: BookLayout,
    depth: usize,
    b: &mut ScanBudget,
) -> Result<BookDepth, RpcError> {
    let own;
    let snapshot = match snap {
        Some(s) => s,
        None => {
            own = state.inner().snapshot();
            &own
        }
    };
    let mut rows: Vec<KeyValue> = Vec::new();
    done(for_each_prefixed(
        state,
        Some(snapshot),
        CF_NATIVE_ORDER_BOOKS,
        &mid.to_be_bytes(),
        b,
        |k, v, _| {
            rows.push((k.to_vec(), v.to_vec()));
            Ok(GO)
        },
    ))?;
    let mut d = book_reader::depth_from_market_rows(state, mid, layout, &rows).map_err(core)?;
    d.bids.truncate(depth);
    d.asks.truncate(depth);
    Ok(d)
}

/// A classic whole-book blob for `torus_getOrderBook`, charged by size
/// before the caller decodes it.
/// s139: `snap` = read as of that snapshot.
pub(crate) fn classic_blob(
    state: &StateDb,
    snap: Snap<'_, '_>,
    mid: MarketId,
    b: &mut ScanBudget,
) -> Result<Option<Vec<u8>>, RpcError> {
    b.step()?;
    let blob = match snap {
        None => state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &mid.to_be_bytes())?,
        Some(snap) => match state.cf_handle(CF_NATIVE_ORDER_BOOKS) {
            Ok(handle) => snap.get_cf(handle, mid.to_be_bytes()).map_err(rocks)?,
            Err(_) => None,
        },
    };
    if let Some(blob) = &blob {
        b.step_n(packed_rows(blob.len()))?;
    }
    Ok(blob)
}

/// `torus_getOpenInterest`: long and short size of market `mid`. Position
/// rows are keyed `trader ‖ market` with no market index, so every row is
/// read (follow-up: a per-market aggregate or holder index in state). A
/// market-`mid` row that does not decode or names another trader / market
/// fails the read (`Internal`, the `CoreError` text).
pub(crate) fn open_interest(
    state: &StateDb,
    mid: MarketId,
    b: &mut ScanBudget,
) -> Result<(FixedPoint, FixedPoint), RpcError> {
    let (mut long, mut short) = (FixedPoint::ZERO, FixedPoint::ZERO);
    let want = mid.to_be_bytes();
    done(for_each_prefixed(
        state,
        None,
        CF_NATIVE_POSITIONS,
        &[],
        b,
        |k, v, _| {
            if k.len() != 28 || k[20..28] != want {
                return Ok(GO);
            }
            // Pre-testnet aliased rows: a row naming another trader / market
            // fails the read, as it fail-stops a consensus load.
            let pos = decode_position_row(k, v).map_err(core)?;
            if pos.is_long {
                long += pos.size;
            } else {
                short += pos.size;
            }
            Ok(GO)
        },
    ))?;
    Ok((long, short))
}

/// A proposal row that does not decode, as main reported it: the
/// governance manager's error (`getProposal`, `getProposals` with a status)
/// or the handler's own "borsh decode proposal" (`getProposals` without).
fn proposal_decode(v: &[u8], governance_text: bool) -> Result<Proposal, RpcError> {
    Proposal::try_from_slice(v).map_err(|e| {
        if governance_text {
            internal(EconomicsError::Borsh(e.to_string()).to_string())
        } else {
            internal(format!("borsh decode proposal: {e}"))
        }
    })
}

/// `torus_getProposals`: every proposal (`status` = only those), id order.
/// A proposal row costs one row per started 128 B (the row itself
/// included), charged before it is decoded: its size is the work.
pub(crate) fn proposals(
    state: &StateDb,
    status: Option<ProposalStatus>,
    b: &mut ScanBudget,
) -> Result<Vec<Proposal>, RpcError> {
    let mut out = Vec::new();
    done(for_each_prefixed(
        state,
        None,
        CF_GOVERNANCE_PROPOSALS,
        &[],
        b,
        |k, v, b| {
            if k.len() != 8 {
                return Ok(GO);
            }
            // The row step is already charged.
            b.step_n(packed_rows(v.len()) - 1)?;
            let p = proposal_decode(v, status.is_some())?;
            if status.as_ref().is_none_or(|s| p.status == *s) {
                out.push(p);
            }
            Ok(GO)
        },
    ))?;
    Ok(out)
}

/// `torus_getProposal`: one proposal row, charged one row per started
/// 128 B (one read for a missing id) before it is decoded.
pub(crate) fn proposal(
    state: &StateDb,
    id: u64,
    b: &mut ScanBudget,
) -> Result<Option<Proposal>, RpcError> {
    b.step()?;
    let Some(v) = state
        .get_cf_raw(CF_GOVERNANCE_PROPOSALS, &id.to_be_bytes())
        .map_err(|e| internal(EconomicsError::from(e).to_string()))?
    else {
        return Ok(None);
    };
    b.step_n(packed_rows(v.len()) - 1)?;
    proposal_decode(&v, true).map(Some)
}

/// `trader`'s positions (`PositionManager::positions_for_trader`), each row
/// charged before it is decoded. A row that does not decode or names another
/// trader / market fails the read (`Internal`, the `CoreError` text).
pub(crate) fn trader_positions(
    state: &StateDb,
    trader: &Address,
    b: &mut ScanBudget,
) -> Result<Vec<Position>, RpcError> {
    let mut out = Vec::new();
    done(for_each_prefixed(
        state,
        None,
        CF_NATIVE_POSITIONS,
        trader.as_slice(),
        b,
        |k, v, _| {
            // A row naming another trader / market fails the read
            // (`decode_position_row`), as it fail-stops a consensus load.
            out.push(decode_position_row(k, v).map_err(core)?);
            Ok(GO)
        },
    ))?;
    Ok(out)
}

/// `torus_getLeader`: the validator whose consensus key is `pubkey`, as
/// `StakingManager::find_validator_by_pubkey` finds it (every row decoded
/// before the search: a row that does not decode makes the lookup fail,
/// `None` here, the handler's fallback). Each row is charged.
pub(crate) fn validator_by_pubkey(
    state: &StateDb,
    pubkey: &[u8; 32],
    b: &mut ScanBudget,
) -> Result<Option<ValidatorState>, RpcError> {
    let mut hit = None;
    let mut corrupt_row = false;
    done(for_each_prefixed(
        state,
        None,
        CF_STAKING_VALIDATORS,
        &[],
        b,
        |_, v, _| {
            match ValidatorState::try_from_slice(v) {
                Ok(s) if hit.is_none() && s.pubkey == *pubkey => hit = Some(s),
                Ok(_) => {}
                Err(_) => {
                    corrupt_row = true;
                    return Ok(STOP);
                }
            }
            Ok(GO)
        },
    ))?;
    Ok(if corrupt_row { None } else { hit })
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod scan_tests;
