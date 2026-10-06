//! Integer-only price math: decimal parsing, mid, lower weighted median, and
//! per-market aggregation.

use std::collections::BTreeMap;

pub use torus_types::FixedPoint;

use crate::config::{Exchange, Quote, QuoteMode};

/// Parse an unsigned decimal ("65012.34", ".5", "2.") digit by digit into a
/// `FixedPoint`, truncating past 8 places. Signs, exponents, whitespace and
/// overflow are rejected.
pub fn parse_price(s: &str) -> Option<FixedPoint> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    if !whole.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut w: i128 = 0;
    for b in whole.bytes() {
        w = w.checked_mul(10)?.checked_add(i128::from(b - b'0'))?;
    }
    let mut f: i128 = 0;
    for i in 0..FixedPoint::DECIMALS as usize {
        let d = frac.as_bytes().get(i).map_or(0, |b| b - b'0');
        f = f * 10 + i128::from(d);
    }
    Some(FixedPoint::from_raw(w.checked_mul(FixedPoint::SCALE)?.checked_add(f)?))
}

/// (bid + ask) / 2 on raw values; `None` unless 0 < bid <= ask.
pub fn mid(bid: FixedPoint, ask: FixedPoint) -> Option<FixedPoint> {
    (bid.raw() > 0 && bid.raw() <= ask.raw())
        .then(|| FixedPoint::from_raw(bid.raw() + (ask.raw() - bid.raw()) / 2))
}

/// Lower weighted median: the first price (ascending) where 2·cum >= total.
pub fn weighted_median(points: &mut [(FixedPoint, u32)]) -> Option<FixedPoint> {
    points.sort_by_key(|p| p.0.raw());
    let total: u64 = points.iter().map(|p| u64::from(p.1)).sum();
    let mut cum = 0u64;
    for &(p, w) in points.iter() {
        cum += u64::from(w);
        if total > 0 && 2 * cum >= total {
            return Some(p);
        }
    }
    None
}

/// One venue's mid for one market.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub exchange: Exchange,
    pub mid: FixedPoint,
    pub quote: Quote,
    pub fetched_at_ms: u64,
}

#[derive(Clone, Debug)]
pub struct Rules {
    pub min_sources: usize,
    pub min_weight_bps: u32,
    pub max_age_ms: u64,
    pub quote_mode: QuoteMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Price { price: FixedPoint, sources: usize, weight: u64 },
    TooFewSources { have: usize, need: usize },
    TooLittleWeight { have: u64, need: u64 },
}

impl Outcome {
    /// Human-readable omission reason (`None` for a price).
    pub fn reason(&self) -> Option<String> {
        match self {
            Outcome::Price { .. } => None,
            Outcome::TooFewSources { have, need } => Some(format!("too few sources ({have} < {need})")),
            Outcome::TooLittleWeight { have, need } => Some(format!("too little weight ({have} < {need})")),
        }
    }
}

