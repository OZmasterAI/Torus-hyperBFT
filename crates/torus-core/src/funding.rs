//! Perp funding engine (docs/plans/funding-engine.md), P0.0 feature
//! `funding_engine`.
//!
//! HL-like: premium samples (impact bid / ask vs the oracle mark) on a block
//! timestamp cadence, an hourly rate `F8 × interval / 8 h` with
//! `F8 = avg premium + clamp(interest − avg premium, ±clamp)`, capped, and a
//! per-market cumulative index (`CF_NATIVE_ORACLE` row `fnd ‖ market`).
//! Positions settle lazily: each stores the index it last settled at and
//! pays `signed size × (index − entry)` (rounded up, against the holder) in
//! cash at its next fill; until then the amount counts in its unrealized
//! PnL. Nothing here walks positions: the block step is O(markets).

use alloy_primitives::U256;
use torus_state::cf::CF_NATIVE_ORACLE;
use torus_state::StateBackend;
use torus_types::{FixedPoint, MarketId};

use crate::error::CoreError;
use crate::order_book::OrderBook;

/// Scale of the cumulative index and of rates / premiums: 1e18 = 1 (an
/// index unit is quote per 1 unit of base). Far finer than `FixedPoint`'s
/// 1e-8, so a low-priced market's hourly index step is not truncated away.
pub const INDEX_SCALE: i128 = 1_000_000_000_000_000_000;

/// The HL 8-hour basis of the interest, the clamp and the premium average.
pub const BASIS_SECS: u64 = 28_800;

/// Most resting orders one side's impact walk reads; a side that cannot fill
/// the impact notional within it has no impact price (its premium term is 0).
pub const IMPACT_WALK_MAX_ORDERS: usize = 4_096;

/// Key prefix of the per-market funding rows in `CF_NATIVE_ORACLE`.
pub const FUNDING_ROW_PREFIX: &[u8; 3] = b"fnd";

const ROW_VERSION: u8 = 1;

/// Encoded size of a [`FundingRow`].
pub const FUNDING_ROW_LEN: usize = 1 + 16 + 8 + 16 + 4 + 8 + 16 + 16 + 8;

/// `fnd ‖ market(8 BE)`.
pub fn funding_key(market_id: MarketId) -> [u8; 11] {
    let mut key = [0u8; 11];
    key[..3].copy_from_slice(FUNDING_ROW_PREFIX);
    key[3..].copy_from_slice(&market_id.to_be_bytes());
    key
}

/// The governance parameters in force (`torus_types::FUNDING_PARAMS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FundingParams {
    pub interval_secs: u64,
    pub sample_secs: u64,
    /// Quote notional of the impact walk.
    pub impact_notional: FixedPoint,
    pub interest_ppm_8h: u64,
    pub clamp_ppm_8h: u64,
    pub max_rate_bps: u64,
}

impl Default for FundingParams {
    fn default() -> Self {
        Self::from_stored(|_| None)
    }
}

impl FundingParams {
    /// The params for stored key values (`get(key)`): each value when it is a
    /// decimal in its range, else its default.
    pub fn from_stored(get: impl Fn(&str) -> Option<Vec<u8>>) -> Self {
        let v = |i: usize| {
            let (key, default, range) = &torus_types::FUNDING_PARAMS[i];
            torus_types::gov_u64_in(get(key).as_deref(), range).unwrap_or(*default)
        };
        Self {
            interval_secs: v(0),
            sample_secs: v(1),
            impact_notional: FixedPoint::from_raw(i128::from(v(2)) * FixedPoint::SCALE),
            interest_ppm_8h: v(3),
            clamp_ppm_8h: v(4),
            max_rate_bps: v(5),
        }
    }

    /// The params stored in `CF_FEE_CONFIG` (one point read per key).
    pub fn read<T: StateBackend>(state: &T) -> Result<Self, CoreError> {
        let mut stored = Vec::with_capacity(torus_types::FUNDING_PARAMS.len());
        for (key, _, _) in &torus_types::FUNDING_PARAMS {
            stored.push((*key, state.get_cf_raw(torus_state::cf::CF_FEE_CONFIG, key.as_bytes())?));
        }
        Ok(Self::from_stored(|k| stored.iter().find(|(s, _)| *s == k).and_then(|(_, v)| v.clone())))
    }
}

