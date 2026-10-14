//! `--max-in-flight N` (option A, s17): a per-sender in-flight cap for the
//! econ load.
//!
//! UNIT: one signed native action — what the econ sender loop signs and
//! submits (a `PlaceOrderBatch` of `--batch-size` orders, a `PlaceOrder`, or a
//! `CancelAllOrders`). A sender may fire only while its in-flight count is
//! below N; a fire of `--submit-batch` S actions takes S slots, so in flight
//! can reach N-1+S (with the harness default `SUBMIT=1` one fire = one action
//! = one slot, and in flight never passes N).
//!
//! A slot is released when the action
//!   * is seen COMMITTED: one shared watcher ([`run_tail`]) tails every new
//!     block body (`torus_getBlockBody`) from ONE validator and releases by
//!     action identity (nonce + signature, [`action_key`]; the body carries no
//!     sender address and recovering it would cost an ecrecover per action).
//!     How the watcher learns a new body exists is `--in-flight-tail`
//!     ([`TailMode`]): `head` (default, every campaign before it) reads
//!     `eth_blockNumber` first, `commit` probes the next body directly;
//!   * is REFUSED by the RPC: an error reply, nothing admitted, or the
//!     per-item error of a partially accepted batch. With `--retry-busy` a
//!     BUSY reply is retried and the slot is held until the final outcome. A
//!     transport failure (no reply / unreadable reply) is NOT a refusal — the
//!     node may hold the action — so commit or timeout releases it;
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

use crate::release_timing::{wall_ms, ReleaseTiming, TailTiming, WaitReason};

/// Slack past the nonce window before an unseen action is written off.
pub const TIMEOUT_MARGIN_MS: u64 = 10_000;
/// Watcher poll period: one `eth_blockNumber` (head mode) or one probe of the
/// next height (commit mode), then every new body in order.
pub const TAIL_POLL: Duration = Duration::from_millis(200);
/// Body fetch attempts (one per poll) before a block is counted missed and
/// skipped; its actions then release by timeout. Commit mode: also the polls
/// a height may stay without a body before the tail checks whether a later
/// height has one (a hole).
pub const TAIL_MAX_ATTEMPTS: u32 = 5;

/// `--in-flight-tail`: how the shared tail learns that a new block body
/// exists on the watch node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailMode {
    /// Default (every campaign before the flag): `eth_blockNumber`, then each
    /// body up to it. `eth_blockNumber` is the node's eth head, min(applied
    /// marker, committed) (`torus-rpc` `eth::eth_head`); under item 6 Phase 3
    /// the applied marker reaches the disk only at checkpoints, so the head
    /// moves in ~100-block steps and slots are released in bursts.
    Head,
    /// Probe `torus_getBlockBody(next)` directly. The body is written at
    /// COMMIT, in the same RocksDB batch as the header
    /// (`torus-consensus` `persist_committed_block_durably_timed`, called by
    /// `dispatch_to_exec` before the block is executed), and the RPC reads
    /// `CF_BLOCK_BODIES` with no applied-marker or head check. A null result
    /// = not committed yet (not an error). A height without a readable body
    /// is skipped only once known committed (a later body exists, or
    /// `eth_blockNumber` has passed it). Released actions carry no
    /// executed/skipped status yet (the status row is written by execution),
    /// so every action counts as executed: a cancel-all that later fails
    /// still resets the sender's open-order estimate.
    Commit,
}

/// Validate `--in-flight-tail` (`""` = the default, head).
pub fn tail_plan(max_in_flight: usize, tail: &str) -> Result<TailMode, String> {
    match tail {
        "" | "head" => Ok(TailMode::Head),
        "commit" if max_in_flight == 0 => {
            Err("--in-flight-tail commit needs --max-in-flight N (N > 0)".into())
        }
        "commit" => Ok(TailMode::Commit),
        other => Err(format!(
            "unknown --in-flight-tail {other:?} (expected \"head\" or \"commit\")"
        )),
    }
}

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
/// status. `None` when the reply has no action list. (Tests only: the tail
/// reads [`body_probe`], which also tells a not-yet-committed height apart.)
#[cfg(test)]
pub fn body_action_keys(reply: &[u8]) -> Option<Vec<(u64, bool)>> {
    match body_probe(reply) {
        BodyFetch::Keys(keys) => Some(keys),
        BodyFetch::Absent | BodyFetch::Error => None,
    }
}

/// One `torus_getBlockBody` read by the tail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyFetch {
    /// The body's actions, as [`body_action_keys`].
    Keys(Vec<(u64, bool)>),
    /// A `null` result: no body stored at this height (not committed yet).
    Absent,
    /// No reply, an error reply, or a reply that does not parse.
    Error,
}

