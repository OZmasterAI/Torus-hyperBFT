//! `--cloid-share` (s114 follow-up): give a share of the placed orders a
//! client order id (cloid), so a later A/B can price the node's cloid path
//! (canonical bytes, EIP-712 `uint128`, the cross-market duplicate check and
//! the per-book cloid index) against cloid-free load.
//!
//! * Selection is per ORDER (each item of a `PlaceOrderBatch` counts), by a
//!   hash of a fixed per-sender seed and the sender's order index: the same
//!   run shape picks the same orders every time, and the action stream's own
//!   RNG is never drawn from, so `--cloid-share 0` is byte-identical to a
//!   bench without the flag.
//! * The cloid is `run_tag << 64 | n`, `n` a per-sender counter from 1: never
//!   0 (the format has an explicit `None`, but 0 is the "no cloid" value in
//!   the EIP-712 field and CoreWriter), never reused by the sender within a
//!   run, and distinct across runs on one chain (the run tag is the bench's
//!   start time in unix ms), so no order can hit `DuplicateCloid`.

use torus_types::{Cloid, NativeAction};

/// Per-sender seed base of the selection hash (sender index XORed in).
pub(crate) const SEED_BASE: u64 = 0xC10D_0000_0000_0000;

/// Parse `--cloid-share`: an integer percent in `0..=100`.
pub(crate) fn parse_share(_s: &str) -> Result<u8, String> {
    Err("unimplemented".to_string())
}

/// The run tag: unix ms at bench start.
pub(crate) fn run_tag_now() -> u64 {
    0
}

/// One sender's cloid source; see the module comment.
#[derive(Clone, Debug)]
pub(crate) struct CloidStamper {
    share: u8,
    seed: u64,
    run_tag: u64,
    /// Orders seen so far (the selection index).
    order_idx: u64,
    /// Cloids handed out so far (the low 64 bits of the last one).
    issued: u64,
}

impl CloidStamper {
    pub(crate) fn new(share: u8, seed: u64, run_tag: u64) -> Self {
        Self { share, seed, run_tag, order_idx: 0, issued: 0 }
    }

    /// The stamper of sender `sender_idx` (seed `SEED_BASE ^ sender_idx`).
    pub(crate) fn for_sender(share: u8, sender_idx: usize, run_tag: u64) -> Self {
        Self::new(share, SEED_BASE ^ sender_idx as u64, run_tag)
    }

    /// Give the selected orders of `action` a cloid (other actions are left
    /// alone). Returns `(orders placed, cloids stamped)`.
    pub(crate) fn stamp(&mut self, _action: &mut NativeAction) -> (u64, u64) {
        let _ = (self.share, self.seed, self.run_tag, self.order_idx, self.issued);
        (0, 0)
    }
}

