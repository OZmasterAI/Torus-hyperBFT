//! s80 PROTOTYPE `exec-commit-units` (`TORUS_S80_EXEC_JOIN=K`, default off):
//! the execution thread runs up to `K` consecutive committed blocks as ONE
//! execution unit (one verify, one sort, one oracle step, one engine pass,
//! one tail, one save / end_resident / flush / hash) at the unit's LAST
//! height. A measurement of the per-block fixed-cost amortisation, NOT
//! production wiring: the unit rule is a pure function of height and block
//! content, so all nodes agree, but the state transition differs from
//! per-block execution (within a unit, the block sort hoists cancels above
//! earlier GTCs; the oracle / liquidation / fee steps run once per unit).
//!
//! Units are aligned inside each 100-block segment `[100m+1, 100m+100]`
//! (the checkpoint / attest heights `h % 100 == 0` always END a unit):
//! `[100m+1, 100m+K]`, `[100m+K+1, 100m+2K]`, ..., the last one truncated at
//! `100m+100`.
//!
//! With `K = 1` (the default) every helper here is exactly the per-block
//! rule it replaces (`prev + 1 == height`, `target == height`).
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

/// The unit size in force (1 = off). Set once from the env by
/// [`init_from_env`] (node start) or directly by tests ([`set_k`]).
static K: AtomicU64 = AtomicU64::new(1);

/// `TORUS_S80_EXEC_JOIN_LIQ=unit`: ONE liquidation pass per unit (the scan
/// cadence drops to one pass per `K` blocks). Default (false): one pass per
/// member block, so the per-block scan / act / ADL budgets are kept.
static LIQ_PER_UNIT: AtomicBool = AtomicBool::new(false);

/// Largest accepted `K` (a prototype bound; larger values are clamped).
pub const MAX_K: u64 = 8;

/// Segment length the units are aligned to (the running-hash checkpoint
/// interval: an attest height always ends a unit).
pub const SEGMENT: u64 = 100;

/// Pure parse of `TORUS_S80_EXEC_JOIN`: unset, empty, unparsable, `0` or `1`
/// = off (1); otherwise `K`, clamped to [`MAX_K`].
pub fn parse(v: Option<&str>) -> u64 {
    match v.map(str::trim).and_then(|s| s.parse::<u64>().ok()) {
        Some(k) if k >= 2 => k.min(MAX_K),
        _ => 1,
    }
}

/// Read `TORUS_S80_EXEC_JOIN` once per process, apply it and log one INFO
/// line (`TORUS_S80_EXEC_JOIN on=<bool> k=<K>`). Later calls return the
/// first value and do not touch [`K`] again.
pub fn init_from_env() -> u64 {
    static ONCE: OnceLock<u64> = OnceLock::new();
    *ONCE.get_or_init(|| {
        let k = parse(std::env::var("TORUS_S80_EXEC_JOIN").ok().as_deref());
        K.store(k, Ordering::SeqCst);
        let liq_unit = std::env::var("TORUS_S80_EXEC_JOIN_LIQ").is_ok_and(|v| v.trim() == "unit");
        LIQ_PER_UNIT.store(liq_unit, Ordering::SeqCst);
        tracing::info!(
            on = k > 1,
            k,
            liq_per_unit = liq_unit,
            "TORUS_S80_EXEC_JOIN on={} k={} liq={}",
            k > 1,
            k,
            if liq_unit { "unit" } else { "member" }
        );
        k
    })
}

/// The unit size in force (1 = off).
pub fn k() -> u64 {
    K.load(Ordering::Relaxed).max(1)
}

/// Set the unit size (tests; `0` and `1` = off).
pub fn set_k(k: u64) {
    K.store(k.clamp(1, MAX_K), Ordering::SeqCst);
}

/// Set the liquidation cadence (tests / µbench): `true` = one pass per unit.
pub fn set_liq_per_unit(on: bool) {
    LIQ_PER_UNIT.store(on, Ordering::SeqCst);
}

/// Liquidation passes for an execution at `height` whose unit starts at
/// `first` (`0` = a single block): one per member block (the per-block scan
/// cadence), or one per unit with `TORUS_S80_EXEC_JOIN_LIQ=unit`.
pub fn liq_passes(first: u64, height: u64) -> u64 {
    liq_passes_with(first, height, LIQ_PER_UNIT.load(Ordering::Relaxed))
}

/// [`liq_passes`] for an explicit cadence.
pub fn liq_passes_with(first: u64, height: u64, per_unit: bool) -> u64 {
    if per_unit || first == 0 || first >= height {
        1
    } else {
        height - first + 1
    }
}

/// `(first, last)` heights of the unit holding `height` (`height >= 1`) for
/// unit size `k`: aligned inside the 100-block segment, truncated at its end.
pub fn unit_bounds(height: u64, k: u64) -> (u64, u64) {
    let k = k.max(1);
    if height == 0 || k == 1 {
        return (height, height);
    }
    let pos = (height - 1) % SEGMENT;
    let seg_first = height - pos;
    let first = seg_first + (pos / k) * k;
    let last = (first + k - 1).min(seg_first + SEGMENT - 1);
    (first, last)
}

/// Whether an execution at `height` may follow one at `prev` with nothing
/// executed in between: `prev + 1 == height`, or (join on) every height in
/// `prev+1 ..= height` lies in `height`'s unit (it was executed as part of it).
pub fn follows(prev: u64, height: u64) -> bool {
    follows_k(prev, height, k())
}

