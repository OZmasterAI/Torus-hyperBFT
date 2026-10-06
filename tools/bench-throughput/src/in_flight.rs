//! `--max-in-flight N` (option A, s17): a per-sender in-flight cap for the
//! econ load.
//!
//! UNIT: one signed native action — what the econ sender loop signs and
//! submits (a `PlaceOrderBatch` of `--batch-size` orders, a `PlaceOrder`, or a
//! `CancelAllOrders`). A sender may fire only while its in-flight count is
//! below N; a fire of `--submit-batch` actions takes that many slots (with the
//! harness default `SUBMIT=1` one fire = one action = one slot).
//!
//! A slot is released when the action
//!   * is seen COMMITTED: one shared watcher ([`run_tail`]) tails every new
//!     block body (`torus_getBlockBody`) from ONE validator and releases by
//!     action identity (nonce + signature, [`action_key`]; the body carries no
//!     sender address and recovering it would cost an ecrecover per action);
//!   * is REFUSED by the RPC: a whole-call error, or the per-item error of a
//!     partially accepted batch. With `--retry-busy` a BUSY reply is retried
//!     and the slot is held until the final outcome;
//!   * TIMES OUT: `nonce + NONCE_WINDOW_MS + TIMEOUT_MARGIN_MS` has passed
//!     (the mempool evicts an admitted action whose nonce is older than the
//!     60 s window, invisibly to the bench; a block the tail missed ends here
//!     too).
//!
//! Open-order budget with the cap: the sender's estimate is the orders placed
//! since its last COMMITTED cancel-all, including its in-flight places
//! ([`Tracker::estimate`]). A cancel-all runs before every place of its block,
//! so places committed in the same block as the cancel-all survive it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::Notify;
use torus_types::{ActionSignature, NativeAction};

/// Slack past the nonce window before an unseen action is written off.
pub const TIMEOUT_MARGIN_MS: u64 = 10_000;
/// Watcher poll period: one `eth_blockNumber`, then every new body in order.
pub const TAIL_POLL: Duration = Duration::from_millis(200);
/// Body fetch attempts (one per poll) before a block is counted missed and
/// skipped; its actions then release by timeout.
pub const TAIL_MAX_ATTEMPTS: u32 = 5;

/// Why a slot was released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    Committed = 0,
    Refused = 1,
    Timeout = 2,
}

/// Identity of one signed action: its nonce and signature. Unique per
/// (sender, nonce) — the signature binds the signer, the action and the nonce.
pub fn action_key(nonce: u64, sig: &ActionSignature) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    nonce.hash(&mut h);
    // bincode, not the JSON text: identical bytes whatever serde_json
    // features the node and the bench were built with.
    bincode::serialize(sig).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// `(places, is_cancel_all)` for one action: orders it places, and whether it
/// is a cancel-all (which resets the open-order estimate when it commits).
pub fn action_shape(action: &NativeAction) -> (u64, bool) {
    match action {
        NativeAction::PlaceOrderBatch(orders) => (orders.len() as u64, false),
        NativeAction::PlaceOrder(_) => (1, false),
        NativeAction::CancelAllOrders { .. } => (0, true),
        _ => (0, false),
    }
}

/// The committed actions of one `torus_getBlockBody` result: `(key, ok)` per
/// native action, where `ok` is false only for a `"skipped"` or `"failed"`
/// status. `None` when the reply has no action list.
pub fn body_action_keys(result: &serde_json::Value) -> Option<Vec<(u64, bool)>> {
    let actions = result.get("nativeActions")?.as_array()?;
    let status = result.get("nativeActionStatus").and_then(|s| s.as_array());
    Some(
        actions
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                let nonce = a.get("nonce")?.as_u64()?;
                let sig: ActionSignature =
                    serde_json::from_value(a.get("signature")?.clone()).ok()?;
                let ok = status
                    .and_then(|s| s.get(i))
                    .and_then(|s| s.as_str())
                    .is_none_or(|s| s == "executed");
                Some((action_key(nonce, &sig), ok))
            })
            .collect(),
    )
}

