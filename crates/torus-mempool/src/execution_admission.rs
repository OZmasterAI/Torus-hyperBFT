//! Local native admission pressure, spanning the pool and committed execution.
//! Work units are a proxy: one per action plus one per placement order.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use alloy_primitives::B256;
use torus_types::NativeAction;

pub fn native_work(action: &NativeAction) -> u64 {
    1 + match action {
        NativeAction::PlaceOrder(_) => 1,
        NativeAction::PlaceOrderBatch(orders) => orders.len() as u64,
        _ => 0,
    }
}

/// An admission budget, not a promise of end-to-end latency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionAdmissionSnapshot {
    pub outstanding_work: u64,
    pub committed_work: u64,
    pub limit_work: u64,
}

struct BlockWork {
    hashes: Vec<B256>,
    units: u64,
    exact: bool,
}

pub(crate) struct ExecutionAdmission {
    horizon: Duration,
    bootstrap: u64,
    minimum: u64,
    pool: HashMap<B256, (u64, u64)>, // units per entry, entry count
    pool_work: u64,
    overlap: u64,
    committed_hashes: HashMap<B256, u64>,
    blocks: BTreeMap<u64, BlockWork>,
    committed_work: u64,
    completed_height: Option<u64>,
    samples: VecDeque<(Instant, u64, Duration)>,
}

impl ExecutionAdmission {
    pub(crate) fn new(horizon_ms: u64, bootstrap: u64, minimum: u64) -> Self {
        Self {
            horizon: Duration::from_millis(horizon_ms),
            bootstrap: bootstrap.max(minimum),
            minimum,
            pool: HashMap::new(),
            pool_work: 0,
            overlap: 0,
            committed_hashes: HashMap::new(),
            blocks: BTreeMap::new(),
            committed_work: 0,
            completed_height: None,
            samples: VecDeque::new(),
        }
    }

    pub(crate) fn pool_insert(&mut self, hash: B256, units: u64) {
        let entry = self.pool.entry(hash).or_insert((units, 0));
        entry.1 += 1;
        self.pool_work = self.pool_work.saturating_add(units);
        if self.committed_hashes.contains_key(&hash) {
            self.overlap = self.overlap.saturating_add(units);
        }
    }

    pub(crate) fn pool_remove(&mut self, hash: &B256) {
        if let Some((units, count)) = self.pool.get_mut(hash) {
            self.pool_work = self.pool_work.saturating_sub(*units);
            if self.committed_hashes.contains_key(hash) {
                self.overlap = self.overlap.saturating_sub(*units);
            }
            *count -= 1;
            if *count == 0 {
                self.pool.remove(hash);
            }
        }
    }

    fn remove_block(&mut self, height: u64) -> Option<BlockWork> {
        let block = self.blocks.remove(&height)?;
        self.committed_work = self.committed_work.saturating_sub(block.units);
        for hash in &block.hashes {
            let refs = self
                .committed_hashes
                .get_mut(hash)
                .expect("registered hash");
            *refs -= 1;
            if *refs == 0 {
                self.committed_hashes.remove(hash);
                if let Some((units, count)) = self.pool.get(hash) {
                    self.overlap = self.overlap.saturating_sub(units.saturating_mul(*count));
                }
            }
        }
        Some(block)
    }

    /// Duplicate announcements/retries are free; a materialized body refines
    /// the conservative estimate once. Execution completes in height order.
    pub(crate) fn register(&mut self, height: u64, hashes: Vec<B256>, units: u64, exact: bool) {
        if !self.registration_needed(height, exact) {
            return;
        }
        self.remove_block(height);
        self.committed_work = self.committed_work.saturating_add(units);
        for hash in &hashes {
            let refs = self.committed_hashes.entry(*hash).or_default();
            if *refs == 0 {
                if let Some((units, count)) = self.pool.get(hash) {
                    self.overlap = self.overlap.saturating_add(units.saturating_mul(*count));
                }
            }
            *refs += 1;
        }
        self.blocks.insert(
            height,
            BlockWork {
                hashes,
                units,
                exact,
            },
        );
    }

    /// Avoid rehashing retries, while allowing a recovered body to replace an
    /// estimate even when it is still parked behind an earlier height.
    pub(crate) fn registration_needed(&self, height: u64, exact: bool) -> bool {
        if self.completed_height.is_some_and(|done| height <= done) {
            return false;
        }
        self.blocks
            .get(&height)
            .is_none_or(|block| exact && !block.exact)
    }

    pub(crate) fn hashes(&self, height: u64) -> Option<Vec<B256>> {
        self.blocks.get(&height).map(|b| b.hashes.clone())
    }

