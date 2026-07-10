//! Committed-block → App feed with a durable frontier (S444).
//!
//! ## The class of bug this closes (the "min-height exec-feed gap")
//!
//! [`BlockTreeSingleton::update`](crate::block_tree::accessors::internal::BlockTreeSingleton::update)
//! durably persists `HIGHEST_COMMITTED_BLOCK` **before** its caller fires
//! `App::on_committed_block` for the newly committed heights, and the commit
//! walk never revisits heights at or below `highest_committed` (`min_height`).
//! The two frontiers — hotstuff's committed frontier and the app's
//! executed/manifest frontier — are therefore *independently persisted with no
//! reconciliation*: a crash (or any future code path that advances
//! `highest_committed` without firing the callbacks) in the window between the
//! commit write and the callback loop loses those heights **forever**. Nothing
//! is ever enqueued for execution, so the app starves silently with no stall
//! logs (devnet t15: v3 committed to 962 while `on_committed_block` fired only
//! to 678 — 284 heights never executed). Block-sync catch-up makes the window
//! wide: a single `update` after a gap-deferral can commit hundreds of heights
//! in one jump.
//!
//! ## The fix
//!
//! Make the feed *replayable*: track a durable [`APP_FED_BLOCK_HEIGHT`]
//! frontier that is advanced **after** the callbacks run (the mirror image of
//! `HIGHEST_COMMITTED_BLOCK`, which is advanced before). Every delivery to the
//! app goes through [`feed_committed_blocks_to_app`], which walks the
//! `BLOCK_AT_HEIGHT` index from `fed + 1 ..= highest_committed` and fires
//! `on_committed_block` strictly in ascending height order. A crash between
//! the commit write and the fed-frontier write re-delivers on the next pass
//! instead of losing heights.
//!
//! The feed runs:
//! 1. after every `update()` that committed at least one block (the healthy
//!    hot path — delivers exactly the newly committed blocks, one extra small
//!    KV write per commit round, O(1) amortized);
//! 2. once at algorithm start-up (closes the crash-window gap on reboot); and
//! 3. periodically (throttled) from the algorithm loop — the watchdog that
//!    catches any *unknown cousin* path which advances `highest_committed`
//!    without feeding, converting it from a silent permanent starvation into a
//!    loud, self-healing event.
//!
//! ## Contract change
//!
//! `App::on_committed_block` is now **at-least-once, strictly ascending**: a
//! height is never skipped and never delivered out of order, but it MAY be
//! re-delivered after a restart. Consumers must treat re-deliveries of heights
//! at or below their own durable execution frontier as no-ops (torus-consensus
//! `TorusApp` does).
//!
//! ## Safety notes
//!
//! - The commit walk's semantics (gap-refusal, `min_height` monotonicity) are
//!   untouched — this module only *observes* `BLOCK_AT_HEIGHT`, which the
//!   gap-safe commit keeps contiguous up to `highest_committed`.
//! - If the feed encounters a height it cannot read (interior hole from a
//!   legacy store, or a fed frontier that fell below the pruner's retention
//!   horizon) it PAUSES at that height without advancing the fed frontier —
//!   strict order is never traded for progress. Backfill/body-heal closes
//!   tree holes and the feed resumes on the next pass; a pruned-away height is
//!   unrecoverable locally and is surfaced loudly (the app's own exec-hole
//!   budget then fail-stops the node, which is the correct terminal state).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::app::App;
use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::block_tree::pluggables::KVStore;
use crate::block_tree::variables::APP_FED_BLOCK_HEIGHT;
use crate::types::data_types::BlockHeight;

// Re-export referenced by the module doc.
#[allow(unused_imports)]
use crate::block_tree::invariants;

use crate::block_tree::accessors::internal::BlockTreeError;

/// Count of consecutive feed passes that could not advance past a missing
/// height (log throttle — see [`feed_committed_blocks_to_app`]).
static FEED_PAUSED_PASSES: AtomicU64 = AtomicU64::new(0);

