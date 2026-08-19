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
/// most `body/k + Merkle proof + fixed header`. The largest body is the 12 MB
/// `NATIVE_BLOCK_BYTES_CAP` (torus-mempool `rate_limit.rs`; r4 raised the
/// compiled default 6 -> 12 MB with the cap-200 block default) and the
/// smallest `k` is `f+1 = 2` at n=3, so the worst-case data shard is ≤ ~6 MB,
/// plus a proof (`≤ ceil(log2(255)) = 8` levels × 32 B = 256 B) and fixed
/// fields (< 64 B). 8 MiB sits above that worst case and at (not above) the
/// whole-body pull cap, so an oversize/bomb shard frame is still rejected at a
/// bound tighter than the block-data sync codec. Accept-side (read cap) only —
/// widening it is liveness-safe.
pub const MAX_NATIVE_DA_SHARDS_MSG_SIZE: usize = 8 * 1024 * 1024; // 8 MB

/// `/torus/block-data` response cap — must fit a full big block during sync.
pub const MAX_BLOCK_DATA_MSG_SIZE: usize = 16 * 1024 * 1024; // 16 MB

/// Gossipsub transport frame cap (bounds publish AND inbound decode). Was
/// hardcoded at the builder call site in behaviour.rs — same silent-drift
/// class as the S391 heartbeat bug. The consensus/tx accept gates
/// (`NetworkConfig::max_consensus_message_size` / `max_tx_message_size`) must
/// stay at or under this or a legal message becomes unpublishable.
pub const GOSSIP_MAX_TRANSMIT_SIZE: usize = 2 * 1024 * 1024; // 2 MiB

/// Direct-msg read cap of the pre-O5 binary (4 MB) — the historical fleet
/// floor. Kept as the documented "straggler" value: an operator whose fleet
/// still carries a pre-O5 codec sets `TORUS_DIRECT_PUSH_BODY_BYTES=4194304`
/// to pin send policy back under it. Since r4 the effective floor is
/// [`direct_push_body_bytes`] (default [`DIRECT_PUSH_BODY_BYTES`]).
pub const LEGACY_FLEET_DIRECT_MSG_FLOOR: usize = 4 * 1024 * 1024; // 4 MB

/// r4 direct-push body floor: the largest pre-proposal BODY SET (encoded
/// bytes) send policy may push as a full-body `/torus/direct` request; body
/// sets above the (env-tunable) hash-only threshold — itself clamped to this
/// floor — go out as a hash manifest and are pulled. O5 widened every
/// validator's `/torus/direct` read cap to 8 MiB but pinned send policy at the
/// 4 MB pre-O5 floor "until every validator carries this build"; the r3
/// block-cap-raise sweep showed cap 200/300 bodies at bs400 (~5.6-8 MB) all
/// falling off the direct path onto ~45 HASH-ONLY manifest pushes per node
/// purely because of that pin. 8_000_000 decimal (the `TORUS_*_THRESHOLD`
/// convention) leaves ~388 KB of headroom under the 8 MiB codec cap for the
/// `DirectRequest` framing (0xFD marker + 32 B sender key + length prefix)
/// and any zstd expansion on incompressible bytes.
///
/// LIVENESS-ONLY, never a consensus rule: a straggler whose codec still reads
/// 4 MB rejects a >4 MB push and recovers the bodies via the chunked
/// `/torus/native-da` pull (exactly how the S388 6 MB env override survived).
/// Send policy CANNOT be raised above our own codec cap (a push no same-build
/// peer could read) — see [`resolve_direct_push_body_bytes`].
pub const DIRECT_PUSH_BODY_BYTES: usize = 8_000_000;

/// Headroom kept between the direct-push body floor and `MAX_DIRECT_MSG_SIZE`
/// so a body set exactly at the floor still fits its `DirectRequest` frame
/// (marker byte + 32 B sender key + 4 B borsh length prefix + slack).
pub const DIRECT_PUSH_FRAME_MARGIN: usize = 4096;

/// Hard ceiling for the direct-push body floor: our own codec cap minus the
/// framing margin. A floor above this orders pushes NO peer (even a same-build
/// one) could read.
pub const DIRECT_PUSH_BODY_BYTES_CEILING: usize = MAX_DIRECT_MSG_SIZE - DIRECT_PUSH_FRAME_MARGIN;

/// Pure resolution seam for [`direct_push_body_bytes`]: unset / malformed / 0
/// => `DIRECT_PUSH_BODY_BYTES`; an explicit positive value is honored but
/// CLAMPED to `ceiling` (never above what any peer's codec can read). Lowering
/// (e.g. `4194304` for a pre-O5 straggler fleet) is always honored verbatim.
pub fn resolve_direct_push_body_bytes(raw: Option<&str>, ceiling: usize) -> usize {
    match raw.and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) if n > 0 => n.min(ceiling),
        _ => DIRECT_PUSH_BODY_BYTES.min(ceiling),
    }
}