struct Pending {
    sender: usize,
    nonce: u64,
    places: u64,
    cancel: bool,
}

#[derive(Clone, Default)]
struct SenderState {
    in_flight: usize,
    /// Orders of in-flight places.
    pending_places: u64,
    /// Orders committed (or timed out) since the last committed cancel-all.
    committed_open: u64,
    cancels_in_flight: usize,
}

/// Pure in-flight accounting (no I/O, no clock): the unit tests drive it.
pub struct Tracker {
    cap: usize,
    timeout_ms: u64,
    pending: HashMap<u64, Pending>,
    senders: Vec<SenderState>,
    /// Indexed by [`Release`].
    pub released: [u64; 3],
}

impl Tracker {
    pub fn new(senders: usize, cap: usize, timeout_ms: u64) -> Self {
        Self {
            cap,
            timeout_ms,
            pending: HashMap::new(),
            senders: vec![SenderState::default(); senders],
            released: [0; 3],
        }
    }

    pub fn in_flight(&self, sender: usize) -> usize {
        self.senders[sender].in_flight
    }

    /// Actions submitted and not yet released, all senders.
    pub fn open(&self) -> usize {
        self.pending.len()
    }

    /// Open-order estimate: orders placed since the last committed
    /// cancel-all, in-flight places included.
    pub fn estimate(&self, sender: usize) -> u64 {
        let s = &self.senders[sender];
        s.committed_open + s.pending_places
    }

    pub fn cancel_pending(&self, sender: usize) -> bool {
        self.senders[sender].cancels_in_flight > 0
    }

    /// May `sender` fire now? Below the cap, and — with a budget — not
    /// waiting on its own in-flight cancel-all while the next batch would
    /// cross the budget (a second cancel-all would only duplicate it).
    pub fn ready(&self, sender: usize, batch: u64, budget: u64) -> bool {
        self.senders[sender].in_flight < self.cap
            && !(budget > 0
                && self.cancel_pending(sender)
                && self.estimate(sender) + batch.max(1) > budget)
    }

    /// Register one action as in flight (BEFORE it is sent, so a fast commit
    /// always finds it).
    pub fn submit(&mut self, sender: usize, key: u64, nonce: u64, places: u64, cancel: bool) {
        if self
            .pending
            .insert(
                key,
                Pending {
                    sender,
                    nonce,
                    places,
                    cancel,
                },
            )
            .is_some()
        {
            return; // same signed action registered twice: one slot
        }
        let s = &mut self.senders[sender];
        s.in_flight += 1;
        s.pending_places += places;
        s.cancels_in_flight += usize::from(cancel);
    }

    /// Apply a submit outcome (`Err` = every item refused; `Ok` = per-item
    /// error, `None` = admitted). Returns the senders that got a slot back.
    pub fn apply_result(
        &mut self,
        keys: &[u64],
        result: &Result<Vec<Option<String>>, String>,
    ) -> Vec<usize> {
        let refused: Vec<u64> = match result {
            Err(_) => keys.to_vec(),
            // Items past the reply's length stay in flight (commit/timeout).
            Ok(items) => keys
                .iter()
                .zip(items)
                .filter(|(_, e)| e.is_some())
                .map(|(k, _)| *k)
                .collect(),
        };
        refused
            .into_iter()
            .filter_map(|k| self.release(k, Release::Refused))
            .map(|(s, _)| s)
            .collect()
    }

    /// One committed block. Cancel-alls are applied before places (the
    /// executor runs a block's cancel-alls first).
    pub fn on_block(&mut self, actions: &[(u64, bool)]) -> Vec<usize> {
        let mut woken = Vec::new();
        for cancels_pass in [true, false] {
            for &(key, ok) in actions {
                if self.pending.get(&key).map(|p| p.cancel) != Some(cancels_pass) {
                    continue;
                }
                let Some((s, p)) = self.release(key, Release::Committed) else {
                    continue;
                };
                if p.cancel {
                    if ok {
                        self.senders[s].committed_open = 0;
                    }
                } else {
                    self.senders[s].committed_open += p.places;
                }
                woken.push(s);
            }
        }
        woken
    }

