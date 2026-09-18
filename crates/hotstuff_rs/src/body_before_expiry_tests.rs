use super::*;
use crate::app::{
    ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_sync::{messages::BlockSyncAdvertiseMessage, worker::SyncCommand};
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at, proposer_for, signing_keys, steady_block_tree, validator_set, MemKV,
    NullNetwork,
};
use crate::hotstuff::messages::{
    BlockDataRequest, BlockDataResponse, HotStuffMessage, ProposalHeader,
};
use crate::hotstuff::types::PhaseCertificate;
use crate::types::block::Block;
use crate::types::crypto_primitives::Keypair;
use crate::types::data_types::{BlockHeight, CryptoHash, Data, EpochLength};
use std::sync::mpsc;

struct CountingApp {
    calls: usize,
    valid: bool,
}
impl App<MemKV> for CountingApp {
    fn produce_block(&mut self, _: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        panic!("receive/retry fixture must not produce a proposal")
    }
    fn validate_block(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        self.calls += 1;
        if self.valid {
            ValidateBlockResponse::Valid {
                app_state_updates: None,
                validator_set_updates: None,
            }
        } else {
            ValidateBlockResponse::Invalid
        }
    }
    fn validate_block_for_sync(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        panic!("ordinary body must use normal validation")
    }
}

type TestAlgorithm = Algorithm<NullNetwork, MemKV, CountingApp>;
struct Fixture {
    algorithm: TestAlgorithm,
    messages: Sender<(VerifyingKey, ProgressMessage)>,
    commands: Receiver<SyncCommand>,
    origin: VerifyingKey,
    body: Block,
}

fn fixture() -> Fixture {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (tree, vss) = steady_block_tree(&set);
    let view = ViewNumber::new(1);
    let origin = proposer_for(view, &keys, &vss, &tree);
    let keypair = Keypair::new(keys[0].clone());
    let chain = ChainID::new(0);
    let (messages, receiver) = mpsc::channel();
    let (worker_commands, commands) = mpsc::channel();
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
        CountingApp {
            calls: 0,
            valid: true,
        },
        NullNetwork,
        receiver,
        BufferSize::new(1_000_000),
        worker_commands,
        worker_results,
        shutdown,
        None,
    );
    algorithm.hotstuff = hotstuff_at(view, keys[0].clone(), vss);
    // A real signed advertisement makes fallback observable as a worker command.
    algorithm
        .block_sync_client
        .on_receive_msg(
            BlockSyncAdvertiseMessage::advertise_block(
                &Keypair::new(keys[1].clone()),
                chain,
                BlockHeight::new(10),
            ),
            &keys[1].verifying_key(),
            &mut algorithm.block_tree,
        )
        .unwrap();
    Fixture {
        algorithm,
        messages,
        commands,
        origin,
        body: Block::new(
            BlockHeight::new(0),
            PhaseCertificate::genesis_pc(),
            CryptoHash::new([91; 32]),
            Data::new(vec![]),
        ),
    }
}

fn header(body: &Block, view: ViewNumber) -> ProposalHeader {
    ProposalHeader {
        chain_id: ChainID::new(0),
        view,
        block_hash: body.hash,
        height: body.height,
        data_hash: body.data_hash,
        justify: body.justify.clone(),
        tc: None,
        nec: None,
        has_validator_set_updates: false,
    }
}

impl Fixture {
    fn track_expiring_body(&mut self) {
        self.algorithm
            .hotstuff
            .on_receive_msg(
                header(&self.body, ViewNumber::new(1)).into(),
                &self.origin,
                &mut self.algorithm.block_tree,
                &mut self.algorithm.app,
            )
            .unwrap();
        assert!(self.algorithm.hotstuff.has_pending_body_fetches());
        assert_eq!(self.algorithm.app.calls, 0);
        self.algorithm
            .hotstuff
            .exhaust_body_fetch_for_test(&self.body.hash);
    }

