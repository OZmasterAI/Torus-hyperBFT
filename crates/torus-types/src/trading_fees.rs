//! Maker / taker trading fees (docs/plans/trading-fees.md), P0.0 feature
//! [`crate::Feature::TradingFees`].
//!
//! Rates are global, in ppm of notional (1 ppm = 0.0001 %). They are two
//! governance keys in `CF_FEE_CONFIG` (decimal strings, `ParameterChange`)
//! with built-in defaults; governance validates them (torus-economics) and
//! the executor / RPC read them (both depend on torus-types).
//!
//! Rounding: a charge rounds up, a rebate rounds toward zero, so rounding
//! always favours the protocol; the treasury is credited the exact sum of
//! what the sides were charged (no dust).

use std::ops::RangeInclusive;

use crate::FixedPoint;

/// Governance key of the taker rate, ppm of notional.
pub const TAKER_FEE_PARAM: &str = "trading_taker_fee_ppm";
/// Governance key of the maker rate, ppm of notional (negative = rebate).
pub const MAKER_FEE_PARAM: &str = "trading_maker_fee_ppm";
/// Taker rate when the key is absent or invalid: 0.045 % (HL perps tier 0).
pub const TAKER_FEE_DEFAULT_PPM: i64 = 450;
/// Maker rate when the key is absent or invalid: 0.015 % (HL perps tier 0).
pub const MAKER_FEE_DEFAULT_PPM: i64 = 150;
/// Valid taker values: 0 .. 1 %.
pub const TAKER_FEE_PPM_RANGE: RangeInclusive<i64> = 0..=10_000;
/// Valid maker values: a rebate of at most 0.1 % .. a fee of 1 %.
pub const MAKER_FEE_PPM_RANGE: RangeInclusive<i64> = -1_000..=10_000;

const PPM: i128 = 1_000_000;

/// The fee rates in force (ppm of notional).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TradingFeeRates {
    pub taker_ppm: i64,
    /// Never below `-taker_ppm` (see [`Self::new`]).
    pub maker_ppm: i64,
}

impl Default for TradingFeeRates {
    fn default() -> Self {
        Self::new(TAKER_FEE_DEFAULT_PPM, MAKER_FEE_DEFAULT_PPM)
    }
}

impl TradingFeeRates {
    /// The rates with the maker rate clamped to `>= -taker`: every fill nets
    /// a non-negative fee, so the treasury never pays out more than it takes
    /// on a fill.
    pub fn new(taker_ppm: i64, maker_ppm: i64) -> Self {
        Self {
            taker_ppm,
            maker_ppm: maker_ppm.max(-taker_ppm),
        }
    }

    /// The rates for the stored values of [`TAKER_FEE_PARAM`] and
    /// [`MAKER_FEE_PARAM`]: each the value when it is a decimal in its range,
    /// else its default; then [`Self::new`]'s clamp.
    pub fn from_stored(taker: Option<&[u8]>, maker: Option<&[u8]>) -> Self {
        Self::new(
            stored_i64_in(taker, &TAKER_FEE_PPM_RANGE).unwrap_or(TAKER_FEE_DEFAULT_PPM),
            stored_i64_in(maker, &MAKER_FEE_PPM_RANGE).unwrap_or(MAKER_FEE_DEFAULT_PPM),
        )
    }

    /// The taker's fee on a fill of `notional`.
    pub fn taker_fee(&self, notional: FixedPoint) -> FixedPoint {
        FixedPoint::from_raw(fee_on_notional(notional.raw(), self.taker_ppm))
    }

    /// The maker's fee on a fill of `notional` (negative = rebate).
    pub fn maker_fee(&self, notional: FixedPoint) -> FixedPoint {
        FixedPoint::from_raw(fee_on_notional(notional.raw(), self.maker_ppm))
    }
}

/// A stored governance value: the signed decimal when it is in `range`.
pub fn stored_i64_in(stored: Option<&[u8]>, range: &RangeInclusive<i64>) -> Option<i64> {
    stored
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|v| range.contains(v))
}

/// The fee (raw `FixedPoint`) on a notional of `notional_raw` at `ppm`:
/// `notional * ppm / 1e6`, rounded up for a charge (`ppm > 0`) and toward
/// zero for a rebate (`ppm < 0`). Exact, no intermediate overflow (the
/// notional is split into whole millions and a remainder); saturates only
/// for a notional near `i128::MAX`. A non-positive notional pays nothing.
pub fn fee_on_notional(notional_raw: i128, ppm: i64) -> i128 {
    if notional_raw <= 0 || ppm == 0 {
        return 0;
    }
    let rate = i128::from(ppm.unsigned_abs());
    let whole = (notional_raw / PPM).saturating_mul(rate);
    let rest = (notional_raw % PPM) * rate; // < 1e6 * 2^63: fits
    if ppm > 0 {
        whole.saturating_add((rest + PPM - 1) / PPM)
    } else {
        -whole.saturating_add(rest / PPM)
    }
}

