//! `TORUS_CORE_BUDGET` — how many cores THIS process may size its thread pools
//! for (P1 pool-bounding knob).
//!
//! Every pool in the node (tokio runtime, global rayon, match workers, ingress
//! and gossip verify pools, RocksDB background jobs) used to size itself from
//! [`std::thread::available_parallelism`], i.e. as if the process owned the
//! whole box. On a shared rig (3 validators + a load generator on one 18-core
//! host) that oversubscribes ~4x and the resulting preemption inflates the
//! in-vivo exec phases well past their micro-bench cost.
//!
//! Semantics (node-local, NEVER consensus-visible — thread counts change only
//! wall-clock, every path is order-preserving / byte-identical):
//!
//! * unset / `0` / garbage  -> host parallelism (exact-today behaviour);
//! * `N >= 1`               -> pools default as if the host had `N` cores.
//!
//! Per-pool env knobs (`TORUS_TOKIO_WORKERS`, `TORUS_MATCH_WORKERS`,
//! `TORUS_MAX_BG_JOBS`, `TORUS_INGRESS_VERIFY_THREADS`,
//! `TORUS_GOSSIP_VERIFY_THREADS`, `RAYON_NUM_THREADS`) still win over the
//! budget-derived default. Read once per process.

use std::sync::OnceLock;

/// Env var name for the umbrella budget.
pub const CORE_BUDGET_ENV: &str = "TORUS_CORE_BUDGET";

/// Pure parse of a `TORUS_CORE_BUDGET` value: `Some(n)` for an integer
/// `n >= 1`, `None` for unset / `0` / garbage (= "no budget, use the host").
pub fn parse_core_budget(raw: Option<&str>) -> Option<usize> {
    raw.map(str::trim)
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n >= 1)
}

/// Pure resolution: budget if set, else host parallelism, else `fallback`.
pub fn resolve_core_budget(raw: Option<&str>, host: Option<usize>, fallback: usize) -> usize {
    parse_core_budget(raw).or(host).unwrap_or(fallback).max(1)
}

/// The configured budget (`Some(n)`), or `None` when the process should size
/// pools from the host. Cached for the process lifetime.
pub fn configured_core_budget() -> Option<usize> {
    static BUDGET: OnceLock<Option<usize>> = OnceLock::new();
    *BUDGET.get_or_init(|| parse_core_budget(std::env::var(CORE_BUDGET_ENV).ok().as_deref()))
}

/// Cores this process should size pools for: `TORUS_CORE_BUDGET` if set, else
/// [`std::thread::available_parallelism`], else `fallback` (each call site keeps
/// the fallback it used before the budget existed, so unset stays exact-today).
pub fn core_budget_or(fallback: usize) -> usize {
    configured_core_budget()
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(fallback)
        .max(1)
}

/// Pure parse of a per-pool override: `Some(n)` for `n >= 1`, else `None`.
pub fn parse_pool_override(raw: Option<&str>) -> Option<usize> {
    parse_core_budget(raw)
}

/// Thread count for one pool: the per-pool env override (`>= 1`) if present,
/// else `default`. Not cached (call sites cache their pools).
pub fn pool_threads(env_name: &str, default: usize) -> usize {
    parse_pool_override(std::env::var(env_name).ok().as_deref()).unwrap_or(default.max(1))
}

/// "Half the cores, min 2" — the sizing rule the ingress / gossip verify pools
/// have always used, now applied to the budget instead of the raw host count.
pub fn half_cores_min2(cores: usize) -> usize {
    (cores / 2).max(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_core_budget_accepts_positive_integers_only() {
        assert_eq!(parse_core_budget(Some("5")), Some(5));
        assert_eq!(parse_core_budget(Some(" 12 ")), Some(12));
        assert_eq!(parse_core_budget(Some("1")), Some(1));
        assert_eq!(parse_core_budget(Some("0")), None);
        assert_eq!(parse_core_budget(Some("-3")), None);
        assert_eq!(parse_core_budget(Some("five")), None);
        assert_eq!(parse_core_budget(Some("")), None);
        assert_eq!(parse_core_budget(None), None);
    }

    #[test]
    fn resolve_prefers_budget_then_host_then_fallback() {
        assert_eq!(resolve_core_budget(Some("5"), Some(18), 8), 5);
        assert_eq!(resolve_core_budget(None, Some(18), 8), 18);
        assert_eq!(resolve_core_budget(Some("0"), Some(18), 8), 18);
        assert_eq!(resolve_core_budget(Some("junk"), None, 8), 8);
        assert_eq!(resolve_core_budget(None, None, 0), 1);
    }

    #[test]
    fn half_cores_rule_matches_legacy_pool_sizing() {
        assert_eq!(half_cores_min2(18), 9); // 18-core box, unbounded: 9 (as before)
        assert_eq!(half_cores_min2(8), 4); // legacy unwrap_or(8) fallback: 4
        assert_eq!(half_cores_min2(5), 2); // budget 5 -> 2
        assert_eq!(half_cores_min2(1), 2);
        assert_eq!(half_cores_min2(0), 2);
    }

    #[test]
    fn pool_override_parse() {
        assert_eq!(parse_pool_override(Some("3")), Some(3));
        assert_eq!(parse_pool_override(Some("0")), None);
        assert_eq!(parse_pool_override(None), None);
    }

    #[test]
    fn core_budget_or_never_returns_zero() {
        // Whatever the env / host says, a pool of zero threads is never sized.
        assert!(core_budget_or(0) >= 1);
        assert!(pool_threads("TORUS_TEST_POOL_THREADS_UNSET_XYZ", 0) >= 1);
        assert_eq!(pool_threads("TORUS_TEST_POOL_THREADS_UNSET_XYZ", 7), 7);
    }
}
