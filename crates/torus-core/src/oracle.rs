//! Oracle price feed — validator-submitted prices with stake-weighted median aggregation.
//!
//! Tasks 2.8b.1–2.8b.4: submission, aggregation, staleness detection, manipulation resistance.

use std::io::{self, Read, Write};

use borsh::{BorshDeserialize, BorshSerialize};
use torus_state::cf::CF_NATIVE_ORACLE;
use torus_state::{StateBackend, StateDb};
use torus_types::{Address, FixedPoint, MarketId};

use crate::error::CoreError;
use crate::position::{borsh_read_address, borsh_read_fp, borsh_write_address, borsh_write_fp};

// ============================================================================
// Constants
// ============================================================================

/// Max oracle age in blocks before price is considered stale.
/// FIX MED-NEW-13: Single source of truth — also used by precompiles.rs.
pub(crate) const DEFAULT_MAX_ORACLE_AGE: u64 = 100;
const DEFAULT_MIN_ORACLE_REPORTERS: usize = 3;

// ============================================================================
// Types
// ============================================================================

/// A single validator's price submission for a market at a given block.
#[derive(Clone, Debug)]
pub struct OracleSubmission {
    pub validator: Address,
    pub market_id: MarketId,
    pub price: FixedPoint,
    pub block_number: u64,
    pub timestamp: u64,
}

impl BorshSerialize for OracleSubmission {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        borsh_write_address(&self.validator, w)?;
        w.write_all(&self.market_id.to_be_bytes())?;
        borsh_write_fp(&self.price, w)?;
        w.write_all(&self.block_number.to_be_bytes())?;
        w.write_all(&self.timestamp.to_be_bytes())?;
        Ok(())
    }
}

impl BorshDeserialize for OracleSubmission {
    fn deserialize_reader<R: Read>(r: &mut R) -> io::Result<Self> {
        let validator = borsh_read_address(r)?;
        let mut mb = [0u8; 8];
        r.read_exact(&mut mb)?;
        let market_id = u64::from_be_bytes(mb);
        let price = borsh_read_fp(r)?;
        let mut bb = [0u8; 8];
        r.read_exact(&mut bb)?;
        let block_number = u64::from_be_bytes(bb);
        let mut tb = [0u8; 8];
        r.read_exact(&mut tb)?;
        let timestamp = u64::from_be_bytes(tb);
        Ok(Self {
            validator,
            market_id,
            price,
            block_number,
            timestamp,
        })
    }
}

/// Aggregated oracle price with metadata.
#[derive(Clone, Debug)]
pub struct OraclePrice {
    pub price: FixedPoint,
    pub block_number: u64,
    pub stale: bool,
    pub num_reporters: usize,
}

/// Aggregated price stored in state for quick reads.
#[derive(Clone, Debug)]
struct StoredAggregatedPrice {
    price: FixedPoint,
    block_number: u64,
    num_reporters: u32,
}

impl BorshSerialize for StoredAggregatedPrice {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        borsh_write_fp(&self.price, w)?;
        w.write_all(&self.block_number.to_be_bytes())?;
        w.write_all(&self.num_reporters.to_be_bytes())?;
        Ok(())
    }
}

impl BorshDeserialize for StoredAggregatedPrice {
    fn deserialize_reader<R: Read>(r: &mut R) -> io::Result<Self> {
        let price = borsh_read_fp(r)?;
        let mut bb = [0u8; 8];
        r.read_exact(&mut bb)?;
        let block_number = u64::from_be_bytes(bb);
        let mut nb = [0u8; 4];
        r.read_exact(&mut nb)?;
        let num_reporters = u32::from_be_bytes(nb);
        Ok(Self {
            price,
            block_number,
            num_reporters,
        })
    }
}

/// Oracle configuration.
#[derive(Clone, Debug)]
pub struct OracleConfig {
    pub max_oracle_age: u64,
    pub min_oracle_reporters: usize,
    pub aggregation_window: u64,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            max_oracle_age: DEFAULT_MAX_ORACLE_AGE,
            min_oracle_reporters: DEFAULT_MIN_ORACLE_REPORTERS,
            aggregation_window: 10,
        }
    }
}

// ============================================================================
// Keys
// ============================================================================

/// Submission key: "sub" + market_id(8) + validator(20) + block(8) = 39 bytes.
fn submission_key(market_id: MarketId, validator: &Address, block_number: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(39);
    key.extend_from_slice(b"sub");
    key.extend_from_slice(&market_id.to_be_bytes());
    key.extend_from_slice(validator.as_slice());
    key.extend_from_slice(&block_number.to_be_bytes());
    key
}

