//! Release timing for the `--max-in-flight` cap (s121): how long a slot stays
//! taken, how long senders wait for one, and how the block tail keeps up.
//! Cheap by design: the sender path adds no lock and no allocation (the wait
//! is recorded under the tracker lock `Cap::wait_ready` already holds, into
//! histogram buffers allocated once at start and cleared in place each
//! window), the tail adds no RPC.
//!
//! bench.log lines (stdout, `key=value`, `-` = no sample in the window):
//!
//! Every 15 s from the tail task (window = since the previous line), and once
//! at the end of the run with `final=1` (the partial last window):
//!
//! ```text
//! release-timing: t=<s since the cap started> wall_ms=<unix ms> final=0|1
//!   released=<n> age_p50_ms= age_p90_ms= age_p99_ms= age_max_ms=
//!   ready_calls=<n> wait_cap_n= wait_cap_s= wait_cap_p50_ms= wait_cap_p99_ms=
//!   wait_cancel_n= wait_cancel_s= wait_cancel_p50_ms= wait_cancel_p99_ms=
//!   tail_rounds= tail_fetched= tail_fetch_max= tail_round_p50_ms=
//!   tail_round_max_ms= tail_lag_max=
//! release-timing-blocks: t=<s> seen=<height>@<unix ms>,...
//! ```
//! (each is ONE line; wrapped here.)
//!
//! * `released`, `age_*`: actions released COMMITTED in the window; age =
//!   wall ms when the tail had the body parsed minus the action nonce (wall ms
//!   at signing, same host clock): sign + submit + mempool + consensus + tail.
//! * `ready_calls`: `Cap::wait_ready` calls that returned a free slot (fires).
//!   `wait_cap_*`: the calls that waited while the sender was at its cap;
//!   `wait_cancel_*`: below the cap but held by its own in-flight cancel-all
//!   over its open-order budget. A wait is split by the reason seen at each
//!   check, so one call can add to both. `_n` = calls, `_s` = total seconds.
//! * `tail_rounds`: tail polls (one per `TAIL_POLL` + the poll itself; a
//!   poll cut short by the end of the run counts, with what it read);
//!   `tail_fetched`: bodies read; `tail_fetch_max`: most bodies in one poll
//!   (commit mode reads until it is caught up, so this is the heights it was
//!   behind at the poll start); `tail_round_*`: one poll's duration (RPC round
//!   trips + parse + release). `tail_lag_max`: head mode only,
//!   `eth_blockNumber` minus the last read height at the poll start (the eth
//!   head is min(applied, committed), so this is lag behind the head, not
//!   behind commit); `-` in commit mode (no extra RPC to learn the head).
//! * `seen`: when the tail had each body parsed. `torus_getBlockBody` carries
//!   no commit or block timestamp, so join these with the node's commit log
//!   offline for the commit-to-seen delay.
//!
//! End of run, one line per histogram (run totals):
//!
//! ```text
//! release-timing-hist: what=<name> n= sum= p50= p90= p99= p999= max= buckets=<lo>:<count>,...
//! ```
//! names: `age_ms`, `wait_cap_us`, `wait_cancel_us`, `tail_round_us`,
//! `tail_fetched_per_round`, `tail_lag_heights`.
//!
//! Histogram: values below 32 exact, above that 16 buckets per power of two;
//! a quantile is its bucket's lower bound (at most 6.25% low), capped at max.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Period of the `release-timing:` lines.
pub const WINDOW: Duration = Duration::from_secs(15);

const EXACT: usize = 32;
const BUCKETS: usize = EXACT + (64 - 5) * 16;

/// Unix wall-clock ms (the clock the action nonces use).
pub fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn bucket(v: u64) -> usize {
    if v < EXACT as u64 {
        return v as usize;
    }
    let e = 63 - v.leading_zeros(); // >= 5
    let sub = (v >> (e - 4)) & 15;
    EXACT + (e as usize - 5) * 16 + sub as usize
}