/// [`follows`] for an explicit unit size.
pub fn follows_k(prev: u64, height: u64, k: u64) -> bool {
    if prev.checked_add(1) == Some(height) {
        return true;
    }
    k > 1 && prev < height && unit_bounds(height, k).0 <= prev + 1
}

/// Whether an execution at `height` covers `target`: `target == height`, or
/// (join on) `target` is an earlier height of `height`'s unit.
pub fn reaches(height: u64, target: u64) -> bool {
    reaches_k(height, target, k())
}

/// [`reaches`] for an explicit unit size.
pub fn reaches_k(height: u64, target: u64, k: u64) -> bool {
    target == height || (k > 1 && target < height && unit_bounds(height, k).0 <= target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_off_unless_two_or_more() {
        for v in [None, Some(""), Some("0"), Some("1"), Some("x"), Some("-2")] {
            assert_eq!(parse(v), 1, "{v:?}");
        }
        assert_eq!(parse(Some("2")), 2);
        assert_eq!(parse(Some(" 3 ")), 3);
        assert_eq!(parse(Some("99")), MAX_K);
    }

    #[test]
    fn units_align_to_segments_and_end_at_attest_heights() {
        // K = 1: every height is its own unit.
        for h in 1..=300 {
            assert_eq!(unit_bounds(h, 1), (h, h));
        }
        assert_eq!(unit_bounds(1, 2), (1, 2));
        assert_eq!(unit_bounds(2, 2), (1, 2));
        assert_eq!(unit_bounds(3, 2), (3, 4));
        assert_eq!(unit_bounds(99, 2), (99, 100));
        assert_eq!(unit_bounds(100, 2), (99, 100));
        assert_eq!(unit_bounds(101, 2), (101, 102));
        // K = 3: 100 is a truncated unit of its own (97..99, then 100).
        assert_eq!(unit_bounds(1, 3), (1, 3));
        assert_eq!(unit_bounds(97, 3), (97, 99));
        assert_eq!(unit_bounds(99, 3), (97, 99));
        assert_eq!(unit_bounds(100, 3), (100, 100));
        assert_eq!(unit_bounds(101, 3), (101, 103));
        // K = 4: 97..100.
        assert_eq!(unit_bounds(98, 4), (97, 100));
        assert_eq!(unit_bounds(200, 4), (197, 200));
        for k in 1..=MAX_K {
            let mut next = 1;
            for h in 1..=1_000u64 {
                let (f, l) = unit_bounds(h, k);
                assert!(f <= h && h <= l && l - f < k, "k={k} h={h}");
                // Units tile the heights: each starts right after the previous one.
                if h == f {
                    assert_eq!(f, next, "k={k} h={h}");
                    next = l + 1;
                }
                // An attest height always ends its unit; no unit crosses a segment.
                if h % SEGMENT == 0 {
                    assert_eq!(l, h, "k={k} h={h}");
                }
                assert_eq!((f - 1) / SEGMENT, (l - 1) / SEGMENT, "k={k} h={h}");
            }
        }
    }

    #[test]
    fn follows_and_reaches_are_the_per_block_rule_at_k1() {
        for h in 2..=300u64 {
            for prev in 0..h + 2 {
                assert_eq!(follows_k(prev, h, 1), prev + 1 == h, "prev={prev} h={h}");
            }
            for t in 0..h + 2 {
                assert_eq!(reaches_k(h, t, 1), t == h, "t={t} h={h}");
            }
        }
    }

    #[test]
    fn liq_passes_keep_the_per_block_cadence_unless_per_unit() {
        // A single block (no unit, or a unit of one): one pass.
        assert_eq!(liq_passes_with(0, 7, false), 1);
        assert_eq!(liq_passes_with(7, 7, false), 1);
        assert_eq!(liq_passes_with(9, 7, false), 1);
        // A unit 5..=6: one pass per member, or one per unit.
        assert_eq!(liq_passes_with(5, 6, false), 2);
        assert_eq!(liq_passes_with(5, 6, true), 1);
        assert_eq!(liq_passes_with(97, 100, false), 4);
        assert_eq!(liq_passes_with(97, 100, true), 1);
    }

    #[test]
    fn follows_accepts_gaps_inside_one_unit_only() {
        // K = 2: unit (3, 4) executed at 4 after 2.
        assert!(follows_k(2, 4, 2));
        assert!(follows_k(3, 4, 2));
        // A gap reaching into the previous unit is stale.
        assert!(!follows_k(1, 4, 2));
        assert!(!follows_k(4, 4, 2));
        assert!(!follows_k(5, 4, 2));
        // K = 4: unit 97..100 at 100 after 96.
        assert!(follows_k(96, 100, 4));
        assert!(!follows_k(95, 100, 4));
        // 100 -> 102 at K = 2: unit (101, 102).
        assert!(follows_k(100, 102, 2));
        assert!(!follows_k(99, 101, 2), "101 starts a unit: nothing may be skipped");
        // reaches: an execution at 2 covers 1 (unit 1..2), not 0.
        assert!(reaches_k(2, 1, 2));
        assert!(reaches_k(2, 2, 2));
        assert!(!reaches_k(2, 3, 2));
        assert!(!reaches_k(3, 2, 2));
        assert!(reaches_k(100, 97, 4));
        assert!(!reaches_k(100, 96, 4));
    }
}
