//! Durable custody record for erasure shards (Sprint 5 T3.1, Phase A / T2).
//!
//! [`StoredShard`] is the on-disk value in [`CF_NATIVE_SHARDS`](crate::cf::CF_NATIVE_SHARDS),
//! keyed by [`shard_key`] = `body_hash(32) ++ shard_index(2 BE)`. It mirrors the
//! wire `NativeDaShardResponse` field-for-field (minus the `present` flag) so the
//! proposer encode path (T5) and the serve path (T7) share one representation and
//! map to/from the wire trivially.
//!
//! The BE `shard_index` in the key makes a prefix scan on a `body_hash` return
//! shards `0..n` in order. Values are bincode 1.x (the crate's native-body
//! convention, `native_da.rs`); a decode failure is a `Result`, never a panic.

use crate::erasure::{verify_shard, EncodedBody, ErasureParams, ShardProof};
use crate::error::StateError;
use alloy_primitives::B256;
use serde::{Deserialize, Serialize};

/// Length of a [`shard_key`]: 32-byte body hash + 2-byte big-endian shard index.
pub const SHARD_KEY_LEN: usize = 34;

/// Durable custody record for a single erasure shard.
///
/// Carries its own `(k, n)` and `erasure_root` so a fetcher reconstructs with the
/// exact params the shard was built under (T4: params travel with the shard, not
/// re-derived at fetch — an epoch set-change does not invalidate stored shards).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredShard {
    /// This shard's index in `0..n` (systematic data shards are `0..k`).
    pub shard_index: u16,
    /// The (padded) shard bytes.
    pub shard_bytes: Vec<u8>,
    /// Merkle inclusion proof: sibling hashes leaf→root, as raw 32-byte hashes
    /// (no `B256` on the wire/disk). Reassembled into a [`ShardProof`] via
    /// [`StoredShard::shard_proof`].
    pub proof: Vec<[u8; 32]>,
    /// The committed erasure root this shard proves against.
    pub erasure_root: [u8; 32],
    /// RS data-shard count (reconstruction threshold).
    pub k: u16,
    /// RS total-shard count.
    pub n: u16,
    /// Original (pre-pad) body length — reconstruction truncates back to this.
    pub body_len: u64,
}

impl StoredShard {
    /// Build the custody record for shard `index` of an [`EncodedBody`].
    ///
    /// # Panics
    /// Never — but `index` must be `< enc.shards.len()`; an out-of-range index is
    /// a programming error at the encode call site (T5 iterates `0..n`).
    pub fn from_encoded_body(enc: &EncodedBody, index: usize) -> Self {
        let proof = enc
            .proof(index)
            .siblings
            .iter()
            .map(|h| h.0)
            .collect();
        StoredShard {
            shard_index: index as u16,
            shard_bytes: enc.shards[index].clone(),
            proof,
            erasure_root: enc.erasure_root.0,
            k: enc.params.k as u16,
            n: enc.params.n as u16,
            body_len: enc.body_len as u64,
        }
    }

    /// The committed root as a [`B256`].
    pub fn erasure_root_b256(&self) -> B256 {
        B256::from(self.erasure_root)
    }

    /// Reassemble the [`ShardProof`] from the stored raw sibling hashes.
    pub fn shard_proof(&self) -> ShardProof {
        ShardProof { siblings: self.proof.iter().map(|h| B256::from(*h)).collect() }
    }

    /// The RS params this shard was built under.
    pub fn params(&self) -> ErasureParams {
        ErasureParams::new(self.k as usize, self.n as usize)
    }

    /// Verify this shard against its own committed root (leaf-index folded in).
    /// The serve/fetch paths still apply the body-hash backstop on top.
    pub fn verify(&self) -> bool {
        verify_shard(
            self.erasure_root_b256(),
            self.shard_index as usize,
            &self.shard_bytes,
            &self.shard_proof(),
        )
    }
}

/// `CF_NATIVE_SHARDS` key: `body_hash(32) ++ shard_index.to_be_bytes()(2)`.
/// BE index ⇒ a prefix scan on `body_hash` yields shards `0..n` in order.
pub fn shard_key(body_hash: &[u8; 32], shard_index: u16) -> [u8; SHARD_KEY_LEN] {
    let mut key = [0u8; SHARD_KEY_LEN];
    key[..32].copy_from_slice(body_hash);
    key[32..].copy_from_slice(&shard_index.to_be_bytes());
    key
}

/// Serialize a [`StoredShard`] for `CF_NATIVE_SHARDS` (bincode 1.x, crate convention).
pub fn encode_stored_shard(shard: &StoredShard) -> Result<Vec<u8>, StateError> {
    bincode::serialize(shard).map_err(|e| StateError::InvalidData(e.to_string()))
}

/// Deserialize a [`StoredShard`] read from `CF_NATIVE_SHARDS`. Never panics on
/// malformed bytes — the caller logs-and-skips like `get_native_da`.
pub fn decode_stored_shard(bytes: &[u8]) -> Result<StoredShard, StateError> {
    bincode::deserialize(bytes).map_err(|e| StateError::InvalidData(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::erasure::encode;

    fn body(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 11 + 5) as u8).collect()
    }

    /// A StoredShard with a real multi-level proof survives bincode round-trip
    /// byte-for-byte (RED: StoredShard does not exist on HEAD).
    #[test]
    fn stored_shard_bincode_roundtrip() {
        let enc = encode(&body(1000), ErasureParams::new(2, 4)).expect("encode");
        let shard = StoredShard::from_encoded_body(&enc, 1);
        assert!(!shard.proof.is_empty(), "n=4 must yield a multi-level proof");
        let bytes = encode_stored_shard(&shard).expect("encode");
        let back = decode_stored_shard(&bytes).expect("decode");
        assert_eq!(shard, back, "StoredShard must round-trip through bincode");
    }

    /// The key is `body_hash ++ BE(index)` and sorts by `(body_hash, index)` so a
    /// prefix scan returns shards in index order.
    #[test]
    fn shard_key_is_body_hash_plus_be_index() {
        let h = [0xABu8; 32];
        let key = shard_key(&h, 258); // 258 = 0x0102
        assert_eq!(&key[..32], &h);
        assert_eq!(&key[32..], &[0x01, 0x02]);
        // BE index ordering: shard 0 sorts before shard 1 before shard 258.
        assert!(shard_key(&h, 0) < shard_key(&h, 1));
        assert!(shard_key(&h, 1) < shard_key(&h, 258));
        // Different body hashes never collide within the same index.
        let h2 = [0xACu8; 32];
        assert_ne!(shard_key(&h, 0), shard_key(&h2, 0));
    }

    /// Converting an EncodedBody shard into a StoredShard preserves exactly the
    /// fields the serve path ships: decode → verify against the stored root → true.
    #[test]
    fn stored_shard_from_encoded_body() {
        let enc = encode(&body(777), ErasureParams::new(2, 3)).expect("encode");
        for i in 0..enc.params.n {
            let shard = StoredShard::from_encoded_body(&enc, i);
            assert_eq!(shard.shard_index as usize, i);
            assert_eq!(shard.k, 2);
            assert_eq!(shard.n, 3);
            assert_eq!(shard.body_len, enc.body_len as u64);
            assert_eq!(shard.erasure_root, enc.erasure_root.0);
            // The stored fields verify-then-reconstruct exactly like the live ones.
            let bytes = encode_stored_shard(&shard).expect("encode");
            let back = decode_stored_shard(&bytes).expect("decode");
            assert!(back.verify(), "stored shard {i} must verify against its root");
        }
    }
}