/// The cloids of `action`'s orders, in order (`None` for an order without
/// one); empty for an action that places nothing.
#[cfg(test)]
fn cloids_of(action: &NativeAction) -> Vec<Option<Cloid>> {
    match action {
        NativeAction::PlaceOrder(p) => vec![p.client_order_id],
        NativeAction::PlaceOrderBatch(orders) => orders.iter().map(|p| p.client_order_id).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{econ_action, EconShape, MarketPlan};
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::collections::HashSet;
    use torus_types::eip712::{
        eip712_struct_hash, sign_native_action, sign_native_action_with_session,
    };
    use torus_types::SignedNativeAction;

    const TAG: u64 = 0x0000_0192_0000_0001;

    /// `n` econ actions of `batch` orders each (cancel-alls mixed in).
    fn econ_stream(seed: u64, n: usize, batch: usize) -> Vec<NativeAction> {
        let shape = EconShape::new(1500, 0, 5, 0.5, 0.1);
        let plan = MarketPlan::new(4, 0);
        let mut rng = StdRng::seed_from_u64(seed);
        (0..n).map(|_| econ_action(&mut rng, 3, plan, batch, &shape)).collect()
    }

    fn stamp_all(st: &mut CloidStamper, actions: &mut [NativeAction]) -> (u64, u64) {
        actions.iter_mut().fold((0, 0), |(p, s), a| {
            let (dp, ds) = st.stamp(a);
            (p + dp, s + ds)
        })
    }

    fn all_cloids(actions: &[NativeAction]) -> Vec<Option<Cloid>> {
        actions.iter().flat_map(cloids_of).collect()
    }

    /// NativeAction has no PartialEq: compare the wire bytes.
    fn ser(actions: &[NativeAction]) -> Vec<Vec<u8>> {
        actions.iter().map(|a| bincode::serialize(a).unwrap()).collect()
    }

    fn strip(actions: &[NativeAction]) -> Vec<Vec<u8>> {
        let mut out = actions.to_vec();
        for a in &mut out {
            match a {
                NativeAction::PlaceOrder(p) => p.client_order_id = None,
                NativeAction::PlaceOrderBatch(orders) => {
                    orders.iter_mut().for_each(|p| p.client_order_id = None)
                }
                _ => {}
            }
        }
        ser(&out)
    }

    #[test]
    fn parse_share_accepts_0_to_100_only() {
        assert_eq!(parse_share("0"), Ok(0));
        assert_eq!(parse_share("50"), Ok(50));
        assert_eq!(parse_share("100"), Ok(100));
        for bad in ["101", "-1", "abc", "", "50.5", "256"] {
            assert!(parse_share(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn run_tag_is_the_start_time_in_ms() {
        let tag = run_tag_now();
        // After 2026-01-01 and below 2^63: a real unix-ms clock reading.
        assert!(tag > 1_767_225_600_000, "run tag {tag} is not unix ms");
    }

    // Share 0: no cloid anywhere and the stream untouched (today's load).
    #[test]
    fn share_zero_stamps_nothing() {
        for batch in [1, 9] {
            let original = econ_stream(5, 300, batch);
            let mut stamped = original.clone();
            let mut st = CloidStamper::for_sender(0, 3, TAG);
            let (placed, n) = stamp_all(&mut st, &mut stamped);
            assert_eq!(ser(&stamped), ser(&original));
            assert_eq!(n, 0);
            assert_eq!(placed, all_cloids(&original).len() as u64);
            assert!(all_cloids(&stamped).iter().all(Option::is_none));
        }
    }

    // Share 100: every order, single or batched, gets a distinct nonzero cloid;
    // cancels and every other order field are untouched.
    #[test]
    fn share_100_gives_every_order_a_distinct_nonzero_cloid() {
        for batch in [1, 9] {
            let original = econ_stream(6, 400, batch);
            let mut stamped = original.clone();
            let mut st = CloidStamper::for_sender(100, 3, TAG);
            let (placed, n) = stamp_all(&mut st, &mut stamped);
            let cloids = all_cloids(&stamped);
            assert!(!cloids.is_empty());
            assert_eq!(placed, cloids.len() as u64);
            assert_eq!(n, placed, "share 100 stamps every order");
            let set: HashSet<Cloid> = cloids.iter().map(|c| c.expect("every order has a cloid")).collect();
            assert_eq!(set.len(), cloids.len(), "cloids must be distinct");
            assert!(!set.contains(&0), "cloid 0 must never be issued");
            assert_eq!(strip(&stamped), ser(&original), "only client_order_id may change");
        }
    }

    // Cloid = run_tag << 64 | n, n = 1, 2, 3, ... per sender; another run tag
    // (a rerun on the same chain) never repeats a cloid; tag 0 still nonzero.
    #[test]
    fn cloids_count_up_per_sender_under_the_run_tag() {
        let mut a = econ_stream(7, 50, 4);
        let mut st = CloidStamper::for_sender(100, 1, TAG);
        stamp_all(&mut st, &mut a);
        let lows: Vec<u64> = all_cloids(&a)
            .into_iter()
            .map(|c| {
                let c = c.unwrap();
                assert_eq!((c >> 64) as u64, TAG);
                c as u64
            })
            .collect();
        assert_eq!(lows, (1..=lows.len() as u64).collect::<Vec<_>>());

        let mut b = econ_stream(7, 50, 4);
        stamp_all(&mut CloidStamper::for_sender(100, 1, TAG + 1), &mut b);
        let first: HashSet<_> = all_cloids(&a).into_iter().collect();
        assert!(all_cloids(&b).iter().all(|c| !first.contains(c)));

        let mut z = econ_stream(7, 5, 4);
        stamp_all(&mut CloidStamper::for_sender(100, 1, 0), &mut z);
        assert!(all_cloids(&z).iter().all(|c| c.is_some_and(|c| c != 0)));
    }

    // Share 50: about half the orders, the same ones for the same seed, other
    // ones for another seed; the picked ones still distinct and nonzero.
    #[test]
    fn share_50_is_about_half_and_deterministic_per_seed() {
        let base = econ_stream(8, 2_000, 10);
        let run = |seed: u64| {
            let mut s = base.clone();
            let (placed, n) = stamp_all(&mut CloidStamper::new(50, seed, TAG), &mut s);
            (s, placed, n)
        };
        let (a, placed, n) = run(42);
        assert!(placed > 15_000, "{placed} orders");
        let half = placed / 2;
        assert!(n.abs_diff(half) <= placed / 20, "{n} of {placed} stamped, want ~half");
        let picked = all_cloids(&a);
        assert_eq!(picked.iter().filter(|c| c.is_some()).count() as u64, n);
        let set: HashSet<_> = picked.iter().flatten().collect();
        assert_eq!(set.len() as u64, n);
        assert!(!set.contains(&0));

        let (again, _, n2) = run(42);
        assert_eq!(ser(&again), ser(&a), "same seed, same orders, same cloids");
        assert_eq!(n2, n);

        let (other, _, _) = run(43);
        let mask = |s: &[NativeAction]| all_cloids(s).iter().map(Option::is_some).collect::<Vec<_>>();
        assert_ne!(mask(&other), mask(&a), "another seed picks other orders");
        assert_eq!(strip(&a), ser(&base));
    }

    // Selection is per sender: the sender seeds differ.
    #[test]
    fn senders_pick_different_orders() {
        let base = econ_stream(9, 200, 10);
        let mask = |sender: usize| {
            let mut s = base.clone();
            stamp_all(&mut CloidStamper::for_sender(50, sender, TAG), &mut s);
            all_cloids(&s).iter().map(Option::is_some).collect::<Vec<_>>()
        };
        assert_eq!(mask(0), mask(0));
        assert_ne!(mask(0), mask(1));
    }

    // An order stamped with a cloid signs and encodes with that cloid: the
    // EIP-712 hash commits to it, the signature recovers the sender, and the
    // bincode / JSON wire forms and the canonical bytes carry it.
    #[test]
    fn stamped_order_signs_and_encodes_with_its_cloid() {
        let key = k256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
        let session = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        for batch in [1, 3] {
            let plain = econ_stream(10, 20, batch)
                .into_iter()
                .find(|a| !cloids_of(a).is_empty())
                .unwrap();
            let mut action = plain.clone();
            let (_, n) = CloidStamper::for_sender(100, 0, TAG).stamp(&mut action);
            assert_eq!(n, batch as u64);
            let cloids: Vec<Cloid> = cloids_of(&action).into_iter().map(Option::unwrap).collect();
            assert_ne!(eip712_struct_hash(&action, 1), eip712_struct_hash(&plain, 1));

            let signed = sign_native_action(action.clone(), 1_700_000_000_000, &key);
            assert_eq!(signed.recover_sender().unwrap(), crate::key_address(&key));
            let wire = bincode::serialize(&signed).unwrap();
            let bin: SignedNativeAction = bincode::deserialize(&wire).unwrap();
            assert_eq!(bincode::serialize(&bin).unwrap(), wire);
            assert_eq!(bin.recover_sender().unwrap(), crate::key_address(&key));
            let json: SignedNativeAction =
                serde_json::from_slice(&serde_json::to_vec(&signed).unwrap()).unwrap();
            assert_eq!(bincode::serialize(&json).unwrap(), wire);
            assert_eq!(
                cloids_of(&json.action).into_iter().map(Option::unwrap).collect::<Vec<_>>(),
                cloids
            );
            let bytes = action.canonical_bytes();
            for c in &cloids {
                let be = c.to_be_bytes();
                assert!(bytes.windows(16).any(|w| w == be), "canonical bytes carry {c:#x}");
            }

            let s = sign_native_action_with_session(action.clone(), 1_700_000_000_001, &session);
            assert!(s.verify_session_signature().is_ok());
            assert_eq!(bincode::serialize(&s.action).unwrap(), bincode::serialize(&action).unwrap());
        }
    }
}
