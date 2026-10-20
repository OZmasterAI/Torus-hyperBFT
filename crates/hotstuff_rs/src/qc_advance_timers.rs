//! s131: step timers for the collector's `qc_to_advance` leg (CollectPC for a
//! generic PC -> our next StartView), so a bench can name which step makes a
//! view's advance slow.
//!
//! The leg, all on the `hotstuff-algo` thread:
//!
//! 1. **Handler** (`on_receive_phase_vote`, right after CollectPC):
//!    `pc_check` (PC correctness / safety), `highest_pc`
//!    (`advance_highest_pc_from_remote`), `tree_update` (`block_tree.update`:
//!    lock + durable commit write), `commit_feed_tree` + `commit_feed_app` (the
//!    committed-block feed: tree reads and fed-frontier write vs the app's
//!    `on_committed_block` calls), `post_commit` (speculative commit, collector
//!    validator sets), `advance_send` (AdvanceView broadcast hand-off).
//! 2. **Wait**: the pacemaker only moves on an AdvanceView message, and the
//!    collector's own one comes back through the network's self-delivery
//!    (swarm loop -> inbound queue -> poller -> progress channel). Until then the
//!    algorithm loop runs: `wait_recv` (blocked in the progress receive),
//!    `wait_msg` (handling other messages), `wait_busy` (all other loop work).
//! 3. **Advance**: `advance_rx` (the AdvanceView that moved the pacemaker,
//!    incl. its PC check), `loop_to_enter` (rest of the loop pass + pacemaker
//!    tick), then `enter_view`: `enter_new_view` (NewView build + send),
//!    `enter_view_write` (`set_highest_view_entered`), `enter_start` (leader
//!    selection up to the StartView event).
//!
//! Per view the steps sum to `total` (Instant clock; the event-clock
//! `torus_view_qc_to_advance_seconds` measures the same span). One sample per
//! advanced collector view goes to the observer the host installs with
//! [`set_observer`]; without one this only costs a few `Instant::now` calls
//! per view. Timing only: nothing here changes protocol behaviour.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ed25519_dalek::VerifyingKey;

/// One step of the leg, in order. Index into [`QcAdvanceSample::steps`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    PcCheck = 0,
    HighestPc,
    TreeUpdate,
    CommitFeedTree,
    CommitFeedApp,
    PostCommit,
    AdvanceSend,
    WaitBusy,
    WaitRecv,
    WaitMsg,
    AdvanceRx,
    LoopToEnter,
    EnterNewView,
    EnterViewWrite,
    EnterStart,
}

/// Number of [`Step`]s.
pub const STEP_COUNT: usize = 15;

/// Metric-name stem of each [`Step`], same order (`torus_qc_advance_<name>_seconds`).
pub const STEP_NAMES: [&str; STEP_COUNT] = [
    "pc_check",
    "highest_pc",
    "tree_update",
    "commit_feed_tree",
    "commit_feed_app",
    "post_commit",
    "advance_send",
    "wait_busy",
    "wait_recv",
    "wait_msg",
    "advance_rx",
    "loop_to_enter",
    "enter_new_view",
    "enter_view_write",
    "enter_start",
];

/// What moved the pacemaker into the next view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdvancedBy {
    /// Our own AdvanceView, back through self-delivery.
    OwnAdvanceView,
    /// Another validator's AdvanceView.
    PeerAdvanceView,
    /// Anything else (timeout, round skip, ...).
    Other,
}

/// One collector view's `qc_to_advance` split.
#[derive(Clone, Debug, PartialEq)]
pub struct QcAdvanceSample {
    /// View of the collected PC.
    pub pc_view: u64,
    /// Time per [`Step`]; sums to `total`.
    pub steps: [Duration; STEP_COUNT],
    /// CollectPC -> StartView.
    pub total: Duration,
    /// Blocks the committed-block feed delivered to the app in the handler.
    pub committed_blocks: u64,
    /// Messages handled while waiting for the AdvanceView.
    pub wait_msgs: u64,
    /// AdvanceView broadcast -> our own copy received on this thread, if seen.
    pub own_loopback: Option<Duration>,
    pub advanced_by: AdvancedBy,
}

