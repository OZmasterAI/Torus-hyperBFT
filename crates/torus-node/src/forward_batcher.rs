//! B1 — batched direct-to-leader forward envelope (Package B, design §1).
//!
//! Coalesces RPC-admitted native actions into ONE `/torus/direct` request per
//! (flush window, leader), reusing the 0xFD pre-proposal batch wire format
//! (`0xFD ‖ bincode(Vec<(Address, SignedNativeAction)>)`) that every deployed
//! binary already decodes, verifies, dedups, and pool-inserts. This replaces
//! the one-request-per-action 0xFE path, whose request rate is O(client
//! actions/s) — at client batch_size=1 that exceeds the request-response
//! stream cap (100/connection) the moment leader latency spikes. Batched, the
//! request rate is O(10–200)/s regardless of client batching.
//!
//! Knobs (all `OnceLock` + pure-parse-seam, same idiom as
//! `TORUS_HASH_ONLY_PUSH_THRESHOLD`):
//! - `TORUS_D2L_BATCH` (default `1`): `0` restores the per-action 0xFE
//!   forward path byte-for-byte (the documented rollback).
//! - `TORUS_D2L_BATCH_MS` (default `25`): flush interval — half the 50 ms
//!   gossip pre-spread cadence, because in d2l mode the forward is on the
//!   inclusion critical path.
//! - `TORUS_D2L_BATCH_MAX_BYTES` (default `524288`, clamped to the 4 MB
//!   legacy-fleet direct-codec floor): envelope byte cap.
//! - `TORUS_D2L_REFORWARD_MS` (default `2000`, `0` = off): re-forward sweep —
//!   re-enqueue the oldest still-pending pool actions to the CURRENT leader,
//!   healing lost envelopes and stale leader hints (dedup at the leader makes
//!   this idempotent).

#[cfg(test)]
mod tests {
    use super::*;
    use torus_network::caps::LEGACY_FLEET_DIRECT_MSG_FLOOR;
    use torus_types::{ActionSignature, NativeAction, Signature, SignedNativeAction};

