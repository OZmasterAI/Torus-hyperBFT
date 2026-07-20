//! [`NumberApp`], a simple implementation of [`App`] currently used in all of the integration tests.

use std::{
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use borsh::{BorshDeserialize, BorshSerialize};
use hotstuff_rs::{
    app::{
        App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
    },
    block_tree::{accessors::public::BlockTreeSnapshot, pluggables::KVGet},
    types::{
        block::Block,
        crypto_primitives::{CryptoHasher, Digest, VerifyingKey},
        data_types::{CryptoHash, Data, Datum, Power},
        update_sets::{AppStateUpdates, ValidatorSetUpdates},
    },
};

use crate::common::{mem_db::MemDB, verifying_key_bytes::VerifyingKeyBytes};

/// A queued transaction paired with a process-unique id assigned by the
/// submitter (see `Node::submit_transaction`). The id travels with the tx inside
/// the block's `Data`, letting the app apply each logical transaction *exactly
/// once* (via per-tx applied-markers in [`NumberApp::execute`]) even when the
/// same tx is re-proposed after its first proposal is orphaned.
pub(crate) type QueuedTransaction = (u64, NumberAppTransaction);

/// A simple implementation of [`App`] for use in integration tests.
///
/// The number app maintains an app state consisting of a single number, which can be queried using the
/// [`number`](`NumberApp::number`) function. Users can increase this number by submitting
/// [`Increment`](NumberAppTransaction::Increment) transactions to the app's `tx_queue`, which is
/// provided to the number app during creation as an argument of the struct's [`new`](NumberApp::new)
/// constructor.
///
/// ## Timing
///
/// In order to reduce the rate at which blocks are produced in integration tests `NumberApp` spends
/// at least `produce_delay` in `produce_block`, and `validate_delay` in `validate_block` (both
/// default to 250 milliseconds via [`new`](NumberApp::new)). This means that at the *bare minimum*,
/// replicas maintaining a default `NumberApp` should configure their `max_view_time` to be 500
/// milliseconds if they are to consistently make progress.
///
/// S470 caution: these delays run ON the algorithm thread, so a sleeping replica's pacemaker
/// stops ticking and its view clock freezes for the duration — a `validate_delay` above
/// `max_view_time` therefore does NOT model a slow off-thread exec pipeline (the S470 wedge
/// repro uses a network-side body delay instead, see `mock_network_with_body_delay`). Keep
/// both delays under `max_view_time`.
pub(crate) struct NumberApp {
    tx_queue: Arc<Mutex<Vec<QueuedTransaction>>>,
    produce_delay: Duration,
    validate_delay: Duration,
}

/// User-sent instructions that number app execute in [`produce_block`](App::produce_block) and
/// [`validate_block`](App::validate_block).
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub enum NumberAppTransaction {
    /// Increase the number in the app state by 1.
    Increment,

    /// Add a new validator with the specified verifying key and power or change the power of an existing
    /// validator.
    SetValidator(VerifyingKeyBytes, Power),

    /// Delete an existing validator. If the specified validator does not exist, this transaction is a no-
    /// op.
    DeleteValidator(VerifyingKeyBytes),
}

// The key in the app state where the "number" is stored.
const NUMBER_KEY: [u8; 1] = [0];

/// App-state key recording that the transaction with the given process-unique
/// `id` has already been applied. Prefixed with `1` so it never collides with
/// [`NUMBER_KEY`] (`[0]`). Committing this marker alongside a transaction's
/// effect makes application idempotent: a re-proposed (previously orphaned) or
/// pipelined re-embedded transaction is skipped the second time, so an
/// Increment is never applied twice while still being re-proposable if orphaned.
fn applied_marker_key(id: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(9);
    key.push(1u8);
    key.extend_from_slice(&id.to_le_bytes());
    key
}

impl NumberApp {
    /// Create a new number app that which will pop and execute transactions from the provided
    /// `tx_queue`.
    ///
    /// Callers should clone a reference to the `tx_queue` before calling this constructor and use the
    /// reference to insert transactions to the `tx_queue` whenever needed.
    #[allow(dead_code)]
    pub(crate) fn new(tx_queue: Arc<Mutex<Vec<QueuedTransaction>>>) -> NumberApp {
        Self::new_with_delays(
            tx_queue,
            Duration::from_millis(250),
            Duration::from_millis(250),
        )
    }

    /// Like [`new`](Self::new), but with caller-chosen `produce_block` /
    /// `validate_block` delays (both must stay under `max_view_time` — see
    /// the S470 caution in the struct docs). The S470 wedge-repro test uses
    /// this to shrink `produce_delay` so proposal production is never the
    /// bottleneck.
    #[allow(dead_code)]
    pub(crate) fn new_with_delays(
        tx_queue: Arc<Mutex<Vec<QueuedTransaction>>>,
        produce_delay: Duration,
        validate_delay: Duration,
    ) -> NumberApp {
        Self {
            tx_queue,
            produce_delay,
            validate_delay,
        }
    }

    /// Return an `AppStateUpdates` that when applied on an empty app state will produce a good "initial"
    /// app state for a number app: one containing the number 0.
    pub(crate) fn initial_app_state() -> AppStateUpdates {
        let mut state = AppStateUpdates::new();
        state.insert(NUMBER_KEY.to_vec(), u32::to_le_bytes(0).to_vec());
        state
    }

    /// Get the number stored in a number app's app state from the given block tree.
    pub(crate) fn number<S: KVGet>(block_tree: BlockTreeSnapshot<S>) -> u32 {
        u32::deserialize(&mut &*block_tree.committed_app_state(&NUMBER_KEY).unwrap()).unwrap()
    }
}

impl App<MemDB> for NumberApp {
    fn produce_block(&mut self, request: ProduceBlockRequest<MemDB>) -> ProduceBlockResponse {
        thread::sleep(self.produce_delay);

        let initial_number = u32::from_le_bytes(
            request
                .block_tree()
                .app_state(&NUMBER_KEY)
                .unwrap()
                .try_into()
                .unwrap(),
        );

        let tx_queue = self.tx_queue.lock().unwrap();

        let (app_state_updates, validator_set_updates) =
            self.execute(initial_number, &tx_queue, |id| {
                request
                    .block_tree()
                    .app_state(&applied_marker_key(id))
                    .is_some()
            });
        let data = Data::new(vec![Datum::new(tx_queue.try_to_vec().unwrap())]);
        let data_hash = {
            let mut hasher = CryptoHasher::new();
            hasher.update(&data.vec()[0].bytes());
            let bytes = hasher.finalize().into();
            CryptoHash::new(bytes)
        };

        // Intentionally do NOT clear the queue here. A transaction is removed
        // only once a block containing it COMMITS (see `on_committed_block`), so
        // if this proposal is orphaned its transactions remain queued and are
        // re-proposable. Exactly-once application is guaranteed independently by
        // the applied-markers written in `execute`, so re-embedding a committed
        // (or pipelined-ancestor) transaction is a no-op.

        ProduceBlockResponse {
            data_hash,
            data,
            app_state_updates,
            validator_set_updates,
        }
    }

    fn validate_block(&mut self, request: ValidateBlockRequest<MemDB>) -> ValidateBlockResponse {
        thread::sleep(self.validate_delay);

        self.validate_block_for_sync(request)
    }

    fn validate_block_for_sync(
        &mut self,
        request: ValidateBlockRequest<MemDB>,
    ) -> ValidateBlockResponse {
        let data = &request.proposed_block().data;
        let data_hash: CryptoHash = {
            let mut hasher = CryptoHasher::new();
            hasher.update(&data.vec()[0].bytes());
            let bytes = hasher.finalize().into();
            CryptoHash::new(bytes)
        };

        if request.proposed_block().data_hash != data_hash {
            ValidateBlockResponse::Invalid
        } else {
            let initial_number = u32::from_le_bytes(
                request
                    .block_tree()
                    .app_state(&NUMBER_KEY)
                    .unwrap()
                    .try_into()
                    .unwrap(),
            );

            if let Ok(transactions) = Vec::<QueuedTransaction>::deserialize(
                &mut &*request.proposed_block().data.vec()[0].bytes().as_slice(),
            ) {
                let (app_state_updates, validator_set_updates) =
                    self.execute(initial_number, &transactions, |id| {
                        request
                            .block_tree()
                            .app_state(&applied_marker_key(id))
                            .is_some()
                    });
                ValidateBlockResponse::Valid {
                    app_state_updates,
                    validator_set_updates,
                }
            } else {
                ValidateBlockResponse::Invalid
            }
        }
    }

    /// Once a block irrevocably commits, drop the transactions it contains from
    /// the local queue so they are no longer re-proposed. Exactly-once
    /// application does not depend on this (the applied-markers guarantee it) —
    /// this only keeps the queue bounded and lets it drain to empty, which is
    /// what the progress tests observe. Transactions in an *orphaned* proposal
    /// never reach here, so they stay queued and remain re-proposable.
    fn on_committed_block(&mut self, block: &Block, _committed_hash: CryptoHash) {
        let data = &block.data;
        if data.vec().is_empty() {
            return;
        }
        if let Ok(transactions) =
            Vec::<QueuedTransaction>::deserialize(&mut &*data.vec()[0].bytes().as_slice())
        {
            if transactions.is_empty() {
                return;
            }
            let committed_ids: std::collections::HashSet<u64> =
                transactions.iter().map(|(id, _)| *id).collect();
            let mut tx_queue = self.tx_queue.lock().unwrap();
            tx_queue.retain(|(id, _)| !committed_ids.contains(id));
        }
    }
}

impl NumberApp {
    /// Given the `current_number` and the block's `transactions`, compute the
    /// resulting `AppStateUpdates` and `ValidatorSetUpdates`.
    ///
    /// `is_applied(id)` reports whether the transaction with that id has already
    /// been applied by an ancestor (committed or pending) block — read from the
    /// parent `AppBlockTreeView` via its applied-marker. Any transaction that is
    /// already applied is skipped, guaranteeing **exactly-once** application: a
    /// transaction re-proposed after an orphaned proposal, or re-embedded in a
    /// pipelined descendant before its first block commits, is not double-applied.
    /// Every transaction this call *does* apply records its own applied-marker in
    /// the returned `AppStateUpdates`, which commits atomically with the block.
    fn execute(
        &self,
        current_number: u32,
        transactions: &[QueuedTransaction],
        is_applied: impl Fn(u64) -> bool,
    ) -> (Option<AppStateUpdates>, Option<ValidatorSetUpdates>) {
        let mut number = current_number;
        let mut validator_set_updates: Option<ValidatorSetUpdates> = None;
        let mut app_state_updates = AppStateUpdates::new();
        let mut any_applied = false;

        for (id, transaction) in transactions {
            // Exactly-once: skip a transaction already applied by an ancestor.
            if is_applied(*id) {
                continue;
            }
            match transaction {
                NumberAppTransaction::Increment => {
                    number += 1;
                }
                NumberAppTransaction::SetValidator(validator, power) => {
                    validator_set_updates
                        .get_or_insert(ValidatorSetUpdates::new())
                        .insert(VerifyingKey::from_bytes(validator).unwrap(), *power);
                }
                NumberAppTransaction::DeleteValidator(validator) => {
                    validator_set_updates
                        .get_or_insert(ValidatorSetUpdates::new())
                        .delete(VerifyingKey::from_bytes(validator).unwrap());
                }
            }
            // Record that this transaction has now been applied so any later
            // re-proposal / re-embedding of it is a no-op.
            app_state_updates.insert(applied_marker_key(*id), vec![1u8]);
            any_applied = true;
        }

        if number != current_number {
            app_state_updates.insert(NUMBER_KEY.to_vec(), number.try_to_vec().unwrap());
        }

        let app_state_updates = if any_applied {
            Some(app_state_updates)
        } else {
            None
        };

        (app_state_updates, validator_set_updates)
    }
}
