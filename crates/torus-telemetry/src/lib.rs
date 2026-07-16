//! Torus-hyperBFT telemetry: Prometheus metrics registry, tracing subscriber, /health endpoint.

pub mod view_metrics;
pub use view_metrics::ViewMetricsRecorder;

use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Global metrics for the Torus node.
///
/// Metric handles are `Clone + Send + Sync` (internally Arc-based).
/// Distribute cloned handles to subsystems; the registry stays here for encoding.
pub struct Metrics {
    registry: Registry,

    // Block metrics
    pub blocks_committed: Counter,
    pub block_height: Gauge,
    pub block_build_seconds: Histogram,

    // Transaction metrics
    pub evm_txs_processed: Counter,
    pub native_actions_processed: Counter,

    // Consensus metrics
    pub consensus_rounds: Counter,
    pub consensus_view: Gauge,

    // State metrics
    pub state_root_compute_seconds: Histogram,

    // Mempool metrics
    pub mempool_evm_size: Gauge,
    pub mempool_native_size: Gauge,

    // Network metrics
    pub peers_connected: Gauge,

    // Database metrics
    pub db_size_bytes: Gauge,

    // Epoch metrics
    pub epoch_number: Gauge,
    pub validator_set_size: Gauge,

    // Trading metrics
    pub orders_matched: Counter,
    pub liquidations_triggered: Counter,

    // Pruner metrics
    pub pruner_blocks_removed: Counter,

    // RPC metrics
    pub rpc_requests_total: Family<Vec<(String, String)>, Counter>,
    pub rpc_request_duration_seconds: Histogram,

    // Gossip metrics
    pub gossip_messages_received: Counter,
    pub gossip_messages_sent: Counter,

    // Native-action gossip pre-spread metrics (Sprint 3.5)
    /// Native actions published to gossipsub (summed per batch).
    pub native_gossip_published_actions: Counter,
    /// Native actions received from gossipsub (decoded OK, forwarded to ingest).
    pub native_gossip_received_actions: Counter,
    /// Outbound native actions dropped because the gossip channel was full.
    /// Must stay 0 under load — drops mean pre-spread is silently failing.
    pub native_gossip_dropped_full: Counter,
    /// Outbound native actions dropped from pre-spread because a single action
    /// exceeds the receivers' gossip cap (`max_tx_message_size`) — it could never
    /// be delivered and would get the forwarder penalized (s339 validator ban).
    /// The pre-proposal push / DA pull path still carries these to inclusion.
    pub native_gossip_dropped_oversized: Counter,
    /// Exec trust-cache HITs: a locally-verified sender was reused, skipping the
    /// secp256k1 recover at execution (double-verify-trust-cache).
    pub verified_sender_cache_hits: Counter,
    /// Exec trust-cache MISSes: no cached sender, fell through to full recover+slash.
    pub verified_sender_cache_misses: Counter,
    /// Exec trust-cache evictions (FIFO cap reached). High vs hits => cap too small.
    pub verified_sender_cache_evictions: Counter,

    // Submit-ack phase timing (Sprint 3.5) — decomposes where multi-second
    // batch-submit acks accrue: semaphore queue vs blocking-pool verify vs
    // pool admission.
    pub rpc_submit_permit_wait_seconds: Histogram,
    pub rpc_submit_verify_seconds: Histogram,
    /// In-closure span of the verify work; `verify_seconds` wraps the
    /// `spawn_blocking` await, so wall − this ≈ blocking-pool queue + scheduling.
    /// Since Option A (ingress-verify-fix) the closure verifies via rayon, so
    /// this is parallel wall time, NOT cumulative CPU (name kept for dashboard
    /// continuity).
    pub rpc_submit_verify_cpu_seconds: Histogram,
    pub rpc_submit_admit_seconds: Histogram,
    /// Admit sub-phase (Option A): cumulative mempool-insert time per batch.
    pub rpc_submit_admit_insert_seconds: Histogram,
    /// Admit sub-phase (Option A): cumulative leader-forward time per batch.
    pub rpc_submit_admit_forward_seconds: Histogram,
    /// Batch-submit items rejected at admission, labeled by concrete reason
    /// (duplicate / sender_queue_full / pool_full / rate_limited /
    /// verify_failed / other) — shows WHICH limit fires under saturation.
    pub rpc_submit_admit_rejects: Family<Vec<(String, String)>, Counter>,

    // Link-storm visibility (Sprint 3.5) — the s338 sweep produced 155+ pull
    // timeouts and 238 substream exhaustions visible only as log warns.
    /// Native-DA pull requests that failed (timeout / substream exhaustion).
    pub native_da_pull_failures: Counter,
    /// Untracked direct sends (native push / leader-forward) that failed.
    pub direct_send_failures_untracked: Counter,
    /// Consensus (hotstuff) Broadcast messages whose gossipsub `publish` returned
    /// an error (RH3 / finding #5). A proposer losing its own proposal this way
    /// burns a full 500ms view fleet-wide; the value is now observable and the
    /// envelope is re-enqueued for a bounded retry instead of silently dropped.
    pub consensus_publish_failures: Counter,
    /// Native-action pre-spread actions lost to a failed gossipsub batch publish
    /// (RH3 / finding #5), summed over the actions in each failed batch. The batch
    /// is cleared before publish, so without this a dropped pre-spread batch
    /// vanished silently (bodies still reach inclusion via the pre-proposal push /
    /// DA pull path).
    pub native_gossip_publish_failures: Counter,
    /// Direct consensus messages from a peer NOT yet in the peer map that were
    /// dropped WITHOUT penalty (RH2 / finding #6). Previously each such message
    /// cost 20 points and 1h-banned an honest RPC/ingress node racing identify
    /// in 5 messages; the penalty is now reserved for cryptographically-proven
    /// invalidity. A rising value is the (now harmless) identify-race rate.
    pub unregistered_peer_no_penalty: Counter,

    // Block detail metrics
    pub block_transactions_count: Histogram,

    // Consensus timeout metrics
    pub consensus_timeout_total: Counter,