    /// `None` for already-applied replay: release pressure without learning an
    /// artificial service rate from a skipped block.
    pub(crate) fn complete(&mut self, height: u64, service: Option<Duration>, now: Instant) {
        let block = self.remove_block(height);
        self.completed_height = Some(self.completed_height.map_or(height, |h| h.max(height)));
        if let (Some(block), Some(service)) = (block, service) {
            if !service.is_zero() {
                self.samples.push_back((now, block.units, service));
            }
        }
        while self
            .samples
            .front()
            .is_some_and(|(at, _, _)| now.saturating_duration_since(*at) > Duration::from_secs(10))
        {
            self.samples.pop_front();
        }
    }

    pub(crate) fn snapshot(&self, now: Instant) -> ExecutionAdmissionSnapshot {
        let (work, busy_ns) = self
            .samples
            .iter()
            .filter(|(at, _, _)| now.saturating_duration_since(*at) <= Duration::from_secs(10))
            .fold((0u128, 0u128), |(work, busy), (_, units, service)| {
                (
                    work.saturating_add(*units as u128),
                    busy.saturating_add(service.as_nanos()),
                )
            });
        let limit = if busy_ns == 0 {
            self.bootstrap
        } else {
            (work.saturating_mul(self.horizon.as_nanos()) / busy_ns).min(u64::MAX as u128) as u64
        };
        ExecutionAdmissionSnapshot {
            outstanding_work: self
                .committed_work
                .saturating_add(self.pool_work.saturating_sub(self.overlap)),
            committed_work: self.committed_work,
            limit_work: limit.max(self.minimum),
        }
    }

    pub(crate) fn rejects(&self, hash: &B256, units: u64, now: Instant) -> bool {
        let extra = if self.committed_hashes.contains_key(hash) {
            0
        } else {
            units
        };
        let snapshot = self.snapshot(now);
        snapshot.outstanding_work.saturating_add(extra) > snapshot.limit_work
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(n: u8) -> B256 {
        B256::repeat_byte(n)
    }

    #[test]
    fn commit_transition_does_not_double_count_and_retries_are_idempotent() {
        let now = Instant::now();
        let mut admission = ExecutionAdmission::new(2000, 100, 1);
        admission.pool_insert(hash(1), 41);
        admission.pool_insert(hash(1), 41); // same signed bytes under a different claimed sender
        admission.pool_insert(hash(2), 2);
        admission.register(7, vec![hash(1)], 41, true);
        assert_eq!(admission.snapshot(now).outstanding_work, 43);
        admission.register(7, vec![hash(1)], 41, true);
        admission.pool_remove(&hash(1));
        admission.pool_remove(&hash(1));
        assert_eq!(admission.snapshot(now).outstanding_work, 43);
        admission.complete(7, Some(Duration::from_secs(1)), now);
        assert_eq!(admission.snapshot(now).outstanding_work, 2);
        assert_eq!(admission.snapshot(now).limit_work, 82);
        admission.register(7, vec![hash(1)], 41, true);
        admission.complete(7, Some(Duration::from_nanos(1)), now);
        assert_eq!(admission.snapshot(now).limit_work, 82);
    }

    #[test]
    fn missing_body_is_conservative_until_materialized_and_replay_does_not_train() {
        let now = Instant::now();
        let mut admission = ExecutionAdmission::new(2000, 100, 10);
        admission.pool_insert(hash(1), 21);
        admission.register(1, vec![hash(1)], 1025, false);
        assert_eq!(admission.snapshot(now).outstanding_work, 1025);
        assert!(admission.rejects(&hash(2), 1, now));
        admission.register(1, vec![hash(1)], 21, true);
        admission.register(1, vec![], 1000, false);
        assert_eq!(admission.snapshot(now).outstanding_work, 21);
        admission.pool_remove(&hash(1));
        admission.complete(1, None, now);
        assert_eq!(admission.snapshot(now).limit_work, 100);
        assert_eq!(admission.snapshot(now).outstanding_work, 0);
    }

    #[test]
    fn service_rate_uses_busy_time_and_expires_to_bootstrap() {
        let now = Instant::now();
        let mut admission = ExecutionAdmission::new(2000, 100, 10);
        admission.register(1, vec![], 80, true);
        admission.complete(1, Some(Duration::from_secs(2)), now);
        assert_eq!(admission.snapshot(now).limit_work, 80);
        admission.register(2, vec![], 20, true);
        admission.complete(
            2,
            Some(Duration::from_secs(2)),
            now + Duration::from_secs(5),
        );
        assert_eq!(
            admission.snapshot(now + Duration::from_secs(5)).limit_work,
            50
        );
        assert_eq!(
            admission.snapshot(now + Duration::from_secs(16)).limit_work,
            100
        );
        admission.register(3, vec![], 1, true);
        admission.complete(
            3,
            Some(Duration::from_secs(100)),
            now + Duration::from_secs(16),
        );
        assert_eq!(
            admission.snapshot(now + Duration::from_secs(16)).limit_work,
            10
        );
    }
}