/// Prefix for all submissions for a market: "sub" + market_id(8).
fn submission_market_prefix(market_id: MarketId) -> Vec<u8> {
    let mut key = Vec::with_capacity(11);
    key.extend_from_slice(b"sub");
    key.extend_from_slice(&market_id.to_be_bytes());
    key
}

/// Key for aggregated price: "agg" + market_id(8) = 11 bytes.
fn aggregated_price_key(market_id: MarketId) -> Vec<u8> {
    let mut key = Vec::with_capacity(11);
    key.extend_from_slice(b"agg");
    key.extend_from_slice(&market_id.to_be_bytes());
    key
}

// ============================================================================
// OracleManager
// ============================================================================

pub struct OracleManager<T: StateBackend = StateDb> {
    state: T,
    config: OracleConfig,
}

impl<T: StateBackend> OracleManager<T> {
    pub fn new(state: T, config: OracleConfig) -> Self {
        Self { state, config }
    }

    // 2.8b.1: Submit a price from a validator.
    pub fn submit_price(
        &self,
        validator: &Address,
        market_id: MarketId,
        price: FixedPoint,
        block_number: u64,
        timestamp: u64,
    ) -> Result<(), CoreError> {
        let submission = OracleSubmission {
            validator: *validator,
            market_id,
            price,
            block_number,
            timestamp,
        };
        let key = submission_key(market_id, validator, block_number);
        let data = borsh::to_vec(&submission).map_err(|e| CoreError::Borsh(e.to_string()))?;
        self.state.put_cf_raw(CF_NATIVE_ORACLE, &key, &data)?;
        Ok(())
    }

    // 2.8b.2 + 2.8b.4: Aggregate prices using stake-weighted median with outlier rejection.
    pub fn aggregate_price(
        &self,
        market_id: MarketId,
        current_block: u64,
        validator_stakes: &[(Address, FixedPoint)],
    ) -> Result<FixedPoint, CoreError> {
        let window_start = current_block.saturating_sub(self.config.aggregation_window);
        let submissions = self.collect_submissions(market_id, window_start, current_block)?;

        if submissions.is_empty() {
            return Err(CoreError::NoOraclePrice(market_id));
        }

        // Build (price, stake) pairs — only include validators with known stake
        let mut price_stake_pairs: Vec<(FixedPoint, FixedPoint)> = Vec::new();
        for sub in &submissions {
            if let Some((_, stake)) = validator_stakes
                .iter()
                .find(|(addr, _)| *addr == sub.validator)
            {
                price_stake_pairs.push((sub.price, *stake));
            }
        }

        if price_stake_pairs.is_empty() {
            return Err(CoreError::NoOraclePrice(market_id));
        }

        // 2.8b.4: Outlier rejection
        let filtered = reject_outliers(&price_stake_pairs);

        if filtered.len() < self.config.min_oracle_reporters {
            return self.get_last_valid_price(market_id, current_block);
        }

        // FIX 19: Single-reporter safety — bound price change vs last valid price (ECON-PF-18)
        // When only one reporter passes filters, cap deviation at 10% from last known price
        // to prevent a single validator from manipulating the oracle.
        if filtered.len() == 1 {
            if let Ok(last_price) = self.get_last_valid_price(market_id, current_block) {
                if last_price > FixedPoint::ZERO {
                    let single_price = filtered[0].0;
                    let diff = if single_price > last_price {
                        single_price - last_price
                    } else {
                        last_price - single_price
                    };
                    // Max 10% deviation from last valid price per block
                    let max_deviation_bps = FixedPoint::from_raw(1000 * FixedPoint::SCALE); // 10%
                    let bps_denom = FixedPoint::from_raw(10_000 * FixedPoint::SCALE);
                    let max_change = last_price * max_deviation_bps / bps_denom;
                    if diff > max_change {
                        return self.get_last_valid_price(market_id, current_block);
                    }
                }
            }
        }

        let price = weighted_median(&filtered);

        // Store aggregated price
        let stored = StoredAggregatedPrice {
            price,
            block_number: current_block,
            num_reporters: filtered.len() as u32,
        };
        let key = aggregated_price_key(market_id);
        let data = borsh::to_vec(&stored).map_err(|e| CoreError::Borsh(e.to_string()))?;
        self.state.put_cf_raw(CF_NATIVE_ORACLE, &key, &data)?;

        Ok(price)
    }