/// Deliver every committed-but-not-yet-delivered block to the app, strictly in
/// ascending height order, then durably advance the fed frontier. Returns the
/// number of blocks delivered in this pass.
///
/// Idempotent and cheap when there is nothing to do (three point reads).
pub(crate) fn feed_committed_blocks_to_app<K: KVStore>(
    block_tree: &mut BlockTreeSingleton<K>,
    app: &mut impl App<K>,
) -> Result<u64, BlockTreeError> {
    let Some(highest) = block_tree.highest_committed_block_height()? else {
        // Nothing committed yet.
        return Ok(0);
    };

    // `anchored` = the feed has a durable frontier (or has delivered something
    // this pass): from then on a missing height is an interior hole and the
    // feed PAUSES on it. On the FIRST pass ever (no frontier yet — fresh chain
    // or first boot after the feed was introduced), leading missing heights are
    // instead SKIPPED: they lie below the chain's first block or below the
    // pruner's retention horizon, so they are locally unavailable by design and
    // pausing on them would wedge the feed forever. (Heights skipped this way
    // are at or below the app's own durable replay frontier in practice; an
    // app genuinely missing them is in the needs-resync state its exec-hole
    // budget already fail-stops on.)
    let (start, mut anchored) = match block_tree.app_fed_block_height()? {
        Some(fed) if fed >= highest => return Ok(0), // fast path: fully fed
        Some(fed) => (fed.int() + 1, true),
        None => (
            block_tree
                .block_tree_pruned_height()?
                .map(|h| h.int())
                .unwrap_or(0),
            false,
        ),
    };

    let mut delivered: u64 = 0;
    let mut last_fed: Option<u64> = None;
    for h in start..=highest.int() {
        let height = BlockHeight::new(h);
        let (hash, block) = match block_tree.block_at_height(height)? {
            Some(hash) => match block_tree.block(&hash)? {
                Some(block) => (hash, block),
                None if anchored => {
                    note_feed_pause(h, highest.int(), "block record missing");
                    break;
                }
                None => continue,
            },
            None if anchored => {
                note_feed_pause(h, highest.int(), "no BLOCK_AT_HEIGHT entry");
                break;
            }
            None => continue,
        };
        app.on_committed_block(&block, hash);
        anchored = true;
        last_fed = Some(h);
        delivered += 1;
    }

    if let Some(f) = last_fed {
        // Durable AFTER delivery: a crash before this write re-delivers, never
        // skips. One small write per feed pass that delivered anything.
        block_tree.advance_app_fed_block_height(BlockHeight::new(f))?;
        if f == highest.int() {
            FEED_PAUSED_PASSES.store(0, Ordering::Relaxed);
        }
    }

    Ok(delivered)
}

/// The feed could not read committed height `h`: log loudly (throttled after
/// the first few passes) without advancing the frontier past the hole.
fn note_feed_pause(h: u64, highest: u64, why: &str) {
    let n = FEED_PAUSED_PASSES.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 3 || n.is_power_of_two() {
        log::warn!(
            "app feed: PAUSED at committed height {} ({}; committed frontier {}, {} consecutive \
             paused passes) — refusing to skip: execution must receive every height in order. \
             A tree hole heals via backfill/body-fetch and the feed resumes; a height pruned \
             below the retention horizon is locally unrecoverable (node needs resync).",
            h,
            why,
            highest,
            n,
        );
    }
    // Silence the unused-import lint for the doc re-export above.
    let _ = &APP_FED_BLOCK_HEIGHT;
}

#[cfg(test)]
mod tests {
    //! S444 RED-first tests for the min-height exec-feed gap.
    //!
    //! RED at 2a80ff0: `feed_committed_blocks_to_app`, `APP_FED_BLOCK_HEIGHT`
    //! and its accessors did not exist (compile failure); behaviorally, heights
    //! committed durably whose callbacks were lost (kill window / sync jump)
    //! were NEVER delivered to the app — the walk's `min_height` boundary
    //! excluded them forever.

    use std::collections::HashMap;

    use super::*;
    use crate::app::{
        App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest,
        ValidateBlockResponse,
    };
    use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
    use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
    use crate::hotstuff::types::{Phase, PhaseCertificate};
    use crate::types::block::Block;
    use crate::types::crypto_primitives::SigningKey;
    use crate::types::data_types::{
        BlockHeight, ChainID, CryptoHash, Data, Datum, Power, SignatureSet, ViewNumber,
    };
    use crate::types::update_sets::AppStateUpdates;
    use crate::types::validator_set::{ValidatorSet, ValidatorSetState};