/// Classify one raw `torus_getBlockBody` reply ([`BodyFetch`]).
pub fn body_probe(reply: &[u8]) -> BodyFetch {
    // Only what the tail needs: unknown fields (the action itself, the EVM
    // list, ...) are skipped by the parser, never built into a Value tree.
    #[derive(serde::Deserialize)]
    struct Reply {
        #[serde(default)]
        result: Option<Body>,
        #[serde(default)]
        error: Option<serde::de::IgnoredAny>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Body {
        native_actions: Vec<Action>,
        #[serde(default)]
        native_action_status: Option<Vec<String>>,
    }
    #[derive(serde::Deserialize)]
    struct Action {
        nonce: u64,
        signature: ActionSignature,
    }
    let body = match serde_json::from_slice::<Reply>(reply) {
        Ok(Reply {
            result: Some(body), ..
        }) => body,
        Ok(Reply {
            result: None,
            error: None,
        }) => return BodyFetch::Absent,
        Ok(_) | Err(_) => return BodyFetch::Error,
    };
    let status = body.native_action_status.unwrap_or_default();
    BodyFetch::Keys(
        body.native_actions
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let ok = status.get(i).is_none_or(|s| s == "executed");
                (action_key(a.nonce, &a.signature), ok)
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
    /// Release age and wait timing (`release_timing`).
    pub timing: ReleaseTiming,
}

impl Tracker {
    pub fn new(senders: usize, cap: usize, timeout_ms: u64) -> Self {
        Self {
            cap,
            timeout_ms,
            pending: HashMap::new(),
            senders: vec![SenderState::default(); senders],
            released: [0; 3],
            timing: ReleaseTiming::default(),
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

    /// Why `sender` is not [`ready`](Self::ready): at its cap, else held by
    /// its own in-flight cancel-all.
    pub fn wait_reason(&self, sender: usize) -> WaitReason {
        let _ = sender;
        todo!()
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
            // Unknown outcome: commit or timeout releases it.
            Err(e) if is_transport_error(e) => Vec::new(),
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

    /// [`on_block_at`](Self::on_block_at) without a clock (tests).
    #[cfg(test)]
    pub fn on_block(&mut self, actions: &[(u64, bool)]) -> Vec<usize> {
        self.on_block_at(actions, 0)
    }

    /// One committed block, its body seen at `now_ms` (release age =
    /// `now_ms` - nonce). Cancel-alls are applied before places (the
    /// executor runs a block's cancel-alls first).
    pub fn on_block_at(&mut self, actions: &[(u64, bool)], now_ms: u64) -> Vec<usize> {
        let _ = now_ms;
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

/// A transport failure (no reply, or an unreadable one): the node may have
/// admitted the action, so it is not a refusal.
pub fn is_transport_error(err: &str) -> bool {
    // The prefixes submit_native_action[s_items] put on reqwest send/read and
    // JSON decode failures; RPC replies are "rpc: ..." or "0 accepted ...".
    err.starts_with("http: ") || err.starts_with("parse: ")
}

/// End-of-run warning when the tail failed to read bodies.
pub fn tail_warning(tail: &TailStats, url: &str) -> Option<String> {
    let errors = tail.errors.load(Ordering::Relaxed);
    let missed = tail.missed.load(Ordering::Relaxed);
    (errors > 0 || missed > 0).then(|| {
        format!(
            "WARNING: in-flight block tail {url}: errors={errors} missed={missed} — unread bodies \
             free their slots only at nonce + {} s (check TORUS_RPC_MAX_RESPONSE_MB on the node)",
            (torus_types::eip712::NONCE_WINDOW_MS + TIMEOUT_MARGIN_MS) / 1000
        )
    })
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
    pub tail_mode: TailMode,
    pub tracker: Mutex<Tracker>,
    wake: Vec<Notify>,
    pub tail: TailStats,
    /// Written by the tail task only (and read at the end of the run).
    tail_timing: Mutex<TailTiming>,
    started: Instant,
}

impl Cap {
    pub fn new(senders: usize, n: usize, url: String, tail_mode: TailMode) -> Self {
        Self {
            n,
            url,
            tail_mode,
            tracker: Mutex::new(Tracker::new(
                senders,
                n,
                torus_types::eip712::NONCE_WINDOW_MS + TIMEOUT_MARGIN_MS,
            )),
            wake: (0..senders).map(|_| Notify::new()).collect(),
            tail: TailStats::default(),
            tail_timing: Mutex::new(TailTiming::default()),
            started: Instant::now(),
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
        report_line_mode(self.n, &t, &self.tail, &self.url, self.tail_mode)
    }

    /// Close the current release-timing window: its two log lines
    /// (`release_timing` module doc).
    pub fn timing_window(&self, last: bool) -> [String; 2] {
        let _ = last;
        todo!()
    }

    /// End of run: the last (partial) window, then one histogram line each.
    pub fn timing_report(&self) -> Vec<String> {
        todo!()
    }
}

/// [`report_line`] plus ` | tail=commit` in commit mode (head mode: the
/// unchanged line).
pub fn report_line_mode(
    n: usize,
    t: &Tracker,
    tail: &TailStats,
    url: &str,
    mode: TailMode,
) -> String {
    let line = report_line(n, t, tail, url);
    match mode {
        TailMode::Head => line,
        TailMode::Commit => line + " | tail=commit",
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

/// Raw `torus_getBlockBody` JSON-RPC reply bytes (`None` on a transport error).
async fn fetch_body_reply(
    client: &reqwest::Client,
    url: &str,
    block_number: u64,
) -> Option<Vec<u8>> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "torus_getBlockBody",
        "params": [block_number],
        "id": 1
    });
    let resp = client.post(url).json(&body).send().await.ok()?;
    Some(resp.bytes().await.ok()?.to_vec())
}

/// What the tail reads from the watch node (the HTTP node in a run, a
/// scripted one in the tests).
trait TailSource {
    /// `eth_blockNumber` (`None` on any failure).
    fn head(&self) -> impl std::future::Future<Output = Option<u64>> + Send;
    /// `torus_getBlockBody(height)`.
    fn body(&self, height: u64) -> impl std::future::Future<Output = BodyFetch> + Send;
}

struct HttpTail {
    client: std::sync::Arc<reqwest::Client>,
    url: String,
}

impl TailSource for HttpTail {
    fn head(&self) -> impl std::future::Future<Output = Option<u64>> + Send {
        super::fetch_block_number(&self.client, &self.url)
    }

    async fn body(&self, height: u64) -> BodyFetch {
        match fetch_body_reply(&self.client, &self.url, height).await {
            // Multi-MB bodies under load: parse off the async workers so the
            // senders' fire timing does not jitter.
            Some(reply) => tokio::task::spawn_blocking(move || body_probe(&reply))
                .await
                .unwrap_or(BodyFetch::Error),
            None => BodyFetch::Error,
        }
    }
}

/// The tail's position: the next height to read, the failed reads of it, and
/// (commit mode) the polls it has had no body.
struct TailCursor {
    next: u64,
    attempts: u32,
    idle: u32,
    /// Head mode: `eth_blockNumber` minus the last read height at this
    /// poll's start (release timing).
    lag: Option<u64>,
    /// `(height, seen wall ms)` of the bodies read this poll.
    seen: Vec<(u64, u64)>,
}

impl TailCursor {
    fn new(start_block: u64) -> Self {
        Self {
            next: start_block + 1,
            attempts: 0,
            idle: 0,
            lag: None,
            seen: Vec::new(),
        }
    }
}

/// Release the actions of the body at the cursor and step past it.
fn tail_apply(cap: &Cap, cur: &mut TailCursor, actions: &[(u64, bool)]) {
    let woken = cap.tracker.lock().unwrap().on_block_at(actions, 0);
    cap.wake(woken);
    cap.tail.fetched.fetch_add(1, Ordering::Relaxed);
    cur.next += 1;
    cur.attempts = 0;
    cur.idle = 0;
}

/// An unreadable body at the cursor: one error; after `TAIL_MAX_ATTEMPTS`
/// it is missed and skipped (its actions release by timeout).
fn tail_error(cap: &Cap, cur: &mut TailCursor) {
    cap.tail.errors.fetch_add(1, Ordering::Relaxed);
    cur.attempts += 1;
    if cur.attempts >= TAIL_MAX_ATTEMPTS {
        cap.tail.missed.fetch_add(1, Ordering::Relaxed);
        cur.next += 1;
        cur.attempts = 0;
    }
}

/// One tail poll in `cap.tail_mode` (see [`TailMode`]).
async fn tail_poll<S: TailSource>(cap: &Cap, src: &S, cur: &mut TailCursor, stop: Instant) {
    match cap.tail_mode {
        TailMode::Head => {
            let Some(height) = src.head().await else {
                cap.tail.errors.fetch_add(1, Ordering::Relaxed);
                return;
            };
            while cur.next <= height && Instant::now() < stop {
                match src.body(cur.next).await {
                    BodyFetch::Keys(actions) => tail_apply(cap, cur, &actions),
                    BodyFetch::Absent | BodyFetch::Error => {
                        tail_error(cap, cur);
                        break;
                    }
                }
            }
        }
        TailMode::Commit => {
            while Instant::now() < stop {
                match src.body(cur.next).await {
                    BodyFetch::Keys(actions) => {
                        tail_apply(cap, cur, &actions);
                        continue;
                    }
                    // Unlike head mode the height may not be committed yet
                    // (node briefly unreachable before the block): errors
                    // alone never skip it, only a later body does.
                    BodyFetch::Error => {
                        cap.tail.errors.fetch_add(1, Ordering::Relaxed);
                        cur.attempts = cur.attempts.saturating_add(1);
                        if cur.attempts < TAIL_MAX_ATTEMPTS {
                            break;
                        }
                    }
                    BodyFetch::Absent => {
                        cur.idle = cur.idle.saturating_add(1);
                        if cur.idle < TAIL_MAX_ATTEMPTS {
                            break;
                        }
                    }
                }
                // Long without a readable body: skip it only once it is known
                // committed, i.e. eth_blockNumber has passed it (it lags but
                // does catch up, so a run of unreadable bodies is crossed one
                // height at a time) or the next height has a body. Commits
                // land in height order, so re-read the gap after the check:
                // it may have just landed.
                let past_head = src.head().await.is_some_and(|h| h >= cur.next);
                if !past_head && !matches!(src.body(cur.next + 1).await, BodyFetch::Keys(_)) {
                    break;
                }
                match src.body(cur.next).await {
                    BodyFetch::Keys(actions) => tail_apply(cap, cur, &actions),
                    BodyFetch::Absent | BodyFetch::Error => {
                        cap.tail.missed.fetch_add(1, Ordering::Relaxed);
                        cur.next += 1;
                        cur.attempts = 0;
                        cur.idle = 0;
                    }
                }
            }
        }
    }
}

/// One [`tail_poll`], timed (`release_timing`).
async fn tail_round<S: TailSource>(cap: &Cap, src: &S, cur: &mut TailCursor, stop: Instant) {
    tail_poll(cap, src, cur, stop).await;
}

/// The shared block-body tail: every `TAIL_POLL` expire timed-out actions,
/// then read every new body in order ([`TailMode`] says how a new body is
/// found). A body that cannot be read is retried on the next poll (the cursor
/// stays on it) up to `TAIL_MAX_ATTEMPTS`, then counted missed and skipped.
/// Runs until `stop`.
pub async fn run_tail(
    cap: std::sync::Arc<Cap>,
    client: std::sync::Arc<reqwest::Client>,
    start_block: u64,
    stop: Instant,
) {
    let src = HttpTail {
        client,
        url: cap.url.clone(),
    };
    let mut cur = TailCursor::new(start_block);
    while Instant::now() < stop {
        tokio::time::sleep(TAIL_POLL).await;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let woken = cap.tracker.lock().unwrap().expire(now_ms);
        cap.wake(woken);
        tail_poll(&cap, &src, &mut cur, stop).await;
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
        let woken = t.apply_result(&[1, 2], &Err("rpc: mempool: native pool full".into()));
        assert_eq!(woken, vec![0, 0]);
        assert_eq!(t.in_flight(0), 0);
        assert_eq!(t.released, [0, 2, 0]);
        assert_eq!(t.estimate(0), 0, "refused places never land");
        assert!(!t.cancel_pending(0));
    }

    // A transport failure (no reply, or an unreadable one) says nothing about
    // admission: the node may hold the action. The slot and its places stay
    // until the action is seen committed or times out.
    #[test]
    fn transport_errors_leave_the_slot_to_commit_or_timeout() {
        for err in [
            "http: error sending request for url (http://127.0.0.1:8645/): operation timed out",
            "http: error sending request for url (http://127.0.0.1:8645/)",
            "parse: error decoding response body",
        ] {
            assert!(is_transport_error(err), "{err}");
            let mut t = Tracker::new(1, 1, T);
            t.submit(0, 1, 1_000, 400, false);
            assert!(
                t.apply_result(&[1], &Err(err.to_string())).is_empty(),
                "{err}"
            );
            assert_eq!(t.in_flight(0), 1);
            assert_eq!(t.estimate(0), 400);
            assert_eq!(t.released, [0, 0, 0]);
            assert_eq!(t.on_block(&[(1, true)]), vec![0], "released by its commit");
            t.submit(0, 2, 2_000, 400, false);
            t.apply_result(&[2], &Err(err.to_string()));
            assert_eq!(t.expire(2_000 + T + 1), vec![0], "or by timeout");
            assert_eq!(t.released, [1, 0, 1]);
        }
        // RPC-level refusals: an error reply, nothing admitted, a BUSY shed.
        for err in [
            "rpc: rate limited: IP request weight over 1200/min, retry later",
            "0 accepted (1 sent): \"mempool: busy, admission limit reached (pre-verify), retry later\"",
        ] {
            assert!(!is_transport_error(err), "{err}");
            let mut t = Tracker::new(1, 1, T);
            t.submit(0, 1, 1_000, 400, false);
            assert_eq!(t.apply_result(&[1], &Err(err.to_string())), vec![0], "{err}");
        }
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
        // The tail parses the raw JSON-RPC reply bytes (only nonce and
        // signature per action; the orders are skipped, never built).
        let reply = |result: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": result}))
                .unwrap()
        };
        let body = reply(serde_json::json!({
            "blockNumber": "0x5",
            "nativeActions": [serde_json::to_value(&a).unwrap(), serde_json::to_value(&b).unwrap()],
            "nativeActionCount": 2,
            "nativeActionStatus": ["executed", "failed"],
            "nativeActionFailures": [],
            "evmTransactions": [],
        }));
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
        let body = reply(serde_json::json!({"nativeActions": [serde_json::to_value(&a).unwrap()]}));
        assert_eq!(body_action_keys(&body).unwrap(), vec![(keys[0].0, true)]);
        // Status not recorded yet: null.
        let body = reply(serde_json::json!({
            "nativeActions": [serde_json::to_value(&a).unwrap()], "nativeActionStatus": null}));
        assert_eq!(body_action_keys(&body).unwrap(), vec![(keys[0].0, true)]);
        // No body yet (null result), an error reply, garbage: retry later.
        assert_eq!(body_action_keys(&reply(serde_json::Value::Null)), None);
        assert_eq!(
            body_action_keys(br#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"x"}}"#),
            None
        );
        assert_eq!(body_action_keys(b"<html>"), None);
        assert_eq!(
            body_action_keys(&reply(serde_json::json!({"nativeActionCount": 0}))),
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
    fn tail_warning_only_when_the_tail_lost_bodies() {
        let tail = TailStats::default();
        tail.fetched.store(10, Ordering::Relaxed);
        assert_eq!(tail_warning(&tail, "http://v2"), None);
        tail.errors.store(3, Ordering::Relaxed);
        assert_eq!(
            tail_warning(&tail, "http://v2").unwrap(),
            "WARNING: in-flight block tail http://v2: errors=3 missed=0 — unread bodies \
             free their slots only at nonce + 70 s (check TORUS_RPC_MAX_RESPONSE_MB on the node)"
        );
        tail.errors.store(0, Ordering::Relaxed);
        tail.missed.store(1, Ordering::Relaxed);
        assert!(tail_warning(&tail, "http://v2")
            .unwrap()
            .contains("errors=0 missed=1"));
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

    // ---- --in-flight-tail commit (bench/inflight-commit-tail) ----

    /// Scripted watch node: a fixed `eth_blockNumber` and, per height, a
    /// sequence of `torus_getBlockBody` outcomes (the last one repeats;
    /// unknown heights are not committed yet).
    struct FakeNode {
        head: Option<u64>,
        bodies: Mutex<HashMap<u64, Vec<BodyFetch>>>,
        body_calls: Mutex<Vec<u64>>,
    }

    impl FakeNode {
        fn new(head: Option<u64>) -> Self {
            Self {
                head,
                bodies: Mutex::new(HashMap::new()),
                body_calls: Mutex::new(Vec::new()),
            }
        }
        fn with(self, height: u64, script: Vec<BodyFetch>) -> Self {
            self.bodies.lock().unwrap().insert(height, script);
            self
        }
        fn calls(&self) -> Vec<u64> {
            std::mem::take(&mut *self.body_calls.lock().unwrap())
        }
    }

    impl TailSource for FakeNode {
        fn head(&self) -> impl std::future::Future<Output = Option<u64>> + Send {
            let h = self.head;
            async move { h }
        }
        fn body(&self, height: u64) -> impl std::future::Future<Output = BodyFetch> + Send {
            self.body_calls.lock().unwrap().push(height);
            let mut bodies = self.bodies.lock().unwrap();
            let out = match bodies.get_mut(&height) {
                Some(s) if s.len() > 1 => s.remove(0),
                Some(s) => s[0].clone(),
                None => BodyFetch::Absent,
            };
            async move { out }
        }
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn stats(cap: &Cap) -> [u64; 3] {
        [
            cap.tail.fetched.load(Ordering::Relaxed),
            cap.tail.errors.load(Ordering::Relaxed),
            cap.tail.missed.load(Ordering::Relaxed),
        ]
    }

    #[test]
    fn tail_mode_defaults_to_head_and_commit_needs_the_cap() {
        assert_eq!(tail_plan(0, ""), Ok(TailMode::Head));
        assert_eq!(tail_plan(4, ""), Ok(TailMode::Head));
        assert_eq!(tail_plan(4, "head"), Ok(TailMode::Head));
        assert_eq!(tail_plan(4, "commit"), Ok(TailMode::Commit));
        assert!(tail_plan(4, "applied").is_err(), "unknown mode");
        assert!(tail_plan(0, "commit").is_err(), "commit tail without a cap");
        assert_eq!(
            tail_plan(0, "head"),
            Ok(TailMode::Head),
            "the default spelled out"
        );
    }

    #[test]
    fn body_probe_tells_not_committed_from_a_bad_reply() {
        let a = signed(1_000, place(3));
        let reply = |result: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": result}))
                .unwrap()
        };
        let body = reply(serde_json::json!({"nativeActions": [serde_json::to_value(&a).unwrap()]}));
        assert_eq!(
            body_probe(&body),
            BodyFetch::Keys(vec![(action_key(a.nonce, &a.signature), true)])
        );
        // No body at this height yet: null result = not committed.
        assert_eq!(
            body_probe(&reply(serde_json::Value::Null)),
            BodyFetch::Absent
        );
        // An error reply, garbage, a body without its action list: bad.
        assert_eq!(
            body_probe(br#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"x"}}"#),
            BodyFetch::Error
        );
        assert_eq!(body_probe(b"<html>"), BodyFetch::Error);
        assert_eq!(
            body_probe(&reply(serde_json::json!({"nativeActionCount": 0}))),
            BodyFetch::Error
        );
    }

    // The Phase 3 stall: eth_blockNumber (the applied marker) stays put while
    // the committed body is already on disk. The head tail cannot see it;
    // the commit tail releases at once.
    #[tokio::test]
    async fn commit_tail_releases_on_a_committed_body_while_the_head_stays_put() {
        for (mode, released) in [(TailMode::Head, 0), (TailMode::Commit, 1)] {
            let cap = Cap::new(1, 1, "http://v2".into(), mode);
            cap.submit(0, &[(11, 1_000, 400, false)]);
            let node = FakeNode::new(Some(0)).with(1, vec![BodyFetch::Keys(vec![(11, true)])]);
            let mut cur = TailCursor::new(0);
            tail_poll(&cap, &node, &mut cur, far()).await;
            let t = cap.tracker.lock().unwrap();
            assert_eq!(t.released, [released, 0, 0], "{mode:?}");
            assert_eq!(t.in_flight(0), 1 - released as usize, "{mode:?}");
            assert_eq!(cur.next, 1 + released, "{mode:?}");
            assert_eq!(stats(&cap), [released, 0, 0], "{mode:?}");
            match mode {
                TailMode::Head => assert!(node.calls().is_empty(), "head 0: no body read"),
                // Body 1 read, body 2 probed (not committed yet), stop.
                TailMode::Commit => assert_eq!(node.calls(), vec![1, 2]),
            }
        }
    }

    #[tokio::test]
    async fn commit_tail_never_releases_twice_or_rereads_a_body() {
        let cap = Cap::new(2, 1, "http://v2".into(), TailMode::Commit);
        cap.submit(0, &[(11, 1_000, 400, false)]);
        cap.submit(1, &[(22, 1_000, 0, true)]);
        // The same action seen again in a later body (a late duplicate).
        let node = FakeNode::new(None)
            .with(1, vec![BodyFetch::Keys(vec![(11, true)])])
            .with(2, vec![BodyFetch::Keys(vec![(11, true), (22, true)])]);
        let mut cur = TailCursor::new(0);
        tail_poll(&cap, &node, &mut cur, far()).await;
        assert_eq!(node.calls(), vec![1, 2, 3]);
        for _ in 0..3 {
            tail_poll(&cap, &node, &mut cur, far()).await;
        }
        assert_eq!(
            node.calls(),
            vec![3, 3, 3],
            "only the next height is probed"
        );
        let t = cap.tracker.lock().unwrap();
        assert_eq!(t.released, [2, 0, 0]);
        assert_eq!(t.open(), 0);
        assert_eq!(cur.next, 3);
        drop(t);
        assert_eq!(stats(&cap), [2, 0, 0], "not committed yet is no error");
    }

    // Refusal and timeout release exactly as before; a later body with the
    // same actions releases nothing more.
    #[tokio::test]
    async fn commit_tail_keeps_the_refused_and_timeout_paths() {
        let cap = Cap::new(1, 3, "http://v2".into(), TailMode::Commit);
        cap.submit(
            0,
            &[
                (1, 1_000, 400, false),
                (2, 1_001, 400, false),
                (3, 1_002, 0, true),
            ],
        );
        cap.apply_result(&[1], &Err("rpc: mempool: native pool full".into()));
        let woken = cap.tracker.lock().unwrap().expire(1_001 + T + 1);
        assert_eq!(woken, vec![0], "action 1 already refused; 2 times out");
        let node = FakeNode::new(None).with(
            1,
            vec![BodyFetch::Keys(vec![(1, true), (2, true), (3, true)])],
        );
        let mut cur = TailCursor::new(0);
        tail_poll(&cap, &node, &mut cur, far()).await;
        let t = cap.tracker.lock().unwrap();
        assert_eq!(t.released, [1, 1, 1], "only the cancel-all commits");
        assert_eq!(t.open(), 0);
    }

    // A height that stays without a body while a later one has its body is a
    // hole: after TAIL_MAX_ATTEMPTS polls it counts as missed and the tail
    // moves on (its actions release by timeout, as in head mode). The ahead
    // probe re-reads the gap first, so a body that just landed is not lost.
    #[tokio::test]
    async fn commit_tail_skips_a_hole_only_after_waiting() {
        let cap = Cap::new(1, 2, "http://v2".into(), TailMode::Commit);
        cap.submit(0, &[(5, 1_000, 400, false)]);
        let node = FakeNode::new(None).with(2, vec![BodyFetch::Keys(vec![(5, true)])]);
        let mut cur = TailCursor::new(0);
        for _ in 1..TAIL_MAX_ATTEMPTS {
            tail_poll(&cap, &node, &mut cur, far()).await;
        }
        assert_eq!(
            node.calls(),
            vec![1; TAIL_MAX_ATTEMPTS as usize - 1],
            "no ahead probe yet"
        );
        assert_eq!(stats(&cap), [0, 0, 0]);
        tail_poll(&cap, &node, &mut cur, far()).await;
        // Probe 1, ahead 2 (present), re-probe 1 (still absent: hole), then
        // read 2 and probe 3.
        assert_eq!(node.calls(), vec![1, 2, 1, 2, 3]);
        assert_eq!(stats(&cap), [1, 0, 1]);
        assert_eq!(cur.next, 3);
        assert_eq!(cap.tracker.lock().unwrap().released, [1, 0, 0]);

        // Race: the gap body lands between the probe and the ahead probe.
        let cap = Cap::new(1, 2, "http://v2".into(), TailMode::Commit);
        cap.submit(0, &[(5, 1_000, 400, false), (6, 1_001, 400, false)]);
        let mut script = vec![BodyFetch::Absent; TAIL_MAX_ATTEMPTS as usize];
        script.push(BodyFetch::Keys(vec![(5, true)]));
        let node = FakeNode::new(None)
            .with(1, script)
            .with(2, vec![BodyFetch::Keys(vec![(6, true)])]);
        let mut cur = TailCursor::new(0);
        for _ in 0..TAIL_MAX_ATTEMPTS {
            tail_poll(&cap, &node, &mut cur, far()).await;
        }
        assert_eq!(stats(&cap), [2, 0, 0], "nothing missed");
        assert_eq!(cap.tracker.lock().unwrap().released, [2, 0, 0]);
        assert_eq!(cur.next, 3);
    }

    // An unreadable body (transport error, error reply, garbage): head mode
    // knows the height is committed (<= eth_blockNumber) and skips it after
    // TAIL_MAX_ATTEMPTS errors. Commit mode skips it only once a later
    // height has a body; errors alone (node unreachable before the block
    // commits) never skip a height that may still come.
    #[tokio::test]
    async fn bad_bodies_skip_in_head_mode_and_only_past_a_later_body_in_commit_mode() {
        let cap = Cap::new(1, 1, "http://v2".into(), TailMode::Head);
        let node = FakeNode::new(Some(1)).with(1, vec![BodyFetch::Error]);
        let mut cur = TailCursor::new(0);
        for _ in 0..TAIL_MAX_ATTEMPTS {
            tail_poll(&cap, &node, &mut cur, far()).await;
        }
        assert_eq!(
            (stats(&cap), cur.next),
            ([0, TAIL_MAX_ATTEMPTS as u64, 1], 2)
        );
        // Head mode: a failed eth_blockNumber is one error, no body read.
        let cap = Cap::new(1, 1, "http://v2".into(), TailMode::Head);
        let node = FakeNode::new(None);
        let mut cur = TailCursor::new(0);
        tail_poll(&cap, &node, &mut cur, far()).await;
        assert_eq!(stats(&cap), [0, 1, 0]);
        assert!(node.calls().is_empty());

        // Commit mode, errors and no later body: stay on the height.
        let cap = Cap::new(1, 1, "http://v2".into(), TailMode::Commit);
        let node = FakeNode::new(None).with(1, vec![BodyFetch::Error]);
        let mut cur = TailCursor::new(0);
        for _ in 0..2 * TAIL_MAX_ATTEMPTS {
            tail_poll(&cap, &node, &mut cur, far()).await;
        }
        assert_eq!(
            (stats(&cap), cur.next),
            ([0, 2 * TAIL_MAX_ATTEMPTS as u64, 0], 1)
        );

        // Commit mode, a later body exists: the unreadable height is missed.
        let cap = Cap::new(1, 1, "http://v2".into(), TailMode::Commit);
        cap.submit(0, &[(9, 1_000, 400, false)]);
        let node = FakeNode::new(None)
            .with(1, vec![BodyFetch::Error])
            .with(2, vec![BodyFetch::Keys(vec![(9, true)])]);
        let mut cur = TailCursor::new(0);
        for _ in 0..TAIL_MAX_ATTEMPTS {
            tail_poll(&cap, &node, &mut cur, far()).await;
        }
        assert_eq!(
            (stats(&cap), cur.next),
            ([1, TAIL_MAX_ATTEMPTS as u64, 1], 3)
        );
        assert_eq!(cap.tracker.lock().unwrap().released, [1, 0, 0]);
    }

    // Review finding (Codex stop review): transport errors before a block
    // commits must not skip it. Five errors, then block 1 commits: its
    // actions are released, nothing missed.
    #[tokio::test]
    async fn commit_tail_errors_before_the_commit_do_not_skip_the_block() {
        let cap = Cap::new(1, 1, "http://v2".into(), TailMode::Commit);
        cap.submit(0, &[(7, 1_000, 400, false)]);
        let mut script = vec![BodyFetch::Error; TAIL_MAX_ATTEMPTS as usize];
        script.push(BodyFetch::Keys(vec![(7, true)]));
        let node = FakeNode::new(None).with(1, script);
        let mut cur = TailCursor::new(0);
        for _ in 0..=TAIL_MAX_ATTEMPTS {
            tail_poll(&cap, &node, &mut cur, far()).await;
        }
        assert_eq!(cap.tracker.lock().unwrap().released, [1, 0, 0]);
        assert_eq!(stats(&cap), [1, TAIL_MAX_ATTEMPTS as u64, 0]);
        assert_eq!(cur.next, 2);
    }

    // Review finding (Codex torus review): two unreadable bodies in a row.
    // The ahead probe cannot confirm height 1 (height 2 is unreadable too),
    // but once eth_blockNumber passes them (it lags, then catches up at a
    // checkpoint) each is skipped after TAIL_MAX_ATTEMPTS and body 3
    // releases. While the head is below them the tail waits.
    #[tokio::test]
    async fn commit_tail_crosses_a_run_of_unreadable_bodies_once_the_head_passes() {
        for (head, released) in [(Some(0), 0), (Some(2), 1)] {
            let cap = Cap::new(1, 1, "http://v2".into(), TailMode::Commit);
            cap.submit(0, &[(9, 1_000, 400, false)]);
            let node = FakeNode::new(head)
                .with(1, vec![BodyFetch::Error])
                .with(2, vec![BodyFetch::Error])
                .with(3, vec![BodyFetch::Keys(vec![(9, true)])]);
            let mut cur = TailCursor::new(0);
            for _ in 0..2 * TAIL_MAX_ATTEMPTS {
                tail_poll(&cap, &node, &mut cur, far()).await;
            }
            let t = cap.tracker.lock().unwrap();
            assert_eq!(t.released, [released, 0, 0], "head {head:?}");
            drop(t);
            if released == 1 {
                assert_eq!(stats(&cap), [1, 2 * TAIL_MAX_ATTEMPTS as u64, 2]);
                assert_eq!(cur.next, 4);
            } else {
                assert_eq!(stats(&cap), [0, 2 * TAIL_MAX_ATTEMPTS as u64, 0]);
                assert_eq!(cur.next, 1);
            }
        }
    }

    #[test]
    fn commit_tail_report_line_names_the_mode() {
        let t = Tracker::new(1, 1, T);
        let tail = TailStats::default();
        assert_eq!(
            report_line_mode(1, &t, &tail, "http://v2", TailMode::Head),
            report_line(1, &t, &tail, "http://v2"),
            "head line unchanged"
        );
        assert_eq!(
            report_line_mode(1, &t, &tail, "http://v2", TailMode::Commit),
            "In-flight cap 1 action(s)/sender: released committed=0 refused=0 timeout=0 \
             | in flight at end 0 | block tail http://v2: fetched=0 errors=0 missed=0 | tail=commit"
        );
    }

    // ---- release timing (s121, `release_timing`) ----

    #[test]
    fn wait_reason_splits_cap_full_from_cancel_pending() {
        let mut t = Tracker::new(1, 3, T);
        t.submit(0, 1, 1_000, 400, false);
        t.submit(0, 2, 1_001, 0, true);
        // Below the cap, own cancel-all in flight, next batch over budget.
        assert!(!t.ready(0, 400, 500));
        assert_eq!(t.wait_reason(0), WaitReason::CancelPending);
        t.submit(0, 3, 1_002, 400, false);
        assert!(!t.ready(0, 400, 0));
        assert_eq!(t.wait_reason(0), WaitReason::CapFull, "the cap first");
    }

    #[test]
    fn release_age_is_recorded_for_committed_actions_only() {
        let mut t = Tracker::new(1, 4, T);
        t.submit(0, 1, 50_000, 400, false);
        t.submit(0, 2, 50_100, 400, false);
        t.submit(0, 3, 1_000, 400, false);
        t.apply_result(&[2], &Err("rpc: refused".into()));
        assert_eq!(t.expire(1_000 + T + 1), vec![0], "key 3 times out");
        assert_eq!(t.on_block_at(&[(1, true), (9, true)], 50_750), vec![0]);
        let w = t.timing.roll();
        assert_eq!((w.age_ms.n, w.age_ms.max), (1, 750));
        assert_eq!(t.released, [1, 1, 1]);
    }

    #[tokio::test]
    async fn wait_ready_records_its_wait_by_reason() {
        use std::sync::Arc;
        let end = far();
        let cap = Arc::new(Cap::new(1, 1, "http://v2".into(), TailMode::Commit));
        assert_eq!(cap.wait_ready(0, 400, 0, end).await, Some((0, false, false)));
        cap.submit(0, &[(1, 1_000, 400, false)]);
        let c = cap.clone();
        let w = tokio::spawn(async move { c.wait_ready(0, 400, 0, end).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        cap.apply_result(&[1], &Err("rpc: refused".into()));
        assert_eq!(w.await.unwrap(), Some((0, false, true)));
        let r = cap.tracker.lock().unwrap().timing.roll();
        assert_eq!(r.ready_calls, 2, "a call that did not wait counts too");
        assert_eq!(r.wait_us[0].n, 1);
        assert!(r.wait_us[0].sum >= 30_000, "{}", r.wait_us[0].sum);
        assert_eq!(r.wait_us[1].n, 0);

        // Below the cap, held by its own cancel-all over the budget.
        let cap = Arc::new(Cap::new(1, 3, "http://v2".into(), TailMode::Commit));
        cap.submit(0, &[(2, 1_000, 400, false), (3, 1_001, 0, true)]);
        let c = cap.clone();
        let w = tokio::spawn(async move { c.wait_ready(0, 400, 500, end).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let woken = cap.tracker.lock().unwrap().on_block_at(&[(3, true)], 2_001);
        cap.wake(woken);
        assert_eq!(w.await.unwrap(), Some((400, false, true)));
        let r = cap.tracker.lock().unwrap().timing.roll();
        assert_eq!((r.ready_calls, r.wait_us[0].n, r.wait_us[1].n), (1, 0, 1));
        assert!(r.wait_us[1].sum >= 20_000, "{}", r.wait_us[1].sum);
        assert_eq!((r.age_ms.n, r.age_ms.max), (1, 1_000));
    }

    #[tokio::test]
    async fn tail_round_times_polls_and_logs_seen_heights() {
        // Head mode: head 3; bodies 1 and 2 read, 3 not readable yet.
        let cap = Cap::new(1, 2, "http://v2".into(), TailMode::Head);
        cap.submit(0, &[(11, 1_000, 400, false)]);
        let node = FakeNode::new(Some(3))
            .with(1, vec![BodyFetch::Keys(vec![(11, true)])])
            .with(2, vec![BodyFetch::Keys(vec![])])
            .with(3, vec![BodyFetch::Error]);
        let mut cur = TailCursor::new(0);
        let before = wall_ms();
        tail_round(&cap, &node, &mut cur, far()).await;
        assert!(cur.seen.is_empty(), "handed to the tail timing");
        let [line, blocks] = cap.timing_window(false);
        assert!(line.starts_with("release-timing: t="), "{line}");
        assert!(line.contains(" final=0 released=1 "), "{line}");
        assert!(
            line.contains(" tail_rounds=1 tail_fetched=2 tail_fetch_max=2 "),
            "{line}"
        );
        assert!(line.ends_with(" tail_lag_max=3"), "{line}");
        let seen: Vec<(u64, u64)> = blocks
            .split_once(" seen=")
            .unwrap()
            .1
            .split(',')
            .map(|e| {
                let (h, ms) = e.split_once('@').unwrap();
                (h.parse().unwrap(), ms.parse().unwrap())
            })
            .collect();
        assert_eq!(seen.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1, 2]);
        assert!(seen.iter().all(|s| s.1 >= before), "{blocks}");

        // Commit mode: no head read, so no lag.
        let cap = Cap::new(1, 2, "http://v2".into(), TailMode::Commit);
        let node = FakeNode::new(Some(9)).with(1, vec![BodyFetch::Keys(vec![])]);
        let mut cur = TailCursor::new(0);
        tail_round(&cap, &node, &mut cur, far()).await;
        let report = cap.timing_report();
        assert_eq!(report.len(), 8, "{report:?}");
        assert!(report[0].contains(" final=1 "), "{}", report[0]);
        assert!(report[0].ends_with(" tail_lag_max=-"), "{}", report[0]);
        assert!(report[1].starts_with("release-timing-blocks: t="));
        assert!(report[2].starts_with("release-timing-hist: what=age_ms n=0 "));
        assert!(report[5].starts_with("release-timing-hist: what=tail_round_us n=1 "));
        assert!(report[6]
            .starts_with("release-timing-hist: what=tail_fetched_per_round n=1 sum=1 "));
        assert!(report[7].starts_with("release-timing-hist: what=tail_lag_heights n=0 "));
    }
}