    /// Release every action whose nonce is older than `now_ms - timeout`.
    pub fn expire(&mut self, now_ms: u64) -> Vec<usize> {
        let late: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.nonce.saturating_add(self.timeout_ms) < now_ms)
            .map(|(k, _)| *k)
            .collect();
        let mut woken = Vec::new();
        for k in late {
            if let Some((s, p)) = self.release(k, Release::Timeout) {
                // It may have landed in a block the tail missed: keep its
                // orders in the estimate until a committed cancel-all.
                self.senders[s].committed_open += p.places;
                woken.push(s);
            }
        }
        woken
    }

    /// Drop `key` from the in-flight set; `None` if it is not (or no
    /// longer) in flight.
    fn release(&mut self, key: u64, cause: Release) -> Option<(usize, Pending)> {
        let p = self.pending.remove(&key)?;
        let s = &mut self.senders[p.sender];
        s.in_flight -= 1;
        s.pending_places -= p.places;
        s.cancels_in_flight -= usize::from(p.cancel);
        self.released[cause as usize] += 1;
        Some((p.sender, p))
    }
}

/// Watcher counters.
#[derive(Default)]
pub struct TailStats {
    pub fetched: AtomicU64,
    pub errors: AtomicU64,
    pub missed: AtomicU64,
}

/// Shared cap state: one per run, cloned (Arc) into every sender.
pub struct Cap {
    pub n: usize,
    pub url: String,
    pub tracker: Mutex<Tracker>,
    wake: Vec<Notify>,
    pub tail: TailStats,
}

impl Cap {
    pub fn new(senders: usize, n: usize, url: String) -> Self {
        Self {
            n,
            url,
            tracker: Mutex::new(Tracker::new(
                senders,
                n,
                torus_types::eip712::NONCE_WINDOW_MS + TIMEOUT_MARGIN_MS,
            )),
            wake: (0..senders).map(|_| Notify::new()).collect(),
            tail: TailStats::default(),
        }
    }

    fn wake(&self, senders: Vec<usize>) {
        for s in senders {
            self.wake[s].notify_one();
        }
    }

    /// Wait (no spinning) until `sender` may fire. `None` at the deadline,
    /// else `(estimate, cancel_pending, waited)`.
    pub async fn wait_ready(
        &self,
        sender: usize,
        batch: u64,
        budget: u64,
        deadline: Instant,
    ) -> Option<(u64, bool, bool)> {
        let mut waited = false;
        loop {
            {
                let t = self.tracker.lock().unwrap();
                if t.ready(sender, batch, budget) {
                    return Some((t.estimate(sender), t.cancel_pending(sender), waited));
                }
            }
            waited = true;
            // A release between the check and this await leaves a stored
            // permit (notify_one), so no wakeup is lost.
            tokio::select! {
                _ = self.wake[sender].notified() => {}
                _ = tokio::time::sleep_until(deadline.into()) => return None,
            }
        }
    }

    pub fn submit(&self, sender: usize, entries: &[(u64, u64, u64, bool)]) {
        let mut t = self.tracker.lock().unwrap();
        for &(key, nonce, places, cancel) in entries {
            t.submit(sender, key, nonce, places, cancel);
        }
    }

    pub fn apply_result(&self, keys: &[u64], result: &Result<Vec<Option<String>>, String>) {
        let woken = self.tracker.lock().unwrap().apply_result(keys, result);
        self.wake(woken);
    }

    /// End-of-run line (parsed by tools/matched-bench/summarize.py).
    pub fn report(&self) -> String {
        let t = self.tracker.lock().unwrap();
        report_line(self.n, &t, &self.tail, &self.url)
    }
}

pub fn report_line(n: usize, t: &Tracker, tail: &TailStats, url: &str) -> String {
    let [committed, refused, timeout] = t.released;
    format!(
        "In-flight cap {n} action(s)/sender: released committed={committed} refused={refused} \
         timeout={timeout} | in flight at end {} | block tail {url}: fetched={} errors={} missed={}",
        t.open(),
        tail.fetched.load(Ordering::Relaxed),
        tail.errors.load(Ordering::Relaxed),
        tail.missed.load(Ordering::Relaxed),
    )
}

