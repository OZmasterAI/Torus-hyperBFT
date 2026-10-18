//! `bench-throughput preload` (bench queue step 8f): put ~TARGET resting
//! orders on the books of a booted devnet before the load starts, so a cell
//! measures the chain with a deep, realistic book (1M orders over 300
//! markets) instead of the empty one a fresh genesis has. The genesis has no
//! orders section (crates/torus-genesis), so this runs after boot.
//!
//! Shape (all deterministic, see [`PreloadPlan`]):
//! - Owners are genesis-funded bulk senders in their OWN index range,
//!   `owner_offset..owner_offset + owners` (default: just past the load's
//!   senders, `sender_offset + senders`), so the load's cancel-alls (one
//!   signature per load sender, all markets) never touch the pre-loaded orders.
//! - Order `k` (0..target) sits in market `k % markets + 1` (so every market
//!   gets `target / markets` or one more) and belongs to owner `k / per_owner`
//!   (so no owner holds more than `per_owner` <= 1000 open orders, the chain's
//!   per-user base limit `OPEN_ORDER_BASE_LIMIT`; default 900 for headroom).
//! - Prices never cross and sit well outside the load's band: buys at
//!   `mid - distance - level`, sells at `mid + distance + level`, `level` in
//!   `0..levels`, alternating side per layer (`k / markets`). The load prices at
//!   `mid +/- d`, `d <= band`, so with `distance > band` neither the load nor
//!   another pre-load order ever reaches them; the farthest price stays within
//!   ±10% of mid (`PRICE_BAND_FALLBACK_CLAMP_BPS`), inside the ±50% placement
//!   band (`PRICE_BAND_DEFAULT_BPS`).
//! - Each owner's orders go out as `PlaceOrderBatch` actions of up to `batch`
//!   orders (default `min(1024, per_owner)`: one action per owner at the
//!   defaults), paced at `rate` actions/s with `concurrency` in flight; a busy
//!   or transport failure resends the SAME signed action (same nonce, so a
//!   duplicate is dropped by the replay guard) until admitted or 30 s old.
//!
//! The resting count comes from the nodes' Prometheus counters
//! (`torus_orders_resting_total`, orders that left liquidity on a book),
//! measured as the delta from just before the first submission, on EVERY
//! node: the per-trader RPCs (`torus_getUserLimits`, `torus_getOpenOrders`)
//! scan every book under a 250k-row budget (`DEFAULT_MAX_SCAN_ROWS`) and fail
//! at 1M orders. `torus_exec_resting_orders` (the gauge of resting orders in
//! the loaded books, set at the start of each native block) is reported too.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use k256::ecdsa::SigningKey;
use serde::Serialize;
use torus_types::{
    eip712::sign_native_action, FixedPoint, NativeAction, OrderType, PlaceOrderParams,
    TimeInForce, NATIVE_ORDERS_PER_BATCH_CAP, PRICE_BAND_FALLBACK_CLAMP_BPS,
};

/// The chain's per-user open-order base limit (all markets).
pub const OWNER_LIMIT: u64 = torus_core::position::OPEN_ORDER_BASE_LIMIT as u64;
/// Default orders per owner: the limit minus 10% headroom (the load's
/// `OPEN_ORDER_BUDGET=900` uses the same margin).
pub const DEFAULT_PER_OWNER: u64 = 900;
/// torus-mempool `NATIVE_PER_BLOCK_CAP`: native actions one sender may have in
/// one block. An owner's actions must fit in one block's worth.
pub const PER_SENDER_BLOCK_CAP: u64 = 64;
/// A pre-load owner index must be a genesis-funded bulk sender (gen-weighted-genesis.sh
/// BULK_OFFSET).
pub const FUNDED_BULK_START: usize = 60;
/// Resend a refused action for at most this long after its first send: well
/// inside the node's 60 s nonce window.
const RESEND_MAX_AGE: Duration = Duration::from_secs(30);
const RESEND_BACKOFF: Duration = Duration::from_millis(100);

pub const M_RESTING: &str = "torus_orders_resting_total";
pub const M_PLACED: &str = "torus_orders_placed_accepted_total";
pub const M_GAUGE: &str = "torus_exec_resting_orders";