    fn act(nonce: u64) -> SignedNativeAction {
        SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        }
    }

    fn addr(seed: u8) -> torus_types::Address {
        torus_types::Address::from([seed; 20])
    }

    fn vk(seed: u8) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0] = seed;
        b
    }

    /// One serialized pair's size on the wire, for byte-cap arithmetic.
    fn pair_size() -> usize {
        bincode::serialized_size(&(addr(1), act(1))).unwrap() as usize
    }

    /// B1 (RED first): items accumulate per target and stay pending under the
    /// byte cap — the tick (flush_all) is what ships them, ONE envelope per
    /// (window, target), pairs in arrival order. MUST fail before the module
    /// exists.
    #[test]
    fn batcher_one_envelope_per_target_per_window() {
        let mut b = ForwardBatcher::new(1024 * 1024);
        // 3 items for leader A, 2 for leader B, all far below the cap.
        assert!(b.push(vk(1), addr(1), act(1)).is_none());
        assert!(b.push(vk(1), addr(2), act(2)).is_none());
        assert!(b.push(vk(1), addr(3), act(3)).is_none());
        assert!(b.push(vk(2), addr(4), act(4)).is_none());
        assert!(b.push(vk(2), addr(5), act(5)).is_none());
        assert!(!b.is_empty());

        let mut flushed = b.flush_all();
        flushed.sort_by_key(|(t, _)| *t);
        assert_eq!(flushed.len(), 2, "one envelope per target per window");
        assert_eq!(flushed[0].0, vk(1));
        assert_eq!(
            flushed[0].1.iter().map(|(a, _)| *a).collect::<Vec<_>>(),
            vec![addr(1), addr(2), addr(3)],
            "arrival order preserved"
        );
        assert_eq!(flushed[1].0, vk(2));
        assert_eq!(flushed[1].1.len(), 2);
        assert!(b.is_empty(), "flush_all drains every bucket");
        assert!(b.flush_all().is_empty(), "nothing pending after a flush");
    }

    /// B1 (RED first): the byte cap forces an envelope out ahead of the tick —
    /// both when the bucket lands exactly at/over the cap (ship it now) and
    /// when the incoming item would push a pending bucket over (ship the
    /// pending pairs first; the newcomer starts the next bucket). Every
    /// envelope stays <= the cap, hence <= the 4 MB legacy codec floor.
    #[test]
    fn batcher_flushes_on_byte_cap() {
        let p = pair_size();

        // Cap = exactly two pairs: the second push lands AT the cap and ships
        // both immediately (no waiting out the tick).
        let mut b = ForwardBatcher::new(VEC_LEN_PREFIX_BYTES + 2 * p);
        assert!(b.push(vk(1), addr(1), act(1)).is_none());
        let (target, pairs) = b.push(vk(1), addr(2), act(2)).expect("cap flush");
        assert_eq!(target, vk(1));
        assert_eq!(pairs.len(), 2);
        assert!(
            bincode::serialized_size(&pairs).unwrap() as usize
                <= VEC_LEN_PREFIX_BYTES + 2 * p,
            "flushed envelope body within the cap"
        );
        assert!(b.is_empty());

        // Cap = one-and-a-half pairs: the second push WOULD exceed, so the
        // pending single-pair bucket ships and the newcomer stays pending.
        let mut b = ForwardBatcher::new(VEC_LEN_PREFIX_BYTES + p + p / 2);
        assert!(b.push(vk(1), addr(1), act(1)).is_none());
        let (_, pairs) = b.push(vk(1), addr(2), act(2)).expect("pre-cap flush");
        assert_eq!(pairs.len(), 1, "pending pairs ship; newcomer starts fresh");
        assert_eq!(pairs[0].0, addr(1));
        let rest = b.flush_all();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].1[0].0, addr(2), "newcomer flushed on the next tick");

        // A single item at/over the cap alone still ships (alone) rather than
        // wedging the bucket.
        let mut b = ForwardBatcher::new(1);
        let (_, pairs) = b.push(vk(1), addr(3), act(3)).expect("oversized-alone flush");
        assert_eq!(pairs.len(), 1);
        assert!(b.is_empty());
    }

    /// B1 (RED first): the incremental byte accounting must equal the real
    /// bincode envelope-body size, or the cap drifts from what the receiving
    /// codec measures (same invariant the gossip batcher pins in
    /// `native_batch_size_accounting_matches_serializer`).
    #[test]
    fn batcher_byte_accounting_matches_serializer() {
        let mut b = ForwardBatcher::new(usize::MAX);
        for i in 0..5u8 {
            b.push(vk(1), addr(i + 1), act(u64::from(i) + 10));
        }
        let tracked = VEC_LEN_PREFIX_BYTES + b.bucket_bytes(&vk(1));
        let flushed = b.flush_all();
        assert_eq!(
            tracked,
            bincode::serialize(&flushed[0].1).unwrap().len(),
            "running budget must equal the on-wire envelope body size"
        );
    }

    /// B1 (RED first): the forward target is re-resolved AT FLUSH TIME — a
    /// leader rotation inside the window re-targets the whole envelope (kills
    /// intra-window staleness); if this node became the leader, the flush is
    /// skipped entirely (the actions are already in the local pool); with no
    /// current hint the admission-time bucket target is the fallback.
    #[test]
    fn flush_target_re_resolves_leader() {
        let own = vk(9);
        // Current leader hint wins over the (stale) bucket target.
        assert_eq!(resolve_flush_target(vk(1), Some(vk(2)), own), Some(vk(2)));
        // We became the leader: skip — never forward to self.
        assert_eq!(resolve_flush_target(vk(1), Some(own), own), None);
        // No hint: fall back to the admission-time target.
        assert_eq!(resolve_flush_target(vk(1), None, own), Some(vk(1)));
        // No hint and the bucket target IS us: skip.
        assert_eq!(resolve_flush_target(own, None, own), None);
    }

    /// B1 (RED first): the knob parse seams. `TORUS_D2L_BATCH` unset/garbage
    /// defaults ON (`0` is the exact-today per-action rollback); the flush
    /// interval and byte cap fall back on unset/zero/garbage; the byte cap is
    /// CLAMPED to the legacy-fleet 4 MB direct-codec floor; the re-forward
    /// sweep accepts `0` as a real value (off).
    #[test]
    fn d2l_knob_parse_seams() {
        // TORUS_D2L_BATCH — default ON.
        assert!(parse_d2l_batch(None));
        assert!(parse_d2l_batch(Some("1")));
        assert!(parse_d2l_batch(Some(" on ")));
        assert!(!parse_d2l_batch(Some("0")));
        assert!(!parse_d2l_batch(Some("false")));
        assert!(parse_d2l_batch(Some("garbage")), "typo never flips the path");

        // TORUS_D2L_BATCH_MS — default 25; zero would busy-loop the tick.
        assert_eq!(parse_d2l_batch_ms(None), 25);
        assert_eq!(parse_d2l_batch_ms(Some("100")), 100);
        assert_eq!(parse_d2l_batch_ms(Some("0")), 25);
        assert_eq!(parse_d2l_batch_ms(Some("junk")), 25);

        // TORUS_D2L_BATCH_MAX_BYTES — default 512 KB, clamped to the floor.
        let floor = LEGACY_FLEET_DIRECT_MSG_FLOOR;
        assert_eq!(parse_d2l_batch_max_bytes(None, floor), 512 * 1024);
        assert_eq!(parse_d2l_batch_max_bytes(Some("1048576"), floor), 1048576);
        assert_eq!(
            parse_d2l_batch_max_bytes(Some("8000000"), floor),
            floor,
            "cap above the legacy codec floor is clamped (S388 lesson)"
        );
        assert_eq!(parse_d2l_batch_max_bytes(Some("0"), floor), 512 * 1024);
        assert_eq!(parse_d2l_batch_max_bytes(Some("junk"), floor), 512 * 1024);

        // TORUS_D2L_REFORWARD_MS — default 2000; 0 is a REAL value (off).
        assert_eq!(parse_d2l_reforward_ms(None), 2000);
        assert_eq!(parse_d2l_reforward_ms(Some("0")), 0);
        assert_eq!(parse_d2l_reforward_ms(Some("500")), 500);
        assert_eq!(parse_d2l_reforward_ms(Some("junk")), 2000);
    }
}
