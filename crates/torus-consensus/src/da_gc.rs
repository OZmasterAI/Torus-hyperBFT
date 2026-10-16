//! RH4: durable DA body GC policy: how long a committed body must stay, and
//! how much one GC pass may do.
//!
//! Committed native-action bodies live in the consensus DB's
//! `cf_native_pending`, and with shard custody on (the production default)
//! as erasure shards in `cf_native_shards`. Before RH4 nothing deleted them.
//! The GC itself is [`torus_state::NativeDaStore::gc_committed`], run once per
//! dispatched committed height from `TorusApp::dispatch_to_exec`.
//!
//! # Why the window follows the block-tree retention
//!
//! A peer that falls behind catches up by block sync, which ships hotstuff
//! blocks verbatim: compact datums that carry only the action hashes. It then
//! pulls the bodies by hash (`/torus/native-da/1.0`, or `k` shards). Sync
//! servers and pull targets are validators only, and there is no state sync.
//! So a body must stay as long as its block can still be served by block sync:
//! otherwise a peer syncing that block finds the body nowhere, opens an
//! execution hole and fail-stops after the heal budget. A validator serves the
//! blocks its block tree keeps: all of them on an archive node (no
//! `--retention-blocks`), the last `max(N, 1000)` heights with
//! `--retention-blocks N` (hotstuff `prune_old_committed_blocks_with`). Hence:
//!
//! - archive: no GC; the node stays a full sync source,
//! - pruning: window = block-tree retention + [`DA_GC_MARGIN_BLOCKS`],
//! - `TORUS_DA_BODY_RETENTION=N` overrides: `0` = off, `N` = window `N`
//!   (floor [`MIN_DA_BODY_RETENTION`]). A window below the block-tree window,
//!   or any window on an archive node, means a peer lagging further than it
//!   cannot get bodies from this node.
//!
//! The window counts DISPATCHED heights (the block's body row is durable in
//! the state DB's `CF_BLOCK_BODIES`), not consensus commits: a committed block
//! still parked behind an execution hole is re-read from the DA store, so its
//! bodies are not even recorded for GC until it is dispatched. The dispatched
//! height never exceeds the committed height, so a body outlives its block in
//! the block tree.

use torus_state::DaGcLimits;

/// Blocks a body outlives the block-tree window by when the window is derived
/// from `--retention-blocks`: covers the block-tree pruner's lag (it prunes at
/// most 64 heights per commit).
pub const DA_GC_MARGIN_BLOCKS: u64 = 128;

/// Smallest window `TORUS_DA_BODY_RETENTION` may set (the July RH4 default):
/// well past the push-race and normal follower lag.
pub const MIN_DA_BODY_RETENTION: u64 = 128;

/// Per-pass bounds on the consensus thread. Steady state is one row per pass
/// (one block: at most the proposer cap of 400 actions by default, each one
/// body key plus one key per shard). The bounds only bite on a backlog (a
/// smaller window after a restart, or a burst of dispatches), which then
/// drains at up to 7 heights per dispatch.
pub const DA_GC_LIMITS: DaGcLimits = DaGcLimits {
    max_rows: 8,
    max_deletes: 16_384,
};

/// The DA body GC window in blocks, `None` = GC off.
///
/// `env` is `TORUS_DA_BODY_RETENTION`; `block_tree_retention` is the active
/// hotstuff block-tree window (`None` = archive). See the module docs. An
/// unparsable `env` falls back to the derived window, so a typo neither turns
/// the GC on for an archive node nor off for a pruning one.
pub fn resolve_da_body_retention(
    env: Option<&str>,
    block_tree_retention: Option<u64>,
) -> Option<u64> {
    match env.map(str::trim).map(str::parse::<u64>) {
        Some(Ok(0)) => None,
        Some(Ok(window)) => Some(window.max(MIN_DA_BODY_RETENTION)),
        Some(Err(_)) | None => {
            block_tree_retention.map(|tree| tree.saturating_add(DA_GC_MARGIN_BLOCKS))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_state::{NativeDaStore, StateDb};
    use torus_types::{compute_action_hash, SignedNativeAction};

    #[test]
    fn archive_node_keeps_every_body_by_default() {
        assert_eq!(resolve_da_body_retention(None, None), None);
    }

    #[test]
    fn pruning_node_derives_the_window_from_the_block_tree() {
        assert_eq!(resolve_da_body_retention(None, Some(1000)), Some(1128));
        assert_eq!(
            resolve_da_body_retention(None, Some(100_000)),
            Some(100_128)
        );
        assert_eq!(
            resolve_da_body_retention(None, Some(u64::MAX)),
            Some(u64::MAX)
        );
    }

    #[test]
    fn env_overrides_the_derived_window() {
        assert_eq!(
            resolve_da_body_retention(Some("0"), Some(1000)),
            None,
            "0 = off"
        );
        assert_eq!(resolve_da_body_retention(Some(" 5000 "), None), Some(5000));
        assert_eq!(
            resolve_da_body_retention(Some("5000"), Some(1000)),
            Some(5000)
        );
        assert_eq!(
            resolve_da_body_retention(Some("5"), Some(1000)),
            Some(MIN_DA_BODY_RETENTION),
            "explicit windows are floored"
        );
    }

    #[test]
    fn unparsable_env_never_silently_disables_or_enables_gc() {
        for raw in ["", "  ", "off", "-1", "12k"] {
            assert_eq!(
                resolve_da_body_retention(Some(raw), Some(1000)),
                Some(1128),
                "{raw:?}"
            );
            assert_eq!(resolve_da_body_retention(Some(raw), None), None, "{raw:?}");
        }
    }

    fn action(nonce: u64) -> SignedNativeAction {
        use torus_types::{ActionSignature, NativeAction, Signature};
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

    /// hotstuff `prune_old_committed_blocks_with` keeps heights
    /// `highest - retention ..= highest` (block_tree/accessors/internal.rs).
    fn tree_serves(height: u64, highest: u64, retention: u64) -> bool {
        highest <= retention || height >= highest - retention
    }

    /// A lagging peer can always finish what block sync hands it: at every
    /// tip, every block the block tree still serves has its body in the DA
    /// store (served by `/torus/native-da/1.0`), while bodies older than the
    /// window are gone. Uses the smallest block-tree window hotstuff allows.
    #[test]
    fn every_block_the_tree_serves_keeps_its_body() {
        let tree = hotstuff_rs::block_tree::MIN_BLOCK_TREE_RETENTION;
        let window = resolve_da_body_retention(None, Some(tree)).expect("pruning => GC on");
        let dir = tempfile::tempdir().unwrap();
        let store = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        let actions: Vec<_> = (0..400).map(action).collect();
        store.put_batch(&actions).unwrap();
        for (tip, a) in actions.iter().enumerate() {
            let tip = tip as u64;
            store
                .gc_committed(tip, &[compute_action_hash(a)], 0, window, DA_GC_LIMITS)
                .unwrap();
            for (h, b) in actions[..=tip as usize].iter().enumerate() {
                let h = h as u64;
                let present = store.contains(&compute_action_hash(b).0).unwrap();
                if tree_serves(h, tip, tree) {
                    assert!(
                        present,
                        "tip {tip}: block {h} is served, its body must be too"
                    );
                }
                assert_eq!(present, h + window > tip, "tip {tip}: body of {h}");
            }
        }
    }
}