/// One market's funding state (the `fnd` row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FundingRow {
    /// Cumulative funding per unit of base, quote, scaled [`INDEX_SCALE`].
    pub cum_index: i128,
    /// Start of the current interval (block seconds, aligned to the interval).
    pub interval_start: u64,
    /// Σ premium samples of the current interval (scaled [`INDEX_SCALE`]).
    pub premium_sum: i128,
    pub samples: u32,
    /// Block timestamp of the last sample slot taken (0 = none yet).
    pub last_sample_ts: u64,
    /// Oracle price of the last premium sample (settlement price fallback).
    pub last_oracle: FixedPoint,
    /// Rate of the last settlement (scaled [`INDEX_SCALE`]).
    pub last_rate: i128,
    /// Block timestamp of the last settlement (0 = none yet).
    pub last_settle_ts: u64,
}

impl FundingRow {
    /// A fresh row at `now`: index 0, interval aligned down to `params`'.
    pub fn new(now: u64, params: &FundingParams) -> Self {
        Self {
            cum_index: 0,
            interval_start: align(now, params.interval_secs),
            premium_sum: 0,
            samples: 0,
            last_sample_ts: 0,
            last_oracle: FixedPoint::ZERO,
            last_rate: 0,
            last_settle_ts: 0,
        }
    }

    /// Version 1: `1 ‖ cum_index ‖ interval_start ‖ premium_sum ‖ samples ‖
    /// last_sample_ts ‖ last_oracle ‖ last_rate ‖ last_settle_ts`, all BE.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FUNDING_ROW_LEN);
        out.push(ROW_VERSION);
        out.extend_from_slice(&self.cum_index.to_be_bytes());
        out.extend_from_slice(&self.interval_start.to_be_bytes());
        out.extend_from_slice(&self.premium_sum.to_be_bytes());
        out.extend_from_slice(&self.samples.to_be_bytes());
        out.extend_from_slice(&self.last_sample_ts.to_be_bytes());
        out.extend_from_slice(&self.last_oracle.raw().to_be_bytes());
        out.extend_from_slice(&self.last_rate.to_be_bytes());
        out.extend_from_slice(&self.last_settle_ts.to_be_bytes());
        out
    }

    /// The row of [`Self::encode`]; any other length or version is a
    /// `CoreError::Borsh` (corrupt state: the block fail-stops, R02).
    pub fn decode(data: &[u8]) -> Result<Self, CoreError> {
        if data.len() != FUNDING_ROW_LEN || data[0] != ROW_VERSION {
            return Err(CoreError::Borsh(format!(
                "funding row: {} bytes, version {:?} (expected {FUNDING_ROW_LEN}, {ROW_VERSION})",
                data.len(),
                data.first()
            )));
        }
        let mut at = 1usize;
        let mut take = |n: usize| {
            let s = &data[at..at + n];
            at += n;
            s
        };
        let i128_of = |b: &[u8]| i128::from_be_bytes(b.try_into().expect("16 bytes"));
        let u64_of = |b: &[u8]| u64::from_be_bytes(b.try_into().expect("8 bytes"));
        let cum_index = i128_of(take(16));
        let interval_start = u64_of(take(8));
        let premium_sum = i128_of(take(16));
        let samples = u32::from_be_bytes(take(4).try_into().expect("4 bytes"));
        let last_sample_ts = u64_of(take(8));
        let last_oracle = FixedPoint::from_raw(i128_of(take(16)));
        let last_rate = i128_of(take(16));
        let last_settle_ts = u64_of(take(8));
        Ok(Self {
            cum_index,
            interval_start,
            premium_sum,
            samples,
            last_sample_ts,
            last_oracle,
            last_rate,
            last_settle_ts,
        })
    }

    /// The rate the current interval would pay now (0 without samples):
    /// [`rate_e18`] of the average premium (truncated toward zero).
    pub fn predicted_rate(&self, params: &FundingParams) -> i128 {
        if self.samples == 0 {
            return 0;
        }
        rate_e18(self.premium_sum / i128::from(self.samples), params)
    }

    /// When the current interval settles (the first block at or after it).
    pub fn next_funding_time(&self, params: &FundingParams) -> u64 {
        self.interval_start.saturating_add(params.interval_secs)
    }
}

