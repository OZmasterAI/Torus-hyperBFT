//! Per-address request limit at RPC native ingress (anti-spam item B).
//!
//! Hyperliquid semantics, node-local and in memory:
//! - allowance = `buffer` (default 10_000) + 1 request per 1 TRS of the
//!   address's cumulative traded volume (`PositionManager::get_cum_volume`);
//! - an action counts as `order_count` requests (a batch of n orders = n);
//!   every other action, cancels included, counts 1;
//! - cancels have their own, larger cumulative allowance
//!   `min(allowance + 100_000, 2 * allowance)`, so a limited address can keep
//!   cancelling for a while;
//! - once exhausted: one weight-1 action (a single order, a cancel, any other
//!   single action) per 10 s, counted from the address's last admitted
//!   action; a batch of n > 1 orders is n requests and is refused.
//!
//! Counters live in this node's memory since it started: they reset on
//! restart and are per node (a client spreading load over N nodes gets up to
//! N allowances — still bounded, and each node protects itself).
//!
//! The volume is read from the DB, which can lag execution by a block or
//! more; the allowance is recomputed from whatever is read each time and the
//! used counter never decreases, so a stale (even regressed) volume only
//! makes the limit briefly stricter. The volume is read only when the buffer
//! alone does not cover the request, so most requests cost no DB read.
//!
//! Memory is bounded by `max_addresses` ([`crate::bounded_map::TwoGen`]):
//! an address idle for a whole generation is forgotten (it comes back with
//! a fresh buffer — the same as a new funded key, which item A prices at
//! 1 TRS). 200_000 addresses at ~100 B per entry is ~20 MB.

use std::collections::HashSet;
use std::sync::Mutex;

use alloy_primitives::Address;

use crate::bounded_map::TwoGen;

/// Default free requests per address (HL: 10_000).
pub const DEFAULT_BUFFER: u64 = 10_000;
/// Extra cumulative allowance for cancels (HL: +100_000, at most 2x).
pub const CANCEL_EXTRA: u64 = 100_000;
/// One action per this many ms once an address is exhausted (HL: 10 s).
pub const EXHAUSTED_INTERVAL_MS: u64 = 10_000;
/// Default bound on tracked addresses.
pub const DEFAULT_MAX_ADDRESSES: usize = 200_000;

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Non-cancel over the allowance.
    Requests { used: u64, allowance: u64 },
    /// Cancel over the cancel allowance.
    Cancels { used: u64, allowance: u64 },
}

impl Refusal {
    /// Metric reason label.
    pub fn reason(&self) -> &'static str {
        match self {
            Refusal::Requests { .. } => "addr_rate_limited",
            Refusal::Cancels { .. } => "addr_cancel_rate_limited",
        }
    }

    /// Per-item reply text.
    pub fn message(&self, sender: &Address) -> String {
        let (kind, used, allowance) = match self {
            Refusal::Requests { used, allowance } => ("requests", used, allowance),
            Refusal::Cancels { used, allowance } => ("cancel requests", used, allowance),
        };
        format!(
            "rate limited: {sender} used {used} of {allowance} {kind} (buffer + 1 per TRS traded); \
             1 single action per 10 s until traded volume grows"
        )
    }
}

#[derive(Clone, Copy, Default)]
struct Entry {
    /// Requests charged since node start (never decreases).
    used: u64,
    /// When (ms) this address last had an action admitted.
    last_ok_ms: u64,
}

/// The per-address limiter. Cheap to share behind an `Arc`.
pub struct AddrRateLimiter {
    buffer: u64,
    exempt: HashSet<Address>,
    entries: Mutex<TwoGen<Address, Entry>>,
}

impl AddrRateLimiter {
    pub fn new(buffer: u64, exempt: HashSet<Address>, max_addresses: usize) -> Self {
        Self {
            buffer,
            exempt,
            entries: Mutex::new(TwoGen::new(max_addresses)),
        }
    }

