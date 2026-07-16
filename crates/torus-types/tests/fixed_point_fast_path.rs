//! Rank-13 (Package D Wave-2): differential test for the FixedPoint i128
//! fast path in `checked_mul` / `checked_div`.
//!
//! HARD REQUIREMENT: the fast-path implementation must be bit-identical to
//! the original ethnum-i256 implementation for ALL inputs, including every
//! overflow edge. The reference below is a verbatim frozen copy of the
//! pre-change i256-only code (operating on raw i128 limbs since the struct
//! field is private).
//!
//! Coverage:
//! 1. Exhaustive cross-product over a hand-picked boundary operand set that
//!    straddles every fast/slow-path decision edge (i128 product overflow,
//!    a*SCALE overflow, i128::MIN / -1, final-result i128 range edge).
//! 2. Randomized magnitude-stratified differential sweep over the full
//!    i128 x i128 operand space (deterministic splitmix64 PRNG, no seed
//!    dependence on test order).

use torus_types::{ArithmeticError, FixedPoint};

const SCALE: i128 = 100_000_000; // must match FixedPoint::SCALE

// ============================================================================
// Frozen reference implementation (pre-change code, i256-only)
// ============================================================================

fn ref_checked_mul(a: i128, b: i128) -> Result<i128, ArithmeticError> {
    use ethnum::i256;
    let result = i256::from(a) * i256::from(b) / i256::from(SCALE);
    if result > i256::from(i128::MAX) || result < i256::from(i128::MIN) {
        return Err(ArithmeticError::Overflow);
    }
    Ok(result.as_i128())
}

fn ref_checked_div(a: i128, b: i128) -> Result<i128, ArithmeticError> {
    if b == 0 {
        return Err(ArithmeticError::DivisionByZero);
    }
    use ethnum::i256;
    let result = i256::from(a) * i256::from(SCALE) / i256::from(b);
    if result > i256::from(i128::MAX) || result < i256::from(i128::MIN) {
        return Err(ArithmeticError::Overflow);
    }
    Ok(result.as_i128())
}

// ============================================================================
// Helpers
// ============================================================================

fn assert_mul_identical(a: i128, b: i128) {
    let new = FixedPoint::from_raw(a)
        .checked_mul(FixedPoint::from_raw(b))
        .map(|v| v.raw());
    let old = ref_checked_mul(a, b);
    assert_eq!(
        new, old,
        "checked_mul divergence: a={a} b={b} new={new:?} old={old:?}"
    );
}

fn assert_div_identical(a: i128, b: i128) {
    let new = FixedPoint::from_raw(a)
        .checked_div(FixedPoint::from_raw(b))
        .map(|v| v.raw());
    let old = ref_checked_div(a, b);
    assert_eq!(
        new, old,
        "checked_div divergence: a={a} b={b} new={new:?} old={old:?}"
    );
}

/// Boundary operand set: every value that sits on (or one step from) a
/// fast/slow path decision edge, plus representative "normal" magnitudes.
fn boundary_values() -> Vec<i128> {
    // sqrt(i128::MAX) — the mul-overflow frontier for equal-magnitude operands.
    const SQRT_MAX: i128 = 13_043_817_825_332_782_212;
    let mut v = vec![
        0,
        1,
        -1,
        2,
        -2,
        7,
        -7,
        SCALE,
        -SCALE,
        SCALE - 1,
        -(SCALE - 1),
        SCALE + 1,
        -(SCALE + 1),
        SCALE * SCALE,
        -(SCALE * SCALE),
        i128::MAX,
        i128::MAX - 1,
        i128::MIN,
        i128::MIN + 1,
        // div fast-path frontier: a * SCALE overflows i128 beyond these.
        i128::MAX / SCALE,
        i128::MAX / SCALE - 1,
        i128::MAX / SCALE + 1,
        i128::MIN / SCALE,
        i128::MIN / SCALE - 1,
        i128::MIN / SCALE + 1,
        // mul fast-path frontier (equal-magnitude operands).
        SQRT_MAX,
        SQRT_MAX - 1,
        SQRT_MAX + 1,
        -SQRT_MAX,
        -(SQRT_MAX - 1),
        -(SQRT_MAX + 1),
        // result-range frontier for mul: |a*b| near i128::MAX * SCALE.
        i128::MAX / 2,
        i128::MIN / 2,
        // typical trading magnitudes.
        i64::MAX as i128,
        i64::MIN as i128,
        123_456_789_012_345,
        -123_456_789_012_345,
        50_000 * SCALE,
        -50_000 * SCALE,
    ];
    v.sort_unstable();
    v.dedup();
    v
}