type Observer = Box<dyn Fn(&QcAdvanceSample) + Send + Sync>;

static OBSERVER: OnceLock<Observer> = OnceLock::new();

/// Install the process-wide sample observer (called on the algorithm thread
/// at each advanced collector view). First call wins; returns whether this
/// call installed it.
pub fn set_observer(f: impl Fn(&QcAdvanceSample) + Send + Sync + 'static) -> bool {
    OBSERVER.set(Box::new(f)).is_ok()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Handler,
    Wait,
    AfterAdvance,
    Entering,
}

struct Tracker {
    sample: QcAdvanceSample,
    start: Instant,
    lap: Instant,
    phase: Phase,
    own: VerifyingKey,
    app_time: Duration,
    advance_sent: Option<Instant>,
    /// The message whose handler collected the PC is still being returned
    /// from when the wait starts: its tail is timed, not counted.
    skip_msg: bool,
}

impl Tracker {
    fn lap_at(&mut self, step: Step, now: Instant) {
        self.sample.steps[step as usize] += now.saturating_duration_since(self.lap);
        self.lap = now;
    }

    fn lap(&mut self, step: Step) {
        self.lap_at(step, Instant::now());
    }
}

thread_local! {
    static TRACK: RefCell<Option<Tracker>> = const { RefCell::new(None) };
    #[cfg(test)]
    static LAST: RefCell<Option<QcAdvanceSample>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Tracker) -> R) -> Option<R> {
    TRACK.with(|t| t.borrow_mut().as_mut().map(f))
}

fn emit(sample: QcAdvanceSample) {
    if let Some(observer) = OBSERVER.get() {
        observer(&sample);
    }
    #[cfg(test)]
    LAST.with(|l| *l.borrow_mut() = Some(sample));
}

/// CollectPC (generic phase): arm the leg at `at`. Replaces any open leg.
pub(crate) fn begin(pc_view: u64, own: VerifyingKey, at: Instant) {
    TRACK.with(|t| {
        *t.borrow_mut() = Some(Tracker {
            sample: QcAdvanceSample {
                pc_view,
                steps: [Duration::ZERO; STEP_COUNT],
                total: Duration::ZERO,
                committed_blocks: 0,
                wait_msgs: 0,
                own_loopback: None,
                advanced_by: AdvancedBy::Other,
            },
            start: at,
            lap: at,
            phase: Phase::Handler,
            own,
            app_time: Duration::ZERO,
            advance_sent: None,
            skip_msg: false,
        })
    });
}

/// The collected PC was dropped (unsafe): no leg. Only a leg still in its
/// handler is dropped.
pub(crate) fn cancel() {
    TRACK.with(|t| {
        let mut slot = t.borrow_mut();
        if slot.as_ref().is_some_and(|t| t.phase == Phase::Handler) {
            *slot = None;
        }
    });
}

/// Handler step boundary: the time since the previous boundary goes to `step`.
pub(crate) fn lap(step: Step) {
    with(|t| {
        if t.phase == Phase::Handler {
            t.lap(step)
        }
    });
}

/// Committed-block feed: one app `on_committed_block` call took `d`.
pub(crate) fn add_app_time(d: Duration) {
    with(|t| {
        if t.phase == Phase::Handler {
            t.app_time += d;
            t.sample.committed_blocks += 1;
        }
    });
}

/// End of the committed-block feed: split the step into app time and the rest.
pub(crate) fn commit_feed_done() {
    with(|t| {
        if t.phase != Phase::Handler {
            return;
        }
        let now = Instant::now();
        let feed = now.saturating_duration_since(t.lap);
        let app = std::mem::take(&mut t.app_time).min(feed);
        t.sample.steps[Step::CommitFeedApp as usize] += app;
        t.sample.steps[Step::CommitFeedTree as usize] += feed - app;
        t.lap = now;
    });
}

