//! Optional economic-load pacing. Pure elapsed-time planning; no catch-up debt.
use std::sync::Arc;
use std::time::Duration;

pub const MAX_PHASES: usize = 64;

#[derive(Clone, Debug)]
pub struct Phase {
    pub start: Duration,
    pub end: Duration,
    pub rate: f64,
}

#[derive(Debug)]
pub struct RateSchedule {
    pub phases: Vec<Phase>,
    pub duration: Duration,
}

impl RateSchedule {
    /// Absolute integer-second offsets: `0:1000,30:2000,60:0`.
    /// A scheduled zero is a pause; the legacy unscheduled zero is unbounded.
    pub fn parse(raw: &str, duration_secs: u64) -> Result<Self, String> {
        if duration_secs == 0 { return Err("scheduled duration must be positive".into()); }
        let duration = Duration::from_secs(duration_secs);
        let mut phases: Vec<Phase> = Vec::new();
        for item in raw.split(',') {
            if phases.len() >= MAX_PHASES { return Err("rate schedule exceeds 64 phases".into()); }
            let (start, rate) = item.split_once(':').ok_or("expected seconds:totalrate")?;
            if start.trim().is_empty() || !start.trim().bytes().all(|c| c.is_ascii_digit()) {
                return Err("phase offset must be nonnegative integer seconds".into());
            }
            let start = start.trim().parse::<u64>().map_err(|_| "phase offset must be nonnegative integer seconds")?;
            let rate = rate.trim().parse::<f64>().map_err(|_| "phase rate must be finite and nonnegative")?;
            if !rate.is_finite() || rate < 0.0 { return Err("phase rate must be finite and nonnegative".into()); }
            if start >= duration_secs { return Err("phase offset must be before duration".into()); }
            let start = Duration::from_secs(start);
            if let Some(previous) = phases.last_mut() {
                if start <= previous.start { return Err("phase offsets must strictly increase".into()); }
                previous.end = start;
            } else if !start.is_zero() { return Err("first phase must start at 0".into()); }
            phases.push(Phase { start, end: duration, rate });
        }
        Ok(Self { phases, duration })
    }
}

#[derive(Debug, PartialEq)]
pub enum Step { Wait(Duration), Ready(usize), Done }

pub struct Pacer {
    schedule: Arc<RateSchedule>,
    phase: Option<usize>,
    next_due: Duration,
    interval: Duration,
    fraction: f64,
    actions_per_round: f64,
}

impl Pacer {
    pub fn new(schedule: Arc<RateSchedule>, sender: usize, senders: usize, submit_batch: usize) -> Self {
        assert!(senders > 0 && sender < senders && submit_batch > 0);
        Self { schedule, phase: None, next_due: Duration::ZERO, interval: Duration::ZERO,
            fraction: (sender as f64 + 0.5) / senders as f64,
            actions_per_round: senders as f64 * submit_batch as f64 }
    }

    pub fn poll(&mut self, elapsed: Duration) -> Step {
        if elapsed >= self.schedule.duration { return Step::Done; }
        let index = self.schedule.phases.partition_point(|p| p.start <= elapsed) - 1;
        let phase = &self.schedule.phases[index];
        if self.phase != Some(index) {
            self.phase = Some(index);
            let seconds = self.actions_per_round / phase.rate;
            // Tiny rates need not construct an overflowing Duration: no next
            // action can be due before the cell ends. Large rates floor at 1ns.
            self.interval = if !seconds.is_finite() || seconds >= self.schedule.duration.as_secs_f64() {
                self.schedule.duration
            } else {
                Duration::try_from_secs_f64(seconds).unwrap_or(Duration::from_nanos(1)).max(Duration::from_nanos(1))
            };
            let delay_seconds = seconds * self.fraction;
            let delay = if !delay_seconds.is_finite() || delay_seconds >= self.schedule.duration.as_secs_f64() {
                self.schedule.duration
            } else {
                Duration::try_from_secs_f64(delay_seconds).unwrap_or(Duration::ZERO)
            };
            self.next_due = elapsed.checked_add(delay)
                .unwrap_or(self.schedule.duration);
        }
        if phase.rate == 0.0 { return Step::Wait(phase.end - elapsed); }
        if self.next_due <= elapsed { Step::Ready(index) }
        else { Step::Wait(self.next_due.min(phase.end) - elapsed) }
    }