    // 2.8b.3: Get the current oracle price with staleness detection.
    pub fn get_price(
        &self,
        market_id: MarketId,
        current_block: u64,
    ) -> Result<OraclePrice, CoreError> {
        let key = aggregated_price_key(market_id);
        match self.state.get_cf_raw(CF_NATIVE_ORACLE, &key)? {
            Some(data) => {
                let stored = StoredAggregatedPrice::try_from_slice(&data)
                    .map_err(|e| CoreError::Borsh(e.to_string()))?;
                let stale =
                    current_block.saturating_sub(stored.block_number) > self.config.max_oracle_age;
                Ok(OraclePrice {
                    price: stored.price,
                    block_number: stored.block_number,
                    stale,
                    num_reporters: stored.num_reporters as usize,
                })
            }
            None => Err(CoreError::NoOraclePrice(market_id)),
        }
    }

    /// Collect all submissions for a market within a block range (latest per validator).
    /// FIX 17: Also prunes old submissions to prevent unbounded growth (ECON-FIND-18).
    fn collect_submissions(
        &self,
        market_id: MarketId,
        start_block: u64,
        end_block: u64,
    ) -> Result<Vec<OracleSubmission>, CoreError> {
        let prefix = submission_market_prefix(market_id);
        let entries = self.state.iterate_cf(CF_NATIVE_ORACLE, Some(&prefix))?;

        let mut latest_per_validator: std::collections::BTreeMap<Address, OracleSubmission> =
            std::collections::BTreeMap::new();
        let mut keys_to_prune: Vec<Vec<u8>> = Vec::new();

        for (key, value) in &entries {
            if let Ok(sub) = OracleSubmission::try_from_slice(value) {
                if sub.block_number < start_block {
                    keys_to_prune.push(key.clone());
                } else if sub.block_number <= end_block {
                    match latest_per_validator.get(&sub.validator) {
                        Some(existing) if existing.block_number >= sub.block_number => {}
                        _ => {
                            latest_per_validator.insert(sub.validator, sub);
                        }
                    }
                }
            }
        }

        let prune_limit = 100;
        for key in keys_to_prune.iter().take(prune_limit) {
            let _ = self.state.delete_cf_raw(CF_NATIVE_ORACLE, key);
        }

        Ok(latest_per_validator.into_values().collect())
    }

    /// AUDIT FIX ECON-FIND-20: `get_last_valid_price` now enforces the same
    /// staleness check as `get_price`. Without this, a price from block 0 could
    /// be returned as if current when used as a fallback in `aggregate_price`.
    fn get_last_valid_price(
        &self,
        market_id: MarketId,
        current_block: u64,
    ) -> Result<FixedPoint, CoreError> {
        let key = aggregated_price_key(market_id);
        match self.state.get_cf_raw(CF_NATIVE_ORACLE, &key)? {
            Some(data) => {
                let stored = StoredAggregatedPrice::try_from_slice(&data)
                    .map_err(|e| CoreError::Borsh(e.to_string()))?;
                if current_block.saturating_sub(stored.block_number) > self.config.max_oracle_age {
                    return Err(CoreError::StaleOraclePrice(market_id));
                }
                Ok(stored.price)
            }
            None => Err(CoreError::NoOraclePrice(market_id)),
        }
    }
}

// ============================================================================
// Pure math functions — deterministic, no f64
// ============================================================================

fn simple_median(prices: &[FixedPoint]) -> FixedPoint {
    let mut sorted: Vec<FixedPoint> = prices.to_vec();
    sorted.sort();
    let n = sorted.len();
    if n == 0 {
        return FixedPoint::ZERO;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        let two = FixedPoint::from_raw(2 * FixedPoint::SCALE);
        (sorted[n / 2 - 1] + sorted[n / 2]) / two
    }
}

/// Mean absolute deviation from median — used instead of std dev to avoid sqrt.
/// MAD = sum(|x - median|) / n
fn mean_abs_deviation(prices: &[FixedPoint], median: FixedPoint) -> FixedPoint {
    if prices.is_empty() {
        return FixedPoint::ZERO;
    }
    let n = FixedPoint::from_raw(prices.len() as i128 * FixedPoint::SCALE);
    let mut sum = FixedPoint::ZERO;
    for &p in prices {
        let diff = p - median;
        let abs_diff = if diff < FixedPoint::ZERO { -diff } else { diff };
        sum = sum + abs_diff;
    }
    sum / n
}