    // ---- Minimal in-memory KVStore (mirrors block_sync::client tests) ----
    #[derive(Clone, Default)]
    struct MemKV {
        map: HashMap<Vec<u8>, Vec<u8>>,
    }
    struct MemWb {
        sets: Vec<(Vec<u8>, Vec<u8>)>,
        deletes: Vec<Vec<u8>>,
    }
    #[derive(Clone)]
    struct MemSnap(HashMap<Vec<u8>, Vec<u8>>);
    impl WriteBatch for MemWb {
        fn new() -> Self {
            Self { sets: Vec::new(), deletes: Vec::new() }
        }
        fn set(&mut self, key: &[u8], value: &[u8]) {
            self.sets.push((key.to_vec(), value.to_vec()));
        }
        fn delete(&mut self, key: &[u8]) {
            self.deletes.push(key.to_vec());
        }
    }
    impl KVGet for MemKV {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.map.get(key).cloned()
        }
    }
    impl KVGet for MemSnap {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.0.get(key).cloned()
        }
    }
    impl KVStore for MemKV {
        type WriteBatch = MemWb;
        type Snapshot<'a> = MemSnap;
        fn write(&mut self, wb: MemWb) {
            for (k, v) in wb.sets {
                self.map.insert(k, v);
            }
            for k in wb.deletes {
                self.map.remove(&k);
            }
        }
        fn clear(&mut self) {
            self.map.clear();
        }
        fn snapshot<'b>(&'b self) -> MemSnap {
            MemSnap(self.map.clone())
        }
    }

    /// Records every height delivered via `on_committed_block`, in order.
    struct RecordingApp {
        seen: Vec<u64>,
    }
    impl App<MemKV> for RecordingApp {
        fn produce_block(&mut self, _r: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
            unreachable!("not reached by the feed")
        }
        fn validate_block(&mut self, _r: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
            unreachable!("not reached by the feed")
        }
        fn validate_block_for_sync(
            &mut self,
            _r: ValidateBlockRequest<MemKV>,
        ) -> ValidateBlockResponse {
            unreachable!("not reached by the feed")
        }
        fn on_committed_block(&mut self, block: &Block, _committed_hash: CryptoHash) {
            self.seen.push(block.height.int());
        }
    }

    fn initialized_tree() -> BlockTreeSingleton<MemKV> {
        let mut vs = ValidatorSet::new();
        vs.put(&SigningKey::from_bytes(&[1u8; 32]).verifying_key(), Power::new(1));
        let vss = ValidatorSetState::new(vs.clone(), vs, None, true);
        let mut tree = BlockTreeSingleton::new(MemKV::default());
        tree.initialize(&AppStateUpdates::new(), &vss).expect("init tree");
        tree
    }

    fn test_block(height: u64, justify: PhaseCertificate) -> Block {
        Block::new(
            BlockHeight::new(height),
            justify,
            CryptoHash::new([height as u8; 32]),
            Data::new(vec![Datum::new(vec![height as u8])]),
        )
    }

    /// Seed the DURABLE post-commit state for heights `1..=n` exactly as a
    /// crash in the write→callback window leaves it: block records +
    /// `BLOCK_AT_HEIGHT` + `HIGHEST_COMMITTED_BLOCK` present, but the app never
    /// received a single callback.
    fn seed_committed_chain(tree: &mut BlockTreeSingleton<MemKV>, n: u64) -> Vec<Block> {
        let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
        let mut blocks = Vec::new();
        for h in 1..=n {
            let b = test_block(h, PhaseCertificate::genesis_pc());
            wb.set_block(&b).expect("seed block");
            wb.set_block_at_height(BlockHeight::new(h), &b.hash).expect("seed height index");
            blocks.push(b);
        }
        wb.set_highest_committed_block(&blocks.last().unwrap().hash)
            .expect("seed committed frontier");
        tree.write(wb);
        blocks
    }

    /// THE work-order seam: heights durably committed (here 1..=5) whose
    /// callbacks never ran must ALL be delivered to the app by the feed, in
    /// strictly ascending order — the walk's `min_height` boundary must never
    /// hide them again.
    #[test]
    fn durably_committed_heights_never_fed_are_replayed_in_order() {
        let mut tree = initialized_tree();
        seed_committed_chain(&mut tree, 5);

        let mut app = RecordingApp { seen: Vec::new() };
        let delivered =
            feed_committed_blocks_to_app(&mut tree, &mut app).expect("feed must not error");

        assert_eq!(delivered, 5);
        assert_eq!(
            app.seen,
            vec![1, 2, 3, 4, 5],
            "EVERY committed height must reach the app exactly once, oldest first"
        );
        assert_eq!(
            tree.app_fed_block_height().unwrap(),
            Some(BlockHeight::new(5)),
            "the fed frontier must be durable after delivery"
        );
    }

    /// A REAL commit jump through `update()`: a chain of Generic-phase PCs with
    /// consecutive views commits heights 1..=4 in ONE update call (the
    /// block-sync catch-up shape). Dropping the `UpdateResult` — exactly what a
    /// kill between the durable commit write and the callback loop does — must
    /// not lose the heights: the next feed pass delivers all of them.
    #[test]
    fn commit_jump_with_lost_callbacks_is_recovered_by_feed() {
        let mut tree = initialized_tree();

        // b1 justified by genesis; b_k justified by a Generic PC over b_{k-1}
        // at view k-1 (consecutive views satisfy the commit rule).
        let mut blocks: Vec<Block> = Vec::new();
        for h in 1..=6u64 {
            let justify = if h == 1 {
                PhaseCertificate::genesis_pc()
            } else {
                PhaseCertificate {
                    chain_id: ChainID::new(0),
                    view: ViewNumber::new(h - 1),
                    block: blocks[(h - 2) as usize].hash,
                    phase: Phase::Generic,
                    signatures: SignatureSet::new(1),
                }
            };
            let b = test_block(h, justify);
            tree.insert(&b, None, None).expect("insert block");
            blocks.push(b);
        }

        // One update over b6's would-be PC commits the whole 1..=4 range
        // (grandparent chain of the 2-chain rule) in a single jump.
        let tip_pc = PhaseCertificate {
            chain_id: ChainID::new(0),
            view: ViewNumber::new(6),
            block: blocks[5].hash,
            phase: Phase::Generic,
            signatures: SignatureSet::new(1),
        };
        let result = tree.update(&tip_pc, &None).expect("update commits the jump");
        assert!(
            result.committed_block_hashes.len() >= 2,
            "precondition: the update must commit a multi-height jump (got {})",
            result.committed_block_hashes.len()
        );
        let committed_top = tree
            .highest_committed_block_height()
            .unwrap()
            .expect("frontier advanced")
            .int();
        // KILL WINDOW: `result` is dropped here — no callbacks fire, yet the
        // committed frontier is already durable. Pre-S444 these heights were
        // permanently lost to the app.
        drop(result);

        let mut app = RecordingApp { seen: Vec::new() };
        feed_committed_blocks_to_app(&mut tree, &mut app).expect("feed must not error");

        let want: Vec<u64> = (1..=committed_top).collect();
        assert_eq!(
            app.seen, want,
            "after a lost-callback commit jump, the feed must deliver every committed height \
             in ascending order"
        );
    }

    /// Incremental + idempotent: with the fed frontier at 2, only 3..=5 are
    /// delivered; a second pass delivers nothing.
    #[test]
    fn feed_is_incremental_and_idempotent() {
        let mut tree = initialized_tree();
        seed_committed_chain(&mut tree, 5);
        tree.advance_app_fed_block_height(BlockHeight::new(2)).unwrap();

        let mut app = RecordingApp { seen: Vec::new() };
        feed_committed_blocks_to_app(&mut tree, &mut app).unwrap();
        assert_eq!(app.seen, vec![3, 4, 5], "only the not-yet-fed suffix is delivered");

        let mut app2 = RecordingApp { seen: Vec::new() };
        let delivered = feed_committed_blocks_to_app(&mut tree, &mut app2).unwrap();
        assert_eq!(delivered, 0);
        assert!(app2.seen.is_empty(), "a fully-fed tree delivers nothing (idempotent)");
    }

    /// Strict order is never traded for progress: an unreadable committed
    /// height (legacy interior hole) PAUSES the feed — heights above it are NOT
    /// delivered and the frontier does not advance past it. Healing the hole
    /// resumes the feed exactly where it paused.
    #[test]
    fn feed_pauses_at_unreadable_height_and_resumes_after_heal() {
        let mut tree = initialized_tree();
        let blocks = seed_committed_chain(&mut tree, 5);

        // Punch a hole at height 3 (as a legacy/pruner artifact would).
        let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
        wb.delete_block_at_height(BlockHeight::new(3));
        tree.write(wb);

        let mut app = RecordingApp { seen: Vec::new() };
        feed_committed_blocks_to_app(&mut tree, &mut app).unwrap();
        assert_eq!(app.seen, vec![1, 2], "the feed must stop AT the hole, never skip it");
        assert_eq!(
            tree.app_fed_block_height().unwrap(),
            Some(BlockHeight::new(2)),
            "the fed frontier must not advance past the hole"
        );

        // Heal the hole; the feed resumes in order.
        let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
        wb.set_block_at_height(BlockHeight::new(3), &blocks[2].hash).unwrap();
        tree.write(wb);

        let mut app2 = RecordingApp { seen: Vec::new() };
        feed_committed_blocks_to_app(&mut tree, &mut app2).unwrap();
        assert_eq!(app2.seen, vec![3, 4, 5], "healing the hole resumes the feed in order");
    }

    /// A fresh (nothing-committed) tree feeds nothing and writes no frontier.
    #[test]
    fn feed_is_noop_on_fresh_tree() {
        let mut tree = initialized_tree();
        let mut app = RecordingApp { seen: Vec::new() };
        let delivered = feed_committed_blocks_to_app(&mut tree, &mut app).unwrap();
        assert_eq!(delivered, 0);
        assert!(app.seen.is_empty());
        assert_eq!(tree.app_fed_block_height().unwrap(), None);
    }
}