    fn queue_body(&self, body: Block) {
        self.messages
            .send((
                self.origin,
                ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataResponse(
                    BlockDataResponse {
                        view: ViewNumber::new(1),
                        block: body,
                    },
                )),
            ))
            .unwrap();
    }

    fn step(&mut self, enabled: bool, deadline: Instant) {
        self.algorithm
            .poll_progress_and_retry(ViewInfo::new(ViewNumber::new(1), deadline), enabled);
    }

    fn assert_sync_started_and_consumed(&mut self) {
        assert!(self.algorithm.block_sync_client.has_pending_sync());
        assert!(matches!(
            self.commands.try_recv(),
            Ok(SyncCommand::Fetch { .. })
        ));
        assert!(matches!(self.commands.try_recv(), Err(TryRecvError::Empty)));
        assert!(!self.algorithm.hotstuff.take_sync_needed());
    }
}

#[test]
fn body_before_expiry_flag_defaults_off_without_global_environment_mutation() {
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some("01"),
        Some(" 1"),
    ] {
        assert!(!body_before_expiry_enabled(value));
    }
    assert!(body_before_expiry_enabled(Some("1")));
}

#[test]
fn queued_tracked_body_gets_normal_validation_before_expiry_only_when_enabled() {
    for enabled in [false, true] {
        for expired_view_deadline in [false, true] {
            let mut f = fixture();
            f.track_expiring_body();
            f.queue_body(f.body.clone());
            let deadline = Instant::now()
                + if expired_view_deadline {
                    Duration::ZERO
                } else {
                    Duration::from_secs(1)
                };
            f.step(enabled, deadline);
            assert_eq!(f.algorithm.block_tree.contains(&f.body.hash), enabled);
            assert_eq!(f.algorithm.app.calls, usize::from(enabled));
            assert!(!f.algorithm.hotstuff.has_pending_body_fetches());
            if enabled {
                assert!(!f.algorithm.block_sync_client.has_pending_sync());
                assert!(matches!(f.commands.try_recv(), Err(TryRecvError::Empty)));
                assert_eq!(f.algorithm.hotstuff.pushed_body_buffer_len(), 0);
            } else {
                f.assert_sync_started_and_consumed();
                assert_eq!(
                    f.algorithm.hotstuff.pushed_body_buffer_len(),
                    usize::from(!expired_view_deadline)
                );
            }
        }
    }
}

#[test]
fn empty_or_metadata_substituted_body_still_expires_in_the_same_step() {
    for malformed in [false, true] {
        let mut f = fixture();
        f.track_expiring_body();
        if malformed {
            let mut substituted = f.body.clone();
            substituted.height = BlockHeight::new(1); // same claimed hash, different authenticated metadata
            f.queue_body(substituted);
        }
        f.step(true, Instant::now());
        assert!(!f.algorithm.block_tree.contains(&f.body.hash));
        assert_eq!(f.algorithm.app.calls, 0);
        assert!(!f.algorithm.hotstuff.has_pending_body_fetches());
        f.assert_sync_started_and_consumed();
    }
}

#[test]
fn earlier_receive_never_bypasses_application_rejection() {
    let mut f = fixture();
    f.track_expiring_body();
    f.algorithm.app.valid = false;
    f.queue_body(f.body.clone());
    f.step(true, Instant::now());
    assert_eq!(f.algorithm.app.calls, 1);
    assert!(!f.algorithm.block_tree.contains(&f.body.hash));
    assert_eq!(f.algorithm.hotstuff.header_vote_invalid_count(), 1);
}

#[test]
fn filtered_flood_does_not_postpone_retry_maintenance_or_consume_ninth_message() {
    let mut f = fixture();
    f.track_expiring_body();
    for _ in 0..ProgressMessageStub::MAX_BEFORE_RETRY_ENVELOPES {
        f.messages
            .send((
                f.origin,
                ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataRequest(
                    BlockDataRequest {
                        chain_id: ChainID::new(99),
                        view: ViewNumber::new(1),
                        block_hash: f.body.hash,
                    },
                )),
            ))
            .unwrap();
    }
    f.queue_body(f.body.clone());
    f.step(true, Instant::now() + Duration::from_secs(1));
    assert_eq!(f.algorithm.app.calls, 0);
    f.assert_sync_started_and_consumed();
    assert!(matches!(
        f.algorithm.pm_stub.recv_before_body_retry(
            ChainID::new(0),
            ViewNumber::new(1),
            Instant::now()
        ),
        Ok((
            _,
            ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataResponse(_))
        ))
    ));
}