    /// Charge `weight` requests to `sender` at `now_ms`. `cum_volume_trs` is
    /// called (at most once) only when the buffer alone does not cover the
    /// request.
    pub fn check(
        &self,
        sender: &Address,
        is_cancel: bool,
        weight: u64,
        now_ms: u64,
        cum_volume_trs: impl FnOnce() -> u64,
    ) -> Result<(), Refusal> {
        if self.exempt.contains(sender) {
            return Ok(());
        }
        let cap = |allowance: u64| {
            if is_cancel {
                allowance
                    .saturating_add(CANCEL_EXTRA)
                    .min(allowance.saturating_mul(2))
            } else {
                allowance
            }
        };
        // Read the volume (a DB point read) only when the buffer alone does
        // not cover the request, and never under the lock. Two concurrent
        // requests of one address can both pass on the same snapshot — a
        // bounded overshoot, fine for an anti-spam rule.
        let used = self
            .entries
            .lock()
            .unwrap()
            .get(sender)
            .map_or(0, |e| e.used);
        let allowance = if used.saturating_add(weight) <= cap(self.buffer) {
            self.buffer
        } else {
            self.buffer.saturating_add(cum_volume_trs())
        };
        let limit = cap(allowance);

        let mut entries = self.entries.lock().unwrap();
        let e = entries.entry_with(*sender, Entry::default);
        let within = e.used.saturating_add(weight) <= limit;
        // Exhausted: one weight-1 action per EXHAUSTED_INTERVAL_MS since the
        // last admitted one (HL: a batch of n is n requests, so it never
        // fits the 1 request the slow mode grants).
        if within || (weight <= 1 && now_ms.saturating_sub(e.last_ok_ms) >= EXHAUSTED_INTERVAL_MS) {
            e.used = e.used.saturating_add(weight);
            e.last_ok_ms = now_ms;
            return Ok(());
        }
        Err(if is_cancel {
            Refusal::Cancels {
                used: e.used,
                allowance: limit,
            }
        } else {
            Refusal::Requests {
                used: e.used,
                allowance: limit,
            }
        })
    }

    /// Number of tracked addresses (both generations).
    pub fn tracked(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

/// Parse `TORUS_ADDR_RATE_LIMIT`: only `"0"` disables (default ON in the node).
pub fn parse_enabled(raw: Option<String>) -> bool {
    !matches!(raw.as_deref().map(str::trim), Some("0"))
}

/// Parse `TORUS_ADDR_RATE_BUFFER` (default [`DEFAULT_BUFFER`]).
pub fn parse_buffer(raw: Option<String>) -> u64 {
    raw.and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_BUFFER)
}

/// Parse `TORUS_ADDR_RATE_EXEMPT`: comma-separated 0x addresses. Invalid
/// entries are returned separately so the node can log them.
pub fn parse_exempt(raw: Option<String>) -> (HashSet<Address>, Vec<String>) {
    let mut ok = HashSet::new();
    let mut bad = Vec::new();
    for item in raw.unwrap_or_default().split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        match item.parse::<Address>() {
            Ok(a) => {
                ok.insert(a);
            }
            Err(_) => bad.push(item.to_string()),
        }
    }
    (ok, bad)
}

/// Build the node's limiter from the env, or `None` when
/// `TORUS_ADDR_RATE_LIMIT=0`. Returns the invalid exempt entries for logging.
pub fn from_env() -> (Option<AddrRateLimiter>, Vec<String>) {
    if !parse_enabled(std::env::var("TORUS_ADDR_RATE_LIMIT").ok()) {
        return (None, Vec::new());
    }
    let buffer = parse_buffer(std::env::var("TORUS_ADDR_RATE_BUFFER").ok());
    let (exempt, bad) = parse_exempt(std::env::var("TORUS_ADDR_RATE_EXEMPT").ok());
    (
        Some(AddrRateLimiter::new(buffer, exempt, DEFAULT_MAX_ADDRESSES)),
        bad,
    )
}