/// Deterministic splitmix64 PRNG.
struct SplitMix64(u64);
impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    fn next_u128(&mut self) -> u128 {
        ((self.next_u64() as u128) << 64) | self.next_u64() as u128
    }

    /// Magnitude-stratified i128: uniform bit-width in 0..=127, random sign.
    /// This exercises both fast paths, both slow paths, and the frontiers
    /// far more densely than uniform-over-i128 would (which is virtually
    /// always slow-path for mul).
    fn next_stratified_i128(&mut self) -> i128 {
        let bits = (self.next_u64() % 128) as u32; // 0..=127
        let mask = if bits == 0 { 0 } else { u128::MAX >> (128 - bits) };
        let mag = (self.next_u128() & mask) as i128;
        if self.next_u64() & 1 == 0 {
            mag
        } else {
            mag.checked_neg().unwrap_or(i128::MIN)
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn exhaustive_boundary_cross_product_matches_reference() {
    let vals = boundary_values();
    for &a in &vals {
        for &b in &vals {
            assert_mul_identical(a, b);
            assert_div_identical(a, b);
        }
    }
}

#[test]
fn randomized_differential_full_range() {
    let mut rng = SplitMix64(0xC0FFEE_D15EA5E);
    for _ in 0..200_000 {
        let a = rng.next_stratified_i128();
        let b = rng.next_stratified_i128();
        assert_mul_identical(a, b);
        assert_div_identical(a, b);
    }
}

/// Extra density right at the fast-path frontiers: perturb boundary values
/// by small random deltas so carries/borrows around the edges get hit.
#[test]
fn randomized_frontier_perturbation() {
    let vals = boundary_values();
    let mut rng = SplitMix64(0xDEADBEEF);
    for _ in 0..20_000 {
        let base_a = vals[(rng.next_u64() as usize) % vals.len()];
        let base_b = vals[(rng.next_u64() as usize) % vals.len()];
        let da = (rng.next_u64() % 5) as i128 - 2; // -2..=2
        let db = (rng.next_u64() % 5) as i128 - 2;
        let a = base_a.saturating_add(da);
        let b = base_b.saturating_add(db);
        assert_mul_identical(a, b);
        assert_div_identical(a, b);
    }
}

/// Pinned known-value edge cases (independent of the reference impl, so a
/// bug in BOTH implementations would still be caught here).
#[test]
fn pinned_edge_semantics() {
    let fp = FixedPoint::from_raw;

    // Deep-negative operand divided by -1.0: intermediate a*SCALE sits just
    // above i128::MIN (the largest scaled magnitude that still fits i128),
    // result is -a exactly.
    let a = i128::MIN / SCALE;
    let expect = a.checked_neg().unwrap();
    assert_eq!(fp(a).checked_div(fp(-SCALE)).unwrap().raw(), expect);

    // Division by zero.
    assert_eq!(
        fp(5).checked_div(fp(0)),
        Err(ArithmeticError::DivisionByZero)
    );
    assert_eq!(
        fp(0).checked_div(fp(0)),
        Err(ArithmeticError::DivisionByZero)
    );

    // Truncation toward zero for negatives (must match i256 semantics).
    // -1 raw / 3.0 = -0.33333333... -> truncates to 0 raw.
    assert_eq!(fp(-1).checked_div(fp(3 * SCALE)).unwrap().raw(), 0);
    // -10.0 / 3.0 = -3.33333333 (truncated toward zero).
    assert_eq!(
        fp(-10 * SCALE).checked_div(fp(3 * SCALE)).unwrap().raw(),
        -333_333_333
    );
    // Mul truncation: (-1 raw) * (1 raw) = -1e-16 -> 0.
    assert_eq!(fp(-1).checked_mul(fp(1)).unwrap().raw(), 0);

    // MAX * 1.0 = MAX exactly (slow path, result on the range edge).
    assert_eq!(
        fp(i128::MAX).checked_mul(fp(SCALE)).unwrap().raw(),
        i128::MAX
    );
    assert_eq!(
        fp(i128::MIN).checked_mul(fp(SCALE)).unwrap().raw(),
        i128::MIN
    );
    // MAX * 1.00000001 overflows.
    assert_eq!(
        fp(i128::MAX).checked_mul(fp(SCALE + 1)),
        Err(ArithmeticError::Overflow)
    );
    // MIN / 1.0 = MIN; MIN / -1.0 overflows (2^127 not representable).
    assert_eq!(fp(i128::MIN).checked_div(fp(SCALE)).unwrap().raw(), i128::MIN);
    assert_eq!(
        fp(i128::MIN).checked_div(fp(-SCALE)),
        Err(ArithmeticError::Overflow)
    );
}

/// Microbenchmark (run explicitly: `cargo test --release -p torus-types \
/// --test fixed_point_fast_path -- --ignored --nocapture bench_`).
#[test]
#[ignore]
fn bench_fast_path_vs_reference() {
    use std::time::Instant;

    // Realistic trading operands: price/qty magnitudes (fit i128 product).
    let mut rng = SplitMix64(42);
    let ops: Vec<(i128, i128)> = (0..1_000_000)
        .map(|_| {
            (
                (rng.next_u64() % 1_000_000_000) as i128 + 1, // up to ~10 units raw e9
                (rng.next_u64() % 10_000_000_000_000) as i128 + 1,
            )
        })
        .collect();

    let t0 = Instant::now();
    let mut acc = 0i128;
    for &(a, b) in &ops {
        acc ^= ref_checked_mul(a, b).unwrap();
        acc ^= ref_checked_div(a, b).unwrap();
    }
    let ref_time = t0.elapsed();

    let t1 = Instant::now();
    let mut acc2 = 0i128;
    for &(a, b) in &ops {
        acc2 ^= FixedPoint::from_raw(a)
            .checked_mul(FixedPoint::from_raw(b))
            .unwrap()
            .raw();
        acc2 ^= FixedPoint::from_raw(a)
            .checked_div(FixedPoint::from_raw(b))
            .unwrap()
            .raw();
    }
    let new_time = t1.elapsed();

    assert_eq!(acc, acc2);
    println!(
        "reference (i256): {:?} total, {:.1} ns/op-pair",
        ref_time,
        ref_time.as_nanos() as f64 / ops.len() as f64
    );
    println!(
        "current impl:     {:?} total, {:.1} ns/op-pair",
        new_time,
        new_time.as_nanos() as f64 / ops.len() as f64
    );
}