    // Step 3 dissemination-hardening metrics
    pub pending_sends_enqueued: Counter,
    pub pending_sends_flushed: Counter,
    pub native_bundle_repushed: Counter,
    pub missing_action_rejections: Counter,
    /// Native-DA pull-fallback fetches issued (Phase C Task 6). Must stay LOW under
    /// load — push covers the common case; a high rate signals push is failing.
    pub native_da_pull_requests: Counter,
    /// Native-DA pull-fallbacks that recovered all missing bodies in-call (Task 6).
    pub native_da_pull_recovered: Counter,
    /// Native-DA pull requests this node SERVED (off-loop, #6 fix A). The serve
    /// side was previously invisible at info level — S387's `serving=0` false
    /// signal. served + serve_dropped ≈ inbound pull requests seen.
    pub native_da_served: Counter,
    /// Native-DA serves answered all-empty because the bounded serve pool was at
    /// capacity (requester retries/rotates). A sustained rate = raise the pool
    /// cap or the pull storm is back.
    pub native_da_serve_dropped: Counter,
    /// Hot-path native-DA misses handed off to the recovery worker (BS-4a: each =
    /// one view failed fast with MissingData).
    pub native_da_recovery_handoffs: Counter,
    /// Recovery-worker batches whose pull budget expired without recovering every
    /// body.
    pub native_da_recovery_timeouts: Counter,

    // Exec-ceiling Option A (s351) — phase decomposition of the execution
    // thread. Phase histograms observe only when a block enters the native
    // section, so per-phase counts equal native-block counts; `exec_block_seconds`
    // observes every executed (non-replay) block.
    pub exec_verify_seconds: Histogram,
    pub exec_replay_guard_seconds: Histogram,
    pub exec_engine_seconds: Histogram,
    /// Sub-phase decomposition of one `execute_batch` call (s372). `exec_engine`
    /// above times the whole native section (two execute_batch calls + governance
    /// + fees + epoch); these three split a single call into margin reservation /
    ///   parallel matching / settlement so we can see which phase dominates.
    pub exec_phase_margin_seconds: Histogram,
    pub exec_phase_match_seconds: Histogram,
    pub exec_phase_settle_seconds: Histogram,
    pub exec_save_books_seconds: Histogram,
    pub exec_flush_seconds: Histogram,
    pub exec_block_seconds: Histogram,
    /// Committed blocks handed to the exec channel but not yet fully executed.
    /// Pinned near the channel bound (64) = execution is the bottleneck.
    pub exec_queue_depth: Gauge,
    /// O3: per-block trade-history batches queued to the background CF writer
    /// but not yet written. Sustained growth = RocksDB stalling behind exec.
    pub trade_writer_queued_batches: Gauge,

    // View-phase timing (hotstuff replica lifecycle, fed by ViewMetricsRecorder).
    // Decomposes per-leg block cadence per node: leader build + QC collection,
    // follower proposal arrival + persist + vote.
    pub view_duration_seconds: Histogram,
    pub view_propose_delay_seconds: Histogram,
    /// Leader propose_delay decomposition (S405 BS-3): StartView to own-block
    /// insert (produce_block + block-tree write)...
    pub view_propose_build_seconds: Histogram,
    /// ...and own-block insert to Propose broadcast (update/commit chain +
    /// event emit + broadcast handoff).
    pub view_propose_finalize_seconds: Histogram,
    pub view_qc_collect_seconds: Histogram,
    pub view_proposal_arrival_seconds: Histogram,
    pub view_insert_persist_seconds: Histogram,
    pub view_vote_delay_seconds: Histogram,
    pub commit_interval_seconds: Histogram,

    // Mesh watchdog (S405) — makes the S395 gossipsub degraded mode (validator
    // connected but never subscribed after a fast restart) visible on /metrics.
    /// Gossipsub mesh size for the consensus topic.
    pub consensus_mesh_peers: Gauge,
    /// Connected validator peers that gossipsub reports as subscribed to the
    /// consensus topic. Below (validator_set_size - 1) for >60s = wedge.
    pub consensus_subscribed_validators: Gauge,
    /// Validators force-disconnected by the mesh watchdog (connected but
    /// unsubscribed past grace) to re-trigger the subscription exchange.
    pub mesh_watchdog_disconnects: Counter,

    // P2 funnel instrumentation (perf/p2-funnel-instr) — closes the
    // offered→executed order-loss ledger: every previously-silent sink gets a
    // counter so sum(rejects + expiry + sheds + placed/filled) ≈ offered per cell.
    /// Native-pool entries (actions) purged by the 60s nonce-window expiry —
    /// previously info-log only, the biggest silent loss sink.
    pub native_pool_expired_actions: Counter,
    /// Orders purged with those expired actions (sum of `order_count` per
    /// evicted entry — actions × batch size for PlaceOrderBatch).
    pub native_pool_expired_orders: Counter,
    /// Batch-submit items shed because no verify permit arrived within
    /// SUBMIT_QUEUE_TIMEOUT ("server overloaded"). Counted in ITEMS (batch len)
    /// where known so the ledger stays in action units; the single-action
    /// endpoint counts 1 per shed call.
    pub rpc_submit_shed: Counter,
    /// Actions the single sequential gossip-ingest task pulled off `native_rx`
    /// and handed to the mempool (admitted or rejected — this counts intake).
    pub native_ingest_processed_actions: Counter,
    /// Instantaneous queue depth of the gossip-ingest channel (`native_rx`),
    /// sampled by the ingest task per message. Sustained growth = the single
    /// ingest task is the bottleneck (Phase-1 prediction (a)).
    pub native_ingest_rx_backlog: Gauge,
    /// Committed blocks enqueued to the exec channel (paired with
    /// `exec_queue_out`; in − out = occupancy incl. in-flight, and unlike the
    /// gauge the pair survives scrape races and shows flow rates).
    pub exec_queue_in: Counter,
    /// Committed blocks fully executed by the exec thread.
    pub exec_queue_out: Counter,
    /// Orders that entered a book as resting at exec
    /// (status Resting / PartiallyFilled / PendingTrigger at placement).
    pub orders_placed: Counter,
    /// Exec-side order deaths by reason (insufficient_margin / balance_error /
    /// fill_failed / engine_rejected / cancelled_unfilled / batch_cap_skipped /
    /// trader_cap).
    pub orders_rejected: Family<Vec<(String, String)>, Counter>,