/// `m`'s funding row (`None`: the market has none yet).
pub fn read_row<T: StateBackend>(state: &T, market_id: MarketId) -> Result<Option<FundingRow>, CoreError> {
    match state.get_cf_raw(CF_NATIVE_ORACLE, &funding_key(market_id))? {
        Some(data) => FundingRow::decode(&data).map(Some),
        None => Ok(None),
    }
}

pub fn write_row<T: StateBackend>(state: &T, market_id: MarketId, row: &FundingRow) -> Result<(), CoreError> {
    state.put_cf_raw(CF_NATIVE_ORACLE, &funding_key(market_id), &row.encode())?;
    Ok(())
}

/// `m`'s cumulative index (`None`: no funding row).
pub fn read_index<T: StateBackend>(state: &T, market_id: MarketId) -> Result<Option<i128>, CoreError> {
    Ok(read_row(state, market_id)?.map(|r| r.cum_index))
}

/// The rate of one interval for an average premium (both scaled
/// [`INDEX_SCALE`]): `F8 = avg + clamp(interest − avg, ±clamp)`, times
/// `interval / 8 h` (truncated toward zero), capped at `±max_rate_bps`.
pub fn rate_e18(avg_premium: i128, params: &FundingParams) -> i128 {
    const PPM: i128 = INDEX_SCALE / 1_000_000;
    const BPS: i128 = INDEX_SCALE / 10_000;
    let interest = i128::from(params.interest_ppm_8h) * PPM;
    let clamp = i128::from(params.clamp_ppm_8h) * PPM;
    // |avg| <= 1 (premiums are clamped to ±1): no overflow anywhere below.
    let f8 = avg_premium + (interest - avg_premium).clamp(-clamp, clamp);
    let rate = mul_div(f8, i128::from(params.interval_secs), i128::from(BASIS_SECS), false);
    let cap = i128::from(params.max_rate_bps) * BPS;
    rate.clamp(-cap, cap)
}

/// One premium sample, scaled [`INDEX_SCALE`]:
/// `(max(bid − oracle, 0) − max(oracle − ask, 0)) / oracle`, clamped to ±1.
/// A side without an impact price contributes 0. `oracle > 0`.
pub fn premium_e18(impact_bid: Option<FixedPoint>, impact_ask: Option<FixedPoint>, oracle: FixedPoint) -> i128 {
    let o = oracle.raw();
    if o <= 0 {
        return 0;
    }
    let up = impact_bid.map_or(0, |b| b.raw().saturating_sub(o).max(0));
    let down = impact_ask.map_or(0, |a| o.saturating_sub(a.raw()).max(0));
    mul_div(up - down, INDEX_SCALE, o, false).clamp(-INDEX_SCALE, INDEX_SCALE)
}

/// Average fill price of `notional` (quote) taken from `orders` (price,
/// remaining qty), best first, reading at most `max_orders` orders. `None`:
/// the orders cannot fill it (or `notional <= 0`).
pub fn impact_price(
    orders: impl IntoIterator<Item = (FixedPoint, FixedPoint)>,
    notional: FixedPoint,
    max_orders: usize,
) -> Option<FixedPoint> {
    if notional <= FixedPoint::ZERO {
        return None;
    }
    let mut left = notional;
    let mut qty = FixedPoint::ZERO;
    for (price, q) in orders.into_iter().take(max_orders) {
        if price <= FixedPoint::ZERO || q <= FixedPoint::ZERO {
            continue;
        }
        let value = price.checked_mul(q).ok()?;
        if value >= left {
            // The last order fills the rest: `left / price` of it.
            qty = qty.checked_add(left.checked_div(price).ok()?).ok()?;
            left = FixedPoint::ZERO;
            break;
        }
        qty = qty.checked_add(q).ok()?;
        left = left.checked_sub(value).ok()?;
    }
    if left > FixedPoint::ZERO || qty <= FixedPoint::ZERO {
        return None;
    }
    notional.checked_div(qty).ok()
}

