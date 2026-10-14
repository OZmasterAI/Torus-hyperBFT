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
//!     `eth_blockNumber` first, `commit` probes the next body directly.
//!     `--in-flight-tail-depth N` keeps N body reads in flight and applies
//!     them strictly in height order (default 1: one at a time);
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

use futures::stream::{FuturesOrdered, StreamExt};
use tokio::sync::Notify;
use torus_types::{ActionSignature, NativeAction};

use crate::release_timing::{wall_ms, ReleaseTiming, TailBody, TailTiming, WaitReason};

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

/// Upper bound of `--in-flight-tail-depth`.
pub const TAIL_DEPTH_MAX: usize = 64;

/// Validate `--in-flight-tail-depth` (body reads the tail keeps in flight;
/// 1, the default, = one at a time).
pub fn tail_depth_plan(max_in_flight: usize, depth: usize) -> Result<usize, String> {
    match depth {
        0 => Err("--in-flight-tail-depth must be at least 1".into()),
        d if d > TAIL_DEPTH_MAX => Err(format!(
            "--in-flight-tail-depth {d}: at most {TAIL_DEPTH_MAX}"
        )),
        d if d > 1 && max_in_flight == 0 => {
            Err("--in-flight-tail-depth N > 1 needs --max-in-flight N (N > 0)".into())
        }
        d => Ok(d),
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

    /// Per-sender in-flight count (test-only: the runtime reads `open`).
    #[cfg(test)]
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
        if self.senders[sender].in_flight >= self.cap {
            WaitReason::CapFull
        } else {
            WaitReason::CancelPending
        }
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
        let mut woken = Vec::new();
        for cancels_pass in [true, false] {
            for &(key, ok) in actions {
                if self.pending.get(&key).map(|p| p.cancel) != Some(cancels_pass) {
                    continue;
                }
                let Some((s, p)) = self.release(key, Release::Committed) else {
                    continue;
                };
                self.timing.on_release(now_ms, p.nonce);
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
    /// `--in-flight-tail-depth`: body reads in flight at once (1 = one at a
    /// time, every bench before the flag).
    pub tail_depth: usize,
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
            tail_depth: 1,
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

    /// `--in-flight-tail-depth` (at least 1).
    pub fn with_tail_depth(mut self, depth: usize) -> Self {
        self.tail_depth = depth.max(1);
        self
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
        // Release timing: the time since each check goes to the reason seen
        // at it (recorded under this lock; no clock read unless waiting).
        let mut since: Option<(Instant, WaitReason)> = None;
        let mut waited_us = [None; 2];
        loop {
            {
                let mut t = self.tracker.lock().unwrap();
                if let Some((t0, why)) = since {
                    let us = t0.elapsed().as_micros() as u64;
                    let w: &mut Option<u64> = &mut waited_us[why as usize];
                    *w = Some(w.unwrap_or(0) + us);
                }
                if t.ready(sender, batch, budget) {
                    t.timing.on_ready(waited_us);
                    let waited = since.is_some();
                    return Some((t.estimate(sender), t.cancel_pending(sender), waited));
                }
                since = Some((Instant::now(), t.wait_reason(sender)));
            }
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
    /// Depth 1 (the default): the line as before the flag; deeper, it ends
    /// in ` | tail_depth=N`.
    pub fn report(&self) -> String {
        let t = self.tracker.lock().unwrap();
        let line = report_line_mode(self.n, &t, &self.tail, &self.url, self.tail_mode);
        match self.tail_depth {
            1 => line,
            d => format!("{line} | tail_depth={d}"),
        }
    }

    /// Close the current release-timing window: its two log lines
    /// (`release_timing` module doc).
    pub fn timing_window(&self, last: bool) -> [String; 2] {
        let rel = self.tracker.lock().unwrap().timing.roll();
        let tail = self.tail_timing.lock().unwrap().roll();
        let t_s = self.started.elapsed().as_secs_f64();
        crate::release_timing::window_lines(t_s, wall_ms(), last, &rel, &tail)
    }

    /// End of run: the last (partial) window, then one histogram line each.
    pub fn timing_report(&self) -> Vec<String> {
        let mut lines = self.timing_window(true).to_vec();
        let hist = crate::release_timing::hist_line;
        let t = self.tracker.lock().unwrap();
        lines.extend(t.timing.totals().iter().map(|(what, h)| hist(what, h)));
        drop(t);
        let tail = self.tail_timing.lock().unwrap();
        lines.extend(tail.totals().iter().map(|(what, h)| hist(what, h)));
        lines
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
}

impl TailCursor {
    fn new(start_block: u64) -> Self {
        Self {
            next: start_block + 1,
            attempts: 0,
            idle: 0,
        }
    }
}

/// One body read by the tail: the reply, when it arrived, how long it took.
struct Fetched {
    height: u64,
    body: BodyFetch,
    /// Wall ms when the reply was parsed (`seen`).
    seen_ms: u64,
    /// Request sent to reply parsed.
    fetch_us: u64,
    arrived: Instant,
}

/// Start reading the body at `height`. The request is issued here (in call
/// order), its reply is timed when it lands.
fn fetch<S: TailSource>(src: &S, height: u64) -> impl std::future::Future<Output = Fetched> + '_ {
    let t0 = Instant::now();
    let reply = src.body(height);
    async move {
        let body = reply.await;
        let arrived = Instant::now();
        Fetched {
            height,
            body,
            seen_ms: wall_ms(),
            fetch_us: arrived.duration_since(t0).as_micros() as u64,
            arrived,
        }
    }
}

/// Release the actions of the body at the cursor (`f`, read for that
/// height) and step past it. The release age counts to now, when the slots
/// free; `seen` stays the reply's arrival.
fn tail_apply(cap: &Cap, cur: &mut TailCursor, f: &Fetched, actions: &[(u64, bool)]) {
    debug_assert_eq!(f.height, cur.next, "bodies apply in height order");
    let now = wall_ms();
    let woken = cap.tracker.lock().unwrap().on_block_at(actions, now);
    cap.tail_timing.lock().unwrap().on_body(TailBody {
        height: cur.next,
        seen_ms: f.seen_ms,
        applied_ms: now.max(f.seen_ms),
        fetch_us: f.fetch_us,
        wait_us: f.arrived.elapsed().as_micros() as u64,
    });
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
///
/// `cap.tail_depth` reads are kept in flight, for heights `cur.next`,
/// `cur.next + 1`, ... (head mode: up to the head), and their replies are
/// taken strictly in height order: a body that lands before a lower height
/// waits for it, so the tracker sees blocks in order and each height once.
/// The first height without a readable body is handled exactly as at depth 1
/// (the reads above it are dropped and issued again next time), so depth 1
/// is the one-at-a-time tail, read for read.
async fn tail_poll<S: TailSource>(cap: &Cap, src: &S, cur: &mut TailCursor, stop: Instant) {
    let depth = cap.tail_depth.max(1);
    match cap.tail_mode {
        TailMode::Head => {
            let Some(height) = src.head().await else {
                cap.tail.errors.fetch_add(1, Ordering::Relaxed);
                return;
            };
            let lag = height.saturating_sub(cur.next - 1);
            cap.tail_timing.lock().unwrap().on_lag(lag);
            let mut ahead = FuturesOrdered::new();
            let mut issued = cur.next;
            loop {
                while ahead.len() < depth && issued <= height && Instant::now() < stop {
                    ahead.push_back(fetch(src, issued));
                    issued += 1;
                }
                let Some(f) = ahead.next().await else { break };
                match &f.body {
                    BodyFetch::Keys(actions) => tail_apply(cap, cur, &f, actions),
                    BodyFetch::Absent | BodyFetch::Error => {
                        tail_error(cap, cur);
                        break;
                    }
                }
            }
        }
        TailMode::Commit => {
            let mut ahead = FuturesOrdered::new();
            let mut issued = cur.next;
            loop {
                while ahead.len() < depth && Instant::now() < stop {
                    ahead.push_back(fetch(src, issued));
                    issued += 1;
                }
                let Some(f) = ahead.next().await else { break };
                match &f.body {
                    BodyFetch::Keys(actions) => {
                        tail_apply(cap, cur, &f, actions);
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
                // The hole check reads one height at a time, as at depth 1:
                // drop the reads above the cursor, re-issued after it.
                ahead = FuturesOrdered::new();
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
                let f = fetch(src, cur.next).await;
                match &f.body {
                    BodyFetch::Keys(actions) => tail_apply(cap, cur, &f, actions),
                    BodyFetch::Absent | BodyFetch::Error => {
                        cap.tail.missed.fetch_add(1, Ordering::Relaxed);
                        cur.next += 1;
                        cur.attempts = 0;
                        cur.idle = 0;
                    }
                }
                issued = cur.next;
            }
        }
    }
}

/// Closes the open tail poll in the tail timing when dropped: at the end of
/// [`tail_round`], or when the tail task is aborted inside the poll (the
/// bodies it read are already recorded; this adds the partial round).
struct RoundTimer<'a> {
    cap: &'a Cap,
    t0: Instant,
}

impl Drop for RoundTimer<'_> {
    fn drop(&mut self) {
        let round_us = self.t0.elapsed().as_micros() as u64;
        // No unwrap in drop: a poisoned lock must not turn a panic into an
        // abort.
        if let Ok(mut t) = self.cap.tail_timing.lock() {
            t.end_round(round_us);
        }
    }
}

/// One [`tail_poll`], timed (`release_timing`).
async fn tail_round<S: TailSource>(cap: &Cap, src: &S, cur: &mut TailCursor, stop: Instant) {
    let _round = RoundTimer {
        cap,
        t0: Instant::now(),
    };
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
    let mut next_line = Instant::now() + crate::release_timing::WINDOW;
    while Instant::now() < stop {
        tokio::time::sleep(TAIL_POLL).await;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let woken = cap.tracker.lock().unwrap().expire(now_ms);
        cap.wake(woken);
        tail_round(&cap, &src, &mut cur, stop).await;
        if Instant::now() >= next_line {
            next_line += crate::release_timing::WINDOW;
            for line in cap.timing_window(false) {
                println!("{line}");
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
        /// Body RPCs at or above this height never return.
        stall_from: Option<u64>,
        /// Heights whose replies are held until the test opens the gate.
        gates: HashMap<u64, std::sync::Arc<tokio::sync::Semaphore>>,
        /// Body RPCs issued and not yet answered (or dropped), and the most
        /// at once.
        open_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        max_open_reads: std::sync::atomic::AtomicUsize,
    }

    /// Counts one open body RPC until it is answered or dropped.
    struct OpenRead(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for OpenRead {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl FakeNode {
        fn new(head: Option<u64>) -> Self {
            Self {
                head,
                bodies: Mutex::new(HashMap::new()),
                body_calls: Mutex::new(Vec::new()),
                stall_from: None,
                gates: HashMap::new(),
                open_reads: Default::default(),
                max_open_reads: Default::default(),
            }
        }
        /// Hold the replies for `height` until [`open`](Self::open).
        fn gate(mut self, height: u64) -> Self {
            let g = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
            self.gates.insert(height, g);
            self
        }
        fn open(&self, height: u64) {
            self.gates[&height].add_permits(1 << 20);
        }
        /// Body RPCs issued so far (not consumed, unlike `calls`).
        fn issued(&self) -> Vec<u64> {
            self.body_calls.lock().unwrap().clone()
        }
        fn max_open(&self) -> usize {
            self.max_open_reads.load(Ordering::SeqCst)
        }
        fn with(self, height: u64, script: Vec<BodyFetch>) -> Self {
            self.bodies.lock().unwrap().insert(height, script);
            self
        }
        fn stall_from(mut self, height: u64) -> Self {
            self.stall_from = Some(height);
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
            let stall = self.stall_from.is_some_and(|s| height >= s);
            let gate = self.gates.get(&height).cloned();
            let open = self.open_reads.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_open_reads.fetch_max(open, Ordering::SeqCst);
            let read = OpenRead(self.open_reads.clone());
            async move {
                let _read = read;
                if stall {
                    std::future::pending::<()>().await;
                }
                if let Some(g) = gate {
                    let _ = g.acquire().await;
                }
                out
            }
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
        assert_eq!(
            cap.wait_ready(0, 400, 0, end).await,
            Some((0, false, false))
        );
        cap.submit(0, &[(1, 1_000, 400, false)]);
        let c = cap.clone();
        let w = tokio::spawn(async move { c.wait_ready(0, 400, 0, end).await });
        // Let the waiter run to Pending first, so its wait starts before the
        // delay does (current-thread runtime: yield_now polls it).
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        cap.apply_result(&[1], &Err("rpc: refused".into()));
        assert_eq!(w.await.unwrap(), Some((0, false, true)));
        let r = cap.tracker.lock().unwrap().timing.roll();
        assert_eq!(r.ready_calls, 2, "a call that did not wait counts too");
        assert_eq!(r.wait_us[0].n, 1);
        assert!(r.wait_us[0].sum >= 29_000, "{}", r.wait_us[0].sum);
        assert_eq!(r.wait_us[1].n, 0);

        // Below the cap, held by its own cancel-all over the budget.
        let cap = Arc::new(Cap::new(1, 3, "http://v2".into(), TailMode::Commit));
        cap.submit(0, &[(2, 1_000, 400, false), (3, 1_001, 0, true)]);
        let c = cap.clone();
        let w = tokio::spawn(async move { c.wait_ready(0, 400, 500, end).await });
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let woken = cap.tracker.lock().unwrap().on_block_at(&[(3, true)], 2_001);
        cap.wake(woken);
        assert_eq!(w.await.unwrap(), Some((400, false, true)));
        let r = cap.tracker.lock().unwrap().timing.roll();
        assert_eq!((r.ready_calls, r.wait_us[0].n, r.wait_us[1].n), (1, 0, 1));
        assert!(r.wait_us[1].sum >= 19_000, "{}", r.wait_us[1].sum);
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
        let [line, blocks] = cap.timing_window(false);
        assert!(line.starts_with("release-timing: t="), "{line}");
        assert!(line.contains(" final=0 released=1 "), "{line}");
        assert!(
            line.contains(" tail_rounds=1 tail_fetched=2 tail_fetch_max=2 "),
            "{line}"
        );
        assert!(
            line.contains(" tail_lag_max=3 tail_fetch_p50_ms="),
            "{line}"
        );
        assert!(line.contains(" tail_apply_wait_max_ms="), "{line}");
        assert!(
            !line.contains("tail_fetch_p50_ms=-"),
            "two bodies timed: {line}"
        );
        // Per body: its read time, when it was applied, when it was seen.
        assert!(blocks.contains(" fetch_ms=1:"), "{blocks}");
        assert!(blocks.contains(" applied=1@"), "{blocks}");
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
        assert_eq!(report.len(), 10, "{report:?}");
        assert!(report[0].contains(" final=1 "), "{}", report[0]);
        assert!(report[0].contains(" tail_lag_max=- "), "{}", report[0]);
        assert!(report[1].starts_with("release-timing-blocks: t="));
        assert!(report[2].starts_with("release-timing-hist: what=age_ms n=0 "));
        assert!(report[5].starts_with("release-timing-hist: what=tail_round_us n=1 "));
        assert!(
            report[6].starts_with("release-timing-hist: what=tail_fetched_per_round n=1 sum=1 ")
        );
        assert!(report[7].starts_with("release-timing-hist: what=tail_lag_heights n=0 "));
        assert!(report[8].starts_with("release-timing-hist: what=tail_fetch_us n=1 "));
        assert!(report[9].starts_with("release-timing-hist: what=tail_apply_wait_us n=1 "));
    }

    #[tokio::test]
    async fn tail_abort_mid_round_keeps_the_bodies_already_read() {
        // Codex r1: the end of the run aborts the tail; when that lands in a
        // later body RPC, the bodies this round already read must still be
        // in the tail timing (seen heights, fetched, the partial round).
        use std::sync::Arc;
        let cap = Arc::new(Cap::new(1, 2, "http://v2".into(), TailMode::Head));
        cap.submit(0, &[(11, 1_000, 400, false)]);
        let node = FakeNode::new(Some(3))
            .with(1, vec![BodyFetch::Keys(vec![(11, true)])])
            .stall_from(2);
        let c = cap.clone();
        let h = tokio::spawn(async move {
            let mut cur = TailCursor::new(0);
            tail_round(&c, &node, &mut cur, far()).await;
        });
        while cap.tail.fetched.load(Ordering::Relaxed) < 1 {
            tokio::task::yield_now().await;
        }
        h.abort();
        assert!(h.await.unwrap_err().is_cancelled());
        let report = cap.timing_report();
        assert!(report[0].contains(" released=1 "), "{}", report[0]);
        assert!(
            report[0].contains(" tail_rounds=1 tail_fetched=1 tail_fetch_max=1 "),
            "{}",
            report[0]
        );
        assert!(report[0].contains(" tail_lag_max=3 "), "{}", report[0]);
        assert!(report[1].contains(" seen=1@"), "{}", report[1]);
        assert!(!report[1].contains(','), "{}", report[1]);
        assert!(
            report[5].starts_with("release-timing-hist: what=tail_round_us n=1 "),
            "{}",
            report[5]
        );
        assert!(
            report[6].starts_with("release-timing-hist: what=tail_fetched_per_round n=1 sum=1 "),
            "{}",
            report[6]
        );
        assert!(
            report[7].starts_with("release-timing-hist: what=tail_lag_heights n=1 "),
            "{}",
            report[7]
        );
    }

    // ---- --in-flight-tail-depth (feat/bench-tail-depth) ----

    #[test]
    fn tail_depth_defaults_to_one_and_needs_the_cap_above_one() {
        assert_eq!(tail_depth_plan(0, 1), Ok(1), "the default, cap off");
        assert_eq!(tail_depth_plan(4, 1), Ok(1));
        assert_eq!(tail_depth_plan(4, 8), Ok(8));
        assert_eq!(tail_depth_plan(4, TAIL_DEPTH_MAX), Ok(TAIL_DEPTH_MAX));
        assert!(tail_depth_plan(4, 0).is_err(), "depth 0 reads nothing");
        assert!(tail_depth_plan(4, TAIL_DEPTH_MAX + 1).is_err());
        assert!(tail_depth_plan(0, 4).is_err(), "a deep tail without a cap");
        let cap = Cap::new(1, 1, "http://v2".into(), TailMode::Head);
        assert_eq!(cap.tail_depth, 1);
        assert_eq!(cap.with_tail_depth(0).tail_depth, 1);
    }

    #[test]
    fn report_line_names_a_deep_tail_only() {
        let cap = Cap::new(1, 1, "http://v2".into(), TailMode::Commit);
        let one = cap.report();
        assert!(one.ends_with("missed=0 | tail=commit"), "{one}");
        let deep = Cap::new(1, 1, "http://v2".into(), TailMode::Commit).with_tail_depth(4);
        assert_eq!(deep.report(), one + " | tail_depth=4");
        let head = Cap::new(1, 1, "http://v2".into(), TailMode::Head).with_tail_depth(2);
        assert!(
            head.report().ends_with("missed=0 | tail_depth=2"),
            "{}",
            head.report()
        );
    }

    /// The tail as it was before `--in-flight-tail-depth` (s121,
    /// 4b928297): one body read at a time. Depth 1 must match it request
    /// for request.
    async fn reference_poll(cap: &Cap, src: &FakeNode, cur: &mut TailCursor, stop: Instant) {
        fn apply(cap: &Cap, cur: &mut TailCursor, actions: &[(u64, bool)]) {
            let now = wall_ms();
            let woken = cap.tracker.lock().unwrap().on_block_at(actions, now);
            cap.tail_timing.lock().unwrap().on_body(TailBody {
                height: cur.next,
                seen_ms: now,
                applied_ms: now,
                ..TailBody::default()
            });
            cap.wake(woken);
            cap.tail.fetched.fetch_add(1, Ordering::Relaxed);
            cur.next += 1;
            cur.attempts = 0;
            cur.idle = 0;
        }
        match cap.tail_mode {
            TailMode::Head => {
                let Some(height) = src.head().await else {
                    cap.tail.errors.fetch_add(1, Ordering::Relaxed);
                    return;
                };
                let lag = height.saturating_sub(cur.next - 1);
                cap.tail_timing.lock().unwrap().on_lag(lag);
                while cur.next <= height && Instant::now() < stop {
                    match src.body(cur.next).await {
                        BodyFetch::Keys(actions) => apply(cap, cur, &actions),
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
                            apply(cap, cur, &actions);
                            continue;
                        }
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
                    let past_head = src.head().await.is_some_and(|h| h >= cur.next);
                    if !past_head && !matches!(src.body(cur.next + 1).await, BodyFetch::Keys(_)) {
                        break;
                    }
                    match src.body(cur.next).await {
                        BodyFetch::Keys(actions) => apply(cap, cur, &actions),
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

    /// Per height, its scripted `torus_getBlockBody` outcomes.
    type Heights = Vec<(u64, Vec<BodyFetch>)>;

    /// A scripted node from `seed`: a head (or none) and, per height 1..=10,
    /// 1-3 outcomes (body with the height's action, not committed, error);
    /// some heights never get a body.
    fn scripted(seed: u64) -> (Option<u64>, Heights) {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut r = move |m: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % m
        };
        let head = match r(4) {
            0 => None,
            _ => Some(r(12)),
        };
        let mut heights = Vec::new();
        for h in 1..=10u64 {
            if r(6) == 0 {
                continue;
            }
            let script = (0..=r(3))
                .map(|_| match r(5) {
                    0..=2 => BodyFetch::Keys(vec![(1_000 + h, true)]),
                    3 => BodyFetch::Absent,
                    _ => BodyFetch::Error,
                })
                .collect();
            heights.push((h, script));
        }
        (head, heights)
    }

    fn node_from(head: Option<u64>, heights: &[(u64, Vec<BodyFetch>)]) -> FakeNode {
        heights
            .iter()
            .fold(FakeNode::new(head), |n, (h, s)| n.with(*h, s.clone()))
    }

    fn cap_for(mode: TailMode, depth: usize) -> Cap {
        let cap = Cap::new(1, 64, "http://v2".into(), mode).with_tail_depth(depth);
        let keys: Vec<_> = (1..=12).map(|h| (1_000 + h, 1_000, 1, false)).collect();
        cap.submit(0, &keys);
        cap
    }

    fn applied_heights(cap: &Cap) -> Vec<u64> {
        cap.tail_timing
            .lock()
            .unwrap()
            .roll()
            .seen
            .iter()
            .map(|b| b.height)
            .collect()
    }

    // (1) Depth 1 (the default) = the tail before the flag: the same body
    // and head reads in the same order, the same releases, counters and
    // cursor after every poll, both modes.
    #[tokio::test]
    async fn depth_one_matches_the_one_at_a_time_tail_request_for_request() {
        for seed in 0..300 {
            for mode in [TailMode::Head, TailMode::Commit] {
                let (head, heights) = scripted(seed);
                let (new_node, old_node) = (node_from(head, &heights), node_from(head, &heights));
                let (new_cap, old_cap) = (cap_for(mode, 1), cap_for(mode, 1));
                let (mut new_cur, mut old_cur) = (TailCursor::new(0), TailCursor::new(0));
                for poll in 0..2 * TAIL_MAX_ATTEMPTS + 3 {
                    tail_poll(&new_cap, &new_node, &mut new_cur, far()).await;
                    reference_poll(&old_cap, &old_node, &mut old_cur, far()).await;
                    let at = format!("seed {seed} {mode:?} poll {poll}");
                    assert_eq!(new_node.calls(), old_node.calls(), "{at}: body reads");
                    assert_eq!(stats(&new_cap), stats(&old_cap), "{at}: tail counters");
                    assert_eq!(
                        (new_cur.next, new_cur.attempts, new_cur.idle),
                        (old_cur.next, old_cur.attempts, old_cur.idle),
                        "{at}: cursor"
                    );
                    assert_eq!(
                        new_cap.tracker.lock().unwrap().released,
                        old_cap.tracker.lock().unwrap().released,
                        "{at}: releases"
                    );
                    assert_eq!(applied_heights(&new_cap), applied_heights(&old_cap), "{at}");
                    assert!(new_node.max_open() <= 1, "{at}: one read at a time");
                }
            }
        }
    }

    // (2) Depth N: up to N body reads in flight at once; replies that land
    // out of order are held and applied in height order, each height once.
    // `seen` is the reply's arrival, `applied` when its slots freed.
    #[tokio::test]
    async fn deep_tail_reads_ahead_and_applies_in_height_order() {
        for mode in [TailMode::Head, TailMode::Commit] {
            let cap = cap_for(mode, 3);
            let node = (1..=5)
                .fold(FakeNode::new(Some(5)), |n, h| {
                    n.with(h, vec![BodyFetch::Keys(vec![(1_000 + h, true)])])
                })
                .gate(1)
                .gate(2)
                .gate(3);
            let mut cur = TailCursor::new(0);
            let poll = tail_poll(&cap, &node, &mut cur, far());
            let driver = async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while node.issued().len() < 3 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap_or_else(|_| panic!("{mode:?}: 3 reads in flight, got {:?}", node.issued()));
                assert_eq!(node.issued(), vec![1, 2, 3], "{mode:?}");
                // Replies land 3, 2, 1: nothing applies before height 1.
                for h in [3, 2] {
                    node.open(h);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    assert_eq!(cap.tail.fetched.load(Ordering::Relaxed), 0, "{mode:?}");
                }
                node.open(1);
            };
            tokio::join!(poll, driver);
            let seen = cap.tail_timing.lock().unwrap().roll().seen;
            assert_eq!(
                seen.iter().map(|b| b.height).collect::<Vec<_>>(),
                vec![1, 2, 3, 4, 5],
                "{mode:?}: height order, no gap, no repeat"
            );
            let at = |h: u64| seen[h as usize - 1];
            assert!(at(3).seen_ms < at(2).seen_ms, "{mode:?}: {seen:?}");
            assert!(at(2).seen_ms < at(1).seen_ms, "{mode:?}: {seen:?}");
            assert!(at(3).applied_ms >= at(1).seen_ms, "{mode:?}: held for 1");
            assert!(at(3).wait_us >= 9_000, "{mode:?}: {seen:?}");
            assert!(at(3).fetch_us < at(1).fetch_us, "{mode:?}: {seen:?}");
            assert!(seen.windows(2).all(|w| w[0].applied_ms <= w[1].applied_ms));
            assert_eq!(node.max_open(), 3, "{mode:?}: depth 3 = 3 reads at once");
            assert_eq!(cap.tracker.lock().unwrap().released, [5, 0, 0], "{mode:?}");
            assert_eq!(cur.next, 6, "{mode:?}");
            assert_eq!(stats(&cap), [5, 0, 0], "{mode:?}");
            let issued = node.calls();
            assert_eq!(
                &issued[..5],
                &[1, 2, 3, 4, 5],
                "{mode:?}: each body read once"
            );
            match mode {
                TailMode::Head => assert_eq!(issued.len(), 5, "head 5: nothing above it"),
                // At the tip: 6, 7, 8 read ahead, 6 not committed, stop.
                TailMode::Commit => assert_eq!(&issued[5..], &[6, 7, 8]),
            }
        }
    }

    // (3) A failed read with later reads in flight: the poll stops at the
    // failed height as at depth 1 (one error, nothing above it applied, the
    // later replies dropped) and the next poll retries it; a height that
    // stays unreadable is skipped exactly as at depth 1. Same releases,
    // counters, cursor and applied heights as depth 1.
    #[tokio::test]
    async fn deep_tail_failures_retry_and_skip_like_depth_one() {
        let keys = |h: u64| BodyFetch::Keys(vec![(1_000 + h, true)]);
        let mut errs = vec![BodyFetch::Error; TAIL_MAX_ATTEMPTS as usize];
        errs.push(keys(2));
        let cases: Vec<(TailMode, Option<u64>, Heights)> = vec![
            // Error once, then the body.
            (
                TailMode::Head,
                Some(5),
                vec![(2, vec![BodyFetch::Error, keys(2)])],
            ),
            (
                TailMode::Commit,
                None,
                vec![(2, vec![BodyFetch::Error, keys(2)])],
            ),
            // Never readable: missed after TAIL_MAX_ATTEMPTS.
            (TailMode::Head, Some(5), vec![(2, vec![BodyFetch::Error])]),
            (TailMode::Commit, None, vec![(2, vec![BodyFetch::Error])]),
            (
                TailMode::Commit,
                Some(5),
                vec![(2, vec![BodyFetch::Absent])],
            ),
            // Commit: errors before the commit never skip it.
            (TailMode::Commit, None, vec![(2, errs)]),
        ];
        for (i, (mode, head, special)) in cases.into_iter().enumerate() {
            let mut heights: Vec<_> = (1..=5u64).map(|h| (h, vec![keys(h)])).collect();
            for (h, s) in special {
                heights[h as usize - 1].1 = s;
            }
            let mut outcome = Vec::new();
            for depth in [1, 3] {
                let node = node_from(head, &heights);
                let cap = cap_for(mode, depth);
                let mut cur = TailCursor::new(0);
                let mut applied = Vec::new();
                for _ in 0..2 * TAIL_MAX_ATTEMPTS + 2 {
                    tail_poll(&cap, &node, &mut cur, far()).await;
                    applied.extend(applied_heights(&cap));
                }
                let released = cap.tracker.lock().unwrap().released;
                outcome.push((applied, released, stats(&cap), cur.next));
            }
            assert_eq!(
                outcome[1], outcome[0],
                "case {i} ({mode:?}): depth 3 vs depth 1"
            );
            let applied = &outcome[1].0;
            assert!(
                applied.windows(2).all(|w| w[0] < w[1]),
                "case {i}: {applied:?}"
            );
        }
    }

    // (3) A slow read that then fails (a timed-out RPC), with the bodies
    // above it already in: they are not applied past it, the poll ends at
    // the error (no stall), and the next poll applies 2, 3, 4 in order.
    #[tokio::test]
    async fn deep_tail_slow_failed_read_holds_the_later_bodies_until_retried() {
        let cap = cap_for(TailMode::Head, 3);
        let node = FakeNode::new(Some(4))
            .with(1, vec![BodyFetch::Keys(vec![(1_001, true)])])
            .with(
                2,
                vec![BodyFetch::Error, BodyFetch::Keys(vec![(1_002, true)])],
            )
            .with(3, vec![BodyFetch::Keys(vec![(1_003, true)])])
            .with(4, vec![BodyFetch::Keys(vec![(1_004, true)])])
            .gate(2);
        let mut cur = TailCursor::new(0);
        let poll = tail_poll(&cap, &node, &mut cur, far());
        let driver = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while node.issued().len() < 4 || cap.tail.fetched.load(Ordering::Relaxed) < 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("reads 1..=4 in flight, got {:?}", node.issued()));
            tokio::time::sleep(Duration::from_millis(5)).await;
            assert_eq!(
                cap.tail.fetched.load(Ordering::Relaxed),
                1,
                "3, 4 wait for 2"
            );
            node.open(2);
        };
        tokio::join!(poll, driver);
        assert_eq!(node.calls(), vec![1, 2, 3, 4]);
        assert_eq!(
            (stats(&cap), cur.next),
            ([1, 1, 0], 2),
            "one error, cursor on 2"
        );
        assert_eq!(applied_heights(&cap), vec![1]);
        tail_poll(&cap, &node, &mut cur, far()).await;
        assert_eq!(node.calls(), vec![2, 3, 4], "2 retried, 3 and 4 read again");
        assert_eq!(applied_heights(&cap), vec![2, 3, 4]);
        assert_eq!((stats(&cap), cur.next), ([4, 1, 0], 5));
        assert_eq!(cap.tracker.lock().unwrap().released, [4, 0, 0]);
    }
}