/// Aggregate one market. `weights` = the market's configured (enabled) venues
/// and their weights; samples from other venues are ignored. A sample counts
/// iff `now - fetched_at <= max_age` and its mid is > 0; USDT mids are scaled
/// by `usdt_usd` in `kraken_usdt` mode (dropped without a rate or on overflow).
pub fn aggregate(
    samples: &[Sample],
    weights: &BTreeMap<Exchange, u32>,
    usdt_usd: Option<FixedPoint>,
    now_ms: u64,
    rules: &Rules,
) -> Outcome {
    let total: u64 = weights.values().map(|w| u64::from(*w)).sum();
    // Review L5: all weight sums are u64 (no overflow, even before the config cap).
    let need_w = total.saturating_mul(u64::from(rules.min_weight_bps)).div_ceil(10_000);
    let mut pts: Vec<(FixedPoint, u32)> = Vec::new();
    for s in samples {
        let Some(&w) = weights.get(&s.exchange) else { continue };
        if now_ms.saturating_sub(s.fetched_at_ms) > rules.max_age_ms || s.mid.raw() <= 0 {
            continue;
        }
        let usd = match (s.quote, rules.quote_mode) {
            (Quote::Usdt, QuoteMode::KrakenUsdt) => match usdt_usd.map(|r| s.mid.checked_mul(r)) {
                Some(Ok(p)) if p.raw() > 0 => p,
                _ => continue,
            },
            _ => s.mid,
        };
        pts.push((usd, w));
    }
    let have_w: u64 = pts.iter().map(|p| u64::from(p.1)).sum();
    if pts.len() < rules.min_sources {
        return Outcome::TooFewSources { have: pts.len(), need: rules.min_sources };
    }
    if have_w < need_w {
        return Outcome::TooLittleWeight { have: have_w, need: need_w };
    }
    let sources = pts.len();
    match weighted_median(&mut pts) {
        Some(price) => Outcome::Price { price, sources, weight: have_w },
        None => Outcome::TooLittleWeight { have: 0, need: need_w.max(1) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Exchange, Quote, QuoteMode};
    use std::collections::BTreeMap;

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    #[test]
    fn parse_price_cases() {
        assert_eq!(parse_price("65012.34"), Some(FixedPoint::from_raw(6_501_234_000_000)));
        assert_eq!(parse_price("0.000012345678901"), Some(FixedPoint::from_raw(1234)));
        assert_eq!(parse_price("1"), Some(FixedPoint::from_raw(100_000_000)));
        assert_eq!(parse_price(".5"), Some(FixedPoint::from_raw(50_000_000)));
        assert_eq!(parse_price("2."), Some(fp(2)));
        let forty = "9".repeat(40);
        for bad in ["", ".", "-1", "+1", "1e5", "1.2.3", " 1", "1 ", "NaN", "0x10", forty.as_str()] {
            assert_eq!(parse_price(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn mid_cases() {
        assert_eq!(mid(fp(100), fp(102)), Some(fp(101)));
        assert_eq!(mid(fp(100), fp(101)), Some(FixedPoint::from_raw(10_050_000_000)));
        assert_eq!(mid(fp(100), fp(100)), Some(fp(100)));
        assert_eq!(mid(fp(102), fp(100)), None, "crossed");
        assert_eq!(mid(FixedPoint::ZERO, fp(1)), None, "bid 0");
        assert_eq!(mid(fp(-1), fp(1)), None);
    }

    #[test]
    fn weighted_median_cases() {
        // HL example: B=100(3) O=101(2) Y=102(2) K=103 Ku=104 G=105 M=106 (1 each), W=11.
        let hl = [(100, 3), (101, 2), (102, 2), (103, 1), (104, 1), (105, 1), (106, 1)];
        let mut pts: Vec<_> = hl.iter().map(|&(p, w)| (fp(p), w)).collect();
        assert_eq!(weighted_median(&mut pts), Some(fp(102)));
        let mut shuffled: Vec<_> = [6usize, 2, 4, 0, 5, 1, 3].iter().map(|&i| (fp(hl[i].0), hl[i].1)).collect();
        assert_eq!(weighted_median(&mut shuffled), Some(fp(102)));
        assert_eq!(weighted_median(&mut [(fp(100), 1), (fp(200), 1)]), Some(fp(100)));
        assert_eq!(weighted_median(&mut [(fp(7), 4)]), Some(fp(7)));
        assert_eq!(weighted_median(&mut []), None);
        assert_eq!(weighted_median(&mut [(fp(7), 0), (fp(8), 0)]), None);
    }

    fn weights() -> BTreeMap<Exchange, u32> {
        Exchange::ALL.iter().map(|e| (*e, e.default_weight())).collect()
    }

    fn rules(mode: QuoteMode) -> Rules {
        Rules { min_sources: 3, min_weight_bps: 5000, max_age_ms: 5000, quote_mode: mode }
    }

    fn s(ex: Exchange, price: i64, quote: Quote, at: u64) -> Sample {
        Sample { exchange: ex, mid: fp(price), quote, fetched_at_ms: at }
    }

    const NOW: u64 = 1_000_000;

    #[test]
    fn aggregate_thresholds() {
        use Exchange::*;
        let r = rules(QuoteMode::Par);
        // B+O+Y: weight 7 of 11 (need ceil(5.5) = 6) -> price (lower weighted median).
        let ok = [s(Binance, 100, Quote::Usdt, NOW), s(Okx, 101, Quote::Usdt, NOW), s(Bybit, 102, Quote::Usdt, NOW)];
        assert_eq!(aggregate(&ok, &weights(), None, NOW, &r), Outcome::Price { price: fp(101), sources: 3, weight: 7 });
        // K+Ku+G: 3 sources, weight 3 < 6.
        let light = [s(Kraken, 100, Quote::Usd, NOW), s(Kucoin, 101, Quote::Usdt, NOW), s(Gate, 102, Quote::Usdt, NOW)];
        assert_eq!(aggregate(&light, &weights(), None, NOW, &r), Outcome::TooLittleWeight { have: 3, need: 6 });
        assert_eq!(aggregate(&ok[..2], &weights(), None, NOW, &r), Outcome::TooFewSources { have: 2, need: 3 });
    }

    #[test]
    fn aggregate_freshness_boundary() {
        use Exchange::*;
        let r = rules(QuoteMode::Par);
        let mut v = vec![s(Binance, 100, Quote::Usdt, NOW), s(Okx, 100, Quote::Usdt, NOW)];
        v.push(s(Bybit, 100, Quote::Usdt, NOW - 5_001));
        assert_eq!(aggregate(&v, &weights(), None, NOW, &r), Outcome::TooFewSources { have: 2, need: 3 });
        v[2].fetched_at_ms = NOW - 5_000;
        assert!(matches!(aggregate(&v, &weights(), None, NOW, &r), Outcome::Price { sources: 3, .. }));
        // A sample stamped after `now` (same-process clock, raced stamp) has age 0.
        v[2].fetched_at_ms = NOW + 1;
        assert!(matches!(aggregate(&v, &weights(), None, NOW, &r), Outcome::Price { sources: 3, .. }));
    }

    #[test]
    fn aggregate_ignores_unconfigured_venues_and_bad_mids() {
        use Exchange::*;
        let r = rules(QuoteMode::Par);
        let mut w = weights();
        w.remove(&Mexc);
        let v = [s(Binance, 100, Quote::Usdt, NOW), s(Okx, 100, Quote::Usdt, NOW), s(Mexc, 100, Quote::Usdt, NOW), s(Bybit, 0, Quote::Usdt, NOW)];
        assert_eq!(aggregate(&v, &w, None, NOW, &r), Outcome::TooFewSources { have: 2, need: 3 });
    }

    #[test]
    fn aggregate_quote_modes() {
        use Exchange::*;
        let v = [s(Binance, 100, Quote::Usdt, NOW), s(Okx, 100, Quote::Usdt, NOW), s(Kraken, 99, Quote::Usd, NOW), s(Bybit, 100, Quote::Usdc, NOW)];
        // par: USDT/USDC count 1:1. Sorted 99(1) 100(3+2+2): median 100.
        assert_eq!(aggregate(&v, &weights(), None, NOW, &rules(QuoteMode::Par)), Outcome::Price { price: fp(100), sources: 4, weight: 8 });
        // kraken_usdt: USDT mids x 0.98 -> 98 (w 5), Kraken 99 (1), USDC 100 (2): median 98.
        let rate = FixedPoint::from_raw(98_000_000);
        assert_eq!(
            aggregate(&v, &weights(), Some(rate), NOW, &rules(QuoteMode::KrakenUsdt)),
            Outcome::Price { price: fp(98), sources: 4, weight: 8 }
        );
        // kraken_usdt without a rate: USDT sources dropped -> 2 sources left.
        assert_eq!(
            aggregate(&v, &weights(), None, NOW, &rules(QuoteMode::KrakenUsdt)),
            Outcome::TooFewSources { have: 2, need: 3 }
        );
        // An overflowing conversion drops the sample instead of panicking.
        let huge = [Sample { exchange: Binance, mid: FixedPoint::from_raw(i128::MAX / 2), quote: Quote::Usdt, fetched_at_ms: NOW }];
        assert_eq!(
            aggregate(&huge, &weights(), Some(fp(3)), NOW, &rules(QuoteMode::KrakenUsdt)),
            Outcome::TooFewSources { have: 0, need: 3 }
        );
    }

    /// Review L5: weight sums are u64 — huge weights cannot overflow.
    #[test]
    fn huge_weights_do_not_overflow() {
        use Exchange::*;
        let w: BTreeMap<Exchange, u32> = Exchange::ALL.iter().map(|e| (*e, u32::MAX)).collect();
        let v = [s(Binance, 100, Quote::Usdt, NOW), s(Okx, 101, Quote::Usdt, NOW), s(Bybit, 102, Quote::Usdt, NOW), s(Gate, 103, Quote::Usdt, NOW)];
        assert!(matches!(aggregate(&v, &w, None, NOW, &rules(QuoteMode::Par)), Outcome::Price { .. }));
    }
}