    // P3 Round-1 ground-truth instrumentation (perf/p3-throughput) — closes the
    // remaining blind spots the P2 funnel report named: the mis-wired pool
    // occupancy gauge, book persistence (E4), and exec book IO cost.
    /// Native-pool insertions that SUCCEEDED (admitted to the pool). Paired with
    /// `mempool_native_size` (a working occupancy gauge, .set() after every
    /// mutation) it closes the intake identity per cell: the P2 gauge read 0 at
    /// every sample while thousands of actions were provably pooled.
    pub native_pool_inserted: Counter,
    // P3 Round-2 (perf/p3-throughput) — availability fast-lane instrumentation.
    /// Native-action bodies mirrored to the durable DA store by the OFF-LOOP
    /// mirror worker AT NETWORK RECEIPT (scope 1), ahead of the verify FIFO. The
    /// availability signal: a block-referenced body is reconstructable this fast
    /// even while ingest verify is backed up (was the 76s stall, R1 proof).
    pub native_da_mirror_actions: Counter,
    /// Raw inbound native bodies DROPPED before the mirror stage — the DoS shed
    /// valve: either the bounded raw-intake channel was full, or a single peer
    /// exceeded its per-peer in-flight byte budget. Recoverable via gossip
    /// redundancy / DA pull; devnet peers are validator-set-only (WAN review).
    pub native_raw_inbound_dropped: Counter,
    /// Decoded, ALREADY-DA-MIRRORED bodies dropped from the verify queue under
    /// backpressure (bounded post-mirror channel full). Safe by construction:
    /// the body is already DA-resident, so a drop only forfeits POOL candidacy,
    /// never availability (the R2.3 drop-safety property).
    pub native_verify_queue_dropped: Counter,
    /// Exec phase: deserializing every market's order book from the CF at the
    /// start of a block (native_executor `load_order_books`) — the O(markets×depth)
    /// per-block reload cost, invisible before this round.
    pub exec_load_books_seconds: Histogram,
    /// Bytes of Borsh-serialized order-book state written by `save_order_books`
    /// per block (summed over dirty books). Book-depth ground truth by weight.
    pub exec_save_books_bytes: Counter,
    /// Per-market resting order-book depth (`OrderBook::order_count`) sampled
    /// after `save_order_books` — the FIRST true depth ground truth at this base
    /// (getOrderBook is borsh-broken). Pins at 200×senders under the trader cap.
    pub native_resting_depth: Family<Vec<(String, String)>, Gauge>,
    /// Order books successfully decoded from the CF at block load (E4 probe).
    pub books_loaded: Counter,
    /// Order-book rows that FAILED to Borsh-decode at load (the silent `if let Ok`
    /// swallow, native_executor `load_order_books`). A nonzero value is a
    /// correctness bug: books are silently losing state. Resolves E4.
    pub books_decode_failed: Counter,

    // RocksDB runtime state (S405 BS-3) — the propose cost accumulates with
    // process lifetime, not persistent DB size; these expose the in-process
    // storage state (all reset on restart) to correlate against.
    /// Files at LSM level 0, per column family (reads probe every L0 file;
    /// write stalls trigger on L0 count).
    pub rocksdb_l0_files: Family<Vec<(String, String)>, Gauge>,
    /// cur-size-all-mem-tables, per column family.
    pub rocksdb_memtable_bytes: Family<Vec<(String, String)>, Gauge>,
    /// estimate-pending-compaction-bytes, per column family.
    pub rocksdb_pending_compaction_bytes: Family<Vec<(String, String)>, Gauge>,
    /// Shared block-cache usage (DB-wide).
    pub rocksdb_block_cache_bytes: Gauge,
}

impl Metrics {
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let blocks_committed = Counter::default();
        registry.register(
            "torus_blocks_committed",
            "Total number of committed blocks",
            blocks_committed.clone(),
        );

        let block_height = Gauge::default();
        registry.register(
            "torus_block_height",
            "Current block height",
            block_height.clone(),
        );

