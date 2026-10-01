//! Oracle price feed — validator-submitted prices with stake-weighted median aggregation.
//!
//! Tasks 2.8b.1–2.8b.4: submission, aggregation, staleness detection, manipulation resistance.

use std::collections::BTreeMap;
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

/// Oracle price max age in SECONDS of block (header) time: an aggregate is
/// usable while `now − its timestamp <= 60`. Single source for precompiles.rs.
pub(crate) const DEFAULT_MAX_ORACLE_AGE_SECS: u64 = 60;
/// A validator's latest submission counts while `now − its block ts <= 10`.
const DEFAULT_ORACLE_WINDOW_SECS: u64 = 10;
const DEFAULT_MIN_ORACLE_REPORTERS: usize = 3;

/// Most entries one `SubmitOraclePrices` may carry. A valid submission has one
/// entry per LISTED market (duplicates are rejected), so the cap only has to
/// exceed the listed-market count (HL lists ~200 perps); it bounds one action's
/// validation reads and row writes. A feeder with more markets splits them.
pub const MAX_ORACLE_PRICES_PER_SUBMISSION: usize = 256;

/// Largest accepted oracle price: 10^12 units (raw 10^20). Far above any asset
/// and small enough that every aggregation sum / product stays far inside i128
/// (its `FixedPoint` operators panic on overflow).
pub const MAX_ORACLE_PRICE_RAW: i128 = 1_000_000_000_000 * FixedPoint::SCALE;

/// Review M1(b) (s517): a submission's `timestamp` is its SAMPLE time (unix
/// ms, signed with the action). Exec rejects a submission sampled more than
/// this long before (or after) the block's header time, so a submission that
/// sat in a mempool cannot land as a fresh price. Within that window the row
/// keeps the NEWEST sample per (market, validator).
pub const MAX_ORACLE_SAMPLE_SKEW_MS: u64 = 5_000;

/// Accepted range of one oracle price: `0 < price <= MAX_ORACLE_PRICE_RAW`.
pub fn valid_oracle_price(price: FixedPoint) -> bool {
    price > FixedPoint::ZERO && price.raw() <= MAX_ORACLE_PRICE_RAW
}

// ============================================================================
// Types
// ============================================================================

/// A validator's latest price submission for a market; `block_number` /
/// `timestamp` are the submitting block's height and header timestamp.
#[derive(Clone, Debug)]
pub struct OracleSubmission {
    pub validator: Address,
    pub market_id: MarketId,
    pub price: FixedPoint,
    /// Sample time (unix ms) the reporter signed (review M1(b)).
    pub sample_ms: u64,
    pub block_number: u64,
    pub timestamp: u64,
}

impl BorshSerialize for OracleSubmission {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        borsh_write_address(&self.validator, w)?;
        w.write_all(&self.market_id.to_be_bytes())?;
        borsh_write_fp(&self.price, w)?;
        w.write_all(&self.sample_ms.to_be_bytes())?;
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
        let mut sb = [0u8; 8];
        r.read_exact(&mut sb)?;
        let sample_ms = u64::from_be_bytes(sb);
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
            sample_ms,
            block_number,
            timestamp,
        })
    }
}

/// Aggregated oracle price with metadata.
#[derive(Clone, Debug)]
pub struct OraclePrice {
    pub price: FixedPoint,
    /// Height of the block that wrote the last FRESH aggregate.
    pub block_number: u64,
    /// `now − timestamp > max_age_secs` (saturating: a future timestamp is age 0).
    pub stale: bool,
    pub num_reporters: usize,
    /// Block (header) timestamp of the last FRESH aggregate, seconds.
    pub timestamp: u64,
}

impl OraclePrice {
    /// The mark rule of every reader: fresh (age <= max age) and > 0.
    pub fn usable(&self) -> Option<FixedPoint> {
        (!self.stale && self.price > FixedPoint::ZERO).then_some(self.price)
    }
}

/// Aggregated price stored in state for quick reads:
/// price(16) ‖ block(8) ‖ reporters(4) ‖ timestamp(8) = 36 bytes
/// (the precompiles decode the same offsets).
#[derive(Clone, Debug)]
struct StoredAggregatedPrice {
    price: FixedPoint,
    block_number: u64,
    num_reporters: u32,
    timestamp: u64,
}