/// What to pre-load (CLI flags, validated by [`PreloadPlan::new`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PreloadSpec {
    /// Resting orders to place in total.
    pub target: u64,
    /// Markets 1..=markets.
    pub markets: u64,
    /// Max orders per owner (<= OWNER_LIMIT).
    pub per_owner: u64,
    /// First owner index (`None` = `load_offset + load_senders`).
    pub owner_offset: Option<usize>,
    /// The load's senders (`consensus --sender-offset/--senders`): owners stay out.
    pub load_offset: usize,
    pub load_senders: usize,
    /// Exclusive end of the genesis-funded index range (60 + BULK_COUNT).
    pub funded_end: usize,
    /// The load's fixed mid (whole TRS) and half-band in ticks.
    pub mid: u64,
    pub load_band: u64,
    /// Ticks from mid to the nearest pre-load price.
    pub distance: u64,
    /// Price levels per side.
    pub levels: u64,
    /// Orders per PlaceOrderBatch (0 = min(1024, per_owner)).
    pub batch: usize,
    /// Lots per order.
    pub qty: u64,
}

/// The validated plan: every order, owner and action is a pure function of it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PreloadPlan {
    pub target: u64,
    pub markets: u64,
    pub per_owner: u64,
    pub owners: u64,
    pub owner_offset: usize,
    pub owner_end: usize,
    pub funded_end: usize,
    pub mid: u64,
    pub distance: u64,
    pub levels: u64,
    pub batch: usize,
    pub qty: u64,
    /// PlaceOrderBatch actions in total.
    pub actions: u64,
    /// Max actions of one owner.
    pub actions_per_owner_max: u64,
    pub per_market_min: u64,
    pub per_market_max: u64,
    /// Lowest buy / highest sell price.
    pub buy_price_min: u64,
    pub buy_price_max: u64,
    pub sell_price_min: u64,
    pub sell_price_max: u64,
    /// Margin one owner reserves at the default 20x leverage, whole TRS (upper bound).
    pub owner_margin_trs_max: u64,
}

