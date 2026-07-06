/// EIP-1559 elasticity multiplier: gas target = gas_limit / 2.
const ELASTICITY_MULTIPLIER: u64 = 2;

/// Maximum base fee change per block: 12.5% (1/8).
const BASE_FEE_MAX_CHANGE_DENOMINATOR: u64 = 8;

/// Calculate the base fee for the next block given the parent block's gas usage.
///
/// - If `gas_used == gas_target`: base fee unchanged.
/// - If `gas_used > gas_target`: base fee increases (up to +12.5%).
/// - If `gas_used < gas_target`: base fee decreases (up to −12.5%).
///
/// The minimum increase is 1 wei when the block is over-target,
/// ensuring the base fee always rises under sustained congestion.
pub fn calc_next_block_base_fee(gas_used: u64, gas_limit: u64, base_fee: u64) -> u64 {
    let gas_target = gas_limit / ELASTICITY_MULTIPLIER;

    // If gas_limit is 0 or 1 (uninitialised parent), no adjustment is possible.
    if gas_target == 0 {
        return base_fee;
    }

    if gas_used == gas_target {
        return base_fee;
    }

    if gas_used > gas_target {
        let delta = gas_used - gas_target;
        let fee_delta = (base_fee as u128 * delta as u128
            / gas_target as u128
            / BASE_FEE_MAX_CHANGE_DENOMINATOR as u128) as u64;
        // Must increase by at least 1 when over-target.
        base_fee + fee_delta.max(1)
    } else {
        let delta = gas_target - gas_used;
        let fee_delta = (base_fee as u128 * delta as u128
            / gas_target as u128
            / BASE_FEE_MAX_CHANGE_DENOMINATOR as u128) as u64;
        base_fee.saturating_sub(fee_delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_fee_unchanged_at_target() {
        // gas_used == gas_target → no change
        let base = calc_next_block_base_fee(15_000_000, 30_000_000, 1_000_000_000);
        assert_eq!(base, 1_000_000_000);
    }

    #[test]
    fn base_fee_increases_above_target() {
        // Full block (30M used, 30M limit) → +12.5%
        let base = calc_next_block_base_fee(30_000_000, 30_000_000, 1_000_000_000);
        assert_eq!(base, 1_125_000_000);
    }

    #[test]
    fn base_fee_decreases_below_target() {
        // Empty block (0 used) → −12.5%
        let base = calc_next_block_base_fee(0, 30_000_000, 1_000_000_000);
        assert_eq!(base, 875_000_000);
    }

    #[test]
    fn base_fee_minimum_increase() {
        // Very low base fee + slightly over target → increase by at least 1
        let base = calc_next_block_base_fee(15_000_001, 30_000_000, 1);
        assert_eq!(base, 2);
    }

    #[test]
    fn base_fee_floor_at_one() {
        // Empty block + base_fee = 1 → 1/8 rounds to 0, stays at 1.
        let base = calc_next_block_base_fee(0, 30_000_000, 1);
        assert_eq!(base, 1);
    }

    #[test]
    fn base_fee_unchanged_for_zero_gas_limit() {
        // gas_limit = 0 (e.g. genesis parent) → must not panic, no adjustment
        assert_eq!(calc_next_block_base_fee(0, 0, 1_000_000_000), 1_000_000_000);
        // nonzero gas_used with zero gas_limit → also must not panic
        assert_eq!(
            calc_next_block_base_fee(1_000_000, 0, 1_000_000_000),
            1_000_000_000
        );
        // gas_limit = 1 → gas_target = 0 via integer division, same guard
        assert_eq!(calc_next_block_base_fee(0, 1, 1_000_000_000), 1_000_000_000);
    }

    #[test]
    fn base_fee_saturates_at_zero() {
        // base_fee = 0 with empty block should not underflow.
        let base = calc_next_block_base_fee(0, 30_000_000, 0);
        assert_eq!(base, 0);
    }

    #[test]
    fn base_fee_partial_usage() {
        // 75% full: gas_used = 22.5M, target = 15M → +6.25%
        let base = calc_next_block_base_fee(22_500_000, 30_000_000, 1_000_000_000);
        // delta = 7.5M, fee_delta = 1B * 7.5M / 15M / 8 = 62_500_000
        assert_eq!(base, 1_062_500_000);
    }
}
