//! Optional scheduled-load accounting, grouped by HTTP-start phase, not ACK time.
//! One ticket per request (never per order); queued tasks count before first poll.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Default, serde::Serialize)]
pub struct Counts {
    pub queued_requests: u64,
    pub http_started_requests: u64,
    pub http_started_actions: u64,
    pub completed_requests: u64,
    pub completed_actions: u64,
    pub acked_actions: u64,
    pub response_error_requests: u64,
    pub response_error_actions: u64,
    pub skipped_expired_requests: u64,
    pub skipped_expired_actions: u64,
    pub abandoned_requests: u64,
    pub outstanding_requests: u64,
}

pub struct Accounting { phases: Vec<Mutex<Counts>> }

impl Accounting {
    pub fn new(phases: usize) -> Arc<Self> {
        assert!(phases > 0 && phases <= crate::rate_schedule::MAX_PHASES);
        Arc::new(Self { phases: (0..phases).map(|_| Mutex::new(Counts::default())).collect() })
    }

    pub fn queue(self: &Arc<Self>, phase: usize, actions: usize) -> Ticket {
        let mut c = self.phases[phase].lock().unwrap();
        c.queued_requests += 1;
        c.outstanding_requests += 1;
        Ticket { accounting: self.clone(), phase, actions: actions as u64, finished: false }
    }

    pub fn snapshot(&self) -> Vec<Counts> {
        self.phases.iter().map(|p| p.lock().unwrap().clone()).collect()
    }

    // Called only after all senders have stopped queueing. Timeout never grants
    // completeness: the final snapshots retain outstanding/abandoned requests.
    pub async fn wait(&self, budget: Duration) {
        let deadline = Instant::now() + budget;
        while self.snapshot().iter().any(|c| c.outstanding_requests != 0) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { break; }
            tokio::time::sleep(remaining.min(Duration::from_millis(20))).await;
        }
    }
}

pub struct Ticket { accounting: Arc<Accounting>, phase: usize, actions: u64, finished: bool }

impl Ticket {
    /// Invoked inside the first-poll deadline check immediately before HTTP.
    pub fn start(&mut self) {
        let mut c = self.accounting.phases[self.phase].lock().unwrap();
        c.http_started_requests += 1;
        c.http_started_actions += self.actions;
    }

    pub fn finish(&mut self, accepted: Option<usize>) {
        let mut c = self.accounting.phases[self.phase].lock().unwrap();
        c.completed_requests += 1;
        c.completed_actions += self.actions;
        match accepted {
            Some(n) => c.acked_actions += n as u64,
            None => { c.response_error_requests += 1; c.response_error_actions += self.actions; }
        }
        c.outstanding_requests -= 1;
        self.finished = true;
    }

    pub fn skip(&mut self) {
        let mut c = self.accounting.phases[self.phase].lock().unwrap();
        c.skipped_expired_requests += 1;
        c.skipped_expired_actions += self.actions;
        c.outstanding_requests -= 1;
        self.finished = true;
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.finished {
            let mut c = self.accounting.phases[self.phase].lock().unwrap();
            c.abandoned_requests += 1;
            c.outstanding_requests -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_cohorts_distinguish_ack_errors_skips_and_abandonment() {
        let a = Accounting::new(2);
        let mut first = a.queue(0, 4);
        first.start();
        let mut second = a.queue(1, 2);
        second.start(); second.finish(None);
        first.finish(Some(3)); // ACK after phase 1 still belongs to phase 0.
        a.queue(1, 5).skip();
        drop(a.queue(0, 1));
        let c = a.snapshot();
        assert_eq!(c[0].acked_actions, 3);
        assert_eq!(c[0].http_started_actions, 4);
        assert_eq!(c[0].abandoned_requests, 1);
        assert_eq!(c[1].response_error_actions, 2);
        assert_eq!(c[1].skipped_expired_actions, 5);
        assert_eq!(c[1].http_started_actions, 2);
        assert!(c.iter().all(|c| c.outstanding_requests == 0));
    }

    #[tokio::test]
    async fn bounded_wait_does_not_hide_unpolled_tasks() {
        let a = Accounting::new(1);
        let ticket = a.queue(0, 3);
        a.wait(Duration::ZERO).await;
        assert_eq!(a.snapshot()[0].outstanding_requests, 1);
        drop(ticket);
        a.wait(Duration::ZERO).await;
        assert_eq!(a.snapshot()[0].abandoned_requests, 1);
    }
}