    pub fn phase_end(&self, phase: usize) -> Duration { self.schedule.phases[phase].end }

    pub fn dispatched(&mut self, elapsed: Duration) {
        // Only actual submission advances pacing; missed intervals never accrue.
        self.next_due = elapsed.checked_add(self.interval).unwrap_or(self.schedule.duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seconds(n: u64) -> Duration { Duration::from_secs(n) }

    #[test]
    fn invalid_schedule_boundaries_are_rejected() {
        for raw in ["", "0", "1:2", "0:NaN", "0:inf", "0:-1", "-1:2", "NaN:2",
            "0:1,0:2", "0:1,3:2,2:3", "0:1,10:2", "0:1,", "0:1:2"] {
            assert!(RateSchedule::parse(raw, 10).is_err(), "{raw}");
        }
        assert!(RateSchedule::parse("0:1", 0).is_err());
        let phases = (0..MAX_PHASES).map(|s| format!("{s}:1")).collect::<Vec<_>>().join(",");
        assert!(RateSchedule::parse(&phases, 100).is_ok());
        assert!(RateSchedule::parse(&format!("{phases},64:1"), 100).is_err());
    }

    #[test]
    fn boundaries_override_slow_rate_waits_and_zero_pauses() {
        let plan = Arc::new(RateSchedule::parse("0:0.001,2:4,4:0,6:2", 8).unwrap());
        let mut p = Pacer::new(plan, 0, 1, 1);
        assert_eq!(p.poll(seconds(0)), Step::Wait(seconds(2)));
        assert_eq!(p.poll(seconds(2)), Step::Wait(Duration::from_millis(125)));
        assert_eq!(p.poll(Duration::from_millis(2125)), Step::Ready(1));
        assert_eq!(p.poll(seconds(4)), Step::Wait(seconds(2)));
        assert_eq!(p.poll(seconds(6)), Step::Wait(Duration::from_millis(250)));
        assert_eq!(p.poll(seconds(8)), Step::Done);
    }

    #[test]
    fn delayed_sender_skips_phases_and_never_catches_up() {
        let plan = Arc::new(RateSchedule::parse("0:10,2:100,3:1", 20).unwrap());
        let mut p = Pacer::new(plan, 0, 1, 1);
        p.poll(seconds(0));
        assert_eq!(p.poll(seconds(10)), Step::Wait(Duration::from_millis(500)));
        assert_eq!(p.poll(seconds(12)), Step::Ready(2));
        p.dispatched(seconds(12));
        assert_eq!(p.poll(seconds(12)), Step::Wait(seconds(1)));
    }

    #[test]
    fn batch_rate_and_sender_offsets_spread_one_round() {
        let plan = Arc::new(RateSchedule::parse("0:8", 10).unwrap());
        let mut first = Pacer::new(plan.clone(), 0, 2, 4);
        let mut second = Pacer::new(plan, 1, 2, 4);
        assert_eq!(first.poll(seconds(0)), Step::Wait(Duration::from_millis(250)));
        assert_eq!(second.poll(seconds(0)), Step::Wait(Duration::from_millis(750)));
        first.dispatched(seconds(1));
        assert_eq!(first.poll(seconds(1)), Step::Wait(seconds(1)));
    }

    #[test]
    fn finite_extreme_rates_are_bounded_without_panics() {
        let plan = Arc::new(RateSchedule::parse("0:1e-300,2:1e300,3:0", 5).unwrap());
        let mut p = Pacer::new(plan, 0, 1, 1);
        assert_eq!(p.poll(seconds(0)), Step::Wait(seconds(2)));
        assert_eq!(p.poll(seconds(2)), Step::Ready(1));
        p.dispatched(seconds(2));
        assert_eq!(p.poll(seconds(2)), Step::Wait(Duration::from_nanos(1)));
        // A permit or signing completion from phase 1 must not survive its boundary.
        assert_eq!(p.poll(seconds(3)), Step::Wait(seconds(2)));
        assert_eq!(p.poll(seconds(5)), Step::Done);
    }
}
