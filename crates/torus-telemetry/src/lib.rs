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

    // Order-funnel metrics (perf A1) — where PlaceOrder actions die inside
    // execute_batch. Observability only: incremented in torus-bridge's
    // native executor, never consulted by execution.
    /// Orders accepted onto a book: status Filled | PartiallyFilled | Resting.
    pub orders_placed_accepted: Counter,
    /// Orders that left liquidity on the book: status Resting | PartiallyFilled.
    pub orders_resting: Counter,
    /// Orders rejected pre-book: available balance below the margin reserve.
    pub orders_rejected_margin: Counter,
    /// Orders the matching engine returned OrderStatus::Rejected for (dust qty,
    /// non-positive/off-tick limit price, per-trader order cap, invalid stop
    /// trigger, PostOnly cross, FOK unfillable, market order into empty book).
    pub orders_rejected_book: Counter,
    /// IOC/FOK/Market orders cancelled on arrival with zero fills.
    pub orders_rejected_cancelled: Counter,
    /// IOC/Market remainders cancelled after partial fills (the filled part
    /// DID trade — counted separately so it never masquerades as a dead order).
    pub orders_cancelled_partial_fill: Counter,
    /// Resting maker orders auto-cancelled by self-trade prevention (counts
    /// cancelled makers, not the incoming order that triggered them).
    pub orders_self_trade_cancels: Counter,
    /// Orders that died on other error paths: balance read/write failures,
    /// fill-application failures.
    pub orders_rejected_other: Counter,

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

    // Block detail metrics
    pub block_transactions_count: Histogram,

    // Consensus timeout metrics
    pub consensus_timeout_total: Counter,

    // Step 3 dissemination-hardening metrics
    pub pending_sends_enqueued: Counter,
    pub pending_sends_flushed: Counter,
    pub native_bundle_repushed: Counter,
    pub missing_action_rejections: Counter,
    /// S459: proposer `produce_block` calls that dropped native actions because the
    /// durable DA-store mirror failed — proposed empty-native (which cannot wedge a
    /// validator) rather than referencing a body no validator could reconstruct.
    /// Monotonic; 0 on a healthy store.
    pub proposer_body_mirror_failures: Counter,
    /// Native-DA pull-fallback fetches issued (Phase C Task 6). Must stay LOW under
    /// load — push covers the common case; a high rate signals push is failing.
    pub native_da_pull_requests: Counter,
    /// Native-DA pull-fallbacks that recovered all missing bodies in-call (Task 6).
    pub native_da_pull_recovered: Counter,
    /// Native-DA bodies recovered by the erasure-shard pre-step (T8-int2): a body
    /// reconstructed from `k` distinct-source shards BEFORE the whole-body pull
    /// fallback ran. The A/B signal that shard recovery is firing — a rising rate
    /// here vs `native_da_pull_recovered` shows the shard path offloading pulls.
    pub native_da_shard_recovered: Counter,
    /// Shard fetch requests that hit a peer not speaking /torus/native-da-shards
    /// (mixed-version fleet): the peer answered the shard request with libp2p
    /// `OutboundFailure::UnsupportedProtocols`. Purely observational — the body
    /// falls back to the whole-body pull (never wedges); a rising rate signals a
    /// pre-shard-version cohort still in the fleet (T9).
    pub native_da_shard_unsupported_peer: Counter,
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
    /// rank-root: flush breakdown — native trie maintenance (bucket rehash +
    /// path propagation) inside the atomic flush.
    pub exec_root_seconds: Histogram,
    /// rank-root: flush breakdown — WriteBatch build + RocksDB write.
    pub exec_state_write_seconds: Histogram,
    /// rank-root: flush breakdown — post-flush EVM account resync.
    pub exec_evm_resync_seconds: Histogram,
    /// rank-root: buckets rehashed per block by the native-trie maintenance
    /// (the O(dirty) witness — compare against rows touched).
    pub exec_root_dirty_buckets: Histogram,
    /// rank-root round-3: CF_NATIVE_HASHED prefix-scans performed by native-trie
    /// maintenance. Drops below dirty-buckets as the bucket-member cache serves
    /// hits — the per-bucket-cost win witness.
    pub exec_root_bucket_scans: Counter,
    /// rank-root round-3: bucket-member cache hits / misses / LRU evictions.
    pub member_cache_hits: Counter,
    pub member_cache_misses: Counter,
    pub member_cache_evictions: Counter,
    pub exec_block_seconds: Histogram,
    /// PROFILER (s470): the EVM section of block execution (validate + commit
    /// bundle + block metadata). Near-zero in native-only bench cells; observed
    /// on every block so the residual accounts for it.
    pub exec_evm_seconds: Histogram,
    /// PROFILER (s470): per-block order-book LOAD + REBUILD. `NativeExecContext`
    /// is reconstructed every block, and its constructor scans the whole
    /// `cf_native_order_books` CF and rebuilds every resting order into memory
    /// (classic whole-book Borsh blobs, or C4 rows). O(total resting depth) per
    /// block — previously buried in the unattributed residual because it runs
    /// BEFORE `exec_engine_seconds` starts.
    pub exec_load_books_seconds: Histogram,
    /// PROFILER (s470): commit-callback persistence — block-body JSON write to
    /// CF_BLOCK_BODIES (+ standalone applied-height marker on non-native blocks).
    pub exec_body_persist_seconds: Histogram,
    /// PROFILER (s470): total resting orders across all books, sampled once per
    /// block right after the load/rebuild. Book-depth axis for the
    /// depth-vs-cost correlation (the funnel `orders_resting` counter is a
    /// monotonic event count, not current depth).
    pub exec_resting_orders: Gauge,
    /// Committed blocks handed to the exec channel but not yet fully executed.
    /// Pinned near the channel bound (64) = execution is the bottleneck.
    pub exec_queue_depth: Gauge,
    /// Package D rank 1: the exec-backlog pacing tier the proposer applied to
    /// its most recent native selection (0 full caps, 1 half, 2 quarter,
    /// 3 cancels-only). Stays 0 with `TORUS_EXEC_THROTTLE_WATERMARKS` unset.
    pub exec_throttle_tier: Gauge,
    /// Package D rank 2: committed blocks whose exec dispatch found the
    /// channel FULL and parked (deferred) instead of blocking the consensus
    /// thread. Monotonic; only moves with `TORUS_EXEC_NONBLOCKING_DISPATCH`.
    pub exec_dispatch_deferred: Counter,
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
    /// Cumulative pre-compression vs on-wire bytes per `/2.0` zstd wire path
    /// (labels: `path`, `kind`={pre,wire}). Refreshed from
    /// `torus_network::codec::wire_compression_stats`; ratio = pre/wire in PromQL.
    /// Proves the shipped body zstd (`/torus/{direct,block-data,native-da}/2.0`)
    /// is live on-wire and by how much (T3.2).
    pub wire_compression_bytes: Family<Vec<(String, String)>, Gauge>,
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

        let orders_placed_accepted = Counter::default();
        registry.register(
            "torus_orders_placed_accepted",
            "Orders accepted onto a book (status Filled|PartiallyFilled|Resting)",
            orders_placed_accepted.clone(),
        );

        let orders_resting = Counter::default();
        registry.register(
            "torus_orders_resting",
            "Orders that left liquidity on the book (status Resting|PartiallyFilled)",
            orders_resting.clone(),
        );

        let orders_rejected_margin = Counter::default();
        registry.register(
            "torus_orders_rejected_margin",
            "Orders rejected pre-book: available balance below the required margin reserve",
            orders_rejected_margin.clone(),
        );

        let orders_rejected_book = Counter::default();
        registry.register(
            "torus_orders_rejected_book",
            "Orders the matching engine rejected (dust, off-tick, order cap, PostOnly cross, FOK unfillable, market into empty book, bad stop trigger)",
            orders_rejected_book.clone(),
        );

        let orders_rejected_cancelled = Counter::default();
        registry.register(
            "torus_orders_rejected_cancelled",
            "IOC/FOK/Market orders cancelled on arrival with zero fills",
            orders_rejected_cancelled.clone(),
        );

        let orders_cancelled_partial_fill = Counter::default();
        registry.register(
            "torus_orders_cancelled_partial_fill",
            "IOC/Market remainders cancelled after partial fills (filled part traded)",
            orders_cancelled_partial_fill.clone(),
        );

        let orders_self_trade_cancels = Counter::default();
        registry.register(
            "torus_orders_self_trade_cancels",
            "Resting maker orders auto-cancelled by self-trade prevention",
            orders_self_trade_cancels.clone(),
        );

        let orders_rejected_other = Counter::default();
        registry.register(
            "torus_orders_rejected_other",
            "Orders that died on other error paths (balance read/write or fill application failures)",
            orders_rejected_other.clone(),
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

        let proposer_body_mirror_failures = Counter::default();
        registry.register(
            "torus_proposer_body_mirror_failures",
            "produce_block proposals that dropped native actions on a durable DA mirror failure (S459)",
            proposer_body_mirror_failures.clone(),
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

        let native_da_shard_recovered = Counter::default();
        registry.register(
            "torus_native_da_shard_recovered",
            "Native-DA bodies recovered by the erasure-shard pre-step before whole-body pull",
            native_da_shard_recovered.clone(),
        );

        let native_da_shard_unsupported_peer = Counter::default();
        registry.register(
            "torus_native_da_shard_unsupported_peer",
            "Shard fetch requests that hit a peer not speaking /torus/native-da-shards (mixed-version fleet); the body falls back to whole-body pull",
            native_da_shard_unsupported_peer.clone(),
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

        let exec_root_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_root_seconds",
            "Flush breakdown: incremental native-trie maintenance (bucket rehash + path)",
            exec_root_seconds.clone(),
        );

        let exec_state_write_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_state_write_seconds",
            "Flush breakdown: WriteBatch build + atomic RocksDB write",
            exec_state_write_seconds.clone(),
        );

        let exec_evm_resync_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_evm_resync_seconds",
            "Flush breakdown: post-flush incremental-trie EVM account resync",
            exec_evm_resync_seconds.clone(),
        );

        let exec_root_dirty_buckets = Histogram::new(exponential_buckets(1.0, 2.0, 16));
        registry.register(
            "torus_exec_root_dirty_buckets",
            "Buckets rehashed per block by native-trie maintenance (O(dirty) witness)",
            exec_root_dirty_buckets.clone(),
        );

        let exec_root_bucket_scans = Counter::default();
        registry.register(
            "torus_exec_root_bucket_scans",
            "CF_NATIVE_HASHED prefix-scans by native-trie maintenance (drops with member-cache hits)",
            exec_root_bucket_scans.clone(),
        );

        let member_cache_hits = Counter::default();
        registry.register(
            "torus_member_cache_hits",
            "Bucket-member cache hits (scan elided)",
            member_cache_hits.clone(),
        );

        let member_cache_misses = Counter::default();
        registry.register(
            "torus_member_cache_misses",
            "Bucket-member cache misses (mirror re-scanned)",
            member_cache_misses.clone(),
        );

        let member_cache_evictions = Counter::default();
        registry.register(
            "torus_member_cache_evictions",
            "Bucket-member cache LRU evictions (memory-budget pressure)",
            member_cache_evictions.clone(),
        );

        let exec_block_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_block_seconds",
            "Total time to execute a committed block on the execution thread",
            exec_block_seconds.clone(),
        );

        let exec_evm_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_evm_seconds",
            "Exec phase: EVM validate + commit bundle + block metadata (native-only cells ~0)",
            exec_evm_seconds.clone(),
        );

        let exec_load_books_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_load_books_seconds",
            "Exec phase: per-block order-book load + rebuild from cf_native_order_books \
             (O(total resting depth); classic blob or C4 rows depending on TORUS_BOOK_ROWS)",
            exec_load_books_seconds.clone(),
        );

        let exec_body_persist_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 14));
        registry.register(
            "torus_exec_body_persist_seconds",
            "Exec phase: commit-callback block-body persist to CF_BLOCK_BODIES (+ marker)",
            exec_body_persist_seconds.clone(),
        );

        let exec_resting_orders = Gauge::default();
        registry.register(
            "torus_exec_resting_orders",
            "Total resting orders across all books, sampled per block after load/rebuild",
            exec_resting_orders.clone(),
        );

        let exec_queue_depth = Gauge::default();
        registry.register(
            "torus_exec_queue_depth",
            "Committed blocks sent to the execution channel but not yet executed",
            exec_queue_depth.clone(),
        );

        let exec_throttle_tier = Gauge::default();
        registry.register(
            "torus_exec_throttle_tier",
            "Exec-backlog pacing tier applied to the proposer's latest native selection \
             (0 full, 1 half, 2 quarter, 3 cancels-only)",
            exec_throttle_tier.clone(),
        );

        let exec_dispatch_deferred = Counter::default();
        registry.register(
            "torus_exec_dispatch_deferred",
            "Committed blocks parked (deferred) because the exec channel was full \
             (non-blocking dispatch)",
            exec_dispatch_deferred.clone(),
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

        let wire_compression_bytes = Family::<Vec<(String, String)>, Gauge>::default();
        registry.register(
            "torus_wire_compression_bytes",
            "Cumulative bytes per /2.0 zstd wire path (labels: path, kind=pre|wire); ratio=pre/wire",
            wire_compression_bytes.clone(),
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
            orders_placed_accepted,
            orders_resting,
            orders_rejected_margin,
            orders_rejected_book,
            orders_rejected_cancelled,
            orders_cancelled_partial_fill,
            orders_self_trade_cancels,
            orders_rejected_other,
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
            block_transactions_count,
            consensus_timeout_total,
            pending_sends_enqueued,
            pending_sends_flushed,
            native_bundle_repushed,
            missing_action_rejections,
            proposer_body_mirror_failures,
            native_da_pull_requests,
            native_da_pull_recovered,
            native_da_shard_recovered,
            native_da_shard_unsupported_peer,
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
            exec_root_seconds,
            exec_state_write_seconds,
            exec_evm_resync_seconds,
            exec_root_dirty_buckets,
            exec_root_bucket_scans,
            member_cache_hits,
            member_cache_misses,
            member_cache_evictions,
            exec_block_seconds,
            exec_evm_seconds,
            exec_load_books_seconds,
            exec_body_persist_seconds,
            exec_resting_orders,
            exec_queue_depth,
            exec_throttle_tier,
            exec_dispatch_deferred,
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
            rocksdb_l0_files,
            rocksdb_memtable_bytes,
            rocksdb_pending_compaction_bytes,
            rocksdb_block_cache_bytes,
            wire_compression_bytes,
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
            "torus_exec_evm_seconds",
            "torus_exec_load_books_seconds",
            "torus_exec_body_persist_seconds",
            "torus_exec_resting_orders",
            "torus_exec_queue_depth",
            "torus_exec_throttle_tier",
            "torus_exec_dispatch_deferred",
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

    #[test]
    fn wire_compression_metrics_register() {
        let m = Metrics::new();
        let pre = vec![
            ("path".to_string(), "native-da".to_string()),
            ("kind".to_string(), "pre".to_string()),
        ];
        let wire = vec![
            ("path".to_string(), "native-da".to_string()),
            ("kind".to_string(), "wire".to_string()),
        ];
        m.wire_compression_bytes.get_or_create(&pre).set(9400);
        m.wire_compression_bytes.get_or_create(&wire).set(1000);
        let text = m.encode();
        assert!(
            text.contains("torus_wire_compression_bytes"),
            "torus_wire_compression_bytes not registered:\n{text}"
        );
        assert!(text.contains("kind=\"pre\""), "pre-bytes series missing:\n{text}");
        assert!(text.contains("kind=\"wire\""), "wire-bytes series missing:\n{text}");
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
