//! Build never-invalid oracle submissions, nonces, signing, error classes.

use std::collections::{BTreeMap, BTreeSet};

use k256::ecdsa::SigningKey;
use torus_core::oracle::{valid_oracle_price, MAX_ORACLE_PRICES_PER_SUBMISSION};
use torus_types::{FixedPoint, MarketId, NativeAction, OracleSubmission, SignedNativeAction};

/// The submissions for this cycle: only LISTED markets with a price that
/// passes `valid_oracle_price` (the exec rule), one entry per market (BTreeMap
/// keys), split into chunks of the per-submission cap. Empty if nothing is left.
pub fn build_submissions(
    prices: &BTreeMap<MarketId, FixedPoint>,
    listed: &BTreeSet<MarketId>,
    sample_ts_ms: u64,
) -> Vec<OracleSubmission> {
    let ok: Vec<(MarketId, FixedPoint)> = prices
        .iter()
        .filter(|(m, p)| listed.contains(m) && valid_oracle_price(**p))
        .map(|(m, p)| (*m, *p))
        .collect();
    ok.chunks(MAX_ORACLE_PRICES_PER_SUBMISSION)
        .map(|c| OracleSubmission { prices: c.to_vec(), timestamp: sample_ts_ms })
        .collect()
}

/// Nonce = `max(now_ms, last + 1)`: strictly increasing even if the clock
/// stalls or steps back, and inside the node's ±60 s nonce window otherwise.
#[derive(Clone, Debug, Default)]
pub struct NonceGen {
    last: u64,
}

impl NonceGen {
    pub fn next(&mut self, now_ms: u64) -> u64 {
        self.last = now_ms.max(self.last.saturating_add(1));
        self.last
    }
}

/// EIP-712-sign a submission with the hot signer key.
pub fn sign(sub: OracleSubmission, nonce: u64, key: &SigningKey) -> SignedNativeAction {
    torus_types::eip712::sign_native_action(NativeAction::SubmitOraclePrices(sub), nonce, key)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitClass {
    /// Node busy / pending cap: the next cycle tries again.
    Retryable,
    /// The node does not accept this signer for the validator: health down.
    NotAuthorized,
    Other,
}

impl SubmitClass {
    pub fn label(self) -> &'static str {
        match self {
            SubmitClass::Retryable => "retryable",
            SubmitClass::NotAuthorized => "not_authorized",
            SubmitClass::Other => "other",
        }
    }
}