/// `(impact bid, impact ask)` of `book` for `notional`.
pub fn book_impact_prices(book: &OrderBook, notional: FixedPoint) -> (Option<FixedPoint>, Option<FixedPoint>) {
    let bids = book
        .bid_queues()
        .rev()
        .flat_map(|(px, q)| q.iter().map(move |o| (*px, o.remaining_qty)));
    let asks = book
        .ask_queues()
        .flat_map(|(px, q)| q.iter().map(move |o| (*px, o.remaining_qty)));
    (
        impact_price(bids, notional, IMPACT_WALK_MAX_ORDERS),
        impact_price(asks, notional, IMPACT_WALK_MAX_ORDERS),
    )
}

/// The index step of one settlement: `rate × price` (truncated toward zero),
/// scaled [`INDEX_SCALE`].
pub fn index_delta(rate: i128, price: FixedPoint) -> i128 {
    mul_div(rate, price.raw(), FixedPoint::SCALE, false)
}

/// What a position of `size` owes from `entry` to `index` (positive = it
/// pays): `±size × (index − entry)`, rounded UP (a payer pays at least, a
/// receiver gets at most the exact amount: funding never creates value).
pub fn owed(is_long: bool, size: FixedPoint, entry: i128, index: i128) -> FixedPoint {
    let delta = index.saturating_sub(entry);
    let signed = if is_long { size.raw() } else { -size.raw() };
    FixedPoint::from_raw(mul_div(signed, delta, INDEX_SCALE, true))
}

/// What [`step`] did to one market's row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub row: FundingRow,
    /// The row must be written (created, sampled, settled).
    pub changed: bool,
    /// A premium sample was added.
    pub sampled: bool,
    /// The interval settled at this rate.
    pub settled: Option<i128>,
}

/// One market's block-start funding step at block time `now`: settle the
/// interval if it is over (index += rate × price, the mark or else the last
/// sampled oracle price), then take the premium sample if its slot is due
/// (`impact` is called only then, and only with a usable `mark`).
pub fn step(
    row: Option<FundingRow>,
    now: u64,
    params: &FundingParams,
    mark: Option<FixedPoint>,
    impact: impl FnOnce() -> (Option<FixedPoint>, Option<FixedPoint>),
) -> Step {
    let mut changed = row.is_none();
    let mut r = row.unwrap_or_else(|| FundingRow::new(now, params));
    let mut settled = None;
    if now >= r.interval_start.saturating_add(params.interval_secs) {
        let rate = r.predicted_rate(params);
        if r.samples > 0 {
            let price = mark.unwrap_or(r.last_oracle);
            if price > FixedPoint::ZERO {
                r.cum_index = r.cum_index.saturating_add(index_delta(rate, price));
            }
        }
        r.last_rate = rate;
        r.last_settle_ts = now;
        r.interval_start = align(now, params.interval_secs);
        r.premium_sum = 0;
        r.samples = 0;
        settled = Some(rate);
        changed = true;
    }
    let slot = |ts: u64| ts / params.sample_secs.max(1);
    let mut sampled = false;
    if r.last_sample_ts == 0 || slot(now) > slot(r.last_sample_ts) {
        r.last_sample_ts = now;
        changed = true;
        if let Some(oracle) = mark.filter(|m| *m > FixedPoint::ZERO) {
            let (bid, ask) = impact();
            r.premium_sum = r.premium_sum.saturating_add(premium_e18(bid, ask, oracle));
            r.samples = r.samples.saturating_add(1);
            r.last_oracle = oracle;
            sampled = true;
        }
    }
    Step { row: r, changed, sampled, settled }
}

/// `ts` aligned down to a multiple of `interval` (> 0; 0 reads as 1).
fn align(ts: u64, interval: u64) -> u64 {
    let interval = interval.max(1);
    ts - ts % interval
}