/// Key of a trader's lifetime net trading fees in `CF_NATIVE_BALANCES`:
/// `b"cfee"`(4) + trader(20) = 24 bytes (next to the `cvlm` row). Value:
/// raw signed `FixedPoint`, 16 bytes BE.
pub fn cum_fee_key(trader: &crate::Address) -> [u8; 24] {
    let mut key = [0u8; 24];
    key[..4].copy_from_slice(b"cfee");
    key[4..].copy_from_slice(trader.as_slice());
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i128 = FixedPoint::SCALE;

    #[test]
    fn charge_rounds_up_rebate_rounds_toward_zero() {
        // 1000 quote at 450 ppm = 0.45 exactly.
        assert_eq!(fee_on_notional(1_000 * S, 450), 45 * S / 100);
        // 1 raw unit at 1 ppm: a charge of 1 raw, a rebate of 0.
        assert_eq!(fee_on_notional(1, 1), 1);
        assert_eq!(fee_on_notional(1, -1), 0);
        // 1_999_999 raw at 1 ppm = 1.999999 raw: 2 up, -1 toward zero.
        assert_eq!(fee_on_notional(1_999_999, 1), 2);
        assert_eq!(fee_on_notional(1_999_999, -1), -1);
        // Exact multiples carry no rounding.
        assert_eq!(fee_on_notional(3_000_000, 7), 21);
        assert_eq!(fee_on_notional(3_000_000, -7), -21);
        assert_eq!(fee_on_notional(0, 450), 0);
        assert_eq!(fee_on_notional(-5, 450), 0);
        assert_eq!(fee_on_notional(5, 0), 0);
    }

    #[test]
    fn huge_notional_does_not_overflow() {
        use ethnum::i256;
        for n in [i128::MAX / 2, i128::MAX] {
            let num = i256::from(n) * i256::from(10_000);
            let floor = (num / i256::from(1_000_000)).as_i128();
            let ceil = ((num + i256::from(999_999)) / i256::from(1_000_000)).as_i128();
            assert_eq!(fee_on_notional(n, 10_000), ceil);
            assert_eq!(fee_on_notional(n, -1_000), -((i256::from(n) * i256::from(1_000) / i256::from(1_000_000)).as_i128()));
            assert!(ceil >= floor);
        }
    }

    #[test]
    fn fee_matches_exact_ceiling_over_a_sweep() {
        for n in [1i128, 7, 999_999, 1_000_000, 1_000_001, 123_456_789_012, 10i128.pow(30) + 3] {
            for ppm in [1i64, 3, 150, 450, 9_999, 10_000, -1, -150, -1_000] {
                let num = n * i128::from(ppm.abs());
                let want = if ppm > 0 { (num + 999_999) / 1_000_000 } else { -(num / 1_000_000) };
                assert_eq!(fee_on_notional(n, ppm), want, "n={n} ppm={ppm}");
            }
        }
    }

    #[test]
    fn stored_rates_default_when_absent_or_invalid_and_maker_is_clamped() {
        let d = TradingFeeRates::from_stored(None, None);
        assert_eq!((d.taker_ppm, d.maker_ppm), (450, 150));
        assert_eq!(d, TradingFeeRates::default());
        let r = TradingFeeRates::from_stored(Some(b"200"), Some(b"-50"));
        assert_eq!((r.taker_ppm, r.maker_ppm), (200, -50));
        // Out of range / garbage: the default for that key only.
        let r = TradingFeeRates::from_stored(Some(b"10001"), Some(b"x"));
        assert_eq!((r.taker_ppm, r.maker_ppm), (450, 150));
        let r = TradingFeeRates::from_stored(Some(b"-1"), Some(b"-1001"));
        assert_eq!((r.taker_ppm, r.maker_ppm), (450, 150));
        // A rebate larger than the taker fee is clamped to it.
        let r = TradingFeeRates::from_stored(Some(b"100"), Some(b"-300"));
        assert_eq!((r.taker_ppm, r.maker_ppm), (100, -100));
        let r = TradingFeeRates::from_stored(Some(b"0"), Some(b"-300"));
        assert_eq!((r.taker_ppm, r.maker_ppm), (0, 0));
    }

    #[test]
    fn every_fill_nets_a_non_negative_fee() {
        for (t, m) in [(100, -100), (450, 150), (1, -1), (0, 0), (10_000, -1_000)] {
            let r = TradingFeeRates::new(t, m);
            for n in [1i128, 999_999, 1_000_001, 77 * S + 3] {
                let n = FixedPoint::from_raw(n);
                assert!(r.taker_fee(n).raw() + r.maker_fee(n).raw() >= 0, "{t} {m} {n:?}");
            }
        }
    }

    #[test]
    fn cum_fee_key_is_cfee_then_trader() {
        let a = crate::Address::repeat_byte(0xab);
        let k = cum_fee_key(&a);
        assert_eq!(&k[..4], b"cfee");
        assert_eq!(&k[4..], a.as_slice());
    }
}