fn bucket_lo(i: usize) -> u64 {
    if i < EXACT {
        return i as u64;
    }
    let j = i - EXACT;
    let e = 5 + (j / 16) as u32;
    (1u64 << e) | (((j % 16) as u64) << (e - 4))
}

/// Log-linear histogram of `u64` samples (see the module doc).
#[derive(Clone, Debug, Default)]
pub struct Hist {
    /// Empty until the first sample unless [`preallocated`](Self::preallocated).
    counts: Vec<u64>,
    pub n: u64,
    pub sum: u64,
    pub max: u64,
}

impl Hist {
    /// A histogram with its bucket buffer already allocated (recording into
    /// it, and [`clear`](Self::clear), never allocate).
    pub fn preallocated() -> Self {
        Self {
            counts: vec![0; BUCKETS],
            ..Self::default()
        }
    }

    /// Empty it, keeping the bucket buffer.
    pub fn clear(&mut self) {
        self.counts.fill(0);
        self.n = 0;
        self.sum = 0;
        self.max = 0;
    }

    /// What a window line needs (no buffer copy).
    pub fn summary(&self) -> Summary {
        Summary {
            n: self.n,
            sum: self.sum,
            max: self.max,
            p50: self.quantile(0.5),
            p90: self.quantile(0.9),
            p99: self.quantile(0.99),
        }
    }

    pub fn record(&mut self, v: u64) {
        if self.counts.is_empty() {
            self.counts = vec![0; BUCKETS];
        }
        self.counts[bucket(v)] += 1;
        self.n += 1;
        self.sum = self.sum.saturating_add(v);
        self.max = self.max.max(v);
    }

    /// Nearest-rank quantile (`q` in 0..=1); `None` when empty.
    pub fn quantile(&self, q: f64) -> Option<u64> {
        if self.n == 0 {
            return None;
        }
        let rank = ((q * self.n as f64).ceil() as u64).clamp(1, self.n);
        let mut seen = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return Some(bucket_lo(i).min(self.max));
            }
        }
        Some(self.max)
    }

    pub fn merge(&mut self, other: &Hist) {
        if other.n == 0 {
            return;
        }
        if self.counts.is_empty() {
            self.counts = vec![0; BUCKETS];
        }
        for (a, b) in self.counts.iter_mut().zip(&other.counts) {
            *a += b;
        }
        self.n += other.n;
        self.sum = self.sum.saturating_add(other.sum);
        self.max = self.max.max(other.max);
    }

    /// `(bucket lower bound, count)` of every non-empty bucket.
    pub fn buckets(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.counts
            .iter()
            .enumerate()
            .filter(|(_, &c)| c > 0)
            .map(|(i, &c)| (bucket_lo(i), c))
    }
}

/// One closed window of a [`Series`]: count, sum, max and quantiles
/// (`None` when empty).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub n: u64,
    pub sum: u64,
    pub max: u64,
    pub p50: Option<u64>,
    pub p90: Option<u64>,
    pub p99: Option<u64>,
}

/// One quantity: the current window and the run total. Both buffers are
/// allocated at construction and reused for the whole run.
pub struct Series {
    pub window: Hist,
    pub total: Hist,
}

impl Default for Series {
    fn default() -> Self {
        Self {
            window: Hist::preallocated(),
            total: Hist::preallocated(),
        }
    }
}

impl Series {
    pub fn record(&mut self, v: u64) {
        self.window.record(v);
    }

    /// Close the window: fold it into the total, clear it in place and
    /// return its summary (no allocation).
    pub fn roll(&mut self) -> Summary {
        let w = self.window.summary();
        self.total.merge(&self.window);
        self.window.clear();
        w
    }
}

/// Why `Cap::wait_ready` is waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    /// The sender is at its in-flight cap.
    CapFull = 0,
    /// Below the cap, held by its own in-flight cancel-all over its budget.
    CancelPending = 1,
}