impl AddrRateLimiter {
    pub fn buffer(&self) -> u64 {
        self.buffer
    }
    pub fn exempt_count(&self) -> usize {
        self.exempt.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(i: u8) -> Address {
        Address::repeat_byte(i)
    }

    #[test]
    fn buffer_then_refused_and_cancels_get_their_own_allowance() {
        let l = AddrRateLimiter::new(10, HashSet::new(), 100);
        for _ in 0..10 {
            l.check(&a(1), false, 1, 0, || 0).unwrap();
        }
        assert_eq!(
            l.check(&a(1), false, 1, 0, || 0),
            Err(Refusal::Requests {
                used: 10,
                allowance: 10
            })
        );
        // Cancels: min(10 + 100_000, 2 * 10) = 20 cumulative.
        for _ in 0..10 {
            l.check(&a(1), true, 1, 0, || 0).unwrap();
        }
        assert_eq!(
            l.check(&a(1), true, 1, 0, || 0),
            Err(Refusal::Cancels {
                used: 20,
                allowance: 20
            })
        );
        // Another address is unaffected.
        l.check(&a(2), false, 1, 0, || 0).unwrap();
    }

    #[test]
    fn cancel_allowance_is_capped_at_plus_100k() {
        let l = AddrRateLimiter::new(DEFAULT_BUFFER, HashSet::new(), 100);
        // allowance 10_000 + 200_000 volume = 210_000; cancels min(310_000, 420_000).
        l.check(&a(1), true, 309_999, 0, || 200_000).unwrap();
        l.check(&a(1), true, 1, 0, || 200_000).unwrap();
        assert!(matches!(
            l.check(&a(1), true, 1, 0, || 200_000),
            Err(Refusal::Cancels {
                allowance: 310_000,
                ..
            })
        ));
    }

    #[test]
    fn batch_counts_its_orders() {
        let l = AddrRateLimiter::new(1_000, HashSet::new(), 100);
        l.check(&a(1), false, 400, 0, || 0).unwrap();
        l.check(&a(1), false, 400, 0, || 0).unwrap();
        // 800 used: a third 400-order batch does not fit 1_000.
        assert_eq!(
            l.check(&a(1), false, 400, 0, || 0),
            Err(Refusal::Requests {
                used: 800,
                allowance: 1_000
            })
        );
        l.check(&a(1), false, 200, 0, || 0).unwrap();
    }

    #[test]
    fn volume_raises_allowance_and_stale_volume_is_tolerated() {
        let l = AddrRateLimiter::new(10, HashSet::new(), 100);
        for _ in 0..10 {
            l.check(&a(1), false, 1, 0, || 5).unwrap();
        }
        // 10 used of 15.
        for _ in 0..5 {
            l.check(&a(1), false, 1, 0, || 5).unwrap();
        }
        assert!(l.check(&a(1), false, 1, 0, || 5).is_err());
        // A stale/regressed volume read just makes the limit stricter.
        assert!(l.check(&a(1), false, 1, 0, || 0).is_err());
        // The volume catches up: allowance grows again.
        l.check(&a(1), false, 1, 0, || 100).unwrap();
    }

    #[test]
    fn volume_is_read_only_past_the_buffer() {
        let l = AddrRateLimiter::new(3, HashSet::new(), 100);
        let reads = std::cell::Cell::new(0);
        let vol = || {
            reads.set(reads.get() + 1);
            10
        };
        for _ in 0..3 {
            l.check(&a(1), false, 1, 0, vol).unwrap();
        }
        assert_eq!(reads.get(), 0, "within the buffer: no DB read");
        l.check(&a(1), false, 1, 0, vol).unwrap();
        assert_eq!(reads.get(), 1);
    }

    #[test]
    fn exhausted_address_gets_one_action_per_10s() {
        let l = AddrRateLimiter::new(1, HashSet::new(), 100);
        l.check(&a(1), false, 1, 1_000, || 0).unwrap();
        // Exhausted: next action only 10 s after the last admitted one.
        assert!(l.check(&a(1), false, 1, 1_000, || 0).is_err());
        // Cancel allowance min(1 + 100_000, 2) = 2 still has one left.
        l.check(&a(1), true, 1, 5_000, || 0).unwrap();
        assert!(l.check(&a(1), true, 1, 6_000, || 0).is_err());
        assert!(l.check(&a(1), false, 1, 14_999, || 0).is_err());
        // Slow mode admits a weight-1 action once per 10 s.
        l.check(&a(1), false, 1, 15_000, || 0).unwrap();
        assert!(l.check(&a(1), false, 1, 24_999, || 0).is_err());
        l.check(&a(1), true, 1, 25_000, || 0).unwrap();
    }

    #[test]
    fn exhausted_mode_admits_only_weight_one_actions() {
        // HL: a batch of n orders is n requests, so once exhausted only a
        // single order / cancel / other single action passes, 1 per 10 s.
        let l = AddrRateLimiter::new(1, HashSet::new(), 100);
        l.check(&a(1), false, 1, 0, || 0).unwrap();
        // Long past the interval, a 400-order batch is still refused.
        assert_eq!(
            l.check(&a(1), false, 400, 20_000, || 0),
            Err(Refusal::Requests {
                used: 1,
                allowance: 1
            })
        );
        assert!(l.check(&a(1), false, 2, 20_000, || 0).is_err());
        // A single order passes 10 s after the last admitted action ...
        l.check(&a(1), false, 1, 20_000, || 0).unwrap();
        // ... and a second one within 10 s is refused.
        assert!(l.check(&a(1), false, 1, 25_000, || 0).is_err());
        l.check(&a(1), false, 1, 30_000, || 0).unwrap();
    }

    #[test]
    fn exhausted_mode_keeps_the_cancel_allowance() {
        // Requests exhausted, cancels min(10 + 100_000, 2 * 10) = 20 untouched.
        let l = AddrRateLimiter::new(10, HashSet::new(), 100);
        l.check(&a(1), false, 10, 0, || 0).unwrap();
        assert!(l.check(&a(1), false, 1, 0, || 0).is_err());
        for _ in 0..10 {
            l.check(&a(1), true, 1, 0, || 0).unwrap();
        }
        assert!(l.check(&a(1), true, 1, 0, || 0).is_err());
        // Exhausted cancels: one per 10 s.
        l.check(&a(1), true, 1, 10_000, || 0).unwrap();
        assert!(l.check(&a(1), true, 1, 19_999, || 0).is_err());
    }

    #[test]
    fn exempt_addresses_are_never_limited() {
        let l = AddrRateLimiter::new(1, [a(9)].into_iter().collect(), 100);
        for _ in 0..1_000 {
            l.check(&a(9), false, 400, 0, || 0).unwrap();
        }
        assert_eq!(l.tracked(), 0, "exempt addresses are not tracked");
    }

    #[test]
    fn memory_is_bounded_and_active_addresses_keep_their_count() {
        let l = AddrRateLimiter::new(5, HashSet::new(), 8);
        for _ in 0..5 {
            l.check(&a(1), false, 1, 0, || 0).unwrap();
        }
        for i in 2..=200u8 {
            l.check(&a(i), false, 1, 0, || 0).unwrap();
            // a(1) stays active: still exhausted.
            assert!(l.check(&a(1), false, 1, 0, || 0).is_err());
            assert!(l.tracked() <= 8, "bound held at {i}: {}", l.tracked());
        }
    }

    #[test]
    fn env_parsers() {
        assert!(parse_enabled(None));
        assert!(parse_enabled(Some("1".into())));
        assert!(!parse_enabled(Some(" 0 ".into())));
        assert_eq!(parse_buffer(None), 10_000);
        assert_eq!(parse_buffer(Some("50".into())), 50);
        assert_eq!(parse_buffer(Some("x".into())), 10_000);
        let (ok, bad) = parse_exempt(Some(format!(" {}, nope ,,{}", a(1), a(2))));
        assert_eq!(ok, [a(1), a(2)].into_iter().collect());
        assert_eq!(bad, vec!["nope".to_string()]);
        assert!(parse_exempt(None).0.is_empty());
    }
}
