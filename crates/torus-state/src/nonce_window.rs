//! D12 nonce window: the consumed-nonce set [`CF_NATIVE_NONCES`] holds only the
//! nonces still inside the execution nonce window.
//!
//! Execution rejects a nonce older than `block_ms - NONCE_WINDOW_MS`
//! (`torus_types::eip712::check_nonce_window`), and block timestamps never go
//! backwards (the parent-link check), so once a block's cutoff passes a row
//! that row can never be needed again. Every block deletes the rows with
//! `cutoff(parent block_ms) <= nonce < cutoff(block_ms)` (strictly below its
//! own cutoff: a nonce equal to the cutoff is still valid). After each block no
//! row below the window exists.
//!
//! The key is time-major (nonce(8 BE) ++ sender(20), [`native_nonce_key`]), so
//! the expired rows are one contiguous key range starting at the parent's
//! cutoff. The parent cutoff only bounds the scan (the rows below it were
//! deleted by earlier blocks); starting lower finds the same rows.

use crate::backend::StateBackend;
use crate::cf::CF_NATIVE_NONCES;
use crate::error::StateError;

pub use crate::cf::{native_nonce_key, native_nonce_key_parts};

/// Rows read per bounded seek. A scan reads at most the expired rows plus one
/// chunk past the cutoff.
pub const NONCE_PRUNE_CHUNK: usize = 64;

/// The oldest nonce (ms) still valid at `block_ms`: nonces strictly below it
/// are expired (`check_nonce_window` rejects them).
pub fn nonce_cutoff_ms(block_ms: u64) -> u64 {
    block_ms.saturating_sub(torus_types::eip712::NONCE_WINDOW_MS)
}

/// The rows one block deletes, and how many rows the scan read.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExpiredNonces {
    /// Keys to delete, in key (= nonce) order.
    pub keys: Vec<Vec<u8>>,
    /// Rows returned by the bounded seeks (deleted rows plus at most one
    /// chunk past the cutoff).
    pub scanned: usize,
}

