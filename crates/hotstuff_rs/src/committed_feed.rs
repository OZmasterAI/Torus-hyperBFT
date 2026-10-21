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
//!
//! ## Fail-stop (s134)
//!
//! An app that has fail-stopped refuses committed blocks (torus-consensus
//! returns from `on_committed_block` before any durable write once its
//! `exec_failed` latch is set; the node exits a few hundred ms later). Hotstuff
//! keeps committing the blocks already in its tree during that window. The
//! feed used to count them delivered, so after the restart it resumed above
//! them and execution froze at the first refused height forever, silently.
//! Two guards close it:
//! - [`App::committed_feed_halted`]: the feed stops before delivering to a
//!   halted app, and does not count a block whose callback left the app
//!   halted (it may have been refused; a re-delivery is a no-op at worst).
//! - [`boot_feed_committed_blocks_to_app`]: at start-up, a fed frontier at or
//!   above [`App::committed_feed_resume_height`] (a store an older binary left,
//!   or an app record lost while the frontier survived) is lowered to the
//!   app's, so the boot feed re-delivers those blocks from the local tree.

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

/// Count of feed passes stopped by a halted app (log throttle — see
/// [`note_feed_halted`]).
static FEED_HALTED_PASSES: AtomicU64 = AtomicU64::new(0);

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
        // Fail-stop: a halted app refuses committed blocks — deliver nothing
        // more and never count a refused height (see the module docs).
        if app.committed_feed_halted() {
            note_feed_halted(h, highest.int());
            break;
        }
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
        if app.committed_feed_halted() {
            // The app halted during (or right before) this callback: it may
            // have refused the block. Do not count it — a restarted app gets
            // it again (a no-op if it was kept: at-least-once).
            note_feed_halted(h, highest.int());
            break;
        }
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

/// Boot pass of the feed (`Algorithm::execute`, before its loop): reconcile
/// the fed frontier with the app's ([`lower_fed_frontier_to_app`]), then
/// [`feed_committed_blocks_to_app`].
pub(crate) fn boot_feed_committed_blocks_to_app<K: KVStore>(
    block_tree: &mut BlockTreeSingleton<K>,
    app: &mut impl App<K>,
) -> Result<u64, BlockTreeError> {
    lower_fed_frontier_to_app(block_tree, app)?;
    feed_committed_blocks_to_app(block_tree, app)
}

/// Fail-stop boot safety net: when the durable fed frontier is at or above
/// [`App::committed_feed_resume_height`] (blocks counted delivered that the app
/// never kept), lower it to just below that height so the feed re-delivers
/// them. Only when every height from there up to the frontier is readable in
/// the local tree: then the re-delivery cannot pause, and lowering can only
/// cause re-deliveries (which the at-least-once contract makes no-ops), never
/// a skip. Otherwise the frontier stays and the gap is logged loudly (the
/// blocks are gone locally: the node needs a resync). Returns whether it
/// lowered the frontier.
fn lower_fed_frontier_to_app<K: KVStore>(
    block_tree: &mut BlockTreeSingleton<K>,
    app: &impl App<K>,
) -> Result<bool, BlockTreeError> {
    let Some(resume) = app.committed_feed_resume_height() else {
        return Ok(false);
    };
    let Some(fed) = block_tree.app_fed_block_height()? else {
        return Ok(false);
    };
    if fed < resume {
        return Ok(false); // nothing counted that the app lacks
    }
    for h in resume.int()..=fed.int() {
        let readable = match block_tree.block_at_height(BlockHeight::new(h))? {
            Some(hash) => block_tree.block(&hash)?.is_some(),
            None => false,
        };
        if !readable {
            log::error!(
                "app feed: BOOT RECONCILE: committed heights {}..={} are counted delivered but the \
                 app does not hold them (it needs {} on), and height {} is no longer in the local \
                 block tree — cannot re-deliver; execution cannot pass {} without a resync",
                resume.int(),
                fed.int(),
                resume.int(),
                h,
                resume.int(),
            );
            return Ok(false);
        }
    }
    match resume.int().checked_sub(1) {
        Some(below) => block_tree.advance_app_fed_block_height(BlockHeight::new(below))?,
        None => block_tree.reset_app_fed_block_height(),
    }
    log::error!(
        "app feed: BOOT RECONCILE: fed frontier {} was above what the app holds (it needs {} on: \
         blocks committed after a fail-stop, or an app record lost) — lowered; re-delivering \
         {}..={} from the local tree",
        fed.int(),
        resume.int(),
        resume.int(),
        fed.int(),
    );
    Ok(true)
}