/// Handler done (AdvanceView handed to the network): start waiting.
pub(crate) fn handler_done() {
    with(|t| {
        if t.phase == Phase::Handler {
            let now = Instant::now();
            t.lap_at(Step::AdvanceSend, now);
            t.advance_sent = Some(now);
            t.phase = Phase::Wait;
            t.skip_msg = true;
        }
    });
}

/// About to block in the progress-message receive.
pub(crate) fn recv_begin() {
    with(|t| {
        if t.phase == Phase::Wait {
            t.lap(Step::WaitBusy)
        }
    });
}

/// Back from the progress-message receive.
pub(crate) fn recv_end() {
    with(|t| {
        if t.phase == Phase::Wait {
            t.lap(Step::WaitRecv)
        }
    });
}

/// A non-pacemaker message was handled while waiting.
pub(crate) fn msg_handled() {
    with(|t| {
        if t.phase == Phase::Wait {
            t.lap(Step::WaitMsg);
            if !std::mem::take(&mut t.skip_msg) {
                t.sample.wait_msgs += 1;
            }
        }
    });
}

/// A pacemaker message from `origin` was received (call right after
/// [`recv_end`], before handling it): our own AdvanceView closes the loopback.
pub(crate) fn pacemaker_msg_received(origin: &VerifyingKey, is_advance_view: bool) {
    with(|t| {
        if is_advance_view && *origin == t.own && t.sample.own_loopback.is_none() {
            if let Some(sent) = t.advance_sent {
                t.sample.own_loopback = Some(Instant::now().saturating_duration_since(sent));
            }
        }
    });
}

/// A pacemaker message from `origin` was handled; `view_advanced` = it moved
/// the pacemaker's view.
pub(crate) fn pacemaker_msg_handled(origin: &VerifyingKey, is_advance_view: bool, view_advanced: bool) {
    with(|t| {
        if t.phase != Phase::Wait {
            return;
        }
        if view_advanced {
            t.lap(Step::AdvanceRx);
            t.phase = Phase::AfterAdvance;
            t.sample.advanced_by = match (is_advance_view, *origin == t.own) {
                (true, true) => AdvancedBy::OwnAdvanceView,
                (true, false) => AdvancedBy::PeerAdvanceView,
                (false, _) => AdvancedBy::Other,
            };
        } else {
            t.lap(Step::WaitMsg);
            t.sample.wait_msgs += 1;
        }
    });
}

/// `enter_view` starts the view transition.
pub(crate) fn enter_view_begin() {
    with(|t| {
        match t.phase {
            Phase::AfterAdvance => t.lap(Step::LoopToEnter),
            Phase::Wait => t.lap(Step::WaitBusy),
            Phase::Handler => t.lap(Step::AdvanceSend),
            Phase::Entering => return,
        }
        t.phase = Phase::Entering;
    });
}

/// `enter_view` step boundary.
pub(crate) fn enter_lap(step: Step) {
    with(|t| {
        if t.phase == Phase::Entering {
            t.lap(step)
        }
    });
}

/// StartView for `view`: closes the leg when `view` is past the PC's view.
pub(crate) fn start_view(view: u64) {
    let done = TRACK.with(|cell| {
        let mut slot = cell.borrow_mut();
        let t = slot.as_mut()?;
        if view <= t.sample.pc_view {
            return None;
        }
        let now = Instant::now();
        let step = if t.phase == Phase::Entering { Step::EnterStart } else { Step::WaitBusy };
        t.lap_at(step, now);
        t.sample.total = now.saturating_duration_since(t.start);
        slot.take().map(|t| t.sample)
    });
    if let Some(sample) = done {
        emit(sample);
    }
}