impl BorshSerialize for StoredAggregatedPrice {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        borsh_write_fp(&self.price, w)?;
        w.write_all(&self.block_number.to_be_bytes())?;
        w.write_all(&self.num_reporters.to_be_bytes())?;
        w.write_all(&self.timestamp.to_be_bytes())?;
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
        let mut tb = [0u8; 8];
        r.read_exact(&mut tb)?;
        let timestamp = u64::from_be_bytes(tb);
        Ok(Self {
            price,
            block_number,
            num_reporters,
            timestamp,
        })
    }
}

/// Oracle configuration. Times are seconds of block (header) time.
#[derive(Clone, Debug)]
pub struct OracleConfig {
    /// Seconds after the last fresh aggregate's block timestamp before it is stale.
    pub max_age_secs: u64,
    pub min_oracle_reporters: usize,
    /// Seconds a submission counts after its block timestamp.
    pub window_secs: u64,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            max_age_secs: DEFAULT_MAX_ORACLE_AGE_SECS,
            min_oracle_reporters: DEFAULT_MIN_ORACLE_REPORTERS,
            window_secs: DEFAULT_ORACLE_WINDOW_SECS,
        }
    }
}

// ============================================================================
// Keys
// ============================================================================

/// Submission key: "sub" + market_id(8) + validator(20) = 31 bytes — ONE row
/// per (market, validator): a new submission overwrites, so the row is the
/// validator's latest.
fn submission_key(market_id: MarketId, validator: &Address) -> Vec<u8> {
    let mut key = Vec::with_capacity(31);
    key.extend_from_slice(b"sub");
    key.extend_from_slice(&market_id.to_be_bytes());
    key.extend_from_slice(validator.as_slice());
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

    // 2.8b.1: Submit a price from a validator (sampled at the block time).
    pub fn submit_price(
        &self,
        validator: &Address,
        market_id: MarketId,
        price: FixedPoint,
        block_number: u64,
        timestamp: u64,
    ) -> Result<(), CoreError> {
        self.submit_sampled(validator, market_id, price, timestamp.saturating_mul(1_000), block_number, timestamp)
    }

    /// Write the (market, validator) row with the reporter's sample time.
    pub fn submit_sampled(
        &self,
        validator: &Address,
        market_id: MarketId,
        price: FixedPoint,
        sample_ms: u64,
        block_number: u64,
        timestamp: u64,
    ) -> Result<(), CoreError> {
        let submission = OracleSubmission {
            validator: *validator,
            market_id,
            price,
            sample_ms,
            block_number,
            timestamp,
        };
        let key = submission_key(market_id, validator);
        let data = borsh::to_vec(&submission).map_err(|e| CoreError::Borsh(e.to_string()))?;
        self.state.put_cf_raw(CF_NATIVE_ORACLE, &key, &data)?;
        Ok(())
    }

    /// The sample time of `validator`'s stored row for `market_id`, if any.
    pub fn stored_sample_ms(&self, market_id: MarketId, validator: &Address) -> Result<Option<u64>, CoreError> {
        match self.state.get_cf_raw(CF_NATIVE_ORACLE, &submission_key(market_id, validator))? {
            None => Ok(None),
            Some(v) => OracleSubmission::try_from_slice(&v)
                .map(|s| Some(s.sample_ms))
                .map_err(|e| CoreError::Borsh(e.to_string())),
        }
    }

    // 2.8b.2 + 2.8b.4: Aggregate prices using stake-weighted median with outlier rejection.
    /// `now` is the current block's header timestamp (s); `validator_stakes` is
    /// the Active set (its sum is the quorum's total). With >= min reporters
    /// holding > 2/3 of that stake (both after the outlier cut) a FRESH
    /// aggregate `{price, current_block, reporters, now}` is written; otherwise
    /// nothing is written and the last aggregate (if not stale) is returned —
    /// it keeps its timestamp and ages.
    pub fn aggregate_price(
        &self,
        market_id: MarketId,
        current_block: u64,
        now: u64,
        validator_stakes: &[(Address, FixedPoint)],
    ) -> Result<FixedPoint, CoreError> {
        let submissions = self.collect_submissions(market_id, now)?;

        // Item 2: no counting row is "fewer than min reporters" too — keep the
        // last price (it ages), never an error while that price is usable.
        if submissions.is_empty() {
            return self.get_last_valid_price(market_id, now);
        }

        // Build (price, stake) pairs — only include validators with known stake.
        // First entry wins on a duplicate address (as the former linear `find`).
        let mut stake_of: BTreeMap<Address, FixedPoint> = BTreeMap::new();
        for (addr, stake) in validator_stakes {
            stake_of.entry(*addr).or_insert(*stake);
        }
        let price_stake_pairs: Vec<(FixedPoint, FixedPoint)> = submissions
            .iter()
            .filter_map(|sub| stake_of.get(&sub.validator).map(|stake| (sub.price, *stake)))
            .collect();

        if price_stake_pairs.is_empty() {
            return self.get_last_valid_price(market_id, now);
        }

        // 2.8b.4: Outlier rejection
        let filtered = reject_outliers(&price_stake_pairs);

        // Review M1: a fresh price needs >= min reporters AND > 2/3 of the Active
        // stake, both over the set AFTER the outlier cut — the set whose
        // weighted median is the price. The cut uses the UNWEIGHTED median, so
        // a low-stake head-count majority can cut high-stake honest reports;
        // counting before the cut would let that minority set the price.
        let total_stake = stake_sum(validator_stakes.iter().map(|(_, s)| *s));
        let counted_stake = stake_sum(filtered.iter().map(|(_, s)| *s));
        if filtered.len() < self.config.min_oracle_reporters
            || !has_stake_quorum(counted_stake, total_stake)
        {
            return self.get_last_valid_price(market_id, now);
        }

        // FIX 19: Single-reporter safety — bound price change vs last valid price (ECON-PF-18)
        // When only one reporter passes filters, cap deviation at 10% from last known price
        // to prevent a single validator from manipulating the oracle.
        if filtered.len() == 1 {
            if let Ok(last_price) = self.get_last_valid_price(market_id, now) {
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
                        return self.get_last_valid_price(market_id, now);
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
            timestamp: now,
        };
        let key = aggregated_price_key(market_id);
        let data = borsh::to_vec(&stored).map_err(|e| CoreError::Borsh(e.to_string()))?;
        self.state.put_cf_raw(CF_NATIVE_ORACLE, &key, &data)?;

        Ok(price)
    }

    // 2.8b.3: Get the current oracle price with staleness detection.
    /// `now` = the reader's block (header) timestamp; stale iff
    /// `now − aggregate timestamp > max_age_secs` (saturating: age clamps at 0).
    pub fn get_price(&self, market_id: MarketId, now: u64) -> Result<OraclePrice, CoreError> {
        let stored = self.stored_aggregate(market_id)?;
        Ok(OraclePrice {
            price: stored.price,
            block_number: stored.block_number,
            stale: self.is_stale(&stored, now),
            num_reporters: stored.num_reporters as usize,
            timestamp: stored.timestamp,
        })
    }

    /// Block-start step: delete every submission row (all markets) whose block
    /// timestamp is more than the window older than `now`, and rows that do not
    /// decode. Rows are one per (market, validator), so the pass is bounded by
    /// validators × markets. Errors propagate. Returns the number deleted.
    pub fn prune_submissions(&self, now: u64) -> Result<usize, CoreError> {
        let mut pruned = 0;
        for (key, value) in self.state.iterate_cf(CF_NATIVE_ORACLE, Some(b"sub"))? {
            let old = match OracleSubmission::try_from_slice(&value) {
                Ok(sub) => now.saturating_sub(sub.timestamp) > self.config.window_secs,
                Err(_) => true,
            };
            if old {
                self.state.delete_cf_raw(CF_NATIVE_ORACLE, &key)?;
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    /// Whether any submission row exists (the block's oracle step is due).
    /// Stops at the first row.
    pub fn has_submissions(&self) -> Result<bool, CoreError> {
        Ok(self.state.prefix_exists(CF_NATIVE_ORACLE, b"sub")?)
    }

    /// The submissions of `market_id` that count at `now`: decodable, block
    /// timestamp within the window (`now − ts <= window_secs`, saturating) and
    /// price in range. Read-only (pruning is [`Self::prune_submissions`]); one
    /// row per validator, in key order.
    fn collect_submissions(
        &self,
        market_id: MarketId,
        now: u64,
    ) -> Result<Vec<OracleSubmission>, CoreError> {
        let prefix = submission_market_prefix(market_id);
        Ok(self
            .state
            .iterate_cf(CF_NATIVE_ORACLE, Some(&prefix))?
            .iter()
            .filter_map(|(_, value)| OracleSubmission::try_from_slice(value).ok())
            .filter(|sub| {
                now.saturating_sub(sub.timestamp) <= self.config.window_secs
                    && valid_oracle_price(sub.price)
            })
            .collect())
    }

    fn stored_aggregate(&self, market_id: MarketId) -> Result<StoredAggregatedPrice, CoreError> {
        let key = aggregated_price_key(market_id);
        match self.state.get_cf_raw(CF_NATIVE_ORACLE, &key)? {
            Some(data) => StoredAggregatedPrice::try_from_slice(&data)
                .map_err(|e| CoreError::Borsh(e.to_string())),
            None => Err(CoreError::NoOraclePrice(market_id)),
        }
    }

    fn is_stale(&self, stored: &StoredAggregatedPrice, now: u64) -> bool {
        now.saturating_sub(stored.timestamp) > self.config.max_age_secs
    }

    /// AUDIT FIX ECON-FIND-20: `get_last_valid_price` now enforces the same
    /// staleness check as `get_price`. Without this, a price from block 0 could
    /// be returned as if current when used as a fallback in `aggregate_price`.
    fn get_last_valid_price(&self, market_id: MarketId, now: u64) -> Result<FixedPoint, CoreError> {
        let stored = self.stored_aggregate(market_id)?;
        if self.is_stale(&stored, now) {
            return Err(CoreError::StaleOraclePrice(market_id));
        }
        Ok(stored.price)
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

/// Reject outliers: keep x iff |x − median| <= 3 × MAD, MAD = Σ|p − median| / n
/// (robust, no sqrt) — compared EXACTLY in raw integers as
/// n·|x − median| <= 3·Σ|p − median| (R9; inputs are bounded by
/// MAX_ORACLE_PRICE_RAW: no overflow). The FixedPoint form truncated MAD and
/// cut the boundary case (two equal prices + one other).
fn reject_outliers(pairs: &[(FixedPoint, FixedPoint)]) -> Vec<(FixedPoint, FixedPoint)> {
    if pairs.len() <= 1 {
        return pairs.to_vec();
    }
    let prices: Vec<FixedPoint> = pairs.iter().map(|(p, _)| *p).collect();
    let med = simple_median(&prices).raw();
    let dev = |p: FixedPoint| (p.raw() - med).abs();
    let n = prices.len() as i128;
    let three_sum: i128 = 3 * prices.iter().map(|&p| dev(p)).sum::<i128>();
    pairs
        .iter()
        .filter(|(p, _)| n * dev(*p) <= three_sum)
        .cloned()
        .collect()
}

/// Raw (i128) sum of stakes, saturating (stakes are whole-token power, >= 0).
fn stake_sum(stakes: impl Iterator<Item = FixedPoint>) -> i128 {
    stakes.fold(0i128, |acc, s| acc.saturating_add(s.raw()))
}

/// `3·counted > 2·total`, integer-exact and overflow-free: with
/// `rest = total − counted` it is `counted > 2·rest`, i.e.
/// `counted − rest > rest`. Requires `0 <= counted <= total`.
fn has_stake_quorum(counted: i128, total: i128) -> bool {
    let rest = total - counted;
    counted > rest && counted - rest > rest
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
        cumulative += stake;
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

    /// 3·counted > 2·total, strict, exact at the boundary, no overflow.
    #[test]
    fn stake_quorum_is_strictly_above_two_thirds() {
        assert!(!has_stake_quorum(6, 9));
        assert!(has_stake_quorum(7, 9));
        assert!(!has_stake_quorum(2, 3));
        assert!(has_stake_quorum(3, 3));
        assert!(!has_stake_quorum(0, 0), "no stake: no quorum");
        assert!(!has_stake_quorum(200_000_000, 300_000_000)); // raw of 2 of 3 tokens
        assert!(has_stake_quorum(200_000_001, 300_000_000));
        let m = i128::MAX;
        assert!(has_stake_quorum(m, m), "no overflow");
        assert!(!has_stake_quorum(m / 3 * 2, m / 3 * 3));
        assert!(has_stake_quorum(m / 3 * 2 + 1, m / 3 * 3));
    }

    #[test]
    fn reject_outliers_single_keeps_value() {
        let pairs = vec![(fp(42000), fp(10))];
        let filtered = reject_outliers(&pairs);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, fp(42000));
    }
}
