//! s139: the `l2Book` WebSocket stream's feed.
//!
//! The stream serves what `torus_getOrderBook` serves: the levels of the book
//! on disk, read with the same function ([`crate::torus::read_book_levels`]),
//! together with the native applied height (the eth head's source) under ONE
//! RocksDB snapshot, so each message is the method's answer at that height.
//!
//! One publisher task runs while at least one `l2Book` subscription exists
//! (none otherwise: no work, no task). Every [`L2_POLL`] it reads the applied
//! height (one point read); when the height moved, it reads each subscribed
//! market's best [`L2_MAX_LEVELS`] levels once, whatever the number of
//! subscribers, and broadcasts the markets whose levels changed as one
//! [`BookTick`]. Execution is never touched: the books are read back from the
//! DB the node already wrote (per block on `--rpc-only` nodes, per checkpoint
//! on validators, exactly as `torus_getOrderBook` sees them).
//!
//! A subscriber first reads its market's snapshot itself (registered before,
//! so no later change is missed), then forwards each tick above that height
//! carrying its market, cut to its `nLevels`, skipping a message whose
//! levels equal the last one sent. The broadcast channel holds
//! [`L2_CHANNEL_CAP`] ticks; a subscriber further behind is closed with a lag
//! error (the next subscription starts with a full snapshot).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::value::RawValue;
use tokio::sync::broadcast;

use crate::types::{RpcL2Level, RpcPriceLevel};
use crate::RpcState;

/// `nLevels` default (Hyperliquid's `l2Book` depth).
pub const L2_DEFAULT_LEVELS: usize = 20;
/// Most `nLevels` a subscription may ask for; the depth the feed reads.
pub const L2_MAX_LEVELS: usize = 100;
/// How often the publisher checks the applied height.
pub const L2_POLL: Duration = Duration::from_millis(25);
/// Ticks a subscriber may fall behind before it is closed.
pub const L2_CHANNEL_CAP: usize = 64;

/// One market's levels at a height, best first, at most [`L2_MAX_LEVELS`] per
/// side. Its `levels` JSON is serialized once per `nLevels` and shared by
/// every subscriber with that depth.
pub struct BookSnap {
    pub bids: Vec<RpcPriceLevel>,
    pub asks: Vec<RpcPriceLevel>,
    json: Mutex<HashMap<usize, Arc<RawValue>>>,
}

impl BookSnap {
    pub fn new(bids: Vec<RpcPriceLevel>, asks: Vec<RpcPriceLevel>) -> Self {
        Self {
            bids,
            asks,
            json: Mutex::new(HashMap::new()),
        }
    }

    /// `[[bids], [asks]]` (Hyperliquid's `levels`) cut to `n` levels per side.
    pub fn levels_json(&self, n: usize) -> Result<Arc<RawValue>, serde_json::Error> {
        let mut cache = self.json.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(raw) = cache.get(&n) {
            return Ok(raw.clone());
        }
        let side = |levels: &[RpcPriceLevel]| -> Vec<RpcL2Level> {
            levels
                .iter()
                .take(n)
                .map(|l| RpcL2Level {
                    px: l.price.clone(),
                    sz: l.quantity.clone(),
                    n: l.order_count,
                })
                .collect()
        };
        let raw: Arc<RawValue> =
            Arc::from(serde_json::value::to_raw_value(&[side(&self.bids), side(&self.asks)])?);
        cache.insert(n, raw.clone());
        Ok(raw)
    }

    fn same_levels(&self, other: &BookSnap) -> bool {
        self.bids == other.bids && self.asks == other.asks
    }
}

/// A market's update in a tick: its new levels, or the read error (the
/// subscriptions on it are closed with the text).
pub type BookUpdate = Result<Arc<BookSnap>, String>;

/// The markets whose levels changed at applied height `height`.
pub struct BookTick {
    pub height: u64,
    pub books: BTreeMap<u64, BookUpdate>,
}

#[derive(Default)]
struct Registry {
    /// Subscribed market -> subscription count.
    markets: BTreeMap<u64, usize>,
    /// Markets with a new subscription since the last read: published at
    /// the next read even if unchanged (a new subscriber's own snapshot may
    /// be newer than the feed's last read).
    fresh: BTreeSet<u64>,
    /// Whether the publisher task runs.
    running: bool,
}

/// The `l2Book` feed of one RPC server (shared by its handlers).
pub struct BookFeed {
    registry: Mutex<Registry>,
    pub(crate) tx: broadcast::Sender<Arc<BookTick>>,
    poll: Duration,
}

/// A subscription's registration; unregisters on drop.
pub struct BookReg {
    feed: Arc<BookFeed>,
    market: u64,
}

impl Drop for BookReg {
    fn drop(&mut self) {
        let mut r = self.feed.lock();
        if let Some(n) = r.markets.get_mut(&self.market) {
            *n -= 1;
            if *n == 0 {
                r.markets.remove(&self.market);
                r.fresh.remove(&self.market);
            }
        }
    }
}

impl BookFeed {
    pub fn new(capacity: usize, poll: Duration) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            registry: Mutex::new(Registry::default()),
            tx,
            poll,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Subscribed markets (tests and metrics).
    pub fn markets(&self) -> Vec<u64> {
        self.lock().markets.keys().copied().collect()
    }

