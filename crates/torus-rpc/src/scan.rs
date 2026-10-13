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

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

use borsh::BorshDeserialize;
use torus_core::book_reader::{
    self, BookDepth, BookLayout, CloidOrder, LayoutSniff, BOOK_MODE_MARKER_KEY,
};
use torus_core::book_rows::{
    book_meta_key, BookMetaRow, ROW_TAG_LEVEL, ROW_TAG_ORDER, ROW_TAG_STOP, SIDE_TAG_ASK,
    SIDE_TAG_BID,
};
use torus_core::error::CoreError;
use torus_core::order_book::{Order, OrderBook, StopOrder};
use torus_core::position::Position;
use torus_economics::governance::{Proposal, ProposalStatus};
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_GOVERNANCE_PROPOSALS, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS,
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
type Snap<'a, 's> = Option<&'a rocksdb::Snapshot<'s>>;

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

/// One market of a row layout (`book_reader`'s meta-row rule, by one seek:
/// the meta row `market ‖ 0x00` sorts first under the market prefix, right
/// after a classic blob's 8-byte key). `false` = no book. A meta row that
/// does not decode (`BookCorrupt`, as `book_reader` reports it), a classic
/// blob under a row layout, or rows without a meta row, is an error (never
/// a silently empty book).
fn row_market_exists(
    state: &StateDb,
    snap: Snap<'_, '_>,
    mid: MarketId,
    b: &mut ScanBudget,
) -> Result<bool, RpcError> {
    match first_row(state, snap, CF_NATIVE_ORDER_BOOKS, &mid.to_be_bytes(), b)? {
        None => Ok(false),
        Some((k, _)) if k.len() == 8 => Err(internal(format!(
            "market {mid}: cf_native_order_books holds a classic whole-book blob under a \
             row layout — the book CF is corrupt (a layout change needs a fresh genesis)"
        ))),
        Some((k, v)) if k[..] == book_meta_key(mid)[..] => {
            BookMetaRow::decode(&v)
                .map_err(|e| core(CoreError::BookCorrupt(format!("market {mid}: {e}"))))?;
            Ok(true)
        }
        Some(_) => Err(internal(format!(
            "market {mid} has order/stop/level rows but no meta row (corrupt row store) — \
             refusing to serve a partial book"
        ))),
    }
}

