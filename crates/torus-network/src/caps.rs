//! Single source of truth for the message-size cap ladder (O5).
//!
//! Every transport/app size gate in the feed path lives here (or is asserted
//! against here) so the ladder ordering can never silently drift — the same
//! failure class as the S391 heartbeat bug, where a hardcoded value shadowed
//! the configured one for months. The ladder, narrow to wide:
//!
//!   consensus accept gate (config) ≤ gossip transmit
//!   hash-only push threshold < legacy fleet direct floor ≤ direct codec
//!   direct codec ≤ native-DA pull codec ≤ block-data sync codec
//!
//! None of these are consensus rules — `validate_block` rejects on none of
//! them. They are receive gates and proposer-local send policy; mixed values
//! cannot fork. The risk class is LIVENESS (a receiver that drops what a
//! sender legitimately produced), which is exactly why the ordering is
//! test-enforced.

/// `/torus/direct` read cap. O5 raised it 4 → 8 MB ACCEPT-FIRST: our reads
/// widen now so a FUTURE threshold raise can full-push bs600–1024 body sets
/// (4.2–7.2 MB); send policy stays pinned to `LEGACY_FLEET_DIRECT_MSG_FLOOR`
/// until every validator carries this build. Oversized pre-proposal bodies
/// are still recovered via the chunked `/torus/native-da` pull path.
pub const MAX_DIRECT_MSG_SIZE: usize = 8 * 1024 * 1024; // 8 MB

/// `/torus/native-da` response cap — generous headroom; bodies are pulled in
/// size-bounded chunks (`NATIVE_DA_FETCH_CHUNK`), so a response stays well under this.
pub const MAX_NATIVE_DA_MSG_SIZE: usize = 8 * 1024 * 1024; // 8 MB

/// `/torus/native-da-shards` response cap (Sprint 5 T3.1). A single shard is at
/// most `body/k + Merkle proof + fixed header`. The largest body is the 6 MB
/// `NATIVE_BLOCK_BYTES_CAP` (torus-mempool `rate_limit.rs`) and the smallest `k`
/// is `f+1 = 2` at n=3, so the worst-case data shard is ≤ ~3 MB, plus a proof
/// (`≤ ceil(log2(255)) = 8` levels × 32 B = 256 B) and fixed fields (< 64 B).
/// 4 MB sits comfortably above that worst case and strictly below the whole-body
/// pull cap, so an oversize/bomb shard frame is rejected at a tight bound instead
/// of being allowed to grow to a whole-body size.
pub const MAX_NATIVE_DA_SHARDS_MSG_SIZE: usize = 4 * 1024 * 1024; // 4 MB

/// `/torus/block-data` response cap — must fit a full big block during sync.
pub const MAX_BLOCK_DATA_MSG_SIZE: usize = 16 * 1024 * 1024; // 16 MB

/// Gossipsub transport frame cap (bounds publish AND inbound decode). Was
/// hardcoded at the builder call site in behaviour.rs — same silent-drift
/// class as the S391 heartbeat bug. The consensus/tx accept gates
/// (`NetworkConfig::max_consensus_message_size` / `max_tx_message_size`) must
/// stay at or under this or a legal message becomes unpublishable.
pub const GOSSIP_MAX_TRANSMIT_SIZE: usize = 2 * 1024 * 1024; // 2 MiB

/// Direct-msg read cap of the OLDEST binary still in the fleet (pre-O5 = 4 MB).
/// Send-side policy (the hash-only push threshold) must stay under THIS, not
/// under our own `MAX_DIRECT_MSG_SIZE`: a push above the floor is accepted by
/// upgraded peers and rejected by stragglers, silently splitting dissemination
/// onto the slow pull path per-peer (the S388 6 MB env override did exactly
/// this fleet-wide). Moving this const IS the act of re-declaring the fleet
/// floor — do it only when every validator carries the raised codec cap.
pub const LEGACY_FLEET_DIRECT_MSG_FLOOR: usize = 4 * 1024 * 1024; // 4 MB

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::HASH_ONLY_PUSH_THRESHOLD;
    use crate::config::NetworkConfig;

    #[test]
    fn size_ladder_is_coherent() {
        let cfg = NetworkConfig::default();
        // App-level accept gates fit inside the transport they ride: a message
        // that passes the app gate must be publishable, and vice versa a frame
        // the transport delivers must be judgeable by the app gate — not
        // silently truncated at a lower layer.
        assert!(
            cfg.max_consensus_message_size <= GOSSIP_MAX_TRANSMIT_SIZE,
            "consensus accept gate {} exceeds gossip transmit cap {}",
            cfg.max_consensus_message_size,
            GOSSIP_MAX_TRANSMIT_SIZE
        );
        assert!(
            cfg.max_tx_message_size <= GOSSIP_MAX_TRANSMIT_SIZE,
            "tx accept gate {} exceeds gossip transmit cap {}",
            cfg.max_tx_message_size,
            GOSSIP_MAX_TRANSMIT_SIZE
        );
        // Manifest mode must engage before the direct codec rejects the push,
        // judged against the OLDEST binary still in the fleet (val3 floor),
        // not just our own codec.
        assert!(
            HASH_ONLY_PUSH_THRESHOLD < LEGACY_FLEET_DIRECT_MSG_FLOOR,
            "push threshold {} not under fleet direct floor {}",
            HASH_ONLY_PUSH_THRESHOLD,
            LEGACY_FLEET_DIRECT_MSG_FLOOR
        );
        assert!(LEGACY_FLEET_DIRECT_MSG_FLOOR <= MAX_DIRECT_MSG_SIZE);
        // Recovery paths widen monotonically: push < pull < sync — each
        // fallback must be able to carry anything the faster path gave up on.
        assert!(MAX_DIRECT_MSG_SIZE <= MAX_NATIVE_DA_MSG_SIZE);
        assert!(MAX_NATIVE_DA_MSG_SIZE <= MAX_BLOCK_DATA_MSG_SIZE);
        // A single shard is strictly smaller than a whole body, so the shard cap
        // is a tighter rung than the native-DA whole-body pull cap.
        assert!(MAX_NATIVE_DA_SHARDS_MSG_SIZE <= MAX_NATIVE_DA_MSG_SIZE);
    }

    #[test]
    fn shard_cap_admits_worst_case_shard() {
        // Worst single data shard = largest body / smallest k, + proof + header.
        // NATIVE_BLOCK_BYTES_CAP (6 MB) mirrors torus-mempool rate_limit.rs; the
        // lower crate can't dep the mempool, so it's pinned here as a literal.
        const NATIVE_BLOCK_BYTES_CAP: usize = 6 * 1024 * 1024;
        const WORST_PROOF: usize = 8 * 32; // ≤ 8 Merkle levels × 32-byte hashes
        const FIXED_HEADER: usize = 64; // index/k/n/body_len/root framing slack
        let worst_shard = NATIVE_BLOCK_BYTES_CAP / 2 + WORST_PROOF + FIXED_HEADER;
        assert!(
            MAX_NATIVE_DA_SHARDS_MSG_SIZE >= worst_shard,
            "shard cap {} must admit the worst-case single shard {}",
            MAX_NATIVE_DA_SHARDS_MSG_SIZE,
            worst_shard
        );
    }
}