        let block_build_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_block_build_seconds",
            "Time to build a block",
            block_build_seconds.clone(),
        );

        let evm_txs_processed = Counter::default();
        registry.register(
            "torus_evm_txs_processed",
            "Total EVM transactions processed",
            evm_txs_processed.clone(),
        );

        let native_actions_processed = Counter::default();
        registry.register(
            "torus_native_actions_processed",
            "Total native actions processed",
            native_actions_processed.clone(),
        );

        let consensus_rounds = Counter::default();
        registry.register(
            "torus_consensus_rounds",
            "Total consensus rounds",
            consensus_rounds.clone(),
        );

        let consensus_view = Gauge::default();
        registry.register(
            "torus_consensus_view",
            "Current consensus view number",
            consensus_view.clone(),
        );

        let state_root_compute_seconds = Histogram::new(exponential_buckets(0.0001, 2.0, 15));
        registry.register(
            "torus_state_root_compute_seconds",
            "Time to compute state root",
            state_root_compute_seconds.clone(),
        );

        let mempool_evm_size = Gauge::default();
        registry.register(
            "torus_mempool_evm_size",
            "Number of pending EVM transactions",
            mempool_evm_size.clone(),
        );

        let mempool_native_size = Gauge::default();
        registry.register(
            "torus_mempool_native_size",
            "Number of pending native actions",
            mempool_native_size.clone(),
        );

        let peers_connected = Gauge::default();
        registry.register(
            "torus_peers_connected",
            "Number of connected peers",
            peers_connected.clone(),
        );

        let db_size_bytes = Gauge::default();
        registry.register(
            "torus_db_size_bytes",
            "Total RocksDB data directory size in bytes",
            db_size_bytes.clone(),
        );

        let epoch_number = Gauge::default();
        registry.register(
            "torus_epoch_number",
            "Current epoch number",
            epoch_number.clone(),
        );

        let validator_set_size = Gauge::default();
        registry.register(
            "torus_validator_set_size",
            "Number of validators in the active set",
            validator_set_size.clone(),
        );

        let orders_matched = Counter::default();
        registry.register(
            "torus_orders_matched",
            "Total number of order fills",
            orders_matched.clone(),
        );

        let liquidations_triggered = Counter::default();
        registry.register(
            "torus_liquidations_triggered",
            "Total liquidations triggered",
            liquidations_triggered.clone(),
        );

        let pruner_blocks_removed = Counter::default();
        registry.register(
            "torus_pruner_blocks_removed",
            "Total blocks removed by pruner",
            pruner_blocks_removed.clone(),
        );

        let rpc_requests_total = Family::<Vec<(String, String)>, Counter>::default();
        registry.register(
            "torus_rpc_requests_total",
            "Total RPC requests by method and status",
            rpc_requests_total.clone(),
        );

        let rpc_request_duration_seconds = Histogram::new(exponential_buckets(0.0001, 2.0, 15));
        registry.register(
            "torus_rpc_request_duration_seconds",
            "RPC request duration in seconds",
            rpc_request_duration_seconds.clone(),
        );

        let gossip_messages_received = Counter::default();
        registry.register(
            "torus_gossip_messages_received",
            "Total gossip messages received",
            gossip_messages_received.clone(),
        );

        let gossip_messages_sent = Counter::default();
        registry.register(
            "torus_gossip_messages_sent",
            "Total gossip messages sent",
            gossip_messages_sent.clone(),
        );

        let native_gossip_published_actions = Counter::default();
        registry.register(
            "torus_native_gossip_published_actions",
            "Native actions published to gossipsub (summed per batch)",
            native_gossip_published_actions.clone(),
        );

        let native_gossip_received_actions = Counter::default();
        registry.register(
            "torus_native_gossip_received_actions",
            "Native actions received from gossipsub and forwarded to ingest",
            native_gossip_received_actions.clone(),
        );

        let native_gossip_dropped_full = Counter::default();
        registry.register(
            "torus_native_gossip_dropped_full",
            "Outbound native actions dropped on full gossip channel",
            native_gossip_dropped_full.clone(),
        );

        let native_gossip_dropped_oversized = Counter::default();
        registry.register(
            "torus_native_gossip_dropped_oversized",
            "Outbound native actions dropped from pre-spread for exceeding the gossip message cap",
            native_gossip_dropped_oversized.clone(),
        );

        let verified_sender_cache_hits = Counter::default();
        registry.register(
            "torus_verified_sender_cache_hits",
            "Exec trust-cache hits (locally-verified sender reused, secp256k1 recover skipped)",
            verified_sender_cache_hits.clone(),
        );

        let verified_sender_cache_misses = Counter::default();
        registry.register(
            "torus_verified_sender_cache_misses",
            "Exec trust-cache misses (fell through to full recover + slash)",
            verified_sender_cache_misses.clone(),
        );

        let verified_sender_cache_evictions = Counter::default();
        registry.register(
            "torus_verified_sender_cache_evictions",
            "Exec trust-cache FIFO evictions",
            verified_sender_cache_evictions.clone(),
        );

        let rpc_submit_permit_wait_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_rpc_submit_permit_wait_seconds",
            "Time a batch submit waited for a verify permit",
            rpc_submit_permit_wait_seconds.clone(),
        );

        let rpc_submit_verify_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_rpc_submit_verify_seconds",
            "Time a batch submit spent in blocking-pool verification",
            rpc_submit_verify_seconds.clone(),
        );

        let rpc_submit_verify_cpu_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_rpc_submit_verify_cpu_seconds",
            "In-closure span of batch verify (parallel wall since Option A; verify_seconds minus this = pool queue)",
            rpc_submit_verify_cpu_seconds.clone(),
        );

        let rpc_submit_admit_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_rpc_submit_admit_seconds",
            "Time a batch submit spent admitting verified actions to the pool",
            rpc_submit_admit_seconds.clone(),
        );

        let rpc_submit_admit_insert_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_rpc_submit_admit_insert_seconds",
            "Cumulative mempool-insert time within one batch admit",
            rpc_submit_admit_insert_seconds.clone(),
        );

        let rpc_submit_admit_forward_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_rpc_submit_admit_forward_seconds",
            "Cumulative leader-forward time within one batch admit",
            rpc_submit_admit_forward_seconds.clone(),
        );

        let rpc_submit_admit_rejects = Family::<Vec<(String, String)>, Counter>::default();
        registry.register(
            "torus_rpc_submit_admit_rejects",
            "Batch-submit items rejected at admission, by reason",
            rpc_submit_admit_rejects.clone(),
        );
        // prometheus_client omits EMPTY families from encode(): without seeding,
        // the counter is invisible on /metrics until the first reject and every
        // before/after delta starts from null (P2 funnel item 2). Pre-create the
        // known reason series at 0 so scrapes always see them.
        for reason in [
            "duplicate",
            "sender_queue_full",
            "pool_full",
            "pool_full_preverify",
            "rate_limited",
            "verify_failed",
            "other",
        ] {
            let _ = rpc_submit_admit_rejects
                .get_or_create(&vec![("reason".to_string(), reason.to_string())]);
        }

        let native_da_pull_failures = Counter::default();
        registry.register(
            "torus_native_da_pull_failures",
            "Native-DA pull requests that failed (timeout or substream exhaustion)",
            native_da_pull_failures.clone(),
        );

        let direct_send_failures_untracked = Counter::default();
        registry.register(
            "torus_direct_send_failures_untracked",
            "Untracked direct sends (native push / leader-forward) that failed",
            direct_send_failures_untracked.clone(),
        );

        let consensus_publish_failures = Counter::default();
        registry.register(
            "torus_consensus_publish_failures",
            "Consensus Broadcast messages whose gossipsub publish failed (re-enqueued for bounded retry)",
            consensus_publish_failures.clone(),
        );

        let native_gossip_publish_failures = Counter::default();
        registry.register(
            "torus_native_gossip_publish_failures",
            "Native pre-spread actions lost to a failed gossipsub batch publish (summed per failed batch)",
            native_gossip_publish_failures.clone(),
        );

        let unregistered_peer_no_penalty = Counter::default();
        registry.register(
            "torus_unregistered_peer_no_penalty",
            "Direct consensus messages from an unregistered peer dropped without penalty (identify race, RH2)",
            unregistered_peer_no_penalty.clone(),
        );

        let block_transactions_count = Histogram::new(exponential_buckets(1.0, 2.0, 12));
        registry.register(
            "torus_block_transactions_count",
            "Number of transactions per committed block",
            block_transactions_count.clone(),
        );

        let consensus_timeout_total = Counter::default();
        registry.register(
            "torus_consensus_timeout_total",
            "Total consensus timeouts",
            consensus_timeout_total.clone(),
        );

        let pending_sends_enqueued = Counter::default();
        registry.register(
            "torus_pending_sends_enqueued",
            "Consensus unicast messages buffered because the target was unreachable",
            pending_sends_enqueued.clone(),
        );

        let pending_sends_flushed = Counter::default();
        registry.register(
            "torus_pending_sends_flushed",
            "Buffered consensus unicast messages flushed on (re)connect",
            pending_sends_flushed.clone(),
        );

        let native_bundle_repushed = Counter::default();
        registry.register(
            "torus_native_bundle_repushed",
            "Recent native-action bundles re-pushed to a (re)connecting validator",
            native_bundle_repushed.clone(),
        );

        let missing_action_rejections = Counter::default();
        registry.register(
            "torus_missing_action_rejections",
            "CompactBlock validations rejected after retry due to missing native actions",
            missing_action_rejections.clone(),
        );

        let native_da_pull_requests = Counter::default();
        registry.register(
            "torus_native_da_pull_requests",
            "Native-DA pull-fallback fetches issued on a reconstruction miss (should stay LOW)",
            native_da_pull_requests.clone(),
        );

        let native_da_pull_recovered = Counter::default();
        registry.register(
            "torus_native_da_pull_recovered",
            "Native-DA pull-fallbacks that recovered all missing bodies in-call",
            native_da_pull_recovered.clone(),
        );

        let native_da_served = Counter::default();
        registry.register(
            "torus_native_da_served",
            "Native-DA pull requests this node served (off-loop serve pool)",
            native_da_served.clone(),
        );

        let native_da_serve_dropped = Counter::default();
        registry.register(
            "torus_native_da_serve_dropped",
            "Native-DA serves answered all-empty because the serve pool was at capacity",
            native_da_serve_dropped.clone(),
        );

        let native_da_recovery_handoffs = Counter::default();
        registry.register(
            "torus_native_da_recovery_handoffs",
            "Hot-path native-DA misses handed off to the recovery worker (BS-4a: each = one view failed fast with MissingData)",
            native_da_recovery_handoffs.clone(),
        );

        let native_da_recovery_timeouts = Counter::default();
        registry.register(
            "torus_native_da_recovery_timeouts",
            "Recovery-worker batches whose pull budget expired without recovering every body",
            native_da_recovery_timeouts.clone(),
        );

        let exec_verify_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_verify_seconds",
            "Exec phase: batch signature verification of native actions",
            exec_verify_seconds.clone(),
        );

        let exec_replay_guard_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_replay_guard_seconds",
            "Exec phase: per-action (sender, nonce) replay-guard point reads",
            exec_replay_guard_seconds.clone(),
        );

        let exec_engine_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_engine_seconds",
            "Exec phase: native engine batches, governance, fees and epoch boundary",
            exec_engine_seconds.clone(),
        );

        let exec_phase_margin_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_phase_margin_seconds",
            "Exec sub-phase: per-order margin reservation + market partition (execute_batch Phase 2)",
            exec_phase_margin_seconds.clone(),
        );

        let exec_phase_match_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_phase_match_seconds",
            "Exec sub-phase: per-market parallel order matching (execute_batch Phase 3)",
            exec_phase_match_seconds.clone(),
        );

        let exec_phase_settle_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_phase_settle_seconds",
            "Exec sub-phase: settlement - margin release, fills, trade persist (execute_batch Phase 4)",
            exec_phase_settle_seconds.clone(),
        );

        let exec_save_books_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_save_books_seconds",
            "Exec phase: serializing dirty order books into the overlay",
            exec_save_books_seconds.clone(),
        );

        let exec_flush_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_flush_seconds",
            "Exec phase: nonce writes, atomic overlay flush and incremental tries",
            exec_flush_seconds.clone(),
        );

        let exec_block_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_block_seconds",
            "Total time to execute a committed block on the execution thread",
            exec_block_seconds.clone(),
        );

        let exec_queue_depth = Gauge::default();
        registry.register(
            "torus_exec_queue_depth",
            "Committed blocks sent to the execution channel but not yet executed",
            exec_queue_depth.clone(),
        );

        let trade_writer_queued_batches = Gauge::default();
        registry.register(
            "torus_trade_writer_queued_batches",
            "Trade-history KV batches queued to the background CF writer but not yet written",
            trade_writer_queued_batches.clone(),
        );

        let view_duration_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_view_duration_seconds",
            "Full view duration: StartView to the next StartView",
            view_duration_seconds.clone(),
        );

        let view_propose_delay_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_view_propose_delay_seconds",
            "Leader: StartView to Propose broadcast (block build + readiness)",
            view_propose_delay_seconds.clone(),
        );

        let view_propose_build_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_view_propose_build_seconds",
            "Leader: StartView to own-block insert (produce_block + block-tree write)",
            view_propose_build_seconds.clone(),
        );

        let view_propose_finalize_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_view_propose_finalize_seconds",
            "Leader: own-block insert to Propose broadcast (update/commit + broadcast handoff)",
            view_propose_finalize_seconds.clone(),
        );

        let view_qc_collect_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_view_qc_collect_seconds",
            "Leader: Propose broadcast to our block certified locally (PC observed via a later justify; full vote round-trip)",
            view_qc_collect_seconds.clone(),
        );

        let view_proposal_arrival_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_view_proposal_arrival_seconds",
            "Follower: StartView to the leader's proposal arriving",
            view_proposal_arrival_seconds.clone(),
        );

        let view_insert_persist_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_view_insert_persist_seconds",
            "Follower: proposal arrival to block persisted (validate + block-tree write)",
            view_insert_persist_seconds.clone(),
        );

        let view_vote_delay_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_view_vote_delay_seconds",
            "Follower: proposal arrival to phase vote sent",
            view_vote_delay_seconds.clone(),
        );

        let commit_interval_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_commit_interval_seconds",
            "Gap between consecutive local block commits (chain cadence per node)",
            commit_interval_seconds.clone(),
        );

        let consensus_mesh_peers = Gauge::default();
        registry.register(
            "torus_consensus_mesh_peers",
            "Gossipsub mesh size for the consensus topic",
            consensus_mesh_peers.clone(),
        );

        let consensus_subscribed_validators = Gauge::default();
        registry.register(
            "torus_consensus_subscribed_validators",
            "Connected validator peers subscribed to the consensus topic",
            consensus_subscribed_validators.clone(),
        );

        let mesh_watchdog_disconnects = Counter::default();
        registry.register(
            "torus_mesh_watchdog_disconnects",
            "Validators force-disconnected by the mesh watchdog (connected but unsubscribed past grace)",
            mesh_watchdog_disconnects.clone(),
        );

        let native_pool_expired_actions = Counter::default();
        registry.register(
            "torus_native_pool_expired_actions",
            "Native-pool actions purged by the 60s nonce-window expiry",
            native_pool_expired_actions.clone(),
        );

        let native_pool_expired_orders = Counter::default();
        registry.register(
            "torus_native_pool_expired_orders",
            "Orders purged with nonce-expired native-pool actions (sum of order_count per evicted entry)",
            native_pool_expired_orders.clone(),
        );

        let rpc_submit_shed = Counter::default();
        registry.register(
            "torus_rpc_submit_shed",
            "Submit items shed on verify-permit timeout (server overloaded), in items where batch length is known",
            rpc_submit_shed.clone(),
        );

        let native_ingest_processed_actions = Counter::default();
        registry.register(
            "torus_native_ingest_processed_actions",
            "Actions the sequential gossip-ingest task drained from native_rx into the mempool",
            native_ingest_processed_actions.clone(),
        );

        let native_ingest_rx_backlog = Gauge::default();
        registry.register(
            "torus_native_ingest_rx_backlog",
            "Queued messages in the gossip-ingest channel (native_rx), sampled per processed message",
            native_ingest_rx_backlog.clone(),
        );

        let exec_queue_in = Counter::default();
        registry.register(
            "torus_exec_queue_in",
            "Committed blocks enqueued to the execution channel",
            exec_queue_in.clone(),
        );

        let exec_queue_out = Counter::default();
        registry.register(
            "torus_exec_queue_out",
            "Committed blocks fully executed by the execution thread",
            exec_queue_out.clone(),
        );

        let orders_placed = Counter::default();
        registry.register(
            "torus_orders_placed",
            "Orders that entered a book as resting at execution (Resting/PartiallyFilled/PendingTrigger)",
            orders_placed.clone(),
        );

        let orders_rejected = Family::<Vec<(String, String)>, Counter>::default();
        registry.register(
            "torus_orders_rejected",
            "Exec-side order deaths, by reason",
            orders_rejected.clone(),
        );
        // Same empty-family pitfall as admit_rejects: pre-seed the fixed exec
        // reject reasons so the series are scrapeable at 0 from boot.
        for reason in [
            "insufficient_margin",
            "balance_error",
            "fill_failed",
            "engine_rejected",
            "cancelled_unfilled",
            "batch_cap_skipped",
            "trader_cap",
        ] {
            let _ =
                orders_rejected.get_or_create(&vec![("reason".to_string(), reason.to_string())]);
        }

        let native_pool_inserted = Counter::default();
        registry.register(
            "torus_native_pool_inserted",
            "Native-pool insertions that succeeded (admitted to the pool)",
            native_pool_inserted.clone(),
        );

        let native_da_mirror_actions = Counter::default();
        registry.register(
            "torus_native_da_mirror_actions",
            "Native bodies mirrored to the durable DA store by the off-loop receipt worker (ahead of verify)",
            native_da_mirror_actions.clone(),
        );

        let native_raw_inbound_dropped = Counter::default();
        registry.register(
            "torus_native_raw_inbound_dropped",
            "Raw inbound native bodies dropped before mirror (bounded intake full or per-peer byte budget)",
            native_raw_inbound_dropped.clone(),
        );

        let native_verify_queue_dropped = Counter::default();
        registry.register(
            "torus_native_verify_queue_dropped",
            "Already-DA-mirrored decoded bodies dropped from the verify queue under backpressure (safe: lose only pool candidacy)",
            native_verify_queue_dropped.clone(),
        );

        let exec_load_books_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_load_books_seconds",
            "Exec phase: deserializing every market's order book from the CF at block start",
            exec_load_books_seconds.clone(),
        );

        let exec_save_books_bytes = Counter::default();
        registry.register(
            "torus_exec_save_books_bytes",
            "Bytes of Borsh-serialized order-book state written by save_order_books per block",
            exec_save_books_bytes.clone(),
        );

        let native_resting_depth = Family::<Vec<(String, String)>, Gauge>::default();
        registry.register(
            "torus_native_resting_depth",
            "Resting order-book depth per market, sampled after save_order_books",
            native_resting_depth.clone(),
        );

        let books_loaded = Counter::default();
        registry.register(
            "torus_books_loaded",
            "Order books successfully decoded from the CF at block load (E4 probe)",
            books_loaded.clone(),
        );

        let books_decode_failed = Counter::default();
        registry.register(
            "torus_books_decode_failed",
            "Order-book rows that failed to Borsh-decode at load (nonzero = silent state loss)",
            books_decode_failed.clone(),
        );

        let rocksdb_l0_files = Family::<Vec<(String, String)>, Gauge>::default();
        registry.register(
            "torus_rocksdb_l0_files",
            "RocksDB files at level 0, per column family",
            rocksdb_l0_files.clone(),
        );

        let rocksdb_memtable_bytes = Family::<Vec<(String, String)>, Gauge>::default();
        registry.register(
            "torus_rocksdb_memtable_bytes",
            "RocksDB cur-size-all-mem-tables, per column family",
            rocksdb_memtable_bytes.clone(),
        );

        let rocksdb_pending_compaction_bytes = Family::<Vec<(String, String)>, Gauge>::default();
        registry.register(
            "torus_rocksdb_pending_compaction_bytes",
            "RocksDB estimate-pending-compaction-bytes, per column family",
            rocksdb_pending_compaction_bytes.clone(),
        );

        let rocksdb_block_cache_bytes = Gauge::default();
        registry.register(
            "torus_rocksdb_block_cache_bytes",
            "RocksDB shared block-cache usage",
            rocksdb_block_cache_bytes.clone(),
        );

        Self {
            registry,
            blocks_committed,
            block_height,
            block_build_seconds,
            evm_txs_processed,
            native_actions_processed,
            consensus_rounds,
            consensus_view,
            state_root_compute_seconds,
            mempool_evm_size,
            mempool_native_size,
            peers_connected,
            db_size_bytes,
            epoch_number,
            validator_set_size,
            orders_matched,
            liquidations_triggered,
            pruner_blocks_removed,
            rpc_requests_total,
            rpc_request_duration_seconds,
            gossip_messages_received,
            gossip_messages_sent,
            native_gossip_published_actions,
            native_gossip_received_actions,
            native_gossip_dropped_full,
            native_gossip_dropped_oversized,
            verified_sender_cache_hits,
            verified_sender_cache_misses,
            verified_sender_cache_evictions,
            rpc_submit_permit_wait_seconds,
            rpc_submit_verify_seconds,
            rpc_submit_verify_cpu_seconds,
            rpc_submit_admit_seconds,
            rpc_submit_admit_insert_seconds,
            rpc_submit_admit_forward_seconds,
            rpc_submit_admit_rejects,
            native_da_pull_failures,
            direct_send_failures_untracked,
            consensus_publish_failures,
            native_gossip_publish_failures,
            unregistered_peer_no_penalty,
            block_transactions_count,
            consensus_timeout_total,
            pending_sends_enqueued,
            pending_sends_flushed,
            native_bundle_repushed,
            missing_action_rejections,
            native_da_pull_requests,
            native_da_pull_recovered,
            native_da_served,
            native_da_serve_dropped,
            native_da_recovery_handoffs,
            native_da_recovery_timeouts,
            exec_verify_seconds,
            exec_replay_guard_seconds,
            exec_engine_seconds,
            exec_phase_margin_seconds,
            exec_phase_match_seconds,
            exec_phase_settle_seconds,
            exec_save_books_seconds,
            exec_flush_seconds,
            exec_block_seconds,
            exec_queue_depth,
            trade_writer_queued_batches,
            view_duration_seconds,
            view_propose_delay_seconds,
            view_propose_build_seconds,
            view_propose_finalize_seconds,
            view_qc_collect_seconds,
            view_proposal_arrival_seconds,
            view_insert_persist_seconds,
            view_vote_delay_seconds,
            commit_interval_seconds,
            consensus_mesh_peers,
            consensus_subscribed_validators,
            mesh_watchdog_disconnects,
            native_pool_expired_actions,
            native_pool_expired_orders,
            rpc_submit_shed,
            native_ingest_processed_actions,
            native_ingest_rx_backlog,
            exec_queue_in,
            exec_queue_out,
            orders_placed,
            orders_rejected,
            native_pool_inserted,
            native_da_mirror_actions,
            native_raw_inbound_dropped,
            native_verify_queue_dropped,
            exec_load_books_seconds,
            exec_save_books_bytes,
            native_resting_depth,
            books_loaded,
            books_decode_failed,
            rocksdb_l0_files,
            rocksdb_memtable_bytes,
            rocksdb_pending_compaction_bytes,
            rocksdb_block_cache_bytes,
        }
    }

    /// Encode all registered metrics in OpenMetrics text format.
    pub fn encode(&self) -> String {
        let mut buf = String::new();
        encode(&mut buf, &self.registry).expect("metrics encoding should not fail");
        buf
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Initialize the global tracing subscriber.
///
/// - Reads `RUST_LOG` env var for filtering (default: `info`).
/// - `json = true` emits structured JSON logs (for production).
/// - `json = false` emits human-readable logs (for development).
pub fn init_tracing(json: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if json {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .init();
    }
}

/// Serve `/health` and `/metrics` endpoints over HTTP.
///
/// - `GET /health` — returns 200 OK (liveness probe).
/// - `GET /metrics` — returns Prometheus/OpenMetrics text format.
pub async fn serve_metrics(
    addr: std::net::SocketAddr,
    metrics: Arc<Metrics>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "telemetry server listening");

    loop {
        let (mut stream, _peer) = listener.accept().await?;
        let metrics = metrics.clone();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = match stream.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let request = String::from_utf8_lossy(&buf[..n]);

            let response = if request.starts_with("GET /health") {
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 3\r\n\r\nOK\n"
                    .to_string()
            } else if request.starts_with("GET /metrics") {
                let body = metrics.encode();
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/openmetrics-text; version=1.0.0; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body,
                )
            } else {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
            };

            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_encode_nonempty() {
        let m = Metrics::new();
        m.blocks_committed.inc();
        let encoded = m.encode();
        assert!(encoded.contains("torus_blocks_committed"));
    }

    /// Ingress-verify-fix Option A: the admit sub-phase histograms must be
    /// registered so the probe can split admit cost into mempool insert vs
    /// leader-forward.
    #[test]
    fn admit_subphase_metrics_register() {
        let m = Metrics::new();
        let text = m.encode();
        for name in [
            "torus_rpc_submit_admit_insert_seconds",
            "torus_rpc_submit_admit_forward_seconds",
        ] {
            assert!(text.contains(name), "{name} not registered:\n{text}");
        }
    }

    /// Exec-ceiling Option A: the six execution-phase histograms and the
    /// exec-queue-depth gauge must be registered (present in encode() with
    /// zero observations), so the live probe can decompose where the
    /// execution thread's time goes.
    #[test]
    fn exec_phase_metrics_register() {
        let m = Metrics::new();
        let text = m.encode();
        for name in [
            "torus_exec_verify_seconds",
            "torus_exec_replay_guard_seconds",
            "torus_exec_engine_seconds",
            "torus_exec_phase_margin_seconds",
            "torus_exec_phase_match_seconds",
            "torus_exec_phase_settle_seconds",
            "torus_exec_save_books_seconds",
            "torus_exec_flush_seconds",
            "torus_exec_block_seconds",
            "torus_exec_queue_depth",
        ] {
            assert!(text.contains(name), "{name} not registered:\n{text}");
        }
    }

    /// RocksDB runtime state (S405 BS-3): per-CF L0/memtable/pending-compaction
    /// gauges and the shared block-cache gauge must be registered so the
    /// uptime-accumulating propose cost can be correlated with storage state.
    #[test]
    fn rocksdb_runtime_metrics_register() {
        let m = Metrics::new();
        let cf = vec![("cf".to_string(), "cf_consensus_meta".to_string())];
        m.rocksdb_l0_files.get_or_create(&cf).set(3);
        m.rocksdb_memtable_bytes.get_or_create(&cf).set(1024);
        m.rocksdb_pending_compaction_bytes.get_or_create(&cf).set(0);
        m.rocksdb_block_cache_bytes.set(4096);
        let text = m.encode();
        for name in [
            "torus_rocksdb_l0_files",
            "torus_rocksdb_memtable_bytes",
            "torus_rocksdb_pending_compaction_bytes",
            "torus_rocksdb_block_cache_bytes",
        ] {
            assert!(text.contains(name), "{name} not registered:\n{text}");
        }
    }

    /// Mesh watchdog (S405): the consensus-mesh gauges and watchdog-kick counter
    /// must be registered so the S395 degraded mode (validator connected but
    /// unsubscribed) is visible on /metrics instead of inferred from views/block.
    #[test]
    fn mesh_watchdog_metrics_register() {
        let m = Metrics::new();
        m.consensus_mesh_peers.set(2);
        m.consensus_subscribed_validators.set(2);
        m.mesh_watchdog_disconnects.inc();
        let text = m.encode();
        for name in [
            "torus_consensus_mesh_peers",
            "torus_consensus_subscribed_validators",
            "torus_mesh_watchdog_disconnects",
        ] {
            assert!(text.contains(name), "{name} not registered:\n{text}");
        }
    }

    /// Exec trust-cache (double-verify-trust-cache T6): hit/miss/eviction counters
    /// must be registered so the cache hit-rate is observable for A/B measurement.
    #[test]
    fn verified_sender_cache_metrics_register() {
        let m = Metrics::new();
        let text = m.encode();
        for name in [
            "torus_verified_sender_cache_hits",
            "torus_verified_sender_cache_misses",
            "torus_verified_sender_cache_evictions",
        ] {
            assert!(text.contains(name), "{name} not registered:\n{text}");
        }
    }

    /// P2 funnel instrumentation (perf/p2-funnel-instr): every silent order-loss
    /// sink gets a counter so the offered→executed ledger closes per bench cell.
    /// All nine metrics must be registered and observable on /metrics.
    #[test]
    fn p2_funnel_metrics_register() {
        let m = Metrics::new();
        m.native_pool_expired_actions.inc();
        m.native_pool_expired_orders.inc_by(400);
        m.rpc_submit_shed.inc();
        m.native_ingest_processed_actions.inc();
        m.native_ingest_rx_backlog.set(3);
        m.exec_queue_in.inc();
        m.exec_queue_out.inc();
        m.orders_placed.inc();
        m.orders_rejected
            .get_or_create(&vec![(
                "reason".to_string(),
                "insufficient_margin".to_string(),
            )])
            .inc();
        let text = m.encode();
        for name in [
            "torus_native_pool_expired_actions",
            "torus_native_pool_expired_orders",
            "torus_rpc_submit_shed",
            "torus_native_ingest_processed_actions",
            "torus_native_ingest_rx_backlog",
            "torus_exec_queue_in",
            "torus_exec_queue_out",
            "torus_orders_placed",
            "torus_orders_rejected",
        ] {
            assert!(text.contains(name), "{name} not registered:\n{text}");
        }
        assert!(
            text.contains(r#"torus_orders_rejected_total{reason="insufficient_margin"} 1"#),
            "labeled reject series must encode:\n{text}"
        );
    }

    /// P2 item 2: `torus_rpc_submit_admit_rejects` must be exported on /metrics —
    /// both the family metadata (visible even before any reject) and a concrete
    /// labeled series once a reason is counted.
    #[test]
    fn admit_rejects_family_exported() {
        let m = Metrics::new();
        let text = m.encode();
        assert!(
            text.contains("torus_rpc_submit_admit_rejects"),
            "admit-rejects family metadata must be exported with zero series:\n{text}"
        );
        m.rpc_submit_admit_rejects
            .get_or_create(&vec![("reason".to_string(), "duplicate".to_string())])
            .inc();
        let text = m.encode();
        assert!(
            text.contains(r#"torus_rpc_submit_admit_rejects_total{reason="duplicate"} 1"#),
            "labeled admit-reject series must encode:\n{text}"
        );
    }

    /// P3 Round-1 ground-truth instrumentation (perf/p3-throughput): the pool
    /// occupancy gauge fix companion counter, exec book-IO metrics, per-market
    /// resting-depth gauge, and the E4 decode probes must all register and encode.
    #[test]
    fn p3_round1_metrics_register() {
        let m = Metrics::new();
        m.native_pool_inserted.inc();
        m.exec_load_books_seconds.observe(0.01);
        m.exec_save_books_bytes.inc_by(4096);
        m.native_resting_depth
            .get_or_create(&vec![("market".to_string(), "1".to_string())])
            .set(200);
        m.books_loaded.inc_by(4);
        m.books_decode_failed.inc();
        let text = m.encode();
        for name in [
            "torus_native_pool_inserted",
            "torus_exec_load_books_seconds",
            "torus_exec_save_books_bytes",
            "torus_native_resting_depth",
            "torus_books_loaded",
            "torus_books_decode_failed",
        ] {
            assert!(text.contains(name), "{name} not registered:\n{text}");
        }
        assert!(
            text.contains(r#"torus_native_resting_depth{market="1"} 200"#),
            "labeled resting-depth series must encode:\n{text}"
        );
    }

    /// P3 Round-1 item 3(d): the exec reject-reason set must include `trader_cap`
    /// so 200-order trader-cap rejects are labeled instead of opaque
    /// `engine_rejected`. The series is pre-seeded, so it scrapes at 0 from boot.
    #[test]
    fn orders_rejected_trader_cap_preseeded() {
        let m = Metrics::new();
        let text = m.encode();
        assert!(
            text.contains(r#"torus_orders_rejected_total{reason="trader_cap"} 0"#),
            "trader_cap reject series must be pre-seeded at 0:\n{text}"
        );
    }

    /// BS-4a: the off-thread DA recovery worker's counters must be registered so
    /// the relaunch A/B can see the lever move (handoffs = hot-path misses handed
    /// off; timeouts = worker budget exhausted without recovery).
    #[test]
    fn da_recovery_metrics_register() {
        let m = Metrics::new();
        m.native_da_recovery_handoffs.inc();
        m.native_da_recovery_timeouts.inc();
        let text = m.encode();
        for name in [
            "torus_native_da_recovery_handoffs",
            "torus_native_da_recovery_timeouts",
        ] {
            assert!(text.contains(name), "{name} not registered:\n{text}");
        }
    }
}