impl PreloadPlan {
    pub fn new(spec: &PreloadSpec) -> Result<Self, String> {
        let s = spec;
        if s.target == 0 || s.markets == 0 {
            return Err(format!("--target ({}) and --markets ({}) must be >= 1", s.target, s.markets));
        }
        if s.per_owner == 0 || s.per_owner > OWNER_LIMIT {
            return Err(format!(
                "--per-owner {} must be in 1..={OWNER_LIMIT} (the chain's per-user open-order limit)",
                s.per_owner
            ));
        }
        if s.qty == 0 || s.levels == 0 {
            return Err(format!("--qty ({}) and --levels ({}) must be >= 1", s.qty, s.levels));
        }
        let batch = if s.batch == 0 { NATIVE_ORDERS_PER_BATCH_CAP.min(s.per_owner as usize) } else { s.batch };
        if batch > NATIVE_ORDERS_PER_BATCH_CAP {
            return Err(format!("--batch {batch} exceeds the {NATIVE_ORDERS_PER_BATCH_CAP}-order PlaceOrderBatch cap"));
        }
        let per_owner_actions = s.per_owner.div_ceil(batch as u64);
        if per_owner_actions > PER_SENDER_BLOCK_CAP {
            return Err(format!(
                "--per-owner {} in batches of {batch} = {per_owner_actions} actions per owner, over the \
                 mempool's {PER_SENDER_BLOCK_CAP} per sender per block; raise --batch",
                s.per_owner
            ));
        }
        if s.distance <= s.load_band {
            return Err(format!(
                "--distance {} must exceed the load's --band {} (else the load's orders reach the pre-load)",
                s.distance, s.load_band
            ));
        }
        let farthest = s.distance + s.levels - 1;
        let clamp = s.mid * PRICE_BAND_FALLBACK_CLAMP_BPS / 10_000;
        if farthest > clamp {
            return Err(format!(
                "--distance {} + --levels {} reach {farthest} ticks from mid {}: over the {clamp}-tick \
                 ({}%) bound that keeps every price inside the placement band",
                s.distance,
                s.levels,
                s.mid,
                PRICE_BAND_FALLBACK_CLAMP_BPS / 100
            ));
        }
        let owners = s.target.div_ceil(s.per_owner);
        let load_end = s.load_offset + s.load_senders;
        let owner_offset = s.owner_offset.unwrap_or(load_end);
        let owner_end = owner_offset + owners as usize;
        if owner_offset < FUNDED_BULK_START || owner_end > s.funded_end {
            return Err(format!(
                "owners {owner_offset}..{owner_end} ({owners} = ceil({} / {})) are not all genesis-funded \
                 ({FUNDED_BULK_START}..{}: --funded-bulk-end = 60 + BULK_COUNT)",
                s.target, s.per_owner, s.funded_end
            ));
        }
        if owner_offset < load_end && s.load_offset < owner_end {
            return Err(format!(
                "owners {owner_offset}..{owner_end} overlap the load's senders {}..{load_end}: the load's \
                 cancel-alls would cancel the pre-load",
                s.load_offset
            ));
        }
        let last = s.target - (owners - 1) * s.per_owner;
        let actions = (owners - 1) * per_owner_actions + last.div_ceil(batch as u64);
        let layers = s.target.div_ceil(s.markets);
        // Buys take even layers, sells odd ones; level = (layer / 2) % levels.
        let used_levels = |side_layers: u64| side_layers.min(s.levels);
        let buy_levels = used_levels(layers.div_ceil(2));
        let sell_levels = used_levels(layers / 2).max(1);
        Ok(Self {
            target: s.target,
            markets: s.markets,
            per_owner: s.per_owner,
            owners,
            owner_offset,
            owner_end,
            funded_end: s.funded_end,
            mid: s.mid,
            distance: s.distance,
            levels: s.levels,
            batch,
            qty: s.qty,
            actions,
            actions_per_owner_max: if owners > 1 { per_owner_actions } else { last.div_ceil(batch as u64) },
            per_market_min: s.target / s.markets,
            per_market_max: layers,
            buy_price_min: s.mid - s.distance - (buy_levels - 1),
            buy_price_max: s.mid - s.distance,
            sell_price_min: s.mid + s.distance,
            sell_price_max: s.mid + s.distance + (sell_levels - 1),
            owner_margin_trs_max: s.per_owner.min(s.target) * s.qty * (s.mid + farthest)
                / super::NATIVE_DEFAULT_LEVERAGE as u64,
        })
    }

    /// Order `k`'s (owner index, order).
    pub fn order(&self, k: u64) -> (usize, PlaceOrderParams) {
        debug_assert!(k < self.target);
        let market_id = k % self.markets + 1;
        let layer = k / self.markets;
        let is_buy = layer.is_multiple_of(2);
        let level = (layer / 2) % self.levels;
        let px = if is_buy { self.mid - self.distance - level } else { self.mid + self.distance + level };
        let order = PlaceOrderParams {
            market_id,
            is_buy,
            price: FixedPoint::from_raw(px as i128 * FixedPoint::SCALE),
            quantity: FixedPoint::from_raw(self.qty as i128 * FixedPoint::SCALE),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        (self.owner_offset + (k / self.per_owner) as usize, order)
    }

    /// Owner `o`'s (0-based in the owner range) PlaceOrderBatch payloads, in order.
    pub fn owner_batches(&self, o: u64) -> Vec<Vec<PlaceOrderParams>> {
        let first = o * self.per_owner;
        let end = (first + self.per_owner).min(self.target);
        let orders: Vec<PlaceOrderParams> = (first..end).map(|k| self.order(k).1).collect();
        orders.chunks(self.batch).map(<[PlaceOrderParams]>::to_vec).collect()
    }

    /// Orders per market, index 0 = market 1 (what `order` produces; the plan
    /// reports its min / max).
    #[cfg(test)]
    pub fn per_market(&self) -> Vec<u64> {
        let (q, r) = (self.target / self.markets, self.target % self.markets);
        (0..self.markets).map(|m| q + u64::from(m < r)).collect()
    }
}

// ---------------------------------------------------------------- runtime

/// Outcome of one pre-load run (printed as one JSON object).
#[derive(Clone, Debug, Default, Serialize)]
pub struct PreloadReport {
    pub target: u64,
    pub markets: u64,
    pub owners: u64,
    pub owner_offset: usize,
    pub owner_end: usize,
    pub per_owner: u64,
    pub batch: usize,
    pub distance: u64,
    pub levels: u64,
    pub actions: u64,
    /// Orders signed and sent (each action counted once, however often resent).
    pub orders_placed: u64,
    pub actions_admitted: u64,
    /// Orders in admitted actions (the RPC took them; execution decides).
    pub orders_accepted: u64,
    pub actions_failed: u64,
    pub resends: u64,
    /// min over nodes of the `torus_orders_resting_total` delta.
    pub resting: Option<u64>,
    pub resting_by_node: Vec<Option<u64>>,
    pub placed_accepted_by_node: Vec<Option<u64>>,
    /// `torus_exec_resting_orders` per node at the end (all books, incl. any
    /// order that rested before the pre-load).
    pub resting_gauge_by_node: Vec<Option<u64>>,
    pub reached: bool,
    pub submit_seconds: f64,
    pub seconds: f64,
    /// First few submit errors (`action <i> owner <idx>: <error>`).
    pub failures: Vec<String>,
}

/// Runtime knobs (CLI flags).
pub struct RunArgs<'a> {
    pub rpc_urls: &'a str,
    pub metrics_urls: &'a str,
    pub rate: f64,
    pub concurrency: usize,
    pub timeout: Duration,
    pub stall: Duration,
}