/// The app is halted ([`App::committed_feed_halted`]): the feed stops at
/// committed height `h` without counting it. Loud, throttled after the first
/// few passes (the live reconcile runs every second until the node exits).
fn note_feed_halted(h: u64, highest: u64) {
    let n = FEED_HALTED_PASSES.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 3 || n.is_power_of_two() {
        log::error!(
            "app feed: app HALTED (fail-stop) — feed stopped at committed height {} (committed \
             frontier {}, {} halted passes); heights from {} on are NOT counted delivered and \
             reach the app after a restart",
            h,
            highest,
            n,
            h,
        );
    }
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
        seed_committed_range(tree, 1, n)
    }

    /// [`seed_committed_chain`] for heights `from..=to` (extends a seeded chain).
    fn seed_committed_range(
        tree: &mut BlockTreeSingleton<MemKV>,
        from: u64,
        to: u64,
    ) -> Vec<Block> {
        let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
        let mut blocks = Vec::new();
        for h in from..=to {
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

    // ---- Fail-stop: the app refuses committed blocks (s134 bug, mem d59e61ea) ----

    /// Models `torus-consensus` `TorusApp` across a fail-stop: once its latch is
    /// set it REFUSES every committed block (returns before any durable write),
    /// and the latch is never cleared for the life of the instance. `durable`
    /// is what the app keeps across a restart (its manifests/headers); a
    /// re-delivery of a durable height is a no-op (the at-least-once contract).
    struct LatchingApp {
        durable: Vec<u64>,
        /// Latch right after accepting this height (the exec thread failing on it).
        latch_on: Option<u64>,
        latched: bool,
        refused: Vec<u64>,
    }

    impl LatchingApp {
        fn new(latch_on: Option<u64>) -> Self {
            Self { durable: Vec::new(), latch_on, latched: false, refused: Vec::new() }
        }

        /// A fresh process on the same data: the latch is clear, the durable
        /// heights survive.
        fn restarted(&self) -> Self {
            Self { durable: self.durable.clone(), latch_on: None, latched: false, refused: Vec::new() }
        }

        /// `durable` holds every height `1..=n`, in order, once.
        fn assert_durable_through(&self, n: u64) {
            let want: Vec<u64> = (1..=n).collect();
            assert_eq!(self.durable, want, "the app must hold every committed height once, in order");
        }
    }

    impl App<MemKV> for LatchingApp {
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
            let h = block.height.int();
            if self.latched {
                self.refused.push(h);
                return;
            }
            if self.durable.last().is_some_and(|&top| h <= top) {
                return; // benign re-delivery
            }
            self.durable.push(h);
            if self.latch_on == Some(h) {
                self.latched = true;
            }
        }
        fn committed_feed_halted(&self) -> bool {
            self.latched
        }
        fn committed_feed_resume_height(&self) -> Option<BlockHeight> {
            Some(BlockHeight::new(self.durable.last().map_or(0, |&top| top + 1)))
        }
    }

    /// The boot pass `Algorithm::execute` runs before its loop.
    fn boot_pass(tree: &mut BlockTreeSingleton<MemKV>, app: &mut LatchingApp) -> u64 {
        boot_feed_committed_blocks_to_app(tree, app).expect("boot feed")
    }

    /// (1) Commit 1..=5 with the app latching after 3, then commit 6..=8: no
    /// height the app refused may count as delivered. Before the fix the fed
    /// frontier reached 8 although the app holds only 1..=3, so after a restart
    /// the feed resumed at 9 and the app never received 4..=8 (execution frozen
    /// forever, silently).
    #[test]
    fn heights_refused_by_a_failstopped_app_are_not_counted_delivered() {
        let mut tree = initialized_tree();
        seed_committed_chain(&mut tree, 5);
        let mut app = LatchingApp::new(Some(3));
        feed_committed_blocks_to_app(&mut tree, &mut app).unwrap();
        seed_committed_range(&mut tree, 6, 8);
        feed_committed_blocks_to_app(&mut tree, &mut app).unwrap();

        assert_eq!(app.durable, vec![1, 2, 3]);
        assert!(app.refused.is_empty(), "a halted app gets no further deliveries");
        let fed = tree.app_fed_block_height().unwrap();
        assert!(
            fed <= Some(BlockHeight::new(3)),
            "fed frontier {fed:?} counts heights the latched app refused (it holds 1..=3)"
        );
    }

    /// (2, in-process restart) The latched app is replaced by a fresh instance
    /// on the same data (what systemd's restart does): the boot pass must hand
    /// it every height it refused, so its execution catches up.
    #[test]
    fn restart_after_failstop_redelivers_the_refused_heights() {
        let mut tree = initialized_tree();
        seed_committed_chain(&mut tree, 5);
        let mut app = LatchingApp::new(Some(3));
        feed_committed_blocks_to_app(&mut tree, &mut app).unwrap();
        seed_committed_range(&mut tree, 6, 8);
        feed_committed_blocks_to_app(&mut tree, &mut app).unwrap();

        let mut restarted = app.restarted();
        boot_pass(&mut tree, &mut restarted);
        restarted.assert_durable_through(8);
        assert_eq!(tree.app_fed_block_height().unwrap(), Some(BlockHeight::new(8)));
    }

    /// (3) Boot safety net: a store an older binary left with the fed frontier
    /// (8) ABOVE what the app durably holds (1..=3) — heights 4..=8 counted
    /// delivered but never accepted. The boot pass must lower the frontier to
    /// the app's and re-deliver 4..=8 from the local tree.
    #[test]
    fn boot_pass_lowers_a_fed_frontier_above_the_app_and_redelivers() {
        let mut tree = initialized_tree();
        seed_committed_chain(&mut tree, 8);
        tree.advance_app_fed_block_height(BlockHeight::new(8)).unwrap();
        let mut app = LatchingApp::new(None);
        app.durable = vec![1, 2, 3];

        boot_pass(&mut tree, &mut app);
        app.assert_durable_through(8);
        assert_eq!(tree.app_fed_block_height().unwrap(), Some(BlockHeight::new(8)));
    }

    /// The safety net never trades order for progress: with a height between
    /// the app's frontier and the fed frontier gone from the local tree (pruned),
    /// re-delivery could not pass it, so the frontier is left alone (logged).
    #[test]
    fn boot_pass_keeps_the_fed_frontier_when_the_gap_is_not_in_the_tree() {
        let mut tree = initialized_tree();
        seed_committed_chain(&mut tree, 8);
        tree.advance_app_fed_block_height(BlockHeight::new(8)).unwrap();
        let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
        wb.delete_block_at_height(BlockHeight::new(5));
        tree.write(wb);
        let mut app = LatchingApp::new(None);
        app.durable = vec![1, 2, 3];

        assert_eq!(boot_pass(&mut tree, &mut app), 0);
        assert_eq!(app.durable, vec![1, 2, 3]);
        assert_eq!(tree.app_fed_block_height().unwrap(), Some(BlockHeight::new(8)));
    }

    /// An app holding no committed height at all (resume height 0) gets the
    /// whole chain again: the frontier is dropped, not set to "0 delivered".
    #[test]
    fn boot_pass_drops_the_fed_frontier_when_the_app_holds_nothing() {
        let mut tree = initialized_tree();
        let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
        let b0 = test_block(0, PhaseCertificate::genesis_pc());
        wb.set_block(&b0).unwrap();
        wb.set_block_at_height(BlockHeight::new(0), &b0.hash).unwrap();
        tree.write(wb);
        seed_committed_chain(&mut tree, 4);
        tree.advance_app_fed_block_height(BlockHeight::new(4)).unwrap();
        let mut app = RecordingApp { seen: Vec::new() };

        struct Empty<'a>(&'a mut RecordingApp);
        impl App<MemKV> for Empty<'_> {
            fn produce_block(&mut self, _r: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
                unreachable!()
            }
            fn validate_block(&mut self, _r: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
                unreachable!()
            }
            fn validate_block_for_sync(
                &mut self,
                _r: ValidateBlockRequest<MemKV>,
            ) -> ValidateBlockResponse {
                unreachable!()
            }
            fn on_committed_block(&mut self, block: &Block, hash: CryptoHash) {
                self.0.on_committed_block(block, hash);
            }
            fn committed_feed_resume_height(&self) -> Option<BlockHeight> {
                Some(BlockHeight::new(0))
            }
        }
        boot_feed_committed_blocks_to_app(&mut tree, &mut Empty(&mut app)).unwrap();
        assert_eq!(app.seen, vec![0, 1, 2, 3, 4]);
        assert_eq!(tree.app_fed_block_height().unwrap(), Some(BlockHeight::new(4)));
    }

    /// An app that does not report a resume height (the default) keeps the
    /// old boot behaviour: nothing is lowered.
    #[test]
    fn boot_pass_without_a_resume_height_lowers_nothing() {
        let mut tree = initialized_tree();
        seed_committed_chain(&mut tree, 5);
        tree.advance_app_fed_block_height(BlockHeight::new(5)).unwrap();
        let mut app = RecordingApp { seen: Vec::new() };
        assert_eq!(boot_feed_committed_blocks_to_app(&mut tree, &mut app).unwrap(), 0);
        assert!(app.seen.is_empty());
        assert_eq!(tree.app_fed_block_height().unwrap(), Some(BlockHeight::new(5)));
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