pub fn classify_submit_error(msg: &str) -> SubmitClass {
    let m = msg.to_ascii_lowercase();
    if m.contains("not a registered validator or oracle signer")
        || m.contains("not an active validator")
    {
        SubmitClass::NotAuthorized
    } else if m.contains("busy") || m.contains("overloaded") || m.contains("oracle pending cap") {
        SubmitClass::Retryable
    } else {
        SubmitClass::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, Rng, SeedableRng};
    use torus_core::oracle::{valid_oracle_price, MAX_ORACLE_PRICES_PER_SUBMISSION as CAP, MAX_ORACLE_PRICE_RAW};

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    #[test]
    fn builds_only_listed_valid_markets() {
        let prices: BTreeMap<MarketId, FixedPoint> = [
            (1, fp(65_000)),
            (2, FixedPoint::ZERO),
            (3, FixedPoint::from_raw(MAX_ORACLE_PRICE_RAW + 1)),
            (4, fp(100)),
            (5, fp(7)),
        ]
        .into_iter()
        .collect();
        let listed: BTreeSet<MarketId> = [1, 2, 3, 4].into_iter().collect();
        let subs = build_submissions(&prices, &listed, 42);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].prices, vec![(1, fp(65_000)), (4, fp(100))]);
        assert_eq!(subs[0].timestamp, 42);
    }

    #[test]
    fn nothing_to_send_is_empty() {
        let listed: BTreeSet<MarketId> = [1].into_iter().collect();
        assert!(build_submissions(&BTreeMap::new(), &listed, 0).is_empty());
        let prices: BTreeMap<MarketId, FixedPoint> = [(2, fp(1))].into_iter().collect();
        assert!(build_submissions(&prices, &listed, 0).is_empty());
    }

    #[test]
    fn chunks_at_the_cap() {
        let prices: BTreeMap<MarketId, FixedPoint> = (1..=600).map(|m| (m, fp(1))).collect();
        let listed: BTreeSet<MarketId> = prices.keys().copied().collect();
        let sizes: Vec<usize> = build_submissions(&prices, &listed, 0).iter().map(|s| s.prices.len()).collect();
        assert_eq!(sizes, vec![256, 256, 88]);
    }

    /// Every submission the feeder builds passes the exec rules
    /// (NE exec_submit_oracle_prices): 1..=256 entries, no duplicates, listed
    /// markets only, `valid_oracle_price`.
    #[test]
    fn every_built_submission_passes_exec_rules() {
        let mut rng = StdRng::seed_from_u64(517);
        for case in 0..500 {
            let n = rng.gen_range(0..700usize);
            let prices: BTreeMap<MarketId, FixedPoint> = (0..n)
                .map(|_| {
                    let raw = match rng.gen_range(0..6) {
                        0 => 0,
                        1 => -rng.gen_range(1..1_000_000i128),
                        2 => MAX_ORACLE_PRICE_RAW + rng.gen_range(0..2i128),
                        3 => i128::MAX,
                        _ => rng.gen_range(1..MAX_ORACLE_PRICE_RAW),
                    };
                    (rng.gen_range(0..800u64), FixedPoint::from_raw(raw))
                })
                .collect();
            let listed: BTreeSet<MarketId> = (0..800u64).filter(|_| rng.gen_bool(0.7)).collect();
            let subs = build_submissions(&prices, &listed, 1);
            let mut seen = BTreeSet::new();
            for s in &subs {
                assert!((1..=CAP).contains(&s.prices.len()), "case {case}");
                let mut in_sub = BTreeSet::new();
                for &(m, p) in &s.prices {
                    assert!(in_sub.insert(m), "case {case}: duplicate {m}");
                    assert!(seen.insert(m), "case {case}: {m} in two chunks");
                    assert!(listed.contains(&m), "case {case}: unlisted {m}");
                    assert!(valid_oracle_price(p), "case {case}: invalid {p}");
                }
            }
            let expect = prices.iter().filter(|(m, p)| listed.contains(m) && valid_oracle_price(**p)).count();
            assert_eq!(seen.len(), expect, "case {case}: nothing valid dropped");
        }
    }

    #[test]
    fn nonce_is_strictly_monotonic() {
        let mut g = NonceGen::default();
        assert_eq!(g.next(1_000), 1_000);
        assert_eq!(g.next(1_000), 1_001);
        assert_eq!(g.next(900), 1_002, "clock going back never reuses a nonce");
        assert_eq!(g.next(5_000), 5_000);
        let mut last = 0;
        for now in [5_000, 5_000, 4_000, 6_000, 6_000] {
            let n = g.next(now);
            assert!(n > last);
            last = n;
        }
    }

    #[test]
    fn signed_by_the_signer_key() {
        let key = k256::ecdsa::SigningKey::from_slice(&[21u8; 32]).unwrap();
        let sub = OracleSubmission { prices: vec![(1, fp(100))], timestamp: 3 };
        let signed = sign(sub.clone(), 77, &key);
        assert_eq!(signed.nonce, 77);
        assert_eq!(signed.recover_sender().unwrap(), torus_wallet::keystore::address_from_key(&key));
        match signed.action {
            NativeAction::SubmitOraclePrices(s) => assert_eq!(s.prices, sub.prices),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn classify_submit_error_cases() {
        use SubmitClass::*;
        let cases = [
            ("RPC error: node busy, retry", Retryable),
            ("mempool overloaded", Retryable),
            ("oracle pending cap 4 reached for validator 0x..", Retryable),
            ("0xab is not a registered validator or oracle signer", NotAuthorized),
            ("validator 0xab is not an active validator", NotAuthorized),
            ("oracle submission from 0xab: not an active validator or its signer", NotAuthorized),
            ("unknown market_id 7", Other),
            ("connection refused", Other),
        ];
        for (msg, want) in cases {
            assert_eq!(classify_submit_error(msg), want, "{msg}");
        }
    }
}