/// Effective direct-push body floor: `TORUS_DIRECT_PUSH_BODY_BYTES` overrides
/// the compiled default PER NODE, read once at first use, clamped to
/// [`DIRECT_PUSH_BODY_BYTES_CEILING`]. Transport send policy only (receivers
/// accept both push forms regardless) — mixed values cannot fork. Both send
/// policies that must stay under the fleet's direct read cap route here: the
/// hash-only push threshold (torus-network `bridge.rs`) and the d2l forward
/// batch size (torus-node `forward_batcher.rs`).
pub fn direct_push_body_bytes() -> usize {
    static FLOOR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *FLOOR.get_or_init(|| {
        let raw = std::env::var("TORUS_DIRECT_PUSH_BODY_BYTES").ok();
        let effective =
            resolve_direct_push_body_bytes(raw.as_deref(), DIRECT_PUSH_BODY_BYTES_CEILING);
        if let Some(ref r) = raw {
            if r.trim()
                .parse::<usize>()
                .map(|b| b > DIRECT_PUSH_BODY_BYTES_CEILING)
                .unwrap_or(false)
            {
                tracing::warn!(
                    requested = %r,
                    ceiling = DIRECT_PUSH_BODY_BYTES_CEILING,
                    "TORUS_DIRECT_PUSH_BODY_BYTES above the /torus/direct codec cap — clamped"
                );
            }
        }
        effective
    })
}

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
        // r4: the direct-push body floor (default AND any env value) sits under
        // our own direct codec cap with framing headroom, and the historical
        // 4 MB straggler value stays reachable by lowering.
        assert!(HASH_ONLY_PUSH_THRESHOLD < DIRECT_PUSH_BODY_BYTES);
        assert!(LEGACY_FLEET_DIRECT_MSG_FLOOR <= DIRECT_PUSH_BODY_BYTES);
        assert!(DIRECT_PUSH_BODY_BYTES <= DIRECT_PUSH_BODY_BYTES_CEILING);
        assert_eq!(
            DIRECT_PUSH_BODY_BYTES_CEILING + DIRECT_PUSH_FRAME_MARGIN,
            MAX_DIRECT_MSG_SIZE
        );
        assert!(direct_push_body_bytes() <= DIRECT_PUSH_BODY_BYTES_CEILING);
        // Recovery paths widen monotonically: push < pull < sync — each
        // fallback must be able to carry anything the faster path gave up on.
        assert!(MAX_DIRECT_MSG_SIZE <= MAX_NATIVE_DA_MSG_SIZE);
        assert!(MAX_NATIVE_DA_MSG_SIZE <= MAX_BLOCK_DATA_MSG_SIZE);
        // A single shard is strictly smaller than a whole body, so the shard cap
        // is a tighter rung than the native-DA whole-body pull cap.
        assert!(MAX_NATIVE_DA_SHARDS_MSG_SIZE <= MAX_NATIVE_DA_MSG_SIZE);
    }

    #[test]
    fn direct_push_body_bytes_resolves_defaults_and_clamps() {
        let ceiling = DIRECT_PUSH_BODY_BYTES_CEILING;
        // Unset / malformed / 0 => the compiled 8 MB default.
        assert_eq!(resolve_direct_push_body_bytes(None, ceiling), 8_000_000);
        assert_eq!(resolve_direct_push_body_bytes(Some("junk"), ceiling), 8_000_000);
        assert_eq!(resolve_direct_push_body_bytes(Some(""), ceiling), 8_000_000);
        assert_eq!(resolve_direct_push_body_bytes(Some("0"), ceiling), 8_000_000);
        // Lowering to the pre-O5 straggler floor is honored verbatim.
        assert_eq!(
            resolve_direct_push_body_bytes(Some("4194304"), ceiling),
            LEGACY_FLEET_DIRECT_MSG_FLOOR
        );
        assert_eq!(resolve_direct_push_body_bytes(Some("  6000000 "), ceiling), 6_000_000);
        // Above our own codec cap => clamped: no peer could read such a push.
        assert_eq!(resolve_direct_push_body_bytes(Some("16000000"), ceiling), ceiling);
        assert_eq!(
            resolve_direct_push_body_bytes(Some(&MAX_DIRECT_MSG_SIZE.to_string()), ceiling),
            ceiling
        );
        // The default itself is clamped by a lower ceiling (defensive: a future
        // codec-cap cut can never leave the default above it).
        assert_eq!(resolve_direct_push_body_bytes(None, 1_000_000), 1_000_000);
    }

    #[test]
    fn shard_cap_admits_worst_case_shard() {
        // Worst single data shard = largest body / smallest k, + proof + header.
        // NATIVE_BLOCK_BYTES_CAP (12 MB since r4) mirrors torus-mempool
        // rate_limit.rs; the lower crate can't dep the mempool, so it's pinned
        // here as a literal (the cross-crate equality is asserted in
        // torus-integration-tests o5_feed_gates.rs).
        const NATIVE_BLOCK_BYTES_CAP: usize = 12_000_000;
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