/// Every classic whole-book blob (`market` = one), charged by size before
/// it is decoded.
fn for_each_classic_book(
    state: &StateDb,
    market: Option<MarketId>,
    b: &mut ScanBudget,
    mut visit: impl FnMut(MarketId, OrderBook) -> Result<Flow, RpcError>,
) -> Result<Flow, RpcError> {
    let decode = |mid: MarketId, blob: &[u8]| {
        OrderBook::try_from_slice(blob).map_err(|e| {
            internal(format!(
                "market {mid}: cf_native_order_books value is not a classic whole-book blob: {e}"
            ))
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

/// `trader`'s resting orders of one row-layout market, in order id order.
/// Rows of other traders are skipped by the trader bytes at a fixed offset
/// of every order row, without decoding them.
fn row_market_orders(
    state: &StateDb,
    trader: &Address,
    mid: MarketId,
    layout: BookLayout,
    b: &mut ScanBudget,
    visit: &mut impl FnMut(MarketId, Order) -> Flow,
) -> Result<Flow, RpcError> {
    let level_store = layout == BookLayout::LevelAuthority;
    let (cf, prefix) = if level_store {
        (CF_BOOK_ORDER_ROWS, mid.to_be_bytes().to_vec())
    } else {
        (
            CF_NATIVE_ORDER_BOOKS,
            [&mid.to_be_bytes()[..], &[ROW_TAG_ORDER]].concat(),
        )
    };
    // Mode 2: committed levels with no node-local order rows = a stale or
    // lost store (an error, never an empty list).
    let has_levels = level_store
        && first_key(
            state,
            CF_NATIVE_ORDER_BOOKS,
            &[&mid.to_be_bytes()[..], &[ROW_TAG_LEVEL]].concat(),
            b,
        )?
        .is_some();
    let mut saw_row = false;
    let flow = for_each_prefixed(state, None, cf, &prefix, b, |key, row, _| {
        saw_row = true;
        if key.len() != 25 || key[8] != ROW_TAG_ORDER {
            if level_store {
                return Err(internal(format!(
                    "unrecognized cf_book_order_rows key (len {}) — corrupt node-local order store",
                    key.len()
                )));
            }
            return Ok(GO);
        }
        if OrderBook::order_row_trader(row) != Some(trader.as_slice()) {
            return Ok(GO);
        }
        let (_, order) = OrderBook::decode_order_row(row)
            .map_err(|e| internal(format!("market {mid}: book order row: {e}")))?;
        let key_id = u128::from_be_bytes(key[9..25].try_into().expect("25-byte order row key"));
        if key_id != order.id {
            return Err(internal(format!(
                "market {mid}: {cf} row key id {key_id} != payload id {} (corrupt row store)",
                order.id
            )));
        }
        if order.trader != *trader {
            return Ok(GO);
        }
        Ok(visit(mid, order))
    })?;
    if has_levels && !saw_row {
        return Err(internal(format!(
            "market {mid}: cf_native_order_books commits level rows but the node-local \
             cf_book_order_rows store holds no orders for it — the order store is stale or \
             lost (refusing to report an empty order list)"
        )));
    }
    Ok(flow)
}

/// `trader`'s resting orders (`market` = one market), ascending
/// `(market, order id)` under the row layouts; `visit` may stop early.
pub(crate) fn trader_orders(
    state: &StateDb,
    trader: &Address,
    market: Option<MarketId>,
    layout: BookLayout,
    b: &mut ScanBudget,
    mut visit: impl FnMut(MarketId, Order) -> Flow,
) -> Result<(), RpcError> {
    if layout == BookLayout::Classic {
        return done(for_each_classic_book(state, market, b, |mid, book| {
            for o in book.orders_for_trader(trader) {
                if visit(mid, o.clone()).is_break() {
                    return Ok(STOP);
                }
            }
            Ok(GO)
        }));
    }
    match market {
        Some(mid) => {
            if !row_market_exists(state, None, mid, b)? {
                return Ok(());
            }
            done(row_market_orders(state, trader, mid, layout, b, &mut visit))
        }
        None => done(for_each_book_market(state, b, |mid, b| {
            row_market_orders(state, trader, mid, layout, b, &mut visit)
        })),
    }
}

/// `trader`'s pending stops of a row layout (stop rows `market ‖ 0x02 ‖ id`
/// of every market, one seek per market).
fn row_trader_stops(
    state: &StateDb,
    trader: &Address,
    b: &mut ScanBudget,
    mut visit: impl FnMut(MarketId, StopOrder) -> Flow,
) -> Result<Flow, RpcError> {
    for_each_book_market(state, b, |mid, b| {
        let prefix = [&mid.to_be_bytes()[..], &[ROW_TAG_STOP]].concat();
        for_each_prefixed(
            state,
            None,
            CF_NATIVE_ORDER_BOOKS,
            &prefix,
            b,
            |key, row, _| {
                if key.len() != 25 {
                    return Ok(GO);
                }
                let stop = OrderBook::decode_stop_row(row).map_err(|e| {
                    internal(format!("market {mid}: stop row does not decode: {e}"))
                })?;
                Ok(if stop.trader == *trader {
                    visit(mid, stop)
                } else {
                    GO
                })
            },
        )
    })
}

/// `torus_getUserLimits`: `trader`'s open orders over all markets (resting +
/// pending stops, as `OrderBook::open_order_count` counts them).
pub(crate) fn count_open_orders(
    state: &StateDb,
    trader: &Address,
    layout: BookLayout,
    b: &mut ScanBudget,
) -> Result<usize, RpcError> {
    let mut count = 0;
    if layout == BookLayout::Classic {
        done(for_each_classic_book(state, None, b, |_, book| {
            count += book.open_order_count(trader);
            Ok(GO)
        }))?;
        return Ok(count);
    }
    trader_orders(state, trader, None, layout, b, |_, _| {
        count += 1;
        GO
    })?;
    done(row_trader_stops(state, trader, b, |_, _| {
        count += 1;
        GO
    }))?;
    Ok(count)
}

/// `torus_getOrderByCloid`: `trader`'s open order with `cloid`, stopping at
/// the first hit. Row layouts scan stops FIRST (a commit between the two
/// scans can only move a cloid from a stop to a resting order, so it is
/// never missed; `book_reader::find_by_cloid`). A classic blob holds both
/// kinds of one market, read at once.
pub(crate) fn find_by_cloid(
    state: &StateDb,
    trader: &Address,
    cloid: Cloid,
    layout: BookLayout,
    b: &mut ScanBudget,
) -> Result<Option<CloidOrder>, RpcError> {
    let mut found = None;
    if layout == BookLayout::Classic {
        done(for_each_classic_book(state, None, b, |mid, book| {
            if let Some(s) = book
                .pending_stops()
                .iter()
                .find(|s| s.trader == *trader && s.client_order_id == Some(cloid))
            {
                found = Some(CloidOrder::Stop(mid, s.clone()));
            } else if let Some(o) = book
                .orders_for_trader(trader)
                .into_iter()
                .find(|o| o.client_order_id == Some(cloid))
            {
                found = Some(CloidOrder::Resting(mid, o.clone()));
            }
            Ok(if found.is_some() { STOP } else { GO })
        }))?;
        return Ok(found);
    }
    done(row_trader_stops(state, trader, b, |mid, s| {
        if s.client_order_id == Some(cloid) {
            found = Some(CloidOrder::Stop(mid, s));
            return STOP;
        }
        GO
    }))?;
    if found.is_none() {
        trader_orders(state, trader, None, layout, b, |mid, o| {
            if o.client_order_id == Some(cloid) {
                found = Some(CloidOrder::Resting(mid, o));
                return STOP;
            }
            GO
        })?;
    }
    Ok(found)
}

/// `torus_getOrderBook` under a row layout: at most `depth` levels per side,
/// best first. Mode 2 reads only those level rows (two bounded seeks); mode 1
/// has no price index, so every order row of the market is aggregated. The
/// meta row and both sides are read from ONE snapshot, so a commit that
/// moves both sides is seen whole or not at all (never a crossed book).
pub(crate) fn row_book_depth(
    state: &StateDb,
    mid: MarketId,
    layout: BookLayout,
    depth: usize,
    b: &mut ScanBudget,
) -> Result<BookDepth, RpcError> {
    let snapshot = state.inner().snapshot();
    let snap = Some(&snapshot);
    if !row_market_exists(state, snap, mid, b)? {
        return Ok(BookDepth::default());
    }
    let mut rows: Vec<KeyValue> = Vec::new();
    if layout == BookLayout::LevelAuthority {
        for side in [SIDE_TAG_BID, SIDE_TAG_ASK] {
            let prefix = [&mid.to_be_bytes()[..], &[ROW_TAG_LEVEL, side]].concat();
            let mut n = 0;
            done(for_each_prefixed(
                state,
                snap,
                CF_NATIVE_ORDER_BOOKS,
                &prefix,
                b,
                |k, v, _| {
                    if k.len() != 26 {
                        return Err(internal(format!(
                            "market {mid}: level row key of {} bytes (corrupt row store)",
                            k.len()
                        )));
                    }
                    rows.push((k.to_vec(), v.to_vec()));
                    n += 1;
                    Ok(if n >= depth { STOP } else { GO })
                },
            ))?;
        }
    } else {
        let prefix = [&mid.to_be_bytes()[..], &[ROW_TAG_ORDER]].concat();
        done(for_each_prefixed(
            state,
            snap,
            CF_NATIVE_ORDER_BOOKS,
            &prefix,
            b,
            |k, v, _| {
                rows.push((k.to_vec(), v.to_vec()));
                Ok(GO)
            },
        ))?;
    }
    let mut d =
        book_reader::depth_from_rows(mid, layout, &rows).map_err(|e| internal(e.to_string()))?;
    d.bids.truncate(depth);
    d.asks.truncate(depth);
    Ok(d)
}

/// A classic whole-book blob for `torus_getOrderBook`, charged by size
/// before the caller decodes it.
pub(crate) fn classic_blob(
    state: &StateDb,
    mid: MarketId,
    b: &mut ScanBudget,
) -> Result<Option<Vec<u8>>, RpcError> {
    b.step()?;
    let blob = state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &mid.to_be_bytes())?;
    if let Some(blob) = &blob {
        b.step_n(packed_rows(blob.len()))?;
    }
    Ok(blob)
}

/// `torus_getOpenInterest`: long and short size of market `mid`. Position
/// rows are keyed `trader ‖ market` with no market index, so every row is
/// read (follow-up: a per-market aggregate or holder index in state).
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
            let pos = Position::try_from_slice(v)
                .map_err(|e| internal(format!("borsh decode position: {e}")))?;
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

/// `torus_getProposals`: every proposal (`status` = only those), id order.
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
        |k, v, _| {
            if k.len() != 8 {
                return Ok(GO);
            }
            let p = Proposal::try_from_slice(v)
                .map_err(|e| internal(format!("borsh decode proposal: {e}")))?;
            if status.as_ref().is_none_or(|s| p.status == *s) {
                out.push(p);
            }
            Ok(GO)
        },
    ))?;
    Ok(out)
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod scan_tests;
