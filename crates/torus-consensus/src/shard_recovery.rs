//! Recovery-path shard gather → verify → reconstruct (Sprint 5 T8).
//!
//! When a node reaches reconstruction missing a native-action body, it requests
//! shard `i` from `k` DIFFERENT peers (spreading the serve, killing the s338
//! single-source hotspot), then hands the responses here. This module is the
//! safety-critical core: it VERIFIES every shard before use, requires the shards
//! to be a consistent set from DISTINCT sources, reconstructs, and applies the
//! body-hash BACKSTOP. It is pure over its inputs so the guarantees are unit-
//! testable without a live swarm; the caller absorbs the returned body into the
//! whole-body DA store and falls back to the whole-body pull on `None`.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use torus_state::erasure::reconstruct;
use torus_state::{ErasureParams, StoredShard};
use torus_types::{compute_action_hash, SignedNativeAction};

/// A shard received from a peer during recovery, tagged with its source peer so
/// the distinct-source rule (k shards from k DIFFERENT peers) can be enforced.
#[derive(Debug, Clone)]
pub struct GatheredShard<S> {
    pub source: S,
    pub shard: StoredShard,
}

/// Try to reconstruct the body for `body_hash` from gathered shards. Returns
/// `Some(body_bytes)` only when ALL of the following hold; otherwise `None`, and
/// the caller falls back to the whole-body pull (never wedges):
///
/// 1. **Verify-then-reconstruct.** Every shard used passed [`StoredShard::verify`]
///    (its Merkle proof against its own committed root) BEFORE being fed to
///    [`reconstruct`] — a Byzantine peer cannot poison the rebuild.
/// 2. **Consistent set.** The shards used share one `(erasure_root, k, n, body_len)`.
/// 3. **Distinct sources.** They come from `>= k` DIFFERENT peers — `k` shards from
///    ONE peer is not "spread", so we fall back to a whole-body pull from that peer.
/// 4. **Body-hash backstop.** The reconstructed body must decode to a
///    `SignedNativeAction` and re-hash to `body_hash`. The peer-provided
///    `erasure_root` is not consensus-committed in Phase A, so this is the ultimate
///    integrity gate — a self-consistent set forged over different bytes is rejected.
pub fn try_reconstruct_from_shards<S: Eq + Hash + Clone>(
    body_hash: &[u8; 32],
    gathered: &[GatheredShard<S>],
) -> Option<Vec<u8>> {
    // (1) Verify each shard BEFORE it can influence reconstruction.
    let verified = gathered.iter().filter(|g| g.shard.verify());

    // (2) Group by the consistency key; only one group can reconstruct a body.
    let mut groups: HashMap<([u8; 32], u16, u16, u64), Vec<&GatheredShard<S>>> = HashMap::new();
    for g in verified {
        let key = (g.shard.erasure_root, g.shard.k, g.shard.n, g.shard.body_len);
        groups.entry(key).or_default().push(g);
    }

    for ((_root, k, n, body_len), shards) in groups {
        let (k, n) = (k as usize, n as usize);
        // One shard per index (first wins), tracking its source for the (3) rule.
        let mut by_index: HashMap<u16, &GatheredShard<S>> = HashMap::new();
        for g in shards {
            by_index.entry(g.shard.shard_index).or_insert(g);
        }
        if by_index.len() < k {
            continue; // not enough distinct indices to reconstruct
        }
        // (3) k shards must come from k DISTINCT peers.
        let sources: HashSet<S> = by_index.values().map(|g| g.source.clone()).collect();
        if sources.len() < k {
            continue;
        }
        // Build the n-slot sparse set and reconstruct.
        let mut slots: Vec<Option<Vec<u8>>> = vec![None; n];
        for (idx, g) in &by_index {
            if (*idx as usize) < n {
                slots[*idx as usize] = Some(g.shard.shard_bytes.clone());
            }
        }
        let Ok(body) = reconstruct(slots, ErasureParams::new(k, n), body_len as usize) else {
            continue;
        };
        // (4) Body-hash backstop — the authoritative check.
        if let Ok(action) = bincode::deserialize::<SignedNativeAction>(&body) {
            if compute_action_hash(&action).0 == *body_hash {
                return Some(body);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_state::erasure::encode;
    use torus_types::{ActionSignature, NativeAction, Signature};

    fn action(nonce: u64) -> SignedNativeAction {
        SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce,
            signature: ActionSignature::Eip712(Signature { v: 27, r: [0u8; 32], s: [0u8; 32] }),
        }
    }

    /// Encode `act`'s body under `(k,n)` and return (body_hash, all n StoredShards).
    fn shards_for(act: &SignedNativeAction, k: usize, n: usize) -> ([u8; 32], Vec<StoredShard>) {
        let body = bincode::serialize(act).expect("serialize");
        let enc = encode(&body, ErasureParams::new(k, n)).expect("encode");
        let shards = (0..n).map(|i| StoredShard::from_encoded_body(&enc, i)).collect();
        (compute_action_hash(act).0, shards)
    }

    /// T8: k verified shards from k DISTINCT peers reconstruct the body, the
    /// backstop passes, and the ORIGINAL body bytes come back (caller then absorbs).
    #[test]
    fn reconstruct_from_k_verified_shards_absorbs_body() {
        let act = action(1);
        let (body_hash, shards) = shards_for(&act, 2, 3);
        // Two distinct peers, distinct data/parity indices (0 from A, 2 from B).
        let gathered = vec![
            GatheredShard { source: "peerA", shard: shards[0].clone() },
            GatheredShard { source: "peerB", shard: shards[2].clone() },
        ];
        let body = try_reconstruct_from_shards(&body_hash, &gathered).expect("reconstructs");
        assert_eq!(body, bincode::serialize(&act).unwrap(), "rebuilt body == original");
    }

    /// T8: a corrupted shard fails its proof and is discarded BEFORE reconstruct;
    /// with only the corrupt one usable, no (wrong) body is absorbed.
    #[test]
    fn corrupt_shard_is_rejected_before_reconstruct() {
        let act = action(2);
        let (body_hash, mut shards) = shards_for(&act, 2, 3);
        shards[0].shard_bytes[0] ^= 0xFF; // corrupt shard 0 → proof now fails
        // Only the corrupt shard + one honest shard from ONE other peer.
        let gathered = vec![
            GatheredShard { source: "peerA", shard: shards[0].clone() }, // rejected by verify
            GatheredShard { source: "peerB", shard: shards[1].clone() },
        ];
        // Only 1 verified shard survives (< k) → None (fall back), never a wrong body.
        assert!(try_reconstruct_from_shards(&body_hash, &gathered).is_none());
    }

    /// T8: a self-consistent erasure set forged over DIFFERENT bytes verifies at
    /// the shard level but the reconstructed body re-hashes to the wrong hash — the
    /// body-hash backstop rejects it (no absorb).
    #[test]
    fn body_hash_backstop_rejects_mismatched_set() {
        let real = action(3);
        let real_hash = compute_action_hash(&real).0;
        // Attacker builds a perfectly valid erasure set for a DIFFERENT action…
        let (_forged_hash, forged_shards) = shards_for(&action(999), 2, 3);
        let gathered = vec![
            GatheredShard { source: "peerA", shard: forged_shards[0].clone() },
            GatheredShard { source: "peerB", shard: forged_shards[1].clone() },
        ];
        // …but claims it is `real_hash`. Shards verify vs their forged root, yet the
        // rebuilt body hashes to action(999) != real_hash → backstop rejects.
        assert!(try_reconstruct_from_shards(&real_hash, &gathered).is_none());
    }

    /// T8: two shards from the SAME peer count as one source — not "spread" — so the
    /// distinct-source rule rejects them and the caller falls back to a whole-body pull.
    #[test]
    fn k_shards_from_one_peer_not_counted_as_spread() {
        let act = action(4);
        let (body_hash, shards) = shards_for(&act, 2, 3);
        // Both shards (distinct indices) from the SAME peer.
        let gathered = vec![
            GatheredShard { source: "peerA", shard: shards[0].clone() },
            GatheredShard { source: "peerA", shard: shards[1].clone() },
        ];
        assert!(
            try_reconstruct_from_shards(&body_hash, &gathered).is_none(),
            "one-source shards are not spread → fall back to whole-body pull"
        );
        // Sanity: the SAME two shards from TWO peers DO reconstruct.
        let spread = vec![
            GatheredShard { source: "peerA", shard: shards[0].clone() },
            GatheredShard { source: "peerB", shard: shards[1].clone() },
        ];
        assert!(try_reconstruct_from_shards(&body_hash, &spread).is_some());
    }
}