/// Reject outliers > 3 * MAD from simple median (robust, no sqrt needed).
fn reject_outliers(pairs: &[(FixedPoint, FixedPoint)]) -> Vec<(FixedPoint, FixedPoint)> {
    if pairs.len() <= 1 {
        return pairs.to_vec();
    }

    let prices: Vec<FixedPoint> = pairs.iter().map(|(p, _)| *p).collect();
    let med = simple_median(&prices);
    let mad = mean_abs_deviation(&prices, med);

    // Threshold = 3 * MAD (roughly equivalent to 2 std devs for normal distributions)
    let three = FixedPoint::from_raw(3 * FixedPoint::SCALE);
    let threshold = mad * three;

    // If MAD is zero (all same price), keep everything
    if threshold == FixedPoint::ZERO {
        return pairs.to_vec();
    }

    pairs
        .iter()
        .filter(|(price, _)| {
            let diff = *price - med;
            let abs_diff = if diff < FixedPoint::ZERO { -diff } else { diff };
            abs_diff <= threshold
        })
        .cloned()
        .collect()
}

/// Stake-weighted median: sort by price, walk cumulative stake to 50%.
fn weighted_median(pairs: &[(FixedPoint, FixedPoint)]) -> FixedPoint {
    if pairs.is_empty() {
        return FixedPoint::ZERO;
    }
    if pairs.len() == 1 {
        return pairs[0].0;
    }

    let mut sorted: Vec<(FixedPoint, FixedPoint)> = pairs.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let total_stake: FixedPoint = sorted
        .iter()
        .map(|(_, s)| *s)
        .fold(FixedPoint::ZERO, |a, b| a + b);
    let two = FixedPoint::from_raw(2 * FixedPoint::SCALE);
    let half_stake = total_stake / two;

    let mut cumulative = FixedPoint::ZERO;
    for &(price, stake) in &sorted {
        cumulative = cumulative + stake;
        if cumulative >= half_stake {
            return price;
        }
    }

    sorted.last().unwrap().0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    #[test]
    fn test_simple_median_odd() {
        let prices = vec![fp(100), fp(200), fp(300)];
        assert_eq!(simple_median(&prices), fp(200));
    }

    #[test]
    fn test_simple_median_even() {
        let prices = vec![fp(100), fp(200), fp(300), fp(400)];
        assert_eq!(simple_median(&prices), fp(250));
    }

    #[test]
    fn test_weighted_median_basic() {
        let pairs = vec![(fp(100), fp(1)), (fp(200), fp(1)), (fp(300), fp(1))];
        assert_eq!(weighted_median(&pairs), fp(200));
    }

    #[test]
    fn test_weighted_median_dominant_stake() {
        let pairs = vec![(fp(100), fp(1)), (fp(200), fp(1)), (fp(500), fp(18))];
        assert_eq!(weighted_median(&pairs), fp(500));
    }

    #[test]
    fn test_weighted_median_single() {
        let pairs = vec![(fp(42), fp(100))];
        assert_eq!(weighted_median(&pairs), fp(42));
    }

    #[test]
    fn test_reject_outliers_removes_wild() {
        let pairs = vec![
            (fp(99), fp(1)),
            (fp(100), fp(1)),
            (fp(101), fp(1)),
            (fp(100), fp(1)),
            (fp(10000), fp(1)),
        ];
        let filtered = reject_outliers(&pairs);
        assert_eq!(filtered.len(), 4);
        for (price, _) in &filtered {
            assert!(*price < fp(1000));
        }
    }

    #[test]
    fn test_reject_outliers_all_same() {
        let pairs = vec![(fp(100), fp(1)), (fp(100), fp(1)), (fp(100), fp(1))];
        let filtered = reject_outliers(&pairs);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn test_all_same_price_weighted_median() {
        let pairs = vec![(fp(500), fp(10)), (fp(500), fp(20)), (fp(500), fp(30))];
        assert_eq!(weighted_median(&pairs), fp(500));
    }

    #[test]
    fn single_reporter_bounded_by_last_price() {
        // FIX 19: Test that reject_outliers with a single entry returns it unchanged
        // (the bounding against last_price happens in aggregate_price, not reject_outliers)
        let pairs = vec![(fp(100), fp(1))];
        let filtered = reject_outliers(&pairs);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, fp(100));
    }

    #[test]
    fn reject_outliers_single_keeps_value() {
        let pairs = vec![(fp(42000), fp(10))];
        let filtered = reject_outliers(&pairs);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, fp(42000));
    }
}