#[cfg(test)]
pub(crate) fn take_last() -> Option<QcAdvanceSample> {
    LAST.with(|l| l.borrow_mut().take())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn key(b: u8) -> VerifyingKey {
        SigningKey::from_bytes(&[b; 32]).verifying_key()
    }

    fn spin(us: u64) {
        let until = Instant::now() + Duration::from_micros(us);
        while Instant::now() < until {}
    }

    #[test]
    fn names_match_steps() {
        assert_eq!(STEP_NAMES.len(), STEP_COUNT);
        assert_eq!(Step::EnterStart as usize, STEP_COUNT - 1);
        assert_eq!(STEP_NAMES[Step::WaitRecv as usize], "wait_recv");
        assert_eq!(STEP_NAMES[Step::CommitFeedApp as usize], "commit_feed_app");
    }

    #[test]
    fn full_leg_by_own_advance_view_sums_to_total() {
        let own = key(1);
        begin(7, own, Instant::now());
        spin(200);
        lap(Step::PcCheck);
        lap(Step::HighestPc);
        spin(300);
        lap(Step::TreeUpdate);
        spin(100);
        add_app_time(Duration::from_micros(50));
        add_app_time(Duration::from_micros(50));
        commit_feed_done();
        lap(Step::PostCommit);
        handler_done();
        msg_handled(); // tail of the phase-vote message
        recv_begin();
        spin(400);
        recv_end();
        // a peer vote arrives first and is handled
        spin(100);
        msg_handled();
        recv_begin();
        recv_end();
        pacemaker_msg_received(&own, true);
        pacemaker_msg_handled(&own, true, true);
        enter_view_begin();
        enter_lap(Step::EnterNewView);
        enter_lap(Step::EnterViewWrite);
        start_view(8);
        let s = take_last().expect("sample emitted");
        assert_eq!(s.pc_view, 7);
        assert_eq!(s.committed_blocks, 2);
        assert_eq!(s.wait_msgs, 1, "the PC's own vote message is not counted");
        assert_eq!(s.advanced_by, AdvancedBy::OwnAdvanceView);
        assert!(s.own_loopback.unwrap() >= Duration::from_micros(500));
        assert!(s.steps[Step::PcCheck as usize] >= Duration::from_micros(200));
        assert!(s.steps[Step::TreeUpdate as usize] >= Duration::from_micros(300));
        assert!(s.steps[Step::WaitRecv as usize] >= Duration::from_micros(400));
        assert!(s.steps[Step::WaitMsg as usize] >= Duration::from_micros(100));
        assert!(s.steps[Step::CommitFeedApp as usize] <= Duration::from_micros(100));
        assert!(s.steps[Step::CommitFeedTree as usize] >= Duration::ZERO);
        let sum: Duration = s.steps.iter().sum();
        assert_eq!(sum, s.total, "steps must sum to total");
        // the leg is closed
        start_view(9);
        assert!(take_last().is_none());
    }

    #[test]
    fn cancel_and_stale_start_view_emit_nothing() {
        begin(3, key(1), Instant::now());
        cancel();
        start_view(4);
        assert!(take_last().is_none());

        begin(5, key(1), Instant::now());
        handler_done();
        start_view(5); // not past the PC's view
        assert!(take_last().is_none());
        // a timeout-driven advance still closes the leg, as Other
        start_view(6);
        let s = take_last().unwrap();
        assert_eq!(s.advanced_by, AdvancedBy::Other);
        assert_eq!(s.steps.iter().sum::<Duration>(), s.total);
    }

    #[test]
    fn peer_advance_view_and_no_tracking_noops() {
        // no leg: every hook is a no-op
        recv_begin();
        recv_end();
        msg_handled();
        enter_view_begin();
        start_view(1);
        assert!(take_last().is_none());

        let own = key(1);
        begin(10, own, Instant::now());
        handler_done();
        pacemaker_msg_received(&key(2), true);
        pacemaker_msg_handled(&key(2), true, true);
        enter_view_begin();
        start_view(11);
        let s = take_last().unwrap();
        assert_eq!(s.advanced_by, AdvancedBy::PeerAdvanceView);
        assert_eq!(s.own_loopback, None);
    }
}
