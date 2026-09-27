//! s70 collector local advance: after the collector assembles a generic PC and
//! broadcasts AdvanceView, its own pacemaker takes that AdvanceView directly
//! instead of waiting for the broadcast's loopback through the inbound queue
//! (~80 ms mean in s70-qcsplit). Same PC, same origin (our key), same pacemaker
//! handler as the loopback: only the delivery time changes.
use super::*;
use crate::app::{
    ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::BlockTreeWriteBatch;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at, proposer_for, signing_keys, steady_block_tree, validator_set, MemKV,
    NullNetwork,
};
use crate::hotstuff::messages::{HotStuffMessage, PhaseVote, ProposalHeader};
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey};
use crate::types::data_types::{BlockHeight, CryptoHash, Data, EpochLength};
use std::sync::mpsc;

struct NullApp;
impl App<MemKV> for NullApp {
    fn produce_block(&mut self, _: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        panic!("local-advance fixture must not produce a proposal")
    }
    fn validate_block(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Invalid
    }
    fn validate_block_for_sync(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Invalid
    }
}

type TestAlgorithm = Algorithm<NullNetwork, MemKV, NullApp>;

struct Fixture {
    algorithm: TestAlgorithm,
    messages: Sender<(VerifyingKey, ProgressMessage)>,
    keys: Vec<SigningKey>,
    block: Block,
}

fn view1() -> ViewNumber {
    ViewNumber::new(1)
}

fn fixture() -> Fixture {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (tree, vss) = steady_block_tree(&set);
    let keypair = Keypair::new(keys[0].clone());
    let chain = ChainID::new(0);
    let (messages, receiver) = mpsc::channel();
    let (worker_commands, _commands) = mpsc::channel();
    let (_, worker_results) = mpsc::channel();
    let (_, shutdown) = mpsc::channel();
    let mut algorithm = Algorithm::new(
        chain,
        HotStuffConfiguration {
            chain_id: chain,
            keypair: keypair.clone(),
        },
        PacemakerConfiguration {
            chain_id: chain,
            keypair,
            epoch_length: EpochLength::new(100),
            max_view_time: Duration::from_secs(1),
            backoff_factor: 1,
            backoff_cap: 0,
            commit_lag_cap: 0,
        },
        BlockSyncClientConfiguration {
            chain_id: chain,
            request_limit: 32,
            response_timeout: Duration::from_secs(1),
            blacklist_expiry_time: Duration::from_secs(60),
            block_sync_trigger_min_view_difference: 10,
            block_sync_trigger_timeout: Duration::from_secs(60),
        },
        tree,
        NullApp,
        NullNetwork,
        receiver,
        BufferSize::new(1_000_000),
        worker_commands,
        worker_results,
        shutdown,
        None,
    );
    algorithm.hotstuff = hotstuff_at(view1(), keys[0].clone(), vss.clone());
    // A genesis-justified block B whose header is pending (body in flight): the
    // realistic state of a collector when the votes for B arrive.
    let block = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([61; 32]),
        Data::new(vec![]),
    );
    let header = ProposalHeader {
        chain_id: chain,
        view: view1(),
        block_hash: block.hash,
        height: block.height,
        data_hash: block.data_hash,
        justify: block.justify.clone(),
        tc: None,
        nec: None,
        has_validator_set_updates: false,
    };
    let proposer = proposer_for(view1(), &keys, &vss, &algorithm.block_tree);
    algorithm
        .hotstuff
        .on_receive_msg(
            header.into(),
            &proposer,
            &mut algorithm.block_tree,
            &mut algorithm.app,
        )
        .unwrap();
    Fixture {
        algorithm,
        messages,
        keys,
        block,
    }
}

impl Fixture {
    /// Queue a quorum (3 of 4) of generic votes for B and dispatch them one per
    /// step, as the algorithm loop does.
    fn collect_quorum(&mut self) {
        for sk in self.keys.iter().take(3) {
            let vote = PhaseVote::new(
                &Keypair::new(sk.clone()),
                ChainID::new(0),
                view1(),
                self.block.hash,
                Phase::Generic,
            );
            self.messages
                .send((
                    sk.verifying_key(),
                    ProgressMessage::HotStuffMessage(HotStuffMessage::PhaseVote(vote)),
                ))
                .unwrap();
            self.algorithm.poll_progress_and_retry(
                ViewInfo::new(view1(), Instant::now() + Duration::from_millis(50)),
                false,
            );
        }
    }

    fn pacemaker_view(&self) -> ViewNumber {
        self.algorithm.pacemaker.query().view
    }
}

#[test]
fn local_advance_flag_defaults_on_and_only_zero_disables() {
    for value in [None, Some(""), Some("1"), Some("true"), Some(" 0")] {
        assert!(collector_local_advance_enabled(value), "{value:?}");
    }
    assert!(!collector_local_advance_enabled(Some("0")));
}

/// The collector's own pacemaker enters PC.view + 1 in the same step that
/// collected the PC; no loopback delivery is needed.
#[test]
fn collector_enters_next_view_on_its_own_pc_without_loopback() {
    let mut f = fixture();
    f.algorithm.local_advance_origin = Some(f.keys[0].verifying_key());
    assert!(f.pacemaker_view() < ViewNumber::new(2), "precondition");
    f.collect_quorum();
    assert_eq!(
        f.algorithm.block_tree.highest_pc().unwrap().view,
        view1(),
        "PC collected and applied"
    );
    assert_eq!(f.pacemaker_view(), ViewNumber::new(2));
    assert!(
        f.algorithm.hotstuff.take_local_advance().is_none(),
        "consumed exactly once"
    );
}

/// Kill switch: with the flag off the pacemaker keeps waiting for the loopback
/// (NullNetwork drops the broadcast, so the view does not move), and the
/// offered PC is still drained so it cannot fire later.
#[test]
fn disabled_local_advance_leaves_the_view_to_the_loopback() {
    let mut f = fixture();
    f.algorithm.local_advance_origin = None;
    let before = f.pacemaker_view();
    f.collect_quorum();
    assert_eq!(f.algorithm.block_tree.highest_pc().unwrap().view, view1());
    assert_eq!(f.pacemaker_view(), before);
    assert!(f.algorithm.hotstuff.take_local_advance().is_none());
}

/// A PC the handler discards (here: it violates the lock clause) is never
/// offered, so the fast path can only advance on a PC the replica accepted.
#[test]
fn discarded_pc_is_not_offered_for_local_advance() {
    let mut f = fixture();
    f.algorithm.local_advance_origin = Some(f.keys[0].verifying_key());
    let set = validator_set(&f.keys);
    let lock = generic_pc(
        ViewNumber::new(10),
        CryptoHash::new([77; 32]),
        &f.keys[..3],
        &set,
    );
    let mut wb: BlockTreeWriteBatch<_> = BlockTreeWriteBatch::new();
    wb.set_locked_pc(&lock).unwrap();
    f.algorithm.block_tree.write(wb);
    let before = f.pacemaker_view();
    f.collect_quorum();
    assert!(
        f.algorithm.block_tree.highest_pc().unwrap().is_genesis_pc(),
        "PC discarded"
    );
    assert!(f.algorithm.hotstuff.take_local_advance().is_none());
    assert_eq!(f.pacemaker_view(), before);
}