/// Release and wait timing, kept in the `Tracker` (under its lock).
#[derive(Default)]
pub struct ReleaseTiming {
    age_ms: Series,
    /// Indexed by [`WaitReason`], microseconds.
    wait_us: [Series; 2],
    ready_calls: u64,
}

/// One closed window of [`ReleaseTiming`].
pub struct RelWindow {
    pub age_ms: Summary,
    pub wait_us: [Summary; 2],
    pub ready_calls: u64,
}

impl ReleaseTiming {
    /// One action released committed, its body seen at `now_ms`.
    pub fn on_release(&mut self, now_ms: u64, nonce: u64) {
        self.age_ms.record(now_ms.saturating_sub(nonce));
    }

    /// One `wait_ready` call returned ready; `waited_us[reason]` = time spent
    /// waiting for that reason (`None` = never waited for it).
    pub fn on_ready(&mut self, waited_us: [Option<u64>; 2]) {
        self.ready_calls += 1;
        for (series, us) in self.wait_us.iter_mut().zip(waited_us) {
            if let Some(us) = us {
                series.record(us);
            }
        }
    }

    pub fn roll(&mut self) -> RelWindow {
        RelWindow {
            age_ms: self.age_ms.roll(),
            wait_us: [self.wait_us[0].roll(), self.wait_us[1].roll()],
            ready_calls: std::mem::take(&mut self.ready_calls),
        }
    }

    /// Run-total histograms (complete after the last [`roll`](Self::roll)).
    pub fn totals(&self) -> [(&'static str, &Hist); 3] {
        [
            ("age_ms", &self.age_ms.total),
            ("wait_cap_us", &self.wait_us[0].total),
            ("wait_cancel_us", &self.wait_us[1].total),
        ]
    }
}

/// One body the tail applied (`release-timing-blocks:`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TailBody {
    pub height: u64,
    /// Wall ms when its reply had arrived and was parsed (`seen`).
    pub seen_ms: u64,
    /// Wall ms when it was applied: its slots released (`applied`).
    pub applied_ms: u64,
    /// Its read: request sent to reply parsed, microseconds.
    pub fetch_us: u64,
    /// Reply parsed to applied, microseconds: the wait for the lower heights
    /// still in flight (`--in-flight-tail-depth` > 1; about 0 at depth 1).
    pub wait_us: u64,
}

/// Block-tail timing (one writer: the tail task). Each body and the head
/// lag are recorded as soon as they are read, so a poll cut short (the tail
/// aborted at the end of the run) keeps what it read.
#[derive(Default)]
pub struct TailTiming {
    round_us: Series,
    fetched: Series,
    lag: Series,
    fetch_us: Series,
    apply_wait_us: Series,
    seen: Vec<TailBody>,
    /// The open poll: bodies read so far and its head lag.
    round_fetched: u64,
    round_lag: Option<u64>,
}

/// One closed window of [`TailTiming`].
pub struct TailWindow {
    pub round_us: Summary,
    pub fetched: Summary,
    pub lag: Summary,
    pub fetch_us: Summary,
    pub apply_wait_us: Summary,
    pub seen: Vec<TailBody>,
}

impl TailTiming {
    /// The open poll applied `body`.
    pub fn on_body(&mut self, body: TailBody) {
        self.round_fetched += 1;
        self.seen.push(body);
    }

    /// Head mode: the open poll's head lag (`eth_blockNumber` minus the last
    /// read height at its start).
    pub fn on_lag(&mut self, lag: u64) {
        self.round_lag = Some(lag);
    }

    /// Close the open poll (finished or cut short) after `round_us`.
    pub fn end_round(&mut self, round_us: u64) {
        self.round_us.record(round_us);
        self.fetched.record(std::mem::take(&mut self.round_fetched));
        if let Some(lag) = self.round_lag.take() {
            self.lag.record(lag);
        }
    }