/// `a × b / d` over 256 bits, truncated toward zero (`up`: rounded toward
/// +∞), saturated to i128. `d > 0`.
fn mul_div(a: i128, b: i128, d: i128, up: bool) -> i128 {
    let wide = |x: i128| U256::from(x.unsigned_abs());
    let p = wide(a) * wide(b);
    let (q, r) = p.div_rem(wide(d));
    let negative = (a < 0) ^ (b < 0) && !p.is_zero();
    let mut q = if q > U256::from(i128::MAX as u128) { U256::from(i128::MAX as u128) } else { q };
    // Toward +∞: a positive inexact quotient rounds away from zero; a
    // negative one is already toward +∞ when truncated.
    if up && !negative && !r.is_zero() && q < U256::from(i128::MAX as u128) {
        q += U256::from(1u8);
    }
    let q = q.to::<u128>() as i128;
    if negative {
        -q
    } else {
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const E18: i128 = INDEX_SCALE;

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(i128::from(v) * FixedPoint::SCALE)
    }

    fn params() -> FundingParams {
        FundingParams::default()
    }

    #[test]
    fn defaults_are_hl_and_stored_values_apply_inside_their_ranges() {
        let p = params();
        assert_eq!(
            (p.interval_secs, p.sample_secs, p.impact_notional, p.interest_ppm_8h, p.clamp_ppm_8h, p.max_rate_bps),
            (3_600, 5, fp(10_000), 100, 500, 400)
        );
        let stored = |k: &str| match k {
            "funding_interval_secs" => Some(b"1800".to_vec()),
            "funding_sample_secs" => Some(b"0".to_vec()), // out of range: default
            "funding_max_rate_bps" => Some(b"abc".to_vec()), // invalid: default
            _ => None,
        };
        let p = FundingParams::from_stored(stored);
        assert_eq!((p.interval_secs, p.sample_secs, p.max_rate_bps), (1_800, 5, 400));
    }

    /// RED before the funding engine: the row did not round-trip.
    #[test]
    fn row_codec_round_trips_and_refuses_bad_rows() {
        let row = FundingRow {
            cum_index: -12_345_678_901_234_567_890,
            interval_start: 7_200,
            premium_sum: 3 * E18 / 1000,
            samples: 720,
            last_sample_ts: 10_799,
            last_oracle: fp(61_234),
            last_rate: -E18 / 10_000,
            last_settle_ts: 7_201,
        };
        let bytes = row.encode();
        assert_eq!(bytes.len(), FUNDING_ROW_LEN);
        assert_eq!(bytes[0], 1, "version byte");
        assert_eq!(FundingRow::decode(&bytes).unwrap(), row);
        assert!(FundingRow::decode(&bytes[..FUNDING_ROW_LEN - 1]).is_err(), "short");
        let mut long = bytes.clone();
        long.push(0);
        assert!(FundingRow::decode(&long).is_err(), "trailing byte");
        let mut v = bytes;
        v[0] = 9;
        assert!(FundingRow::decode(&v).is_err(), "unknown version");
        assert_eq!(&funding_key(0x0102)[..], b"fnd\x00\x00\x00\x00\x00\x00\x01\x02");
    }

    #[test]
    fn new_row_aligns_the_interval_down() {
        let r = FundingRow::new(7_250, &params());
        assert_eq!((r.cum_index, r.interval_start, r.samples, r.last_sample_ts), (0, 7_200, 0, 0));
        assert_eq!(r.next_funding_time(&params()), 10_800);
    }

    /// HL: interest 0.01% / 8 h, clamp 0.05%, 1/8 paid per hour, cap 4%.
    #[test]
    fn rate_formula_interest_clamp_and_cap() {
        let p = params();
        let ppm = E18 / 1_000_000;
        // No premium: interest only, 100 ppm / 8 = 12.5 ppm per hour.
        assert_eq!(rate_e18(0, &p), 100 * ppm / 8);
        // A premium inside the clamp band: the interest wins (F8 = I).
        assert_eq!(rate_e18(300 * ppm, &p), 100 * ppm / 8);
        // Above: F8 = P − clamp.
        assert_eq!(rate_e18(2_000 * ppm, &p), (2_000 - 500) * ppm / 8);
        // Below: F8 = P + clamp.
        assert_eq!(rate_e18(-2_000 * ppm, &p), (-2_000 + 500) * ppm / 8);
        // Cap: |rate| <= 4%.
        assert_eq!(rate_e18(E18, &p), 4 * E18 / 100);
        assert_eq!(rate_e18(-E18, &p), -4 * E18 / 100);
        // Interval scales the 8 h rate; truncation toward zero.
        let half = FundingParams { interval_secs: 1_800, ..p };
        assert_eq!(rate_e18(-2_000 * ppm, &half), -(1_500 * ppm) / 16);
        let odd = FundingParams { interval_secs: 7, interest_ppm_8h: 0, clamp_ppm_8h: 0, ..p };
        assert_eq!(rate_e18(-1_000_003, &odd), -(1_000_003 * 7 / 28_800));
    }

    #[test]
    fn premium_uses_each_side_only_when_it_crosses_the_oracle() {
        let o = fp(100);
        // Book above the oracle: bid impact 101 → +1%.
        assert_eq!(premium_e18(Some(fp(101)), Some(fp(102)), o), E18 / 100);
        // Below: ask impact 98 → −2%.
        assert_eq!(premium_e18(Some(fp(97)), Some(fp(98)), o), -2 * E18 / 100);
        // Around the oracle: 0.
        assert_eq!(premium_e18(Some(fp(99)), Some(fp(101)), o), 0);
        // A missing side is 0.
        assert_eq!(premium_e18(None, Some(fp(98)), o), -2 * E18 / 100);
        assert_eq!(premium_e18(Some(fp(101)), None, o), E18 / 100);
        assert_eq!(premium_e18(None, None, o), 0);
        // Clamped to ±100%.
        assert_eq!(premium_e18(Some(fp(1_000)), None, o), E18);
        // Truncated toward zero: 1/3 %.
        let third = premium_e18(Some(FixedPoint::from_raw(100 * FixedPoint::SCALE + FixedPoint::SCALE / 3)), None, o);
        assert_eq!(third, (FixedPoint::SCALE / 3) * E18 / (100 * FixedPoint::SCALE));
    }

    #[test]
    fn impact_walk_averages_the_notional_and_respects_its_budget() {
        // 10 @ 100 then 10 @ 90: 1500 notional = 10 @ 100 + 500/90 @ 90.
        let orders = [(fp(100), fp(10)), (fp(90), fp(10))];
        let q2 = fp(500) / fp(90);
        let want = fp(1_500) / (fp(10) + q2);
        assert_eq!(impact_price(orders, fp(1_500), 10), Some(want));
        // Exactly the first order.
        assert_eq!(impact_price(orders, fp(1_000), 10), Some(fp(100)));
        // Not enough depth, or not within the budget.
        assert_eq!(impact_price(orders, fp(2_000), 10), None);
        assert_eq!(impact_price(orders, fp(1_500), 1), None);
        assert_eq!(impact_price(Vec::new(), fp(1), 10), None);
        assert_eq!(impact_price(orders, FixedPoint::ZERO, 10), None);
    }

    #[test]
    fn book_impact_prices_walk_bids_down_and_asks_up() {
        use crate::order_book::OrderBook;
        let mut book = OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1));
        let t = torus_types::Address::new([7; 20]);
        for (buy, px, qty) in [(true, 99, 5), (true, 98, 100), (false, 101, 5), (false, 103, 100)] {
            let p = torus_types::PlaceOrderParams {
                market_id: 1,
                is_buy: buy,
                price: fp(px),
                quantity: fp(qty),
                order_type: torus_types::OrderType::Limit,
                time_in_force: torus_types::TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            };
            book.place_order(p, t, 1);
        }
        let (bid, ask) = book_impact_prices(&book, fp(990));
        // Bids: 5 @ 99 (495) then 495 / 98 @ 98.
        assert_eq!(bid, Some(fp(990) / (fp(5) + fp(495) / fp(98))));
        // Asks: 5 @ 101 (505) then 485 / 103 @ 103.
        assert_eq!(ask, Some(fp(990) / (fp(5) + fp(485) / fp(103))));
    }

    #[test]
    fn index_step_and_owed_round_against_the_holder() {
        // 0.01% of 60000 = 6 quote per unit.
        let rate = E18 / 10_000;
        assert_eq!(index_delta(rate, fp(60_000)), 6 * E18);
        assert_eq!(index_delta(-rate, fp(60_000)), -6 * E18);
        // Low price: 1.25e-5 × 0.001 keeps its digits at 1e18.
        let tiny = FixedPoint::from_raw(FixedPoint::SCALE / 1_000);
        assert_eq!(index_delta(12_500_000_000_000, tiny), 12_500_000_000);
        // 2 units long, index +6: pays 12; the short receives 12.
        assert_eq!(owed(true, fp(2), 0, 6 * E18), fp(12));
        assert_eq!(owed(false, fp(2), 0, 6 * E18), -fp(12));
        // Inexact: 1 raw unit × 1.5e-18-ish → the payer rounds up, the
        // receiver's credit rounds toward zero (both toward +∞).
        let size = FixedPoint::from_raw(3);
        let idx = E18 / 2; // 1.5 raw owed exactly
        assert_eq!(owed(true, size, 0, idx), FixedPoint::from_raw(2));
        assert_eq!(owed(false, size, 0, idx), FixedPoint::from_raw(-1));
        // Entry index matters, and an index below it reverses the flow.
        assert_eq!(owed(true, fp(1), 10 * E18, 4 * E18), -fp(6));
        assert_eq!(owed(true, fp(1), 4 * E18, 4 * E18), FixedPoint::ZERO);
        // Σ owed over a balanced book never pays out more than it takes.
        for idx in [E18 / 3, -E18 / 7, 12_345, -1] {
            let longs = owed(true, FixedPoint::from_raw(7), 0, idx) + owed(true, FixedPoint::from_raw(5), 0, idx);
            let short = owed(false, FixedPoint::from_raw(12), 0, idx);
            assert!(longs + short >= FixedPoint::ZERO, "{idx}: {longs:?} + {short:?}");
        }
    }

    fn sample_impact() -> (Option<FixedPoint>, Option<FixedPoint>) {
        (Some(fp(101)), Some(fp(102))) // +1% premium at oracle 100
    }

    #[test]
    fn step_creates_the_row_and_samples_on_the_timestamp_cadence() {
        let p = params();
        let s = step(None, 1_000, &p, Some(fp(100)), sample_impact);
        assert!(s.changed && s.sampled && s.settled.is_none());
        assert_eq!((s.row.interval_start, s.row.samples, s.row.last_sample_ts), (0, 1, 1_000));
        assert_eq!(s.row.premium_sum, E18 / 100);
        assert_eq!(s.row.last_oracle, fp(100));
        // Same 5 s slot (1000..1004): nothing, and the book is not read.
        let again = step(Some(s.row), 1_004, &p, Some(fp(100)), || panic!("no sample due"));
        assert!(!again.changed && !again.sampled);
        // Time going backwards never samples again.
        let back = step(Some(s.row), 990, &p, Some(fp(100)), || panic!("no sample due"));
        assert!(!back.changed);
        // Next slot.
        let next = step(Some(s.row), 1_005, &p, Some(fp(100)), sample_impact);
        assert!(next.sampled);
        assert_eq!((next.row.samples, next.row.premium_sum), (2, 2 * E18 / 100));
        // No fresh mark: the slot is consumed, nothing is added, no book read.
        let stale = step(Some(next.row), 1_010, &p, None, || panic!("no book read without a mark"));
        assert!(stale.changed && !stale.sampled);
        assert_eq!((stale.row.samples, stale.row.last_sample_ts), (2, 1_010));
    }

    #[test]
    fn step_settles_once_per_interval_into_the_index() {
        let p = params();
        // Activation on an interval boundary: every interval is whole (the
        // first-interval pro-rating has its own test).
        let mut row = step(None, 0, &p, Some(fp(100)), sample_impact).row;
        row = step(Some(row), 2_000, &p, Some(fp(100)), sample_impact).row;
        assert_eq!(row.samples, 2);
        // avg +1%: F8 = 1% − 0.05%, hourly = 0.95% / 8.
        let rate = rate_e18(E18 / 100, &p);
        assert_eq!(row.predicted_rate(&p), rate);
        // 3600: settle at the mark 200 (the mark of the settling block), then
        // sample into the new interval.
        let s = step(Some(row), 3_600, &p, Some(fp(200)), sample_impact);
        assert_eq!(s.settled, Some(rate));
        assert_eq!(s.row.cum_index, index_delta(rate, fp(200)));
        assert_eq!((s.row.interval_start, s.row.last_rate, s.row.last_settle_ts), (3_600, rate, 3_600));
        assert_eq!(s.row.samples, 1, "the settling block samples the new interval");
        // A halt over several intervals pays one interval, once.
        let late = step(Some(s.row), 20_000, &p, None, sample_impact);
        // The 3600 sample was at mark 200: premium (0 − (200 − 102)) / 200.
        let p3600 = premium_e18(Some(fp(101)), Some(fp(102)), fp(200));
        assert_eq!(p3600, -49 * E18 / 100);
        assert_eq!(
            late.row.cum_index,
            s.row.cum_index + index_delta(rate_e18(p3600, &p), fp(200)),
            "the last sampled oracle prices the settlement when the mark is stale"
        );
        assert_eq!(late.row.interval_start, 18_000);
        let again = step(Some(late.row), 20_001, &p, None, sample_impact);
        assert_eq!(again.settled, None);
        // An interval without samples settles at rate 0.
        let empty = step(Some(FundingRow::new(0, &p)), 3_600, &p, None, sample_impact);
        assert_eq!((empty.settled, empty.row.cum_index), (Some(0), 0));
    }

    /// Owner s138 (option B): the interval containing the activation pays
    /// `rate × (interval end − activation) / interval`, so a market activated
    /// late in an interval does not pay a whole interval for a fraction of
    /// it. Later intervals are whole. RED before: the first interval paid the
    /// whole rate.
    #[test]
    fn step_pro_rates_the_first_interval_after_activation() {
        let p = params();
        let full = rate_e18(E18 / 100, &p);
        let first = full * 2_600 / 3_600; // truncated toward zero
        assert_ne!(first, full);
        // Activation at 1000: the interval 0..3600 accrues 2600 s.
        let mut row = step(None, 1_000, &p, Some(fp(100)), sample_impact).row;
        row = step(Some(row), 2_000, &p, Some(fp(100)), sample_impact).row;
        assert_eq!(row.predicted_rate(&p), first, "the prediction is pro-rated too");
        let s = step(Some(row), 3_600, &p, Some(fp(100)), sample_impact);
        assert_eq!((s.settled, s.row.last_rate), (Some(first), first));
        assert_eq!(s.row.cum_index, index_delta(first, fp(100)));
        // The next interval is whole.
        let r2 = step(Some(s.row), 3_700, &p, Some(fp(100)), sample_impact).row;
        assert_eq!(r2.predicted_rate(&p), full);
        assert_eq!(step(Some(r2), 7_200, &p, Some(fp(100)), sample_impact).settled, Some(full));
        // A late first settlement (a halt) still pays the activation's share
        // of one interval.
        let late = step(Some(row), 20_000, &p, None, sample_impact);
        assert_eq!(late.settled, Some(first));
        // Activation on a boundary: whole.
        let b = step(None, 3_600, &p, Some(fp(100)), sample_impact).row;
        assert_eq!(step(Some(b), 7_200, &p, Some(fp(100)), sample_impact).settled, Some(full));
        // A negative rate is pro-rated toward zero as well.
        let neg = |ts| step(None, ts, &p, Some(fp(100)), || (Some(fp(97)), Some(fp(98)))).row;
        let full_neg = rate_e18(-2 * E18 / 100, &p);
        assert!(full_neg < 0);
        assert_eq!(step(Some(neg(2_700)), 3_600, &p, None, sample_impact).settled, Some(full_neg * 900 / 3_600));
    }

    #[test]
    fn mul_div_rounds_as_asked() {
        assert_eq!(mul_div(7, 3, 2, false), 10);
        assert_eq!(mul_div(7, 3, 2, true), 11);
        assert_eq!(mul_div(-7, 3, 2, false), -10);
        assert_eq!(mul_div(-7, 3, 2, true), -10);
        assert_eq!(mul_div(i128::MAX, i128::MAX, 1, false), i128::MAX);
    }
}