#[test]
fn treatment_dispatches_one_ordinary_message_and_leaves_next_queued() {
    let mut f = fixture();
    f.track_expiring_body();
    f.queue_body(f.body.clone());
    f.queue_body(f.body.clone());
    f.step(true, Instant::now());
    assert_eq!(f.algorithm.app.calls, 1);
    assert!(f.algorithm.block_tree.contains(&f.body.hash));
    assert!(matches!(
        f.algorithm.pm_stub.recv_before_body_retry(
            ChainID::new(0),
            ViewNumber::new(1),
            Instant::now()
        ),
        Ok((
            _,
            ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataResponse(_))
        ))
    ));
}

#[test]
fn handler_and_expiry_sync_requests_are_both_consumed_without_duplicate_sessions() {
    let mut f = fixture();
    f.track_expiring_body();
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let missing_pc = generic_pc(
        ViewNumber::new(1),
        CryptoHash::new([55; 32]),
        &keys[..3],
        &set,
    );
    let child = Block::new(
        BlockHeight::new(1),
        missing_pc,
        CryptoHash::new([56; 32]),
        Data::new(vec![]),
    );
    let future = ViewNumber::new(2);
    let origin = proposer_for(
        future,
        &keys,
        &f.algorithm.block_tree.validator_set_state().unwrap(),
        &f.algorithm.block_tree,
    );
    f.messages
        .send((
            origin,
            ProgressMessage::HotStuffMessage(header(&child, future).into()),
        ))
        .unwrap();
    f.step(true, Instant::now());
    assert!(
        !f.algorithm.hotstuff.has_pending_body_fetches(),
        "original body still expires after handler"
    );
    assert!(
        f.algorithm.hotstuff.has_pending_justify_fetches(),
        "unsafe header retains missing-parent recovery"
    );
    f.assert_sync_started_and_consumed();
}

#[test]
fn no_pending_fetch_still_arms_missing_pc_before_ordinary_receive() {
    for enabled in [false, true] {
        let mut f = fixture();
        let keys = signing_keys(&[1, 2, 3, 4]);
        let set = validator_set(&keys);
        let pc = generic_pc(ViewNumber::new(1), f.body.hash, &keys[..3], &set);
        f.algorithm
            .block_tree
            .advance_highest_pc_from_remote(&pc, &None)
            .unwrap();
        f.algorithm.hotstuff.make_missing_pc_check_due_for_test();
        assert!(!f.algorithm.hotstuff.has_pending_body_fetches());
        assert!(!f.algorithm.hotstuff.has_pending_justify_fetches());
        f.queue_body(f.body.clone());
        f.step(enabled, Instant::now() + Duration::from_secs(1));
        assert_eq!(
            f.algorithm.app.calls, 1,
            "maintenance must claim hash before queued body is dispatched"
        );
        assert!(f.algorithm.block_tree.contains(&f.body.hash));
        assert_eq!(f.algorithm.hotstuff.pushed_body_buffer_len(), 0);
    }
}

#[test]
fn existing_justify_only_fetch_activates_ready_receive_after_deadline() {
    let mut f = fixture();
    let keys = signing_keys(&[1, 2, 3, 4]);
    let pc = generic_pc(
        ViewNumber::new(1),
        f.body.hash,
        &keys[..3],
        &validator_set(&keys),
    );
    f.algorithm
        .block_tree
        .advance_highest_pc_from_remote(&pc, &None)
        .unwrap();
    f.algorithm.hotstuff.make_missing_pc_check_due_for_test();
    f.algorithm
        .hotstuff
        .tick_missing_pc_block_fetch(&f.algorithm.block_tree)
        .unwrap();
    assert!(!f.algorithm.hotstuff.has_pending_body_fetches());
    assert!(f.algorithm.hotstuff.has_pending_justify_fetches());
    f.queue_body(f.body.clone());
    f.step(true, Instant::now());
    assert!(f.algorithm.block_tree.contains(&f.body.hash));
    assert_eq!(f.algorithm.app.calls, 1);
    assert!(!f.algorithm.hotstuff.has_pending_justify_fetches());
}

#[test]
#[should_panic(expected = "The poller has disconnected!")]
fn bounded_algorithm_receive_preserves_disconnection_failure() {
    let mut f = fixture();
    f.track_expiring_body();
    let Fixture {
        mut algorithm,
        messages,
        ..
    } = f;
    drop(messages);
    algorithm.poll_progress_and_retry(ViewInfo::new(ViewNumber::new(1), Instant::now()), true);
}