    pub fn roll(&mut self) -> TailWindow {
        TailWindow {
            round_us: self.round_us.roll(),
            fetched: self.fetched.roll(),
            lag: self.lag.roll(),
            fetch_us: self.fetch_us.roll(),
            apply_wait_us: self.apply_wait_us.roll(),
            seen: std::mem::take(&mut self.seen),
        }
    }

    pub fn totals(&self) -> [(&'static str, &Hist); 5] {
        [
            ("tail_round_us", &self.round_us.total),
            ("tail_fetched_per_round", &self.fetched.total),
            ("tail_lag_heights", &self.lag.total),
            ("tail_fetch_us", &self.fetch_us.total),
            ("tail_apply_wait_us", &self.apply_wait_us.total),
        ]
    }
}

/// The `release-timing:` and `release-timing-blocks:` lines of one window.
pub fn window_lines(
    t_s: f64,
    wall_ms: u64,
    last: bool,
    rel: &RelWindow,
    tail: &TailWindow,
) -> [String; 2] {
    let n = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
    let ms =
        |us: Option<u64>| us.map_or_else(|| "-".to_string(), |v| format!("{:.1}", v as f64 / 1e3));
    let max = |h: &Summary| (h.n > 0).then_some(h.max);
    let age = &rel.age_ms;
    let wait = |name: &str, h: &Summary| {
        format!(
            "wait_{name}_n={} wait_{name}_s={:.3} wait_{name}_p50_ms={} wait_{name}_p99_ms={}",
            h.n,
            h.sum as f64 / 1e6,
            ms(h.p50),
            ms(h.p99),
        )
    };
    let main = format!(
        "release-timing: t={t_s:.1} wall_ms={wall_ms} final={} released={} age_p50_ms={} \
         age_p90_ms={} age_p99_ms={} age_max_ms={} ready_calls={} {} {} tail_rounds={} \
         tail_fetched={} tail_fetch_max={} tail_round_p50_ms={} tail_round_max_ms={} \
         tail_lag_max={}",
        u8::from(last),
        age.n,
        n(age.p50),
        n(age.p90),
        n(age.p99),
        n(max(age)),
        rel.ready_calls,
        wait("cap", &rel.wait_us[0]),
        wait("cancel", &rel.wait_us[1]),
        tail.round_us.n,
        tail.fetched.sum,
        n(max(&tail.fetched)),
        ms(tail.round_us.p50),
        ms(max(&tail.round_us)),
        n(max(&tail.lag)),
    );
    let seen: Vec<String> = tail
        .seen
        .iter()
        .map(|b| format!("{}@{}", b.height, b.seen_ms))
        .collect();
    let blocks = format!("release-timing-blocks: t={t_s:.1} seen={}", seen.join(","));
    [main, blocks]
}

/// One end-of-run `release-timing-hist:` line.
pub fn hist_line(what: &str, h: &Hist) -> String {
    let n = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
    let buckets: Vec<String> = h.buckets().map(|(lo, c)| format!("{lo}:{c}")).collect();
    format!(
        "release-timing-hist: what={what} n={} sum={} p50={} p90={} p99={} p999={} max={} \
         buckets={}",
        h.n,
        h.sum,
        n(h.quantile(0.5)),
        n(h.quantile(0.9)),
        n(h.quantile(0.99)),
        n(h.quantile(0.999)),
        n((h.n > 0).then_some(h.max)),
        buckets.join(","),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_exact_below_32_and_within_one_sixteenth_above() {
        for v in 0..32 {
            assert_eq!(bucket_lo(bucket(v)), v);
        }
        let mut v = 32u64;
        while v < u64::MAX / 3 {
            for x in [v, v + 1, v * 3 / 2, v * 2 - 1] {
                let lo = bucket_lo(bucket(x));
                assert!(lo <= x && x - lo <= x / 16, "{x}: lo {lo}");
            }
            v *= 2;
        }
        assert_eq!(bucket(u64::MAX), BUCKETS - 1);
        assert_eq!(bucket(32), 32);
        assert_eq!(bucket_lo(bucket(63)), 62, "width 2 just above 32");
        assert_eq!(bucket_lo(bucket(64)), 64);
    }

    #[test]
    fn quantiles_are_nearest_rank_bucket_lower_bounds_capped_at_max() {
        let mut h = Hist::default();
        assert_eq!(h.quantile(0.5), None);
        for v in 1..=100 {
            h.record(v);
        }
        assert_eq!((h.n, h.sum, h.max), (100, 5050, 100));
        assert_eq!(h.quantile(0.5), Some(50));
        assert_eq!(h.quantile(0.9), Some(88), "90 is in [88, 92)");
        assert_eq!(h.quantile(0.99), Some(96), "99 is in [96, 100)");
        assert_eq!(h.quantile(1.0), Some(100));
        assert_eq!(h.quantile(0.0), Some(1));
        let mut small = Hist::default();
        for v in [3, 3, 7] {
            small.record(v);
        }
        assert_eq!(small.quantile(0.5), Some(3));
        assert_eq!(small.quantile(0.99), Some(7));
        // Capped at max: one sample in a wide bucket.
        let mut one = Hist::default();
        one.record(1_000);
        assert_eq!(one.quantile(0.5), Some(992));
        assert_eq!(one.max, 1_000);
        assert_eq!(small.buckets().collect::<Vec<_>>(), vec![(3, 2), (7, 1)]);
    }

    #[test]
    fn series_roll_folds_the_window_into_the_total() {
        let mut s = Series::default();
        s.record(5);
        s.record(9);
        let w = s.roll();
        assert_eq!((w.n, w.sum, w.max), (2, 14, 9));
        assert_eq!(s.window.n, 0, "window cleared");
        s.record(40);
        let w = s.roll();
        assert_eq!((w.n, w.sum), (1, 40));
        assert_eq!((s.total.n, s.total.sum, s.total.max), (3, 54, 40));
        let mut m = Hist::default();
        m.merge(&s.total);
        m.merge(&Hist::default());
        assert_eq!((m.n, m.quantile(1.0)), (3, Some(40)));
    }

    #[test]
    fn waits_split_by_reason_and_ages_per_release() {
        let mut r = ReleaseTiming::default();
        r.on_ready([None, None]);
        r.on_ready([None, None]);
        r.on_ready([Some(5_000), None]);
        r.on_ready([Some(1_000), Some(2_000)]);
        r.on_ready([None, Some(0)]);
        r.on_release(10_500, 10_000);
        r.on_release(10_500, 10_700); // nonce ahead of the clock: 0
        let w = r.roll();
        assert_eq!(w.ready_calls, 5);
        assert_eq!((w.wait_us[0].n, w.wait_us[0].sum), (2, 6_000));
        assert_eq!((w.wait_us[1].n, w.wait_us[1].sum), (2, 2_000));
        assert_eq!((w.age_ms.n, w.age_ms.max), (2, 500));
        let w = r.roll();
        assert_eq!((w.ready_calls, w.age_ms.n, w.wait_us[0].n), (0, 0, 0));
        let totals = r.totals();
        assert_eq!(totals[0].1.n, 2);
        assert_eq!(totals[1].1.sum, 6_000);
        assert_eq!(totals[2].1.n, 2);
    }

    /// A body seen at `seen_ms`, read in `fetch_us`, applied `wait_us` later.
    fn body(height: u64, seen_ms: u64, fetch_us: u64, wait_us: u64) -> TailBody {
        TailBody {
            height,
            seen_ms,
            applied_ms: seen_ms + wait_us / 1_000,
            fetch_us,
            wait_us,
        }
    }

    #[test]
    fn tail_rounds_and_seen_heights_roll_per_window() {
        let mut t = TailTiming::default();
        t.on_lag(3);
        t.on_body(body(7, 1_000, 900, 0));
        t.on_body(body(8, 1_050, 2_000, 3_000));
        t.end_round(4_000);
        t.end_round(1_500);
        let w = t.roll();
        assert_eq!((w.round_us.n, w.round_us.max), (2, 4_000));
        assert_eq!((w.fetched.sum, w.fetched.max), (2, 2));
        assert_eq!((w.lag.n, w.lag.max), (1, 3), "lag only when known");
        assert_eq!(
            w.seen.iter().map(|b| (b.height, b.seen_ms, b.applied_ms)).collect::<Vec<_>>(),
            vec![(7, 1_000, 1_000), (8, 1_050, 1_053)]
        );
        // Per-body read time and apply wait, one sample per applied body.
        assert_eq!((w.fetch_us.n, w.fetch_us.max), (2, 2_000));
        assert_eq!((w.apply_wait_us.n, w.apply_wait_us.max), (2, 3_000));
        assert!(t.roll().seen.is_empty());
        let totals = t.totals();
        assert_eq!(totals[0].1.n, 2);
        assert_eq!(
            totals.iter().map(|(what, _)| *what).collect::<Vec<_>>(),
            [
                "tail_round_us",
                "tail_fetched_per_round",
                "tail_lag_heights",
                "tail_fetch_us",
                "tail_apply_wait_us"
            ]
        );
        assert_eq!((totals[3].1.n, totals[3].1.sum), (2, 2_900));
        assert_eq!((totals[4].1.n, totals[4].1.sum), (2, 3_000));
    }

    /// Counts this thread's heap allocations (the test binary's allocator).
    mod alloc_count {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        thread_local! {
            static COUNT: Cell<u64> = const { Cell::new(0) };
        }

        struct Counting;

        // SAFETY: every call forwards to `System`; the counter is a
        // destructor-free const thread-local (no allocation to reach it).
        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, l: Layout) -> *mut u8 {
                let _ = COUNT.try_with(|c| c.set(c.get() + 1));
                unsafe { System.alloc(l) }
            }
            unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
                let _ = COUNT.try_with(|c| c.set(c.get() + 1));
                unsafe { System.alloc_zeroed(l) }
            }
            unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
                let _ = COUNT.try_with(|c| c.set(c.get() + 1));
                unsafe { System.realloc(p, l, new) }
            }
            unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
                unsafe { System.dealloc(p, l) }
            }
        }

        #[global_allocator]
        static ALLOC: Counting = Counting;

        pub fn allocations() -> u64 {
            COUNT.with(|c| c.get())
        }
    }

    #[test]
    fn recording_and_rolling_never_allocate_after_setup() {
        // Codex r1: each window's first wait allocated the histogram buffer
        // under the tracker lock (wait_ready), and roll threw it away.
        use alloc_count::allocations;
        let mut r = ReleaseTiming::default();
        for window in 0..4 {
            let a0 = allocations();
            r.on_ready([Some(5_000 + window), None]);
            r.on_ready([None, Some(700)]);
            r.on_ready([None, None]);
            r.on_release(10_500, 10_000 - window);
            assert_eq!(allocations() - a0, 0, "window {window}: wait/release path");
            let a0 = allocations();
            let w = r.roll();
            assert_eq!(
                allocations() - a0,
                0,
                "window {window}: roll (tracker lock)"
            );
            assert_eq!((w.ready_calls, w.wait_us[0].n, w.wait_us[1].n), (3, 1, 1));
            assert_eq!((w.age_ms.n, w.age_ms.max), (1, 500 + window));
        }
        assert_eq!(r.totals()[1].1.n, 4);
        assert_eq!(r.totals()[1].1.max, 5_003);
    }

    #[test]
    fn window_and_hist_lines_are_stable() {
        let mut r = ReleaseTiming::default();
        for age in [400, 420, 900, 1_600] {
            r.on_release(10_000 + age, 10_000);
        }
        r.on_ready([None, None]);
        r.on_ready([Some(1_000), None]);
        r.on_ready([Some(5_000), Some(2_000)]);
        let mut t = TailTiming::default();
        t.on_lag(3);
        t.on_body(body(7, 1_000, 1_200, 0));
        t.on_body(body(8, 1_050, 3_400, 2_000));
        t.end_round(4_000);
        t.on_lag(1);
        t.on_body(body(9, 1_300, 90_000, 0));
        t.end_round(12_000);
        let lines = window_lines(15.04, 1_760_000_015_000, false, &r.roll(), &t.roll());
        assert_eq!(
            lines[0],
            "release-timing: t=15.0 wall_ms=1760000015000 final=0 released=4 \
             age_p50_ms=416 age_p90_ms=1600 age_p99_ms=1600 age_max_ms=1600 ready_calls=3 \
             wait_cap_n=2 wait_cap_s=0.006 wait_cap_p50_ms=1.0 wait_cap_p99_ms=4.9 \
             wait_cancel_n=1 wait_cancel_s=0.002 wait_cancel_p50_ms=2.0 wait_cancel_p99_ms=2.0 \
             tail_rounds=2 tail_fetched=3 tail_fetch_max=2 tail_round_p50_ms=4.0 \
             tail_round_max_ms=12.0 tail_lag_max=3 tail_fetch_p50_ms=3.3 \
             tail_fetch_p99_ms=86.0 tail_fetch_max_ms=90.0 tail_apply_wait_p50_ms=0.0 \
             tail_apply_wait_p99_ms=2.0 tail_apply_wait_max_ms=2.0"
        );
        // `seen` stays the last field (parsers of the s121 line read it to
        // the end of the line).
        assert_eq!(
            lines[1],
            "release-timing-blocks: t=15.0 fetch_ms=7:1.2,8:3.4,9:90.0 \
             applied=7@1000,8@1052,9@1300 seen=7@1000,8@1050,9@1300"
        );
        // An idle window: no samples.
        let idle = window_lines(30.0, 1_760_000_030_000, true, &r.roll(), &t.roll());
        assert_eq!(
            idle[0],
            "release-timing: t=30.0 wall_ms=1760000030000 final=1 released=0 \
             age_p50_ms=- age_p90_ms=- age_p99_ms=- age_max_ms=- ready_calls=0 \
             wait_cap_n=0 wait_cap_s=0.000 wait_cap_p50_ms=- wait_cap_p99_ms=- \
             wait_cancel_n=0 wait_cancel_s=0.000 wait_cancel_p50_ms=- wait_cancel_p99_ms=- \
             tail_rounds=0 tail_fetched=0 tail_fetch_max=- tail_round_p50_ms=- \
             tail_round_max_ms=- tail_lag_max=- tail_fetch_p50_ms=- tail_fetch_p99_ms=- \
             tail_fetch_max_ms=- tail_apply_wait_p50_ms=- tail_apply_wait_p99_ms=- \
             tail_apply_wait_max_ms=-"
        );
        assert_eq!(
            idle[1],
            "release-timing-blocks: t=30.0 fetch_ms= applied= seen="
        );
        let (what, age) = r.totals()[0];
        assert_eq!(
            hist_line(what, age),
            "release-timing-hist: what=age_ms n=4 sum=3320 p50=416 p90=1600 p99=1600 \
             p999=1600 max=1600 buckets=400:1,416:1,896:1,1600:1"
        );
        assert_eq!(
            hist_line("wait_cancel_us", &Hist::default()),
            "release-timing-hist: what=wait_cancel_us n=0 sum=0 p50=- p90=- p99=- p999=- \
             max=- buckets="
        );
    }
}
