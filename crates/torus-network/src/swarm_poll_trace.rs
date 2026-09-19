//! Local-only, default-OFF scheduling diagnostics. Never a wire latency clock.
use serde::Serialize;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const WINDOW_US: u128 = 1_000_000;
const RECEIVE_LIMIT: u64 = 32;
pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| parse_enabled(std::env::var("TORUS_SWARM_POLL_TRACE").ok().as_deref()))
}
fn parse_enabled(value: Option<&str>) -> bool {
    value == Some("1")
}
fn now() -> u128 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros()
}

/// Bounded cross-clock observation, only at aggregate emission (never per poll).
#[derive(Debug, Serialize)]
struct ClockAnchor {
    local_before_us: u128,
    local_after_us: u128,
    body_fetch_pid: u32,
    body_fetch_seq: u64,
    body_fetch_mono_us: u128,
    unix_us: i128,
}
fn clock_anchor(
    mut local: impl FnMut() -> u128,
    capture: impl FnOnce() -> hotstuff_rs::logging::BodyFetchTraceStamp,
) -> ClockAnchor {
    let local_before_us = local();
    let stamp = capture();
    let local_after_us = local();
    ClockAnchor {
        local_before_us,
        local_after_us,
        body_fetch_pid: stamp.pid,
        body_fetch_seq: stamp.seq,
        body_fetch_mono_us: stamp.mono_us,
        unix_us: stamp.unix_us,
    }
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct PollTrace {
    window_start_us: u128,
    polls: u64,
    pending: u64,
    ready: u64,
    selected_commands: u64,
    selected_command_closed: u64,
    poll_total_us: u128,
    poll_max_us: u128,
    last_poll_end_us: Option<u128>,
    max_interpoll_us: u128,
    max_interpoll_from_us: Option<u128>,
    max_interpoll_to_us: Option<u128>,
}
impl PollTrace {
    pub(crate) fn new() -> Self {
        Self {
            window_start_us: now(),
            ..Self::default()
        }
    }
    fn record(&mut self, start: u128, end: u128, ready: bool) {
        self.polls += 1;
        if ready {
            self.ready += 1;
        } else {
            self.pending += 1;
        }
        let duration = end - start;
        self.poll_total_us += duration;
        self.poll_max_us = self.poll_max_us.max(duration);
        if let Some(previous) = self.last_poll_end_us {
            let gap = start - previous;
            if self.max_interpoll_from_us.is_none() || gap > self.max_interpoll_us {
                self.max_interpoll_us = gap;
                self.max_interpoll_from_us = Some(previous);
                self.max_interpoll_to_us = Some(start);
            }
        }
        self.last_poll_end_us = Some(end);
    }
    pub(crate) fn command(&mut self, closed: bool) {
        if closed {
            self.selected_command_closed += 1;
        } else {
            self.selected_commands += 1;
        }
    }
    pub(crate) fn maybe_emit(&mut self) {
        let end = now();
        if end - self.window_start_us < WINDOW_US {
            return;
        }
        // Only called between select iterations: no new timer/wakeup. Last interval
        // remains open/censored; a completed gap belongs to its ending window.
        let anchor = clock_anchor(now, hotstuff_rs::logging::BodyFetchTraceStamp::capture);
        tracing::info!(
            "swarm_poll_diag {}",
            serde_json::json!({
                "clock": "swarm_poll_trace/process_monotonic_us", "clock_anchor": anchor,
                "schema": 1, "pid": std::process::id(), "window_end_us": end,
                "open_interpoll_us": self.last_poll_end_us.map(|t| end-t),
                "stats": self,
            })
        );
        self.reset(end);
    }
    fn reset(&mut self, end: u128) {
        *self = Self {
            window_start_us: end,
            last_poll_end_us: self.last_poll_end_us,
            ..Self::default()
        };
    }
}

/// Poll the ORIGINAL future once with the ORIGINAL context on each wrapper poll.
/// State lives outside the recreated select future; no wake, loop, or extra poll.
pub(crate) async fn observe<F: Future>(future: F, state: &mut Option<PollTrace>) -> F::Output {
    observe_with_clock(future, state, now).await
}
async fn observe_with_clock<F: Future>(
    future: F,
    state: &mut Option<PollTrace>,
    mut clock: impl FnMut() -> u128,
) -> F::Output {
    futures::pin_mut!(future);
    if state.is_none() {
        return future.await;
    }
    futures::future::poll_fn(|cx| {
        let start = clock();
        let result = future.as_mut().poll(cx);
        let end = clock();
        state
            .as_mut()
            .unwrap()
            .record(start, end, result.is_ready());
        result
    })
    .await
}

#[derive(Debug, Serialize)]
pub(crate) struct ReceiveTrace {
    #[serde(skip)]
    emit: bool,
    pid: u32,
    id: u64,
    event_us: u128,
    pub(crate) decode_begin_us: Option<u128>,
    pub(crate) decode_end_us: Option<u128>,
    pub(crate) tee_begin_us: Option<u128>,
    pub(crate) tee_end_us: Option<u128>,
    pub(crate) lock_begin_us: Option<u128>,
    pub(crate) lock_acquired_us: Option<u128>,
    pub(crate) admission_us: Option<u128>,
    pub(crate) admission_seq: Option<u64>,
    pub(crate) kind: Option<&'static str>,
    pub(crate) view: Option<u64>,
    pub(crate) hash: Option<[u8; 32]>,
    pub(crate) sender: Option<[u8; 32]>,
    pub(crate) outcome: &'static str,
}
impl ReceiveTrace {
    pub(crate) fn start() -> Option<Self> {
        if !enabled() {
            return None;
        }
        static ID: AtomicU64 = AtomicU64::new(1);
        Some(Self::at(ID.fetch_add(1, Ordering::Relaxed), now()))
    }
    fn at(id: u64, event_us: u128) -> Self {
        Self {
            emit: true,
            pid: std::process::id(),
            id,
            event_us,
            decode_begin_us: None,
            decode_end_us: None,
            tee_begin_us: None,
            tee_end_us: None,
            lock_begin_us: None,
            lock_acquired_us: None,
            admission_us: None,
            admission_seq: None,
            kind: None,
            view: None,
            hash: None,
            sender: None,
            outcome: "filtered",
        }
    }
    pub(crate) fn stamp() -> u128 {
        now()
    }
    #[cfg(test)]
    pub(crate) fn for_test(id: u64) -> Self {
        let mut trace = Self::at(id, now());
        trace.emit = false;
        trace
    }
}
#[derive(Default)]
struct ReceiveBudget {
    start: u128,
    emitted: u64,
    suppressed: u64,
}
impl ReceiveBudget {
    fn take(&mut self, now: u128) -> Option<u64> {
        if now - self.start >= WINDOW_US {
            self.start = now;
            self.emitted = 0;
        }
        if self.emitted >= RECEIVE_LIMIT {
            self.suppressed += 1;
            return None;
        }
        self.emitted += 1;
        Some(std::mem::take(&mut self.suppressed))
    }
}
fn take_receive_budget(budget: &Mutex<ReceiveBudget>, clock: impl FnOnce() -> u128) -> Option<u64> {
    let mut budget = budget.lock().unwrap();
    // Capture under lock: finish-time order need not match lock-acquisition order.
    budget.take(clock())
}
impl Drop for ReceiveTrace {
    fn drop(&mut self) {
        // Ordinary non-body traffic is intentionally filtered. Malformed/denied
        // direct envelopes remain observable without guessing their inner kind.
        if !self.emit || (self.kind.is_none() && self.outcome == "filtered") {
            return;
        }
        static BUDGET: Mutex<ReceiveBudget> = Mutex::new(ReceiveBudget {
            start: 0,
            emitted: 0,
            suppressed: 0,
        });
        let finished = now();
        let suppressed = take_receive_budget(&BUDGET, now);
        if let Some(suppressed) = suppressed {
            tracing::info!(
                "swarm_receive_diag {}",
                serde_json::json!({
                    "schema": 1, "clock": "swarm_poll_trace/process_monotonic_us", "record": self, "finished_us": finished,
                    "suppressed_since_previous_record": suppressed,
                })
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::task::noop_waker;
    use std::task::{Context, Poll};
    #[test]
    fn swarm_poll_trace_flag_exact() {
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some(" 1"),
            Some("1 "),
        ] {
            assert!(!parse_enabled(value));
        }
        assert!(parse_enabled(Some("1")));
    }
    #[test]
    fn swarm_poll_trace_pending_ready_cancel_recreate() {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut state = Some(PollTrace::default());
        let mut stamps = [10, 12, 20, 23].into_iter();
        let mut polls = 0;
        let f = futures::future::poll_fn(|_| {
            polls += 1;
            if polls == 1 {
                Poll::Pending
            } else {
                Poll::Ready(7)
            }
        });
        let mut wrapped = Box::pin(observe_with_clock(f, &mut state, || stamps.next().unwrap()));
        assert!(wrapped.as_mut().poll(&mut cx).is_pending());
        assert_eq!(wrapped.as_mut().poll(&mut cx), Poll::Ready(7));
        drop(wrapped);
        let mut stamps = [30, 31].into_iter();
        let mut pending = Box::pin(observe_with_clock(
            futures::future::pending::<()>(),
            &mut state,
            || stamps.next().unwrap(),
        ));
        assert!(pending.as_mut().poll(&mut cx).is_pending());
        drop(pending);
        state.as_mut().unwrap().command(false);
        state.as_mut().unwrap().command(true);
        let mut stamps = [50, 54].into_iter();
        let mut recreated = Box::pin(observe_with_clock(
            futures::future::ready(9),
            &mut state,
            || stamps.next().unwrap(),
        ));
        assert_eq!(recreated.as_mut().poll(&mut cx), Poll::Ready(9));
        drop(recreated);
        let s = state.as_mut().unwrap();
        assert_eq!(
            (
                s.polls,
                s.pending,
                s.ready,
                s.selected_commands,
                s.selected_command_closed
            ),
            (4, 2, 2, 1, 1)
        );
        assert_eq!(
            (s.poll_total_us, s.poll_max_us, s.max_interpoll_us),
            (10, 4, 19)
        );
        assert_eq!(
            (s.max_interpoll_from_us, s.max_interpoll_to_us),
            (Some(31), Some(50))
        );
        s.reset(60);
        s.record(90, 92, false);
        assert_eq!(
            (s.polls, s.max_interpoll_us, s.max_interpoll_from_us),
            (1, 36, Some(54))
        );
    }
    #[tokio::test]
    async fn swarm_poll_trace_biased_command_skips_unpolled_future() {
        let mut state = Some(PollTrace::default());
        let selected_command = tokio::select! {
            biased;
            _ = futures::future::ready(()) => true,
            _ = observe_with_clock(futures::future::pending::<()>(), &mut state, || panic!("lower priority branch polled")) => false,
        };
        assert!(selected_command);
        let state = state.as_mut().unwrap();
        state.command(false);
        assert_eq!((state.polls, state.selected_commands), (0, 1));
    }

    #[test]
    fn swarm_poll_trace_off_never_reads_clock() {
        let mut off = None;
        assert_eq!(
            futures::executor::block_on(observe_with_clock(
                futures::future::ready(4),
                &mut off,
                || panic!("off clock")
            )),
            4
        );
    }
    #[test]
    fn swarm_poll_trace_anchor_bounds_existing_clock_capture() {
        let mut stamps = [100, 108].into_iter();
        let anchor = clock_anchor(
            || stamps.next().unwrap(),
            || hotstuff_rs::logging::BodyFetchTraceStamp {
                pid: 7,
                seq: 11,
                mono_us: 900,
                unix_us: -12,
            },
        );
        assert_eq!((anchor.local_before_us, anchor.local_after_us), (100, 108));
        assert_eq!(
            (
                anchor.body_fetch_pid,
                anchor.body_fetch_seq,
                anchor.body_fetch_mono_us,
                anchor.unix_us
            ),
            (7, 11, 900, -12)
        );
    }
    #[test]
    fn swarm_receive_trace_budget_samples_clock_only_under_lock() {
        let budget = Mutex::new(ReceiveBudget {
            start: WINDOW_US,
            emitted: RECEIVE_LIMIT,
            suppressed: 0,
        });
        // A handler's earlier finish stamp could precede this window. The helper
        // must instead capture a NEW timestamp while holding the budget lock.
        assert_eq!(
            take_receive_budget(&budget, || {
                assert!(budget.try_lock().is_err());
                WINDOW_US + 1
            }),
            None
        );
        assert_eq!(
            take_receive_budget(&budget, || {
                assert!(budget.try_lock().is_err());
                2 * WINDOW_US
            }),
            Some(1)
        );
    }

    #[test]
    fn swarm_receive_trace_local_ids_and_budget() {
        let mut first = ReceiveTrace::at(1, 10);
        let second = ReceiveTrace::at(2, 10);
        first.outcome = "decode_error";
        let value = serde_json::to_value(&first).unwrap();
        assert_eq!(value["id"], 1);
        assert_eq!(value["outcome"], "decode_error");
        assert!(value["admission_us"].is_null());
        assert_ne!(first.id, second.id);
        // Keep test records filtered, so test never writes diagnostic logs.
        first.outcome = "filtered";
        let mut budget = ReceiveBudget::default();
        for _ in 0..RECEIVE_LIMIT {
            assert_eq!(budget.take(0), Some(0));
        }
        assert_eq!(budget.take(1), None);
        assert_eq!(budget.take(2), None);
        assert_eq!(budget.take(WINDOW_US), Some(2));
    }
}