/// A node's (resting_total, placed_accepted_total, resting gauge), `None` = unreadable.
type Counters = Option<(u64, u64, u64)>;

async fn scrape(client: &reqwest::Client, url: &str) -> Counters {
    let text = client.get(url).send().await.ok()?.text().await.ok()?;
    if !text.contains(M_RESTING) {
        return None;
    }
    Some((
        super::metric_u64(&text, M_RESTING),
        super::metric_u64(&text, M_PLACED),
        super::metric_u64(&text, M_GAUGE),
    ))
}

async fn scrape_all(client: &reqwest::Client, urls: &[String]) -> Vec<Counters> {
    let mut out = Vec::with_capacity(urls.len());
    for u in urls {
        out.push(scrape(client, u).await);
    }
    out
}

/// min over nodes of `now - base` (resting), `None` if any node is unreadable.
pub fn min_delta(base: &[Counters], now: &[Counters], pick: fn(&(u64, u64, u64)) -> u64) -> Option<u64> {
    let mut min: Option<u64> = None;
    for (b, n) in base.iter().zip(now) {
        let d = pick(n.as_ref()?).saturating_sub(pick(b.as_ref()?));
        min = Some(min.map_or(d, |m| m.min(d)));
    }
    min
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn split_urls(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|u| !u.is_empty()).map(String::from).collect()
}

/// Is a refused submission worth resending (same payload)? Busy sheds and
/// transport failures are; a validation refusal (bad signature, margin,
/// nonce) is not.
pub fn resendable(err: &str) -> bool {
    super::is_busy_reject(err) || err.starts_with("http:") || err.starts_with("parse:")
}