/// The [`CF_NATIVE_NONCES`] rows a block at `block_ms` whose parent ran at
/// `parent_block_ms` deletes: every live row (read through `state`, so every
/// overlay layer and its tombstones count) with
/// `cutoff(parent_block_ms) <= nonce < cutoff(block_ms)`. Nothing is read when
/// both cutoffs are equal (blocks in the same second).
pub fn expired_nonce_keys<B: StateBackend>(
    state: &B,
    parent_block_ms: u64,
    block_ms: u64,
) -> Result<ExpiredNonces, StateError> {
    let from = nonce_cutoff_ms(parent_block_ms);
    let to = nonce_cutoff_ms(block_ms);
    let mut out = ExpiredNonces::default();
    if from >= to {
        return Ok(out);
    }
    // Keys are nonce(8 BE) ++ sender(20): a key is below `to` exactly when it
    // sorts below the 8-byte prefix `to` (a key whose nonce equals `to` has
    // that prefix and is longer, so it sorts above it).
    let end = to.to_be_bytes();
    let mut start = from.to_be_bytes().to_vec();
    loop {
        let rows = state.iterate_cf_from(CF_NATIVE_NONCES, &start, NONCE_PRUNE_CHUNK)?;
        out.scanned += rows.len();
        let full = rows.len() == NONCE_PRUNE_CHUNK;
        for (key, _) in rows {
            if key.as_slice() >= end.as_slice() {
                return Ok(out);
            }
            out.keys.push(key);
        }
        if !full {
            return Ok(out);
        }
        // The smallest key above the last one read.
        start = out.keys.last().expect("a full chunk below the cutoff").clone();
        start.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NativeStateOverlay;
    use crate::db::StateDb;
    use alloy_primitives::Address;

    fn temp_db() -> (StateDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let db = StateDb::open(dir.path()).expect("open db");
        (db, dir)
    }

    fn put(db: &impl StateBackend, sender: u8, nonce: u64) {
        db.put_cf_raw(
            CF_NATIVE_NONCES,
            &native_nonce_key(&Address::repeat_byte(sender), nonce),
            &1u64.to_be_bytes(),
        )
        .unwrap();
    }

    fn nonces(e: &ExpiredNonces) -> Vec<(u64, Address)> {
        e.keys
            .iter()
            .map(|k| native_nonce_key_parts(k).unwrap())
            .collect()
    }

    const T: u64 = 1_700_000_000_000;
    const W: u64 = torus_types::eip712::NONCE_WINDOW_MS;

    /// D12: the key is time-major — nonce(8 BE) then sender(20) — and
    /// decodes back to the pair.
    #[test]
    fn nonce_key_is_time_major_and_round_trips() {
        let a = Address::repeat_byte(0xaa);
        let b = Address::repeat_byte(0x01);
        let k = native_nonce_key(&a, 0x0102_0304_0506_0708);
        assert_eq!(k.len(), 28);
        assert_eq!(&k[..8], &0x0102_0304_0506_0708u64.to_be_bytes());
        assert_eq!(&k[8..], a.as_slice());
        assert_eq!(native_nonce_key_parts(&k), Some((0x0102_0304_0506_0708, a)));
        assert_eq!(native_nonce_key_parts(&k[..27]), None, "wrong length");
        // Ordered by nonce first: sender 0xaa at nonce 5 sorts below sender
        // 0x01 at nonce 6.
        assert!(native_nonce_key(&a, 5) < native_nonce_key(&b, 6));
    }

    /// Boundary: a nonce equal to `block_ms - 60 s` stays (still valid), one
    /// ms older goes; rows above stay; the parent's cutoff starts the range.
    #[test]
    fn expired_keys_strictly_below_the_cutoff() {
        let (db, _d) = temp_db();
        let parent = T;
        let block = T + 5_000;
        let cutoff = block - W;
        put(&db, 1, nonce_cutoff_ms(parent)); // first row of the range
        put(&db, 2, cutoff - 1); // last row of the range
        put(&db, 3, cutoff); // valid at block: stays
        put(&db, 4, cutoff + 1);
        put(&db, 5, block);
        let e = expired_nonce_keys(&db, parent, block).unwrap();
        assert_eq!(
            nonces(&e),
            vec![
                (nonce_cutoff_ms(parent), Address::repeat_byte(1)),
                (cutoff - 1, Address::repeat_byte(2)),
            ]
        );
        // Same senders in the other order: still nonce-major.
        put(&db, 0xff, cutoff - 2);
        let e = expired_nonce_keys(&db, parent, block).unwrap();
        assert_eq!(e.keys.len(), 3);
        assert_eq!(native_nonce_key_parts(&e.keys[1]).unwrap().0, cutoff - 2);
    }

    /// Blocks in the same second share `block_ms`: nothing is read.
    #[test]
    fn same_second_block_reads_nothing() {
        let (db, _d) = temp_db();
        put(&db, 1, T - W - 1);
        let e = expired_nonce_keys(&db, T, T).unwrap();
        assert_eq!(e, ExpiredNonces::default());
    }

    /// First block after genesis (parent time 0) and block times below the
    /// window: the cutoffs saturate at 0, nothing below 0 exists.
    #[test]
    fn genesis_parent_and_small_times() {
        let (db, _d) = temp_db();
        put(&db, 1, 0);
        put(&db, 2, 10);
        assert!(expired_nonce_keys(&db, 0, W).unwrap().keys.is_empty());
        let e = expired_nonce_keys(&db, 0, W + 11).unwrap();
        assert_eq!(e.keys.len(), 2);
    }

    /// The scan is bounded: many expired rows across several chunks are all
    /// found, and many live rows above the cutoff cost at most one chunk.
    #[test]
    fn scan_is_bounded_by_expired_rows_plus_one_chunk() {
        let (db, _d) = temp_db();
        let parent = T;
        let block = T + 10_000;
        let cutoff = block - W;
        let expired = 3 * NONCE_PRUNE_CHUNK + 5;
        for i in 0..expired as u64 {
            put(&db, (i % 7) as u8, nonce_cutoff_ms(parent) + i * 10);
        }
        // Rows below the parent cutoff would have been deleted by earlier
        // blocks; a stray one is outside the range and never read.
        put(&db, 9, nonce_cutoff_ms(parent) - 1);
        for i in 0..20 * NONCE_PRUNE_CHUNK as u64 {
            put(&db, (i % 5) as u8, cutoff + i);
        }
        let e = expired_nonce_keys(&db, parent, block).unwrap();
        assert_eq!(e.keys.len(), expired);
        assert!(nonces(&e)
            .iter()
            .all(|(n, _)| *n >= nonce_cutoff_ms(parent) && *n < cutoff));
        assert!(
            e.scanned <= expired + NONCE_PRUNE_CHUNK,
            "scanned {} > {expired} + {NONCE_PRUNE_CHUNK}",
            e.scanned
        );
        // Exactly a multiple of the chunk: the last full chunk ends at the
        // cutoff, one more seek reads the first live rows.
        let (db, _d) = temp_db();
        for i in 0..2 * NONCE_PRUNE_CHUNK as u64 {
            put(&db, 1, nonce_cutoff_ms(parent) + i);
        }
        let e = expired_nonce_keys(&db, parent, block).unwrap();
        assert_eq!(e.keys.len(), 2 * NONCE_PRUNE_CHUNK);
    }

    /// Read through every overlay layer: rows of the DB, of a checkpoint
    /// layer, of the parent block's frozen set and of the overlay's own
    /// pending set are found; a tombstone in any upper layer hides the row.
    #[test]
    fn reads_through_parent_layer_and_tombstones() {
        let (db, _d) = temp_db();
        let parent = T;
        let block = T + 9_000;
        let base = nonce_cutoff_ms(parent);
        put(&db, 1, base); // DB, live
        put(&db, 2, base + 1); // DB, deleted in L
        put(&db, 3, base + 2); // DB, deleted by the parent block
        let l_set = NativeStateOverlay::new(db.clone());
        put(&l_set, 4, base + 3); // L, live
        l_set
            .delete_cf_raw(
                CF_NATIVE_NONCES,
                &native_nonce_key(&Address::repeat_byte(2), base + 1),
            )
            .unwrap();
        let mut layer = crate::checkpoint_layer::CheckpointLayer::new();
        layer.merge(&l_set.freeze(1));
        let layer = std::sync::Arc::new(layer);
        let p_set = NativeStateOverlay::with_layers(db.clone(), None, Some(layer.clone()), None);
        put(&p_set, 5, base + 4); // parent, live
        p_set
            .delete_cf_raw(
                CF_NATIVE_NONCES,
                &native_nonce_key(&Address::repeat_byte(3), base + 2),
            )
            .unwrap();
        let parent_set = p_set.freeze(2);
        let ov = NativeStateOverlay::with_layers(db.clone(), Some(parent_set), Some(layer), None);
        put(&ov, 6, base + 5); // own pending, live
        let e = expired_nonce_keys(&ov, parent, block).unwrap();
        let senders: Vec<u8> = nonces(&e).iter().map(|(_, a)| a.as_slice()[0]).collect();
        assert_eq!(senders, vec![1, 4, 5, 6]);
    }
}