    /// Whether the publisher task runs.
    pub fn running(&self) -> bool {
        self.lock().running
    }

    /// Register a subscription on `market` and return its tick receiver,
    /// subscribed BEFORE the caller reads its first snapshot. Starts the
    /// publisher if it is not running.
    pub fn subscribe(
        self: &Arc<Self>,
        rpc: &RpcState,
        market: u64,
    ) -> (broadcast::Receiver<Arc<BookTick>>, BookReg) {
        let rx = self.tx.subscribe();
        let start = {
            let mut r = self.lock();
            *r.markets.entry(market).or_insert(0) += 1;
            r.fresh.insert(market);
            !std::mem::replace(&mut r.running, true)
        };
        if start {
            tokio::spawn(self.clone().run(rpc.clone()));
        }
        (
            rx,
            BookReg {
                feed: self.clone(),
                market,
            },
        )
    }

    /// The publisher: exits (clearing `running` under the registry lock)
    /// once no market is subscribed.
    async fn run(self: Arc<Self>, rpc: RpcState) {
        let mut last_height: Option<u64> = None;
        let mut last: HashMap<u64, Arc<BookSnap>> = HashMap::new();
        loop {
            tokio::time::sleep(self.poll).await;
            let (markets, fresh) = {
                let mut r = self.lock();
                if r.markets.is_empty() {
                    r.running = false;
                    return;
                }
                (
                    r.markets.keys().copied().collect::<Vec<u64>>(),
                    std::mem::take(&mut r.fresh),
                )
            };
            last.retain(|m, _| markets.binary_search(m).is_ok());
            let rpc2 = rpc.clone();
            let prev_height = last_height;
            let need_fresh = !fresh.is_empty();
            let read = tokio::task::spawn_blocking(move || {
                read_markets(&rpc2, &markets, prev_height, need_fresh)
            })
            .await;
            let Ok(Some((height, reads))) = read else {
                // Unchanged height (or a failed height read): retry the
                // fresh markets next time.
                if need_fresh {
                    self.lock().fresh.extend(fresh);
                }
                continue;
            };
            let mut books = BTreeMap::new();
            for (m, r) in reads {
                match r {
                    Ok(snap) => {
                        let changed = fresh.contains(&m)
                            || last.get(&m).is_none_or(|old| !old.same_levels(&snap));
                        if changed {
                            let snap = Arc::new(snap);
                            last.insert(m, snap.clone());
                            books.insert(m, Ok(snap));
                        }
                    }
                    Err(e) => {
                        last.remove(&m);
                        books.insert(m, Err(e));
                    }
                }
            }
            last_height = Some(height);
            if !books.is_empty() {
                let _ = self.tx.send(Arc::new(BookTick { height, books }));
            }
        }
    }
}

/// One read of the publisher: under one snapshot, the applied height and,
/// when it differs from `prev` (or `force`), each market's levels. `None`:
/// unchanged, or the height read failed.
#[allow(clippy::type_complexity)]
fn read_markets(
    rpc: &RpcState,
    markets: &[u64],
    prev: Option<u64>,
    force: bool,
) -> Option<(u64, Vec<(u64, Result<BookSnap, String>)>)> {
    let snap = rpc.state.inner().snapshot();
    let height = crate::scan::applied_height_at(&rpc.state, &snap).ok()?;
    if prev == Some(height) && !force {
        return None;
    }
    let mut out = Vec::with_capacity(markets.len());
    let mut layout_budget = rpc.scan_budget();
    let layout = crate::torus::book_layout(&rpc.state, &mut layout_budget);
    for &m in markets {
        let r = match &layout {
            Ok(layout) => {
                let mut budget = rpc.scan_budget();
                crate::torus::read_book_levels(
                    &rpc.state,
                    Some(&snap),
                    *layout,
                    m,
                    L2_MAX_LEVELS,
                    &mut budget,
                )
                .map(|(bids, asks)| BookSnap::new(bids, asks))
                .map_err(|e| e.message().to_string())
            }
            Err(e) => Err(e.message().to_string()),
        };
        out.push((m, r));
    }
    Some((height, out))
}

/// A subscription's first snapshot of `market`: the applied height and the
/// levels under one snapshot (`torus_getOrderBook`'s errors).
pub fn first_snapshot(
    rpc: &RpcState,
    market: u64,
) -> Result<(u64, BookSnap), jsonrpsee::types::ErrorObjectOwned> {
    let snap = rpc.state.inner().snapshot();
    let height = crate::scan::applied_height_at(&rpc.state, &snap)
        .map_err(jsonrpsee::types::ErrorObjectOwned::from)?;
    let mut budget = rpc.scan_budget();
    let layout = crate::torus::book_layout(&rpc.state, &mut budget)?;
    let (bids, asks) = crate::torus::read_book_levels(
        &rpc.state,
        Some(&snap),
        layout,
        market,
        L2_MAX_LEVELS,
        &mut budget,
    )?;
    Ok((height, BookSnap::new(bids, asks)))
}

/// One `l2Book` message: Hyperliquid's `WsBook` (`levels` = `[bids, asks]`)
/// with this node's encodings, plus the applied height and the
/// subscription's message sequence number.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct L2BookMessage<'a> {
    pub market_id: String,
    pub block_number: String,
    pub seq: String,
    pub levels: &'a RawValue,
}