/// Accepted actions by kind, both modes (cap on or off).
#[derive(Default)]
pub struct MixStats {
    pub sent_place: AtomicU64,
    pub sent_cancel: AtomicU64,
    pub acc_place: AtomicU64,
    pub acc_cancel: AtomicU64,
}

impl MixStats {
    /// Count one fire: `cancels[i]` = item i is a cancel-all.
    pub fn record(&self, cancels: &[bool], result: &Result<Vec<Option<String>>, String>) {
        for (i, &cancel) in cancels.iter().enumerate() {
            let (sent, acc) = if cancel {
                (&self.sent_cancel, &self.acc_cancel)
            } else {
                (&self.sent_place, &self.acc_place)
            };
            sent.fetch_add(1, Ordering::Relaxed);
            let admitted = match result {
                Err(_) => false,
                Ok(items) => items.get(i).is_none_or(|e| e.is_none()),
            };
            if admitted {
                acc.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// End-of-run line (parsed by tools/matched-bench/summarize.py).
    pub fn report(&self) -> String {
        let ld = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let (p, c) = (ld(&self.acc_place), ld(&self.acc_cancel));
        let pct = |x: u64| {
            if p + c == 0 {
                0.0
            } else {
                x as f64 * 100.0 / (p + c) as f64
            }
        };
        format!(
            "Econ mix (load-gen accepted): place {p} ({:.1}%) | cancel-all {c} ({:.1}%) \
             [sent: place {} cancel-all {}]",
            pct(p),
            pct(c),
            ld(&self.sent_place),
            ld(&self.sent_cancel),
        )
    }
}

/// Validate `--max-in-flight` / `--in-flight-watch-rpc`. `Ok(None)` = off.
/// The default watch url is the LAST `--rpc-urls` entry: each sender spreads
/// its fires round-robin over every url, so no url is any sender's main one,
/// and the live monitor and post-run reads use the first.
pub fn cap_plan(
    max_in_flight: usize,
    econ: bool,
    watch_rpc: &str,
    rpc_urls: &str,
) -> Result<Option<(usize, String)>, String> {
    if max_in_flight == 0 {
        if !watch_rpc.is_empty() {
            return Err("--in-flight-watch-rpc needs --max-in-flight N (N > 0)".into());
        }
        return Ok(None);
    }
    if !econ {
        return Err("--max-in-flight needs --econ".into());
    }
    let url = if watch_rpc.is_empty() {
        rpc_urls
            .split(',')
            .map(str::trim)
            .rfind(|u| !u.is_empty())
            .ok_or("--max-in-flight: no --rpc-urls to watch")?
    } else {
        watch_rpc
    };
    Ok(Some((max_in_flight, url.to_string())))
}

/// The shared block-body tail: every `TAIL_POLL` expire timed-out actions,
/// read the watch node's height and process every new body in order. A body
/// that cannot be read is retried on the next poll (the cursor stays on it)
/// up to `TAIL_MAX_ATTEMPTS`, then counted missed and skipped. Runs until
/// `stop`.
pub async fn run_tail(
    cap: std::sync::Arc<Cap>,
    client: std::sync::Arc<reqwest::Client>,
    start_block: u64,
    stop: Instant,
) {
    let mut next = start_block + 1;
    let mut attempts = 0u32;
    while Instant::now() < stop {
        tokio::time::sleep(TAIL_POLL).await;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let woken = cap.tracker.lock().unwrap().expire(now_ms);
        cap.wake(woken);
        let Some(height) = super::fetch_block_number(&client, &cap.url).await else {
            cap.tail.errors.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        while next <= height && Instant::now() < stop {
            match super::fetch_block_body_json(&client, &cap.url, next)
                .await
                .as_ref()
                .and_then(body_action_keys)
            {
                Some(actions) => {
                    let woken = cap.tracker.lock().unwrap().on_block(&actions);
                    cap.wake(woken);
                    cap.tail.fetched.fetch_add(1, Ordering::Relaxed);
                    next += 1;
                    attempts = 0;
                }
                None => {
                    cap.tail.errors.fetch_add(1, Ordering::Relaxed);
                    attempts += 1;
                    if attempts >= TAIL_MAX_ATTEMPTS {
                        cap.tail.missed.fetch_add(1, Ordering::Relaxed);
                        next += 1;
                        attempts = 0;
                    }
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_types::{eip712::sign_native_action, OrderType, PlaceOrderParams, TimeInForce};

    const T: u64 = 70_000; // timeout_ms used below

    fn place(n: usize) -> NativeAction {
        let o = PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: torus_types::FixedPoint::ONE,
            quantity: torus_types::FixedPoint::ONE,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        if n == 1 {
            NativeAction::PlaceOrder(o)
        } else {
            NativeAction::PlaceOrderBatch(vec![o; n])
        }
    }

    #[test]
    fn action_shape_counts_orders_and_cancel_alls() {
        assert_eq!(action_shape(&place(400)), (400, false));
        assert_eq!(action_shape(&place(1)), (1, false));
        assert_eq!(
            action_shape(&NativeAction::CancelAllOrders { market_id: None }),
            (0, true)
        );
    }

    #[test]
    fn slot_is_held_until_commit_then_released() {
        let mut t = Tracker::new(2, 1, T);
        assert!(t.ready(0, 400, 0));
        t.submit(0, 11, 1_000, 400, false);
        assert_eq!(t.in_flight(0), 1);
        assert!(!t.ready(0, 400, 0), "cap 1 reached");
        assert!(t.ready(1, 400, 0), "other senders are independent");
        // A block with someone else's action (unknown key) releases nothing.
        assert!(t.on_block(&[(99, true)]).is_empty());
        assert_eq!(t.on_block(&[(11, true)]), vec![0]);
        assert_eq!(t.in_flight(0), 0);
        assert!(t.ready(0, 400, 0));
        assert_eq!(t.released, [1, 0, 0]);
        // Seen again (late duplicate): idempotent.
        assert!(t.on_block(&[(11, true)]).is_empty());
        assert_eq!(t.released, [1, 0, 0]);
        assert_eq!(t.open(), 0);
    }

    #[test]
    fn refusal_releases_the_whole_call() {
        let mut t = Tracker::new(1, 2, T);
        t.submit(0, 1, 1_000, 400, false);
        t.submit(0, 2, 1_001, 0, true);
        assert!(!t.ready(0, 400, 0));
        let woken = t.apply_result(&[1, 2], &Err("http: connection refused".into()));
        assert_eq!(woken, vec![0, 0]);
        assert_eq!(t.in_flight(0), 0);
        assert_eq!(t.released, [0, 2, 0]);
        assert_eq!(t.estimate(0), 0, "refused places never land");
        assert!(!t.cancel_pending(0));
    }

    #[test]
    fn partial_batch_releases_only_the_refused_items() {
        let mut t = Tracker::new(1, 3, T);
        for k in 1..=3 {
            t.submit(0, k, 1_000 + k, 400, false);
        }
        let r = Ok(vec![None, Some("\"mempool: busy\"".to_string()), None]);
        assert_eq!(t.apply_result(&[1, 2, 3], &r), vec![0]);
        assert_eq!(t.in_flight(0), 2);
        assert_eq!(t.released, [0, 1, 0]);
        assert_eq!(t.estimate(0), 800);
        // The admitted two release on commit.
        assert_eq!(t.on_block(&[(1, true), (3, true)]), vec![0, 0]);
        assert_eq!(t.released, [2, 1, 0]);
    }

    #[test]
    fn timeout_releases_after_nonce_window_plus_margin() {
        let mut t = Tracker::new(1, 2, T);
        t.submit(0, 1, 1_000, 400, false);
        t.submit(0, 2, 5_000, 0, true);
        assert!(t.expire(1_000 + T).is_empty(), "not before nonce + timeout");
        assert_eq!(t.expire(1_000 + T + 1), vec![0]);
        assert_eq!(t.in_flight(0), 1);
        // A timed-out place may have landed in a missed block: it stays in
        // the estimate (upper bound) until a committed cancel-all.
        assert_eq!(t.estimate(0), 400);
        assert_eq!(t.expire(5_000 + T + 1), vec![0]);
        assert_eq!(t.released, [0, 0, 2]);
        assert!(!t.cancel_pending(0));
        assert_eq!(t.estimate(0), 400, "a timed-out cancel-all clears nothing");
    }

    #[tokio::test]
    async fn busy_retry_holds_the_slot_until_the_final_outcome() {
        use std::sync::atomic::AtomicUsize;
        const BUSY: &str =
            "0 accepted (1 sent): \"mempool: busy, admission limit reached (pre-verify), retry later\"";
        let t = Mutex::new(Tracker::new(1, 1, T));
        t.lock().unwrap().submit(0, 7, 1_000, 400, false);
        let calls = AtomicUsize::new(0);
        let result = crate::submit_until_admitted(
            || {
                let n = calls.fetch_add(1, Ordering::Relaxed);
                // Every BUSY retry still sees the slot taken.
                assert_eq!(t.lock().unwrap().in_flight(0), 1);
                async move {
                    if n < 2 {
                        Err(BUSY.to_string())
                    } else {
                        Ok(vec![None])
                    }
                }
            },
            Duration::from_millis(1),
            Instant::now() + Duration::from_secs(5),
        )
        .await;
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        assert!(t.lock().unwrap().apply_result(&[7], &result).is_empty());
        assert_eq!(
            t.lock().unwrap().in_flight(0),
            1,
            "admitted: held until commit"
        );
        // A retry that gives up is one refusal.
        let mut t2 = Tracker::new(1, 1, T);
        t2.submit(0, 8, 1_000, 400, false);
        assert_eq!(t2.apply_result(&[8], &Err(BUSY.to_string())), vec![0]);
        assert_eq!(t2.released, [0, 1, 0]);
    }

    // Budget with the cap: estimate = places since the last COMMITTED
    // cancel-all, in-flight places included.
    #[test]
    fn budget_estimate_counts_in_flight_places_since_last_committed_cancel_all() {
        let mut t = Tracker::new(1, 8, T);
        t.submit(0, 1, 1_000, 400, false);
        assert_eq!(t.estimate(0), 400, "in flight counts");
        t.on_block(&[(1, true)]);
        assert_eq!(t.estimate(0), 400, "committed counts");
        t.submit(0, 2, 1_001, 400, false);
        t.submit(0, 3, 1_002, 0, true);
        assert!(t.cancel_pending(0));
        assert_eq!(t.estimate(0), 800, "an in-flight cancel-all clears nothing");
        // With a cancel-all in flight and the next batch over budget the
        // sender waits; under budget it may place.
        assert!(!t.ready(0, 400, 900));
        assert!(t.ready(0, 100, 900));
        assert!(t.ready(0, 400, 0), "no budget: only the cap gates");
        // Place 2 commits in the SAME block as the cancel-all: the executor
        // runs the cancel-all first, so place 2 survives it.
        t.on_block(&[(2, true), (3, true)]);
        assert!(!t.cancel_pending(0));
        assert_eq!(t.estimate(0), 400);
        // A place still in flight when a cancel-all commits survives it.
        t.submit(0, 4, 1_003, 0, true);
        t.submit(0, 5, 1_004, 400, false);
        t.on_block(&[(4, true)]);
        assert_eq!(t.estimate(0), 400, "place 5 still in flight");
        // A skipped/failed cancel-all clears nothing.
        t.submit(0, 6, 1_005, 0, true);
        t.on_block(&[(6, false)]);
        assert_eq!(t.estimate(0), 400);
    }

    fn signed(nonce: u64, action: NativeAction) -> torus_types::SignedNativeAction {
        let key = k256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
        sign_native_action(action, nonce, &key)
    }

    #[test]
    fn body_keys_match_the_submit_side_key() {
        let a = signed(1_000, place(3));
        let b = signed(1_001, NativeAction::CancelAllOrders { market_id: None });
        // What torus_getBlockBody returns: serde_json::to_value per action.
        let body = serde_json::json!({
            "blockNumber": "0x5",
            "nativeActions": [serde_json::to_value(&a).unwrap(), serde_json::to_value(&b).unwrap()],
            "nativeActionCount": 2,
            "nativeActionStatus": ["executed", "failed"],
        });
        let keys = body_action_keys(&body).unwrap();
        assert_eq!(
            keys,
            vec![
                (action_key(a.nonce, &a.signature), true),
                (action_key(b.nonce, &b.signature), false)
            ]
        );
        assert_ne!(keys[0].0, keys[1].0);
        // Same action, other nonce: other key.
        let c = signed(1_002, place(3));
        assert_ne!(action_key(c.nonce, &c.signature), keys[0].0);
        // No status list (old node / not executed yet): every action ok.
        let body = serde_json::json!({"nativeActions": [serde_json::to_value(&a).unwrap()]});
        assert_eq!(body_action_keys(&body).unwrap(), vec![(keys[0].0, true)]);
        assert_eq!(
            body_action_keys(&serde_json::json!({"nativeActionCount": 0})),
            None
        );
    }

    #[test]
    fn report_lines_are_stable() {
        let mut t = Tracker::new(1, 1, T);
        t.submit(0, 1, 1_000, 400, false);
        t.submit(0, 2, 1_000, 400, false);
        t.submit(0, 3, 1_000, 400, false);
        t.submit(0, 4, 1_000, 400, false);
        t.on_block(&[(1, true)]);
        t.apply_result(&[2], &Err("x".into()));
        t.expire(1_000 + T + 1);
        t.submit(0, 5, 1_000_000, 400, false);
        let tail = TailStats::default();
        tail.fetched.store(120, Ordering::Relaxed);
        tail.errors.store(3, Ordering::Relaxed);
        tail.missed.store(1, Ordering::Relaxed);
        assert_eq!(
            report_line(1, &t, &tail, "http://127.0.0.1:8647"),
            "In-flight cap 1 action(s)/sender: released committed=1 refused=1 timeout=2 \
             | in flight at end 1 | block tail http://127.0.0.1:8647: fetched=120 errors=3 missed=1"
        );
        let m = MixStats::default();
        m.record(
            &[false, true, false],
            &Ok(vec![None, None, Some("e".into())]),
        );
        m.record(&[false], &Err("e".into()));
        m.record(&[false], &Ok(vec![None]));
        assert_eq!(
            m.report(),
            "Econ mix (load-gen accepted): place 2 (66.7%) | cancel-all 1 (33.3%) \
             [sent: place 4 cancel-all 1]"
        );
        assert_eq!(
            MixStats::default().report(),
            "Econ mix (load-gen accepted): place 0 (0.0%) | cancel-all 0 (0.0%) \
             [sent: place 0 cancel-all 0]"
        );
    }

    #[test]
    fn cap_plan_defaults_off_needs_econ_and_picks_the_last_url() {
        let urls = "http://a:1,http://b:2,http://c:3";
        assert_eq!(cap_plan(0, true, "", urls), Ok(None));
        assert_eq!(cap_plan(0, false, "", urls), Ok(None));
        assert!(
            cap_plan(0, true, "http://x:9", urls).is_err(),
            "watch url without a cap"
        );
        assert!(cap_plan(1, false, "", urls).is_err(), "cap needs --econ");
        assert_eq!(
            cap_plan(1, true, "", urls),
            Ok(Some((1, "http://c:3".to_string())))
        );
        assert_eq!(
            cap_plan(4, true, "http://x:9", urls),
            Ok(Some((4, "http://x:9".to_string())))
        );
        assert_eq!(
            cap_plan(1, true, "", "http://a:1"),
            Ok(Some((1, "http://a:1".to_string())))
        );
    }
}
