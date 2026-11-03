//! perf/pacemaker-vset-cache (follow-up to perf/vote-vset-cache, c40992d7): `Pacemaker::tick` read
//! the block tree's validator-set state (~5 KV gets, 2 validator-set deserializations) on every
//! pass of the algorithm loop. It only has to read it again once it has changed: the block tree
//! keeps a generation of its validator-set state (bumped by every write that touches it, block
//! sync's included), and the pacemaker reuses the state it last read while the generation is the
//! same.
//!
//! - Steady ticks read the state once (the first tick), and a block-tree write that touches no
//!   validator-set key does not cost a read.
//! - A validator-set write between ticks is seen by the next tick (it acts on the new set), and
//!   the ticks after it do not read again.
//! - Generations are unique across block-tree instances: a tick against another tree acts on that
//!   tree's set.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::block_tree::accessors::internal::{validator_set_state_reads, BlockTreeWriteBatch};
use crate::hotstuff::header_fast_path_regression_test::{signing_keys, steady_block_tree, validator_set, MemKV};
use crate::types::crypto_primitives::SigningKey;

const CHAIN: ChainID = ChainID::new(0);

#[derive(Clone, Default)]
struct RecordingNetwork {
    broadcasts: Arc<Mutex<Vec<Message>>>,
}

impl Network for RecordingNetwork {
    fn init_validator_set(&mut self, _: ValidatorSet) {}
    fn update_validator_set(&mut self, _: crate::types::update_sets::ValidatorSetUpdates) {}
    fn broadcast(&mut self, message: Message) {
        self.broadcasts.lock().unwrap().push(message);
    }
    fn send(&mut self, _: VerifyingKey, _: Message) {}
    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        None
    }
}

impl RecordingNetwork {
    fn timeout_votes_sent(&self) -> usize {
        use crate::networking::messages::ProgressMessage;
        self.broadcasts
            .lock()
            .unwrap()
            .iter()
            .filter(|m| {
                matches!(
                    m,
                    Message::ProgressMessage(ProgressMessage::PacemakerMessage(PacemakerMessage::TimeoutVote(_)))
                )
            })
            .count()
    }
}

/// A pacemaker for `local` at view 5, built over `vss`, with views lasting `max_view_time`.
fn pacemaker(
    local: &SigningKey,
    vss: &ValidatorSetState,
    max_view_time: Duration,
) -> (Pacemaker<RecordingNetwork>, RecordingNetwork) {
    let config = PacemakerConfiguration {
        chain_id: CHAIN,
        keypair: Keypair::new(local.clone()),
        // Short epoch: `new` schedules every view to the epoch's end, and 100k views take most of a
        // short view to schedule in a debug build.
        epoch_length: EpochLength::new(1_000),
        max_view_time,
        backoff_factor: 2,
        backoff_cap: 8,
        commit_lag_cap: 0,
    };
    let net = RecordingNetwork::default();
    let pacemaker = Pacemaker::new(config, SenderHandle::new(net.clone()), ViewNumber::new(5), vss, None).unwrap();
    (pacemaker, net)
}

/// Let the current view's deadline pass.
fn wait_past(pacemaker: &Pacemaker<RecordingNetwork>) {
    while Instant::now() <= pacemaker.query().deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Commit `set` as both the committed and the previous validator set (a decided update), out of
/// band: the write block sync's commit + decide makes, as far as the validator-set state goes.
fn write_steady_set(tree: &mut BlockTreeSingleton<MemKV>, set: &ValidatorSet) {
    let mut wb = BlockTreeWriteBatch::new();
    wb.set_committed_validator_set(set).unwrap();
    wb.set_previous_validator_set(set).unwrap();
    tree.write(wb);
}

#[test]
fn steady_ticks_read_the_validator_set_state_once() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let (mut tree, vss) = steady_block_tree(&validator_set(&keys));
    let (mut pacemaker, _net) = pacemaker(&keys[3], &vss, Duration::from_secs(3600));

    let before = validator_set_state_reads::count();
    for i in 0..10 {
        if i == 5 {
            // A block-tree write that touches no validator-set key, between ticks.
            tree.set_highest_view_entered(ViewNumber::new(5)).unwrap();
        }
        pacemaker.tick(&tree).unwrap();
    }
    let reads = validator_set_state_reads::count() - before;
    assert_eq!(
        reads, 1,
        "10 ticks in a steady view must read the validator-set state once (the first tick), not \
         once per tick: got {reads} reads"
    );
}

#[test]
fn a_validator_set_write_between_ticks_is_seen_by_the_next_tick_only() {
    // The local replica n3 is not in the old set {n0, n1, n2} and is in the new set {n0..n3}.
    let keys = signing_keys(&[1, 2, 3, 4]);
    let old_set = validator_set(&keys[..3]);
    let new_set = validator_set(&keys);
    let (mut tree, vss) = steady_block_tree(&old_set);
    let (mut pacemaker, net) = pacemaker(&keys[3], &vss, Duration::from_millis(150));

    let before = validator_set_state_reads::count();
    pacemaker.tick(&tree).unwrap();
    assert_eq!(validator_set_state_reads::count() - before, 1, "the first tick reads the state");
    assert_eq!(net.timeout_votes_sent(), 0, "n3 is not in the old set: it sends no TimeoutVote");

    write_steady_set(&mut tree, &new_set);
    wait_past(&pacemaker);

    let view = pacemaker.query().view;
    let before = validator_set_state_reads::count();
    pacemaker.tick(&tree).unwrap();
    assert_eq!(
        net.timeout_votes_sent(),
        1,
        "the timed-out view must be handled over the new set: n3 is a validator now and sends a TimeoutVote"
    );
    assert_eq!(validator_set_state_reads::count() - before, 1, "the tick after the write re-reads the state");
    assert_eq!(pacemaker.query().view, view + 1, "the timeout moves the pacemaker to the next view");

    let before = validator_set_state_reads::count();
    for _ in 0..5 {
        pacemaker.tick(&tree).unwrap();
    }
    let reads = validator_set_state_reads::count() - before;
    assert_eq!(reads, 0, "the state is unchanged since the last read: later ticks must not read it, got {reads}");
}

#[test]
fn a_tick_against_another_tree_acts_on_that_trees_set() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    // Both trees made the same validator-set writes (one `initialize`), over different sets: n3
    // is a validator only in `new_tree`.
    let (old_tree, old_vss) = steady_block_tree(&validator_set(&keys[..3]));
    let (new_tree, _) = steady_block_tree(&validator_set(&keys));
    let (mut pacemaker, net) = pacemaker(&keys[3], &old_vss, Duration::from_millis(150));

    pacemaker.tick(&old_tree).unwrap();
    wait_past(&pacemaker);
    pacemaker.tick(&new_tree).unwrap();
    assert_eq!(
        net.timeout_votes_sent(),
        1,
        "the timeout must be handled over `new_tree`'s set, where n3 is a validator"
    );
}