pub async fn run(plan: &PreloadPlan, a: RunArgs<'_>) -> PreloadReport {
    let t0 = Instant::now();
    let urls = split_urls(a.rpc_urls);
    let murls = split_urls(a.metrics_urls);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("http client");
    let mut rep = PreloadReport {
        target: plan.target,
        markets: plan.markets,
        owners: plan.owners,
        owner_offset: plan.owner_offset,
        owner_end: plan.owner_end,
        per_owner: plan.per_owner,
        batch: plan.batch,
        distance: plan.distance,
        levels: plan.levels,
        actions: plan.actions,
        ..Default::default()
    };
    let base = scrape_all(&client, &murls).await;
    let keys: Arc<Vec<SigningKey>> = Arc::new(
        super::load_sender_keys(plan.owners as usize, plan.owner_offset)
            .into_iter()
            .map(|k| k.signing_key)
            .collect(),
    );
    let placed = Arc::new(AtomicU64::new(0));
    let admitted = Arc::new(AtomicU64::new(0));
    let accepted = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let resends = Arc::new(AtomicU64::new(0));
    let failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sem = Arc::new(tokio::sync::Semaphore::new(a.concurrency.max(1)));
    let gap = Duration::from_secs_f64(1.0 / a.rate.max(0.001));
    let mut next = Instant::now();
    let mut tasks = Vec::new();
    let plan = Arc::new(plan.clone());
    let mut action_no = 0u64;
    for o in 0..plan.owners {
        let batches = plan.owner_batches(o);
        let mut last_nonce = 0u64;
        for orders in batches {
            tokio::time::sleep_until(next.into()).await;
            next += gap;
            let permit = sem.clone().acquire_owned().await.expect("semaphore");
            let n = orders.len() as u64;
            let nonce = now_ms().max(last_nonce + 1);
            last_nonce = nonce;
            let signed = sign_native_action(NativeAction::PlaceOrderBatch(orders), nonce, &keys[o as usize]);
            let payload = format!("0x{}", hex::encode(bincode::serialize(&signed).expect("bincode")));
            let url = urls[o as usize % urls.len()].clone();
            let (client, placed, admitted, accepted, failed, resends, failures) = (
                client.clone(),
                placed.clone(),
                admitted.clone(),
                accepted.clone(),
                failed.clone(),
                resends.clone(),
                failures.clone(),
            );
            let id = action_no;
            let owner_idx = plan.owner_offset + o as usize;
            action_no += 1;
            tasks.push(tokio::spawn(async move {
                let _permit = permit;
                placed.fetch_add(n, Ordering::Relaxed);
                let first = Instant::now();
                loop {
                    let r = super::submit_items(&client, &url, std::slice::from_ref(&payload), id, true).await;
                    let err = match r {
                        Ok(items) if items.first().is_some_and(|e| e.is_none()) => {
                            admitted.fetch_add(1, Ordering::Relaxed);
                            accepted.fetch_add(n, Ordering::Relaxed);
                            return;
                        }
                        Ok(items) => items.into_iter().flatten().next().unwrap_or_else(|| "empty reply".into()),
                        Err(e) => e,
                    };
                    if resendable(&err) && first.elapsed() + RESEND_BACKOFF < RESEND_MAX_AGE {
                        resends.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(RESEND_BACKOFF).await;
                        continue;
                    }
                    failed.fetch_add(1, Ordering::Relaxed);
                    let mut f = failures.lock().unwrap();
                    if f.len() < 10 {
                        f.push(format!("action {id} owner {owner_idx}: {err}"));
                    }
                    return;
                }
            }));
        }
    }
    for t in tasks {
        let _ = t.await;
    }
    rep.submit_seconds = t0.elapsed().as_secs_f64();
    rep.orders_placed = placed.load(Ordering::Relaxed);
    rep.actions_admitted = admitted.load(Ordering::Relaxed);
    rep.orders_accepted = accepted.load(Ordering::Relaxed);
    rep.actions_failed = failed.load(Ordering::Relaxed);
    rep.resends = resends.load(Ordering::Relaxed);
    rep.failures = failures.lock().unwrap().clone();
    eprintln!(
        "[preload] submitted {} actions ({} orders) in {:.1}s: admitted {} ({} orders), failed {}, resends {}",
        plan.actions, rep.orders_placed, rep.submit_seconds, rep.actions_admitted, rep.orders_accepted,
        rep.actions_failed, rep.resends
    );

    // Wait until every node shows the target resting (or timeout / stall).
    let deadline = t0 + a.timeout;
    let mut best = 0u64;
    let mut last_progress = Instant::now();
    let mut now = base.clone();
    loop {
        if !murls.is_empty() {
            now = scrape_all(&client, &murls).await;
            if let Some(r) = min_delta(&base, &now, |c| c.0) {
                if r > best {
                    best = r;
                    last_progress = Instant::now();
                }
                if r >= plan.target {
                    rep.reached = true;
                    break;
                }
            }
        }
        if murls.is_empty() || Instant::now() >= deadline || last_progress.elapsed() >= a.stall {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    rep.resting = if murls.is_empty() { None } else { min_delta(&base, &now, |c| c.0) };
    rep.resting_by_node = base
        .iter()
        .zip(&now)
        .map(|(b, n)| Some(n.as_ref()?.0.saturating_sub(b.as_ref()?.0)))
        .collect();
    rep.placed_accepted_by_node = base
        .iter()
        .zip(&now)
        .map(|(b, n)| Some(n.as_ref()?.1.saturating_sub(b.as_ref()?.1)))
        .collect();
    // The gauge is set at the start of the NEXT native block: give it a few s.
    let gauge_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let g: Vec<Option<u64>> = scrape_all(&client, &murls).await.iter().map(|c| c.map(|c| c.2)).collect();
        let done = g.iter().all(|v| v.is_some_and(|v| v >= plan.target));
        rep.resting_gauge_by_node = g;
        if done || Instant::now() >= gauge_deadline || murls.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    rep.seconds = t0.elapsed().as_secs_f64();
    rep
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_types::PRICE_BAND_DEFAULT_BPS;

    fn spec() -> PreloadSpec {
        PreloadSpec {
            target: 1_000_000,
            markets: 300,
            per_owner: DEFAULT_PER_OWNER,
            owner_offset: None,
            load_offset: 60,
            load_senders: 5_000,
            funded_end: 1_000_060,
            mid: 30_000,
            load_band: 5,
            distance: 1_000,
            levels: 100,
            batch: 0,
            qty: 1,
        }
    }

    fn whole(p: FixedPoint) -> u64 {
        (p.raw() / FixedPoint::SCALE) as u64
    }

    #[test]
    fn default_1m_over_300_markets() {
        let p = PreloadPlan::new(&spec()).unwrap();
        // ceil(1M / 900) owners, just past the load's senders 60..5060.
        assert_eq!(p.owners, 1_112);
        assert_eq!((p.owner_offset, p.owner_end), (5_060, 6_172));
        // One max-size action per owner: 900 <= 1024.
        assert_eq!(p.batch, 900);
        assert_eq!(p.actions, 1_112);
        assert_eq!(p.actions_per_owner_max, 1);
        assert_eq!((p.per_market_min, p.per_market_max), (3_333, 3_334));
        assert_eq!((p.buy_price_min, p.buy_price_max), (28_901, 29_000));
        assert_eq!((p.sell_price_min, p.sell_price_max), (31_000, 31_099));
    }

    #[test]
    fn every_market_gets_its_share_and_the_total_is_exact() {
        for (target, markets) in [(1_000_000u64, 300u64), (20_000, 10), (7, 3), (2, 5)] {
            let p = PreloadPlan::new(&PreloadSpec { target, markets, ..spec() }).unwrap();
            let per = p.per_market();
            assert_eq!(per.len() as u64, markets);
            assert_eq!(per.iter().sum::<u64>(), target);
            let (lo, hi) = (target / markets, target.div_ceil(markets));
            assert!(per.iter().all(|&n| n == lo || n == hi), "{per:?}");
            assert_eq!((p.per_market_min, p.per_market_max), (*per.iter().min().unwrap(), *per.iter().max().unwrap()));
        }
    }

    #[test]
    fn no_owner_passes_its_cap_and_owners_cover_every_order_once() {
        let p = PreloadPlan::new(&PreloadSpec { target: 20_003, markets: 10, per_owner: 700, ..spec() }).unwrap();
        assert_eq!(p.owners, 29); // ceil(20_003 / 700)
        let mut per_owner = vec![0u64; p.owners as usize];
        let mut total = 0u64;
        for o in 0..p.owners {
            let batches = p.owner_batches(o);
            let n: u64 = batches.iter().map(|b| b.len() as u64).sum();
            assert!(n <= 700);
            assert!(batches.iter().all(|b| !b.is_empty() && b.len() <= p.batch));
            per_owner[o as usize] = n;
            total += n;
        }
        assert_eq!(total, 20_003);
        assert_eq!(per_owner.last(), Some(&(20_003 - 28 * 700)));
        // order(k) agrees with the batches.
        let flat: Vec<PlaceOrderParams> = p.owner_batches(1).concat();
        for (i, o) in flat.iter().enumerate() {
            let (owner, got) = p.order(700 + i as u64);
            assert_eq!(owner, p.owner_offset + 1);
            assert_eq!(bincode::serialize(&got).unwrap(), bincode::serialize(o).unwrap());
        }
    }

    #[test]
    fn per_owner_above_the_chain_limit_is_refused() {
        assert!(PreloadPlan::new(&PreloadSpec { per_owner: OWNER_LIMIT + 1, ..spec() }).is_err());
        assert!(PreloadPlan::new(&PreloadSpec { per_owner: 0, ..spec() }).is_err());
        assert_eq!(PreloadPlan::new(&PreloadSpec { per_owner: OWNER_LIMIT, ..spec() }).unwrap().owners, 1_000);
    }

    #[test]
    fn prices_never_cross_and_stay_outside_the_load_band() {
        let p = PreloadPlan::new(&PreloadSpec { target: 50_000, markets: 7, levels: 13, ..spec() }).unwrap();
        let band = 5;
        let (mut buys, mut sells) = (0, 0);
        for k in 0..p.target {
            let (_, o) = p.order(k);
            let px = whole(o.price);
            assert_eq!(o.price.raw() % FixedPoint::SCALE, 0, "tick 1.0 aligned");
            assert_eq!(o.order_type, OrderType::Limit);
            assert_eq!(o.time_in_force, TimeInForce::GTC);
            assert!(!o.reduce_only && o.client_order_id.is_none());
            assert_eq!(o.quantity, FixedPoint::ONE);
            if o.is_buy {
                buys += 1;
                assert!(px <= 30_000 - 1_000 && px > 30_000 - 1_000 - 13, "{px}");
                assert!(px < 30_000 - band);
            } else {
                sells += 1;
                assert!((30_000 + 1_000..30_000 + 1_000 + 13).contains(&px), "{px}");
                assert!(px > 30_000 + band);
            }
        }
        // Both sides in (about) equal numbers, in every market.
        assert!((buys as i64 - sells as i64).abs() <= 7, "{buys} {sells}");
        for m in 1..=7u64 {
            let sides: Vec<bool> = (0..p.target).map(|k| p.order(k).1).filter(|o| o.market_id == m).map(|o| o.is_buy).collect();
            assert!(sides.contains(&true) && sides.contains(&false));
        }
    }

    #[test]
    fn distance_must_clear_the_load_band_and_stay_in_the_placement_band() {
        assert!(PreloadPlan::new(&PreloadSpec { distance: 5, ..spec() }).is_err(), "inside the load band");
        assert!(PreloadPlan::new(&PreloadSpec { distance: 6, ..spec() }).is_ok());
        // farthest = distance + levels - 1 <= 10% of mid (3000 ticks at 30000)
        assert!(PreloadPlan::new(&PreloadSpec { distance: 2_901, levels: 100, ..spec() }).is_ok());
        assert!(PreloadPlan::new(&PreloadSpec { distance: 2_902, levels: 100, ..spec() }).is_err());
        assert!(PreloadPlan::new(&PreloadSpec { levels: 0, ..spec() }).is_err());
    }

    #[test]
    fn batch_sizing_respects_the_batch_cap_and_the_per_sender_block_cap() {
        // auto = min(1024, per_owner)
        assert_eq!(PreloadPlan::new(&PreloadSpec { per_owner: 1_000, ..spec() }).unwrap().batch, 1_000);
        assert_eq!(PreloadPlan::new(&PreloadSpec { per_owner: 300, ..spec() }).unwrap().batch, 300);
        let p = PreloadPlan::new(&PreloadSpec { batch: 400, ..spec() }).unwrap();
        assert_eq!((p.batch, p.actions_per_owner_max), (400, 3)); // 400 + 400 + 100
        assert_eq!(p.actions, 1_111 * 3 + 1); // last owner holds 1M - 1111*900 = 100
        assert!(PreloadPlan::new(&PreloadSpec { batch: NATIVE_ORDERS_PER_BATCH_CAP + 1, ..spec() }).is_err());
        // 900 orders in batches of 10 = 90 actions > 64 per sender per block.
        assert!(PreloadPlan::new(&PreloadSpec { batch: 10, ..spec() }).is_err());
        assert!(PreloadPlan::new(&PreloadSpec { batch: 15, ..spec() }).is_ok()); // 60 actions
    }

    #[test]
    fn owners_are_funded_and_disjoint_from_the_load() {
        // Default: just past the load.
        let p = PreloadPlan::new(&PreloadSpec { load_senders: 100, ..spec() }).unwrap();
        assert_eq!(p.owner_offset, 160);
        // Explicit offset inside the load's range is refused.
        assert!(PreloadPlan::new(&PreloadSpec { owner_offset: Some(5_059), ..spec() }).is_err());
        assert!(PreloadPlan::new(&PreloadSpec { owner_offset: Some(5_060), ..spec() }).is_ok());
        // Below the load is fine when it is funded and does not overlap.
        assert!(PreloadPlan::new(&PreloadSpec { owner_offset: Some(60), load_offset: 2_000, target: 900 * 1_940, ..spec() }).is_ok());
        assert!(PreloadPlan::new(&PreloadSpec { owner_offset: Some(60), load_offset: 2_000, target: 900 * 1_941, ..spec() }).is_err());
        // Not funded: below the bulk range, or past the genesis' funded end.
        assert!(PreloadPlan::new(&PreloadSpec { owner_offset: Some(20), load_offset: 6_000, ..spec() }).is_err());
        // The 100k genesis (funded_end 100_060) holds 1112 owners at 5060..6172.
        assert!(PreloadPlan::new(&PreloadSpec { funded_end: 100_060, ..spec() }).is_ok());
        assert!(PreloadPlan::new(&PreloadSpec { funded_end: 6_171, ..spec() }).is_err());
        assert!(PreloadPlan::new(&PreloadSpec { funded_end: 6_172, ..spec() }).is_ok());
    }

    #[test]
    fn owner_margin_is_reported() {
        let p = PreloadPlan::new(&spec()).unwrap();
        // 900 orders x 1 lot x <= 31_099 / 20.
        assert_eq!(p.owner_margin_trs_max, 900 * 31_099 / 20);
    }

    #[test]
    fn zero_target_or_markets_is_refused() {
        assert!(PreloadPlan::new(&PreloadSpec { target: 0, ..spec() }).is_err());
        assert!(PreloadPlan::new(&PreloadSpec { markets: 0, ..spec() }).is_err());
    }

    #[test]
    fn min_delta_is_over_every_node_and_none_if_one_is_unreadable() {
        let base = vec![Some((10, 0, 0)), Some((20, 0, 0)), Some((0, 0, 0))];
        let now = vec![Some((110, 0, 0)), Some((70, 0, 0)), Some((100, 0, 0))];
        assert_eq!(min_delta(&base, &now, |c| c.0), Some(50));
        let now_bad = vec![Some((110, 0, 0)), None, Some((100, 0, 0))];
        assert_eq!(min_delta(&base, &now_bad, |c| c.0), None);
    }

    #[test]
    fn busy_and_transport_errors_are_resent_validation_errors_are_not() {
        assert!(resendable("rpc: mempool: pool full (pre-verify)"));
        assert!(resendable("\"admission limit reached, retry\""));
        assert!(resendable("http: connection refused"));
        assert!(!resendable("\"order rejected: insufficient margin\""));
        assert!(!resendable("\"invalid signature\""));
    }

    #[test]
    fn signed_batch_round_trips_and_stays_under_the_body_cap() {
        let p = PreloadPlan::new(&spec()).unwrap();
        let key = super::super::load_sender_keys(1, p.owner_offset).remove(0).signing_key;
        let b = p.owner_batches(0).remove(0);
        assert_eq!(b.len(), 900);
        let signed = sign_native_action(NativeAction::PlaceOrderBatch(b), 1, &key);
        let bytes = bincode::serialize(&signed).unwrap();
        // ~70 B per order; a 12 MB block body holds well over 100 such actions.
        assert!(bytes.len() < 900 * 100, "{}", bytes.len());
    }

    #[test]
    fn price_band_constants_match_the_assumptions() {
        assert_eq!(PRICE_BAND_FALLBACK_CLAMP_BPS, 1_000);
        const { assert!(PRICE_BAND_DEFAULT_BPS >= PRICE_BAND_FALLBACK_CLAMP_BPS) };
        assert_eq!(NATIVE_ORDERS_PER_BATCH_CAP, 1_024);
        assert_eq!(OWNER_LIMIT, 1_000);
    }
}
