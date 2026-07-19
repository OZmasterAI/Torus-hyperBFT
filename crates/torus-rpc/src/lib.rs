//! JSON-RPC server for the Torus-hyperBFT blockchain.
//!
//! Implements Ethereum-compatible `eth_*`, `net_*`, and `web3_*` namespaces
//! using jsonrpsee 0.26 with WebSocket subscription support.

pub mod error;
pub mod eth;
pub mod net;
pub mod torus;
pub mod types;
pub mod web3;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Concurrent in-flight native-action submissions. 16 measurably capped ingress
/// at ~100-240 actions/s (s334 off-box bench was submit-bound, not
/// consensus-bound); 64 keeps deserialize+ecrecover work bounded on the
/// blocking pool while letting bursts through.
pub(crate) const SUBMIT_PERMITS: usize = 64;

/// How long a submission may wait for a permit before the server sheds it.
/// Bursts queue briefly instead of instantly erroring "overloaded"; sustained
/// saturation still rejects after this bound.
pub(crate) const SUBMIT_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// Max items per `torus_submitNativeActions` call. Bounds the work one permit
/// admits: ~100 ecrecovers ≈ 5–10ms on the blocking pool.
pub(crate) const SUBMIT_BATCH_MAX: usize = 100;

/// Default jsonrpsee max response body size, in MiB. Matches jsonrpsee 0.26's
/// implicit default (`TEN_MB_SIZE_BYTES = 10 * 1024 * 1024`) so a node with no
/// `TORUS_RPC_MAX_RESPONSE_MB` set behaves byte-for-byte as before. The full-
/// est blocks (`torus_getBlockBody`) can exceed 10 MiB and get silently dropped,
/// under-reporting throughput exactly when it matters most (#40) — a bench sets
/// this higher explicitly via the env var below.
pub(crate) const DEFAULT_MAX_RESPONSE_MB: u32 = 10;

/// Default jsonrpsee `max_connections`. Today's hardcoded value (64) — a low cap
/// both throttles ingress and starves the bench monitor's connections
/// (measurement blind spot, #41). Overridable via `TORUS_RPC_MAX_CONNS`.
pub(crate) const DEFAULT_MAX_CONNECTIONS: u32 = 64;

/// Env var: max jsonrpsee response body size in MiB (#40).
pub(crate) const ENV_MAX_RESPONSE_MB: &str = "TORUS_RPC_MAX_RESPONSE_MB";
/// Env var: max concurrent jsonrpsee connections (#41).
pub(crate) const ENV_MAX_CONNS: &str = "TORUS_RPC_MAX_CONNS";

/// Resolve the jsonrpsee max response body size (bytes) from a raw env value.
/// Pure (takes the raw string, not the process env) so it is unit-testable
/// without env races. Empty / unset / unparsable / zero all fall back to the
/// exact-today default (10 MiB). A configured MiB value is saturating-multiplied
/// into bytes and clamped to `u32::MAX` (the jsonrpsee field width).
pub(crate) fn resolve_max_response_bytes(raw: Option<&str>) -> u32 {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => match s.parse::<u32>() {
            Ok(mb) if mb > 0 => mb.saturating_mul(1024 * 1024),
            _ => DEFAULT_MAX_RESPONSE_MB.saturating_mul(1024 * 1024),
        },
        None => DEFAULT_MAX_RESPONSE_MB.saturating_mul(1024 * 1024),
    }
}

/// Resolve jsonrpsee `max_connections` from a raw env value. Pure/testable;
/// empty / unset / unparsable / zero fall back to the exact-today default (64).
pub(crate) fn resolve_max_connections(raw: Option<&str>) -> u32 {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => match s.parse::<u32>() {
            Ok(n) if n > 0 => n,
            _ => DEFAULT_MAX_CONNECTIONS,
        },
        None => DEFAULT_MAX_CONNECTIONS,
    }
}

/// Acquire a submission permit, waiting at most [`SUBMIT_QUEUE_TIMEOUT`].
/// `None` ⇒ saturated past the queue bound (caller returns "overloaded").
pub(crate) async fn acquire_submit_permit(
    sem: &tokio::sync::Semaphore,
) -> Option<tokio::sync::SemaphorePermit<'_>> {
    tokio::time::timeout(SUBMIT_QUEUE_TIMEOUT, sem.acquire())
        .await
        .ok()?
        .ok()
}

use alloy_primitives::B256;
use jsonrpsee::server::middleware::rpc::{self as rpc_mw, RpcServiceT};
use jsonrpsee::server::{ServerBuilder, ServerHandle};
use tokio::sync::broadcast;
use torus_evm::EvmExecutor;
use torus_mempool::Mempool;
use torus_state::cf::CF_BLOCK_HEADERS;
use torus_state::StateDb;

use crate::eth::EthApiServer;
use crate::net::NetApiServer;
use crate::torus::TorusApiServer;
use crate::web3::Web3ApiServer;

/// Broadcast channels for WebSocket subscriptions.
#[derive(Clone)]
pub struct BlockNotifier {
    pub new_heads: broadcast::Sender<serde_json::Value>,
    pub new_logs: broadcast::Sender<Vec<serde_json::Value>>,
    pub pending_txs: broadcast::Sender<B256>,
    pub new_trades: broadcast::Sender<Vec<serde_json::Value>>,
}

impl BlockNotifier {
    pub fn new() -> Self {
        let (new_heads, _) = broadcast::channel(256);
        let (new_logs, _) = broadcast::channel(256);
        let (pending_txs, _) = broadcast::channel(1024);
        let (new_trades, _) = broadcast::channel(256);
        Self {
            new_heads,
            new_logs,
            pending_txs,
            new_trades,
        }
    }

    /// Notify subscribers of a new block head.
    pub fn notify_new_block(&self, head: serde_json::Value) {
        let _ = self.new_heads.send(head);
    }

    /// Notify subscribers of a pending transaction.
    pub fn notify_pending_tx(&self, hash: B256) {
        let _ = self.pending_txs.send(hash);
    }

    /// Notify subscribers of new trades from a committed block.
    pub fn notify_new_trades(&self, trades: Vec<serde_json::Value>) {
        if !trades.is_empty() {
            let _ = self.new_trades.send(trades);
        }
    }
}

impl Default for BlockNotifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-sender transaction submission rate limiter (Batch EK: EVM-FIND-19).
/// Counts submissions at send_raw_transaction time, not at block commit time.
#[derive(Clone)]
pub struct TxSubmitLimiter {
    /// (count, window_start) per sender address.
    inner: Arc<Mutex<HashMap<alloy_primitives::Address, (u32, Instant)>>>,
    max_per_window: u32,
}

impl TxSubmitLimiter {
    pub fn new(max_per_window: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_per_window,
        }
    }

    /// Returns true if the sender is within rate limits. Increments the counter.
    pub fn check_sender(&self, sender: &alloy_primitives::Address) -> bool {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        let entry = map.entry(*sender).or_insert((0, now));
        // 10-second sliding window
        if now.duration_since(entry.1) >= std::time::Duration::from_secs(10) {
            entry.0 = 0;
            entry.1 = now;
        }
        if entry.0 >= self.max_per_window {
            return false;
        }
        entry.0 += 1;
        true
    }
}

/// Shared state for all RPC handlers.
#[derive(Clone)]
pub struct RpcState {
    pub(crate) state: StateDb,
    pub(crate) mempool: Arc<Mempool>,
    pub(crate) executor: Arc<EvmExecutor>,
    pub(crate) chain_id: u64,
    pub(crate) epoch_length: u64,
    pub(crate) latest_height: Arc<AtomicU64>,
    pub(crate) notifier: BlockNotifier,
    /// Block height up to which historical data has been pruned (0 = archive mode).
    pub(crate) pruned_up_to: Arc<AtomicU64>,
    /// Per-sender tx submission rate limiter (Batch EK: EVM-FIND-19).
    pub(crate) tx_submit_limiter: TxSubmitLimiter,
    /// Global cap on active WebSocket subscriptions (HIGH-NEW-05).
    pub(crate) active_subscriptions: Arc<AtomicUsize>,
    /// Prometheus metrics handle.
    pub(crate) metrics: Option<Arc<torus_telemetry::Metrics>>,
    /// This node's ed25519 verifying key (for leader comparison).
    pub(crate) own_vk: Option<[u8; 32]>,
    /// Returns the current leader's verifying key bytes.
    pub(crate) leader_vk_fn: Option<Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync>>,
    /// Channel to forward native actions to the leader: (leader_vk, sender_addr ++ action_json).
    pub(crate) forward_action_tx: Option<tokio::sync::mpsc::UnboundedSender<([u8; 32], Vec<u8>)>>,
    /// Channel to forward raw EVM txs to the leader: (leader_vk, raw_rlp). Option B — EVM tx
    /// dissemination. Unconditional (no `forward_bodies` gate): EVM has no gossip pre-spread,
    /// so the unicast is the ONLY way a tx submitted to a non-proposer reaches the producer.
    pub(crate) forward_evm_tx: Option<tokio::sync::mpsc::UnboundedSender<([u8; 32], Vec<u8>)>>,
    /// Forward full action bodies to the leader. Only wanted when native gossip
    /// is OFF (fallback mode) — with gossip pre-spread on, bodies already reach
    /// every validator and the duplicate forward just burns the leader's link
    /// (Sprint 3.5; the s338 sweep storm was partly this).
    pub(crate) forward_bodies: bool,
    /// Admission control: cap concurrent submit_native_action calls.
    pub(crate) submit_semaphore: Arc<tokio::sync::Semaphore>,
}

/// JSON-RPC server combining eth, net, and web3 namespaces.
pub struct RpcServer {
    state: RpcState,
}

impl RpcServer {
    /// Create a new RPC server. Scans the DB to find the latest block height.
    pub fn new(
        state_db: StateDb,
        mempool: Arc<Mempool>,
        executor: Arc<EvmExecutor>,
        chain_id: u64,
        epoch_length: u64,
        notifier: BlockNotifier,
    ) -> Self {
        let latest = find_latest_height(&state_db);
        Self {
            state: RpcState {
                state: state_db,
                mempool,
                executor,
                chain_id,
                epoch_length,
                latest_height: Arc::new(AtomicU64::new(latest)),
                notifier,
                pruned_up_to: Arc::new(AtomicU64::new(0)),
                tx_submit_limiter: TxSubmitLimiter::new(50), // 50 tx per 10s per sender
                active_subscriptions: Arc::new(AtomicUsize::new(0)),
                metrics: None,
                own_vk: None,
                leader_vk_fn: None,
                forward_action_tx: None,
                forward_evm_tx: None,
                forward_bodies: false,
                submit_semaphore: Arc::new(tokio::sync::Semaphore::new(SUBMIT_PERMITS)),
            },
        }
    }

    /// Configure leader forwarding for direct-to-leader native action submission.
    /// `forward_bodies`: send full action bodies to the leader — pass `true`
    /// only when native gossip is OFF; with gossip on, bodies already pre-spread
    /// and the forward would duplicate every body on the leader's link.
    pub fn set_leader_forwarding(
        &mut self,
        own_vk: [u8; 32],
        leader_vk_fn: Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync>,
        forward_tx: tokio::sync::mpsc::UnboundedSender<([u8; 32], Vec<u8>)>,
        evm_forward_tx: tokio::sync::mpsc::UnboundedSender<([u8; 32], Vec<u8>)>,
        forward_bodies: bool,
    ) {
        self.state.own_vk = Some(own_vk);
        self.state.leader_vk_fn = Some(leader_vk_fn);
        self.state.forward_action_tx = Some(forward_tx);
        self.state.forward_evm_tx = Some(evm_forward_tx);
        self.state.forward_bodies = forward_bodies;
    }

    /// Use an externally-created height counter (shared with the commit handler).
    pub fn set_latest_height_handle(&mut self, handle: Arc<AtomicU64>) {
        self.state.latest_height = handle;
    }

    /// Get a shared handle to the latest block height atomic.
    pub fn latest_height(&self) -> Arc<AtomicU64> {
        self.state.latest_height.clone()
    }

    /// Set the pruned-up-to height for RPC error handling.
    pub fn set_pruned_up_to(&self, pruned: Arc<AtomicU64>) {
        let val = pruned.load(Ordering::Relaxed);
        self.state.pruned_up_to.store(val, Ordering::Relaxed);
    }

    /// Get a shared handle to the pruned-up-to atomic.
    pub fn pruned_up_to(&self) -> Arc<AtomicU64> {
        self.state.pruned_up_to.clone()
    }

    /// Set the Prometheus metrics handle for RPC instrumentation.
    pub fn set_metrics(&mut self, metrics: Arc<torus_telemetry::Metrics>) {
        self.state.metrics = Some(metrics);
    }

    /// Start the RPC server on the given address.
    pub async fn start(
        self,
        addr: SocketAddr,
    ) -> Result<(ServerHandle, SocketAddr), Box<dyn std::error::Error + Send + Sync>> {
        let layer = MetricsLayer {
            metrics: self.state.metrics.clone(),
        };
        let rpc_middleware = rpc_mw::RpcServiceBuilder::new().layer(layer);
        // #40/#41: response-size and connection caps are env-configurable, both
        // defaulting to today's values (10 MiB / 64) so an unset environment is
        // byte-for-byte identical to the hardcoded config it replaces.
        let max_response_bytes =
            resolve_max_response_bytes(std::env::var(ENV_MAX_RESPONSE_MB).ok().as_deref());
        let max_connections =
            resolve_max_connections(std::env::var(ENV_MAX_CONNS).ok().as_deref());
        let server_cfg = jsonrpsee::server::ServerConfig::builder()
            .max_connections(max_connections)
            .max_response_body_size(max_response_bytes)
            .build();
        let server = ServerBuilder::with_config(server_cfg)
            .set_rpc_middleware(rpc_middleware)
            .build(addr)
            .await?;
        let local_addr = server.local_addr()?;
        let mut module = jsonrpsee::RpcModule::new(());
        module.merge(EthApiServer::into_rpc(self.state.clone()))?;
        module.merge(NetApiServer::into_rpc(self.state.clone()))?;
        module.merge(Web3ApiServer::into_rpc(self.state.clone()))?;
        module.merge(TorusApiServer::into_rpc(self.state.clone()))?;
        let handle = server.start(module);
        Ok((handle, local_addr))
    }

    /// Get a reference to the RPC state.
    pub fn state(&self) -> &RpcState {
        &self.state
    }
}

// ---------------------------------------------------------------------------
// RPC metrics middleware
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MetricsLayer {
    metrics: Option<Arc<torus_telemetry::Metrics>>,
}

impl<S: Clone> tower::Layer<S> for MetricsLayer {
    type Service = MetricsMiddleware<S>;
    fn layer(&self, inner: S) -> Self::Service {
        MetricsMiddleware {
            inner,
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Clone)]
struct MetricsMiddleware<S> {
    inner: S,
    metrics: Option<Arc<torus_telemetry::Metrics>>,
}

impl<S> RpcServiceT for MetricsMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = rpc_mw::MethodResponse,
            BatchResponse = rpc_mw::MethodResponse,
            NotificationResponse = rpc_mw::MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = rpc_mw::MethodResponse;
    type BatchResponse = rpc_mw::MethodResponse;
    type NotificationResponse = rpc_mw::MethodResponse;

    fn call<'a>(
        &self,
        request: rpc_mw::Request<'a>,
    ) -> impl std::future::Future<Output = Self::MethodResponse> + Send + 'a {
        let metrics = self.metrics.clone();
        let method = request.method_name().to_string();
        let inner = self.inner.clone();
        let start = Instant::now();

        async move {
            let response = inner.call(request).await;
            if let Some(ref m) = metrics {
                let duration = start.elapsed();
                let status = if response.is_success() { "ok" } else { "error" };
                m.rpc_requests_total
                    .get_or_create(&vec![
                        ("method".into(), method),
                        ("status".into(), status.into()),
                    ])
                    .inc();
                m.rpc_request_duration_seconds
                    .observe(duration.as_secs_f64());
            }
            response
        }
    }

    fn batch<'a>(
        &self,
        batch: rpc_mw::Batch<'a>,
    ) -> impl std::future::Future<Output = Self::BatchResponse> + Send + 'a {
        self.inner.batch(batch)
    }

    fn notification<'a>(
        &self,
        n: rpc_mw::Notification<'a>,
    ) -> impl std::future::Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(n)
    }
}

/// Find the latest block height using a reverse iterator on CF_BLOCK_HEADERS.
///
/// FIX 9 (EVM-FIND-14): Previous implementation scanned from block 0 upward (O(N)).
/// Now uses a reverse iterator to find the last key in O(1).
pub fn find_latest_height(state: &StateDb) -> u64 {
    let Ok(cf) = state.cf_handle(CF_BLOCK_HEADERS) else {
        return 0;
    };
    // Non-height entries (e.g. the pruner's `__prune_meta__` key) sort AFTER
    // the 8-byte big-endian height keys, so skip past them instead of bailing
    // to 0 — on a pruned DB the meta key is the last entry and bailing froze
    // eth_blockNumber at 0 and short-circuited on_commit_block (s334). The
    // take(8) bounds the scan; today there is exactly one meta key.
    for entry in state
        .inner()
        .iterator_cf(cf, rocksdb::IteratorMode::End)
        .take(8)
    {
        match entry {
            Ok((key, _)) if key.len() == 8 => {
                return u64::from_be_bytes(key[..8].try_into().unwrap());
            }
            Ok(_) => continue,
            Err(_) => return 0,
        }
    }
    0
}

/// Public helper to update latest height after committing a new block.
pub fn set_latest_height(state: &RpcState, height: u64) {
    state.latest_height.store(height, Ordering::Relaxed);
}

/// Scan CF_NATIVE_TRADES for all trades at a given block height across all markets.
/// Used by the on_commit_block handler to feed the `new_trades` broadcast channel.
pub fn scan_trades_for_block(state: &StateDb, block_height: u64) -> Vec<serde_json::Value> {
    use borsh::BorshDeserialize;
    use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_TRADES};

    let db = state.inner();
    let market_cf = match db.cf_handle(CF_NATIVE_MARKETS) {
        Some(cf) => cf,
        None => return vec![],
    };
    let trade_cf = match db.cf_handle(CF_NATIVE_TRADES) {
        Some(cf) => cf,
        None => return vec![],
    };

    let height_bytes = block_height.to_be_bytes();
    let mut trades = Vec::new();

    // Iterate all known market IDs and seek into CF_NATIVE_TRADES for each.
    for item in db.iterator_cf(market_cf, rocksdb::IteratorMode::Start) {
        let (market_key, _) = match item {
            Ok(kv) => kv,
            Err(_) => continue,
        };
        if market_key.len() != 8 {
            continue;
        }

        // Build seek key: market_id(8) + block_height(8) + trade_index(0)
        let mut start_key = [0u8; 20];
        start_key[..8].copy_from_slice(&market_key);
        start_key[8..16].copy_from_slice(&height_bytes);

        let iter = db.iterator_cf(
            trade_cf,
            rocksdb::IteratorMode::From(&start_key, rocksdb::Direction::Forward),
        );
        for item in iter {
            let (key, value) = match item {
                Ok(kv) => kv,
                Err(_) => break,
            };
            if key.len() < 16 {
                break;
            }
            // Verify market_id prefix matches
            if key[..8] != market_key[..] {
                break;
            }
            // Verify block_number matches
            if key[8..16] != height_bytes[..] {
                break;
            }

            // Deserialize StoredTrade (borsh: trade_id u128 + price_raw i128 +
            // quantity_raw i128 + side u8 + block_number u64 + timestamp u64)
            #[derive(BorshDeserialize)]
            struct StoredTrade {
                trade_id: u128,
                price_raw: i128,
                quantity_raw: i128,
                side: u8,
                block_number: u64,
                timestamp: u64,
            }

            if let Ok(t) = StoredTrade::try_from_slice(&value) {
                let market_id = u64::from_be_bytes(market_key[..8].try_into().unwrap());
                trades.push(serde_json::json!({
                    "marketId": format!("0x{:x}", market_id),
                    "tradeId": format!("0x{:x}", t.trade_id),
                    "price": format!("0x{:x}", t.price_raw),
                    "quantity": format!("0x{:x}", t.quantity_raw),
                    "side": if t.side == 0 { "buy" } else { "sell" },
                    "blockNumber": format!("0x{:x}", t.block_number),
                    "timestamp": format!("0x{:x}", t.timestamp),
                }));
            }
        }
    }

    trades
}

#[cfg(test)]
mod rpc_limit_env_tests {
    use super::*;

    const TEN_MIB: u32 = 10 * 1024 * 1024;

    #[test]
    fn max_response_unset_is_todays_10_mib_default() {
        // Unset / empty / whitespace all resolve to jsonrpsee's implicit 10 MiB,
        // so an env-free node is byte-for-byte identical to before (#40).
        assert_eq!(resolve_max_response_bytes(None), TEN_MIB);
        assert_eq!(resolve_max_response_bytes(Some("")), TEN_MIB);
        assert_eq!(resolve_max_response_bytes(Some("   ")), TEN_MIB);
        assert_eq!(TEN_MIB, DEFAULT_MAX_RESPONSE_MB * 1024 * 1024);
    }

    #[test]
    fn max_response_parses_mib_to_bytes() {
        assert_eq!(resolve_max_response_bytes(Some("64")), 64 * 1024 * 1024);
        assert_eq!(resolve_max_response_bytes(Some(" 128 ")), 128 * 1024 * 1024);
        assert_eq!(resolve_max_response_bytes(Some("1")), 1024 * 1024);
    }

    #[test]
    fn max_response_bad_or_zero_falls_back_to_default() {
        assert_eq!(resolve_max_response_bytes(Some("0")), TEN_MIB);
        assert_eq!(resolve_max_response_bytes(Some("abc")), TEN_MIB);
        assert_eq!(resolve_max_response_bytes(Some("-5")), TEN_MIB);
    }

    #[test]
    fn max_response_saturates_instead_of_overflowing() {
        // MiB values that would overflow u32 bytes clamp to u32::MAX rather than
        // panicking or wrapping.
        assert_eq!(resolve_max_response_bytes(Some("100000")), u32::MAX);
    }

    #[test]
    fn max_connections_unset_is_todays_64_default() {
        assert_eq!(resolve_max_connections(None), 64);
        assert_eq!(resolve_max_connections(Some("")), 64);
        assert_eq!(resolve_max_connections(Some("  ")), 64);
        assert_eq!(DEFAULT_MAX_CONNECTIONS, 64);
    }

    #[test]
    fn max_connections_parses_and_rejects_bad_values() {
        assert_eq!(resolve_max_connections(Some("256")), 256);
        assert_eq!(resolve_max_connections(Some(" 1024 ")), 1024);
        // Zero / garbage fall back to the default rather than disabling ingress.
        assert_eq!(resolve_max_connections(Some("0")), 64);
        assert_eq!(resolve_max_connections(Some("xyz")), 64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use alloy_primitives::{Address, Bloom, B256, U256};
    use revm::state::AccountInfo;
    use tempfile::TempDir;
    use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
    use torus_mempool::{Mempool, MempoolConfig};
    use torus_state::cf::{CF_BLOCK_BODIES, CF_BLOCK_HEADERS, CF_RECEIPTS};
    use torus_types::{Receipt, TorusBlockBody, TorusBlockHeader};

    fn test_header(height: u64, gas_used: u64, base_fee: u64) -> TorusBlockHeader {
        TorusBlockHeader {
            height,
            parent_hash: B256::ZERO,
            timestamp: 1_700_000_000 + height,
            proposer: Address::ZERO,
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: Bloom::ZERO,
            evm_gas_used: gas_used,
            evm_fee_revenue: 0,
            evm_gas_limit: 30_000_000,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: base_fee,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            sig_attestation: [0u8; 64],
        }
    }

    fn store_header(state: &StateDb, header: &TorusBlockHeader) -> B256 {
        // Match commit_block format: block_hash(32) || header_json.
        let json_bytes = serde_json::to_vec(header).unwrap();
        let hash = alloy_primitives::keccak256(&header.canonical_header_bytes());
        let mut data = Vec::with_capacity(32 + json_bytes.len());
        data.extend_from_slice(hash.as_slice());
        data.extend_from_slice(&json_bytes);
        state
            .put_cf_raw(CF_BLOCK_HEADERS, &header.height.to_be_bytes(), &data)
            .unwrap();
        state
            .put_cf_raw(
                torus_state::cf::CF_BLOCK_HASH_TO_NUMBER,
                hash.as_slice(),
                &header.height.to_be_bytes(),
            )
            .unwrap();
        hash
    }

    fn store_body(state: &StateDb, height: u64, body: &TorusBlockBody) {
        state
            .put_cf_raw(
                CF_BLOCK_BODIES,
                &height.to_be_bytes(),
                &serde_json::to_vec(body).unwrap(),
            )
            .unwrap();
    }

    fn store_receipt(state: &StateDb, height: u64, receipt: &Receipt) {
        let mut key = [0u8; 12];
        key[..8].copy_from_slice(&height.to_be_bytes());
        key[8..12].copy_from_slice(&receipt.tx_index.to_be_bytes());
        state
            .put_cf_raw(CF_RECEIPTS, &key, &serde_json::to_vec(receipt).unwrap())
            .unwrap();
    }

    fn setup() -> (TempDir, StateDb, Arc<Mempool>, Arc<EvmExecutor>) {
        let dir = TempDir::new().unwrap();
        let state = StateDb::open(dir.path()).unwrap();
        (
            dir,
            state.clone(),
            Arc::new(Mempool::new(state.clone(), MempoolConfig::default())),
            Arc::new(EvmExecutor::new(TORUS_CHAIN_ID)),
        )
    }

    #[test]
    fn find_latest_height_skips_prune_meta_key() {
        let dir = TempDir::new().unwrap();
        let state = StateDb::open(dir.path()).unwrap();
        for h in 1u64..=5 {
            state
                .put_cf_raw(CF_BLOCK_HEADERS, &h.to_be_bytes(), b"hdr")
                .unwrap();
        }
        // The pruner's meta key sorts AFTER all 8-byte big-endian height keys; a
        // pruned (or transplanted-from-pruned) DB must still report the real tip
        // (s334: eth_blockNumber froze at 0 and on_commit_block early-returned).
        state
            .put_cf_raw(
                CF_BLOCK_HEADERS,
                b"__prune_meta__\x00\x00",
                &9999u64.to_be_bytes(),
            )
            .unwrap();
        assert_eq!(find_latest_height(&state), 5);
    }

    async fn start_server(
        state: StateDb,
        mempool: Arc<Mempool>,
        executor: Arc<EvmExecutor>,
    ) -> (ServerHandle, SocketAddr) {
        RpcServer::new(
            state,
            mempool,
            executor,
            TORUS_CHAIN_ID,
            100,
            BlockNotifier::new(),
        )
        .start("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap()
    }

    #[test]
    fn hex_encoding_roundtrip() {
        assert_eq!(hex_u64(0), "0x0");
        assert_eq!(hex_u64(255), "0xff");
        assert_eq!(hex_u64(7777), "0x1e61");
        assert_eq!(
            U256::from(42u64),
            parse_u256(&hex_u256(U256::from(42u64))).unwrap()
        );
        assert_eq!(
            Address::from([0xab; 20]),
            parse_address(&hex_address(Address::from([0xab; 20]))).unwrap()
        );
    }

    #[test]
    fn block_tag_parsing() {
        assert_eq!(resolve_block_tag("latest", 100).unwrap(), 100);
        assert_eq!(resolve_block_tag("earliest", 100).unwrap(), 0);
        assert_eq!(resolve_block_tag("pending", 100).unwrap(), 100);
        assert_eq!(resolve_block_tag("0xa", 100).unwrap(), 10);
    }

    #[tokio::test]
    async fn submit_native_actions_batch_per_item_results() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool.clone(), executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let mk = |key_hex: &str, nonce: u64| {
            let key = k256::ecdsa::SigningKey::from_slice(&hex::decode(key_hex).unwrap()).unwrap();
            let signed = torus_types::eip712::sign_native_action(
                torus_types::NativeAction::ClaimRewards,
                nonce,
                &key,
            );
            format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()))
        };
        // Hardhat accounts 0 and 1 (the funded bench senders).
        let a = mk(
            "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            now_ms,
        );
        let b = mk(
            "59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
            now_ms + 1,
        );

        let results: Vec<RpcSubmitResult> = client
            .request(
                "torus_submitNativeActions",
                jsonrpsee::rpc_params![vec![a, b, "0xzz-not-hex".to_string()]],
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 3, "one result per submitted item, in order");
        assert!(results[0].hash.is_some() && results[0].error.is_none());
        assert!(results[1].hash.is_some() && results[1].error.is_none());
        assert!(results[2].hash.is_none() && results[2].error.is_some());
        assert_eq!(
            mempool.native_pool_size(),
            2,
            "only the valid actions admitted"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn submit_rejects_unknown_market_single_and_batch() {
        let (_dir, state, mempool, executor) = setup();
        // Seed market 1 (existence is what the guard reads; genesis writes
        // borsh StoredMarket bytes under the same 8-byte BE key).
        state
            .put_cf_raw(
                torus_state::cf::CF_NATIVE_MARKETS,
                &1u64.to_be_bytes(),
                b"seeded-market",
            )
            .unwrap();
        let (handle, addr) = start_server(state, mempool.clone(), executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let key = k256::ecdsa::SigningKey::from_slice(
            &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap(),
        )
        .unwrap();
        let params = |market_id: u64| torus_types::PlaceOrderParams {
            market_id,
            is_buy: true,
            price: torus_types::FixedPoint::from_raw(6_500_000_000_000),
            quantity: torus_types::FixedPoint::from_raw(10_000_000),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let mk = |action: torus_types::NativeAction, nonce: u64| {
            let signed = torus_types::eip712::sign_native_action(action, nonce, &key);
            format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()))
        };

        let good_single = mk(torus_types::NativeAction::PlaceOrder(params(1)), now_ms);
        let bad_single = mk(
            torus_types::NativeAction::PlaceOrder(params(99)),
            now_ms + 1,
        );
        let bad_batch = mk(
            torus_types::NativeAction::PlaceOrderBatch(vec![params(1), params(99)]),
            now_ms + 2,
        );

        // Batch pipeline: per-item errors, listed-market order admitted.
        let results: Vec<RpcSubmitResult> = client
            .request(
                "torus_submitNativeActions",
                jsonrpsee::rpc_params![vec![good_single, bad_single, bad_batch]],
            )
            .await
            .unwrap();
        assert!(results[0].hash.is_some() && results[0].error.is_none());
        assert!(results[1]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("unknown market_id 99"));
        assert!(results[2]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("unknown market_id 99"));
        assert_eq!(
            mempool.native_pool_size(),
            1,
            "only the listed-market order admitted"
        );

        // Single endpoint: call-level error.
        let err = client
            .request::<String, _>(
                "torus_submitNativeAction",
                jsonrpsee::rpc_params![mk(
                    torus_types::NativeAction::PlaceOrder(params(77)),
                    now_ms + 3
                )],
            )
            .await
            .expect_err("unknown market must be rejected on the single endpoint");
        assert!(err.to_string().contains("unknown market_id 77"));
        handle.stop().unwrap();
    }

    /// Option A (ingress-verify-fix): per-item result ORDER is pinned with
    /// failures interleaved at fixed indexes, so the parallel verify swap
    /// (rayon par_iter) can never silently reorder or misalign results.
    #[tokio::test]
    async fn submit_batch_order_preserved_with_interleaved_failures() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool.clone(), executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let mk = |key_hex: &str, nonce: u64| {
            let key = k256::ecdsa::SigningKey::from_slice(&hex::decode(key_hex).unwrap()).unwrap();
            let signed = torus_types::eip712::sign_native_action(
                torus_types::NativeAction::ClaimRewards,
                nonce,
                &key,
            );
            format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()))
        };
        const KEY_A: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
        const KEY_B: &str = "59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";

        // 12 items; indexes 2, 5, 8, 11 invalid; valid ones alternate A/B
        // senders with distinct nonces.
        let bad_indexes = [2usize, 5, 8, 11];
        let mut batch: Vec<String> = Vec::new();
        let mut n = 0u64;
        for i in 0..12usize {
            if bad_indexes.contains(&i) {
                batch.push(format!("0xzz-bad-{i}"));
            } else {
                let key = if n % 2 == 0 { KEY_A } else { KEY_B };
                batch.push(mk(key, now_ms + n));
                n += 1;
            }
        }

        let results: Vec<RpcSubmitResult> = client
            .request("torus_submitNativeActions", jsonrpsee::rpc_params![batch])
            .await
            .unwrap();
        assert_eq!(results.len(), 12, "one result per item, in order");
        for (i, r) in results.iter().enumerate() {
            if bad_indexes.contains(&i) {
                assert!(r.hash.is_none() && r.error.is_some(), "index {i} must fail");
            } else {
                assert!(
                    r.hash.is_some() && r.error.is_none(),
                    "index {i} must succeed"
                );
            }
        }
        assert_eq!(
            mempool.native_pool_size(),
            8,
            "exactly the 8 valid actions admitted"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn submit_records_phase_histograms() {
        let (_dir, state, mempool, executor) = setup();
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        let mut server = RpcServer::new(
            state,
            mempool,
            executor,
            TORUS_CHAIN_ID,
            100,
            BlockNotifier::new(),
        );
        server.set_metrics(metrics.clone());
        let (handle, addr) = server.start("127.0.0.1:0".parse().unwrap()).await.unwrap();
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let key = k256::ecdsa::SigningKey::from_slice(
            &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap(),
        )
        .unwrap();
        let signed = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            now_ms,
            &key,
        );
        let payload = format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()));

        let results: Vec<RpcSubmitResult> = client
            .request(
                "torus_submitNativeActions",
                jsonrpsee::rpc_params![vec![payload]],
            )
            .await
            .unwrap();
        assert!(results[0].hash.is_some());

        // One batch call = exactly one observation in each phase histogram.
        let text = metrics.encode();
        for name in [
            "torus_rpc_submit_permit_wait_seconds",
            "torus_rpc_submit_verify_seconds",
            "torus_rpc_submit_verify_cpu_seconds",
            "torus_rpc_submit_admit_seconds",
            "torus_rpc_submit_admit_insert_seconds",
            "torus_rpc_submit_admit_forward_seconds",
        ] {
            assert!(
                text.contains(&format!("{name}_count 1")),
                "{name} not observed exactly once; metrics dump:\n{text}"
            );
        }
        handle.stop().unwrap();
    }

    /// Sprint 5 Task 2 (instrumentation): per-phase cost of `verify_one_action`
    /// at bs 1/100/500. Prints µs per phase under --nocapture; asserts only
    /// correctness so timing noise can't flake CI.
    #[test]
    fn verify_breakdown_by_batch_size() {
        let (_dir, state, _mempool, _executor) = setup();
        // Market 1 must exist: verify_one_action_with now runs the O2
        // unknown-market ingress guard (validate_known_markets).
        state
            .put_cf_raw(
                torus_state::cf::CF_NATIVE_MARKETS,
                &1u64.to_be_bytes(),
                b"seeded-market",
            )
            .unwrap();
        let key = k256::ecdsa::SigningKey::from_slice(
            &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap(),
        )
        .unwrap();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        for n in [1usize, 100, 500] {
            let orders: Vec<torus_types::PlaceOrderParams> = (0..n)
                .map(|i| torus_types::PlaceOrderParams {
                    market_id: 1,
                    is_buy: i % 2 == 0,
                    price: torus_types::FixedPoint::from_raw(1_000_000_000 + i as i128),
                    quantity: torus_types::FixedPoint::from_raw(100_000_000),
                    order_type: torus_types::OrderType::Limit,
                    time_in_force: torus_types::TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: Some(i as u64),
                })
                .collect();
            let signed = torus_types::eip712::sign_native_action(
                torus_types::NativeAction::PlaceOrderBatch(orders),
                now_ms,
                &key,
            );
            let payload = format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()));

            let t = std::time::Instant::now();
            let bytes = crate::types::parse_bytes(&payload).unwrap();
            let d_hex = t.elapsed();

            let t = std::time::Instant::now();
            let action: torus_types::SignedNativeAction = serde_json::from_slice(&bytes).unwrap();
            let d_parse = t.elapsed();

            let t = std::time::Instant::now();
            action
                .validate_with_sessions(now_ms, TORUS_CHAIN_ID, |pk| {
                    state.get_session(pk).ok().flatten()
                })
                .unwrap();
            let d_sig = t.elapsed();

            let t = std::time::Instant::now();
            let canonical = serde_json::to_vec(&action).unwrap();
            let d_ser = t.elapsed();

            let t = std::time::Instant::now();
            let _ = alloy_primitives::keccak256(&canonical);
            let d_keccak = t.elapsed();

            let t = std::time::Instant::now();
            let (_, _, _, _hash) =
                crate::torus::verify_one_action(&payload, TORUS_CHAIN_ID, &state, now_ms).unwrap();
            let d_total = t.elapsed();

            println!(
                "bs={n:>4} bytes={:>7} | hex={:>6}µs parse={:>6}µs sig={:>6}µs ser={:>6}µs keccak={:>5}µs | verify_one_action total={:>6}µs",
                payload.len(),
                d_hex.as_micros(),
                d_parse.as_micros(),
                d_sig.as_micros(),
                d_ser.as_micros(),
                d_keccak.as_micros(),
                d_total.as_micros(),
            );
        }
    }

    /// Sprint 5 Task 3: admission rejects are counted by concrete reason so
    /// saturation regimes show WHICH limit fires (duplicate / pool_full / ...).
    #[tokio::test]
    async fn admit_rejects_counted_by_reason() {
        let (_dir, state, mempool, executor) = setup();
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        let mut server = RpcServer::new(
            state,
            mempool,
            executor,
            TORUS_CHAIN_ID,
            100,
            BlockNotifier::new(),
        );
        server.set_metrics(metrics.clone());
        let (handle, addr) = server.start("127.0.0.1:0".parse().unwrap()).await.unwrap();
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let key = k256::ecdsa::SigningKey::from_slice(
            &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap(),
        )
        .unwrap();
        let signed = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            now_ms,
            &key,
        );
        let payload = format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()));

        // Same action twice in one batch: first admits, second is a duplicate.
        let results: Vec<RpcSubmitResult> = client
            .request(
                "torus_submitNativeActions",
                jsonrpsee::rpc_params![vec![payload.clone(), payload]],
            )
            .await
            .unwrap();
        assert!(results[0].hash.is_some() && results[0].error.is_none());
        assert!(
            results[1]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("duplicate"),
            "second submit should be rejected as duplicate: {:?}",
            results[1]
        );

        let text = metrics.encode();
        assert!(
            text.contains(r#"torus_rpc_submit_admit_rejects_total{reason="duplicate"} 1"#),
            "duplicate reject not counted; metrics dump:\n{text}"
        );
        handle.stop().unwrap();
    }

    /// Sprint 5 Task 5: ingress format determinism — the action hash is
    /// keccak256 of the canonical serde_json bytes; a bincode round-trip must
    /// reproduce the exact same hash for every action shape (gates the
    /// torus_submitNativeActionsBin endpoint). Also asserts the wire size win.
    #[test]
    fn bincode_roundtrip_preserves_action_hash() {
        let key = k256::ecdsa::SigningKey::from_slice(
            &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap(),
        )
        .unwrap();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let order = |i: usize, coid: Option<u64>| torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy: i % 2 == 0,
            price: torus_types::FixedPoint::from_raw(1_000_000_000 + i as i128),
            quantity: torus_types::FixedPoint::from_raw(100_000_000),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: coid,
        };
        let cases: Vec<torus_types::NativeAction> = vec![
            torus_types::NativeAction::ClaimRewards,
            // Option::None exercises the human-readable/binary serde split.
            torus_types::NativeAction::PlaceOrder(order(0, None)),
            torus_types::NativeAction::PlaceOrderBatch(
                (0..500).map(|i| order(i, Some(i as u64))).collect(),
            ),
            torus_types::NativeAction::CancelOrder { order_id: 7 },
            torus_types::NativeAction::ModifyOrder {
                order_id: 9,
                new_price: Some(torus_types::FixedPoint::from_raw(2_000_000_000)),
                new_qty: None,
            },
        ];
        for action in cases {
            let label = format!("{action:?}");
            let signed = torus_types::eip712::sign_native_action(action, now_ms, &key);
            let json = serde_json::to_vec(&signed).unwrap();
            let json_hash = alloy_primitives::keccak256(&json);

            let wire = bincode::serialize(&signed).unwrap();
            let back: torus_types::SignedNativeAction = bincode::deserialize(&wire).unwrap();
            let bin_hash = alloy_primitives::keccak256(&serde_json::to_vec(&back).unwrap());

            assert_eq!(
                json_hash,
                bin_hash,
                "hash identity diverged after bincode round-trip: {}",
                &label[..label.len().min(60)]
            );
            assert!(
                wire.len() < json.len(),
                "bincode not smaller ({} >= {}): {}",
                wire.len(),
                json.len(),
                &label[..label.len().min(60)]
            );
        }
    }

    /// Sprint 5 Task 6: the bincode endpoint returns the same canonical-JSON
    /// keccak hash as the legacy path, fails per-item on undecodable payloads,
    /// and enforces the shared batch cap.
    #[tokio::test]
    async fn submit_native_actions_bin_endpoint() {
        let (_dir, state, mempool, executor) = setup();
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        let mut server = RpcServer::new(
            state,
            mempool.clone(),
            executor,
            TORUS_CHAIN_ID,
            100,
            BlockNotifier::new(),
        );
        server.set_metrics(metrics.clone());
        let (handle, addr) = server.start("127.0.0.1:0".parse().unwrap()).await.unwrap();
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let key = k256::ecdsa::SigningKey::from_slice(
            &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap(),
        )
        .unwrap();
        let signed = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            now_ms,
            &key,
        );
        let expected = alloy_primitives::keccak256(&serde_json::to_vec(&signed).unwrap());
        let bin_payload = format!("0x{}", hex::encode(bincode::serialize(&signed).unwrap()));

        let results: Vec<RpcSubmitResult> = client
            .request(
                "torus_submitNativeActionsBin",
                jsonrpsee::rpc_params![vec![bin_payload, "0xdeadbeef".to_string()]],
            )
            .await
            .unwrap();
        // Hash identity: bin ingress yields the canonical-JSON keccak.
        use std::str::FromStr;
        let got =
            alloy_primitives::B256::from_str(results[0].hash.as_ref().expect("admitted")).unwrap();
        assert_eq!(got, expected, "bin-path hash != canonical JSON hash");
        assert_eq!(mempool.native_pool_size(), 1, "valid bin action admitted");
        // Garbage bincode: per-item error, call still succeeds.
        assert!(results[1].hash.is_none() && results[1].error.is_some());
        // Batch cap applies to the bin endpoint too.
        let oversize: Vec<String> = (0..101).map(|_| "0x00".to_string()).collect();
        let over = client
            .request::<Vec<RpcSubmitResult>, _>(
                "torus_submitNativeActionsBin",
                jsonrpsee::rpc_params![oversize],
            )
            .await;
        assert!(over.is_err(), "oversize batch must be a call-level error");
        handle.stop().unwrap();
    }

    /// Sprint 5 Task 4: with the native pool at capacity, non-cancel actions
    /// are shed after a decode-only pass (no signature verification spent),
    /// while cancels still travel the full verify path so pool eviction
    /// semantics are preserved.
    #[tokio::test]
    async fn pool_full_sheds_non_cancels_before_verify() {
        let dir = TempDir::new().unwrap();
        let state = StateDb::open(dir.path()).unwrap();
        // Capacity 0: the pool is permanently "full" from the first submit.
        let cfg = MempoolConfig {
            native_pool_max_size: 0,
            ..Default::default()
        };
        let mempool = Arc::new(Mempool::new(state.clone(), cfg));
        let executor = Arc::new(EvmExecutor::new(TORUS_CHAIN_ID));
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        let mut server = RpcServer::new(
            state,
            mempool,
            executor,
            TORUS_CHAIN_ID,
            100,
            BlockNotifier::new(),
        );
        server.set_metrics(metrics.clone());
        let (handle, addr) = server.start("127.0.0.1:0".parse().unwrap()).await.unwrap();
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let key = k256::ecdsa::SigningKey::from_slice(
            &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap(),
        )
        .unwrap();
        let mk = |action: torus_types::NativeAction, nonce: u64| {
            let signed = torus_types::eip712::sign_native_action(action, nonce, &key);
            format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()))
        };
        let batch = vec![
            mk(torus_types::NativeAction::ClaimRewards, now_ms),
            mk(
                torus_types::NativeAction::CancelOrder { order_id: 7 },
                now_ms + 1,
            ),
        ];

        let results: Vec<RpcSubmitResult> = client
            .request("torus_submitNativeActions", jsonrpsee::rpc_params![batch])
            .await
            .unwrap();
        // Non-cancel: shed before signature verification.
        assert_eq!(
            results[0].error.as_deref(),
            Some("mempool: pool full (pre-verify)"),
            "non-cancel should be pre-verify shed: {:?}",
            results[0]
        );
        // Cancel: full verify path, rejected at admission (nothing to evict).
        assert!(
            results[1]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("native action pool full"),
            "cancel should reach real admission: {:?}",
            results[1]
        );

        let text = metrics.encode();
        assert!(
            text.contains(
                r#"torus_rpc_submit_admit_rejects_total{reason="pool_full_preverify"} 1"#
            ),
            "pre-verify shed not counted; dump:\n{text}"
        );
        assert!(
            text.contains(r#"torus_rpc_submit_admit_rejects_total{reason="pool_full"} 1"#),
            "cancel admission reject not counted; dump:\n{text}"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn leader_forward_gated_by_forward_bodies() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let mk_payload = |nonce: u64| {
            let key = k256::ecdsa::SigningKey::from_slice(
                &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                    .unwrap(),
            )
            .unwrap();
            let signed = torus_types::eip712::sign_native_action(
                torus_types::NativeAction::ClaimRewards,
                nonce,
                &key,
            );
            format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()))
        };
        let own = [1u8; 32];
        let leader = [2u8; 32];

        // forward_bodies=false (gossip carries bodies): fwd channel stays empty.
        let (_dir, state, mempool, executor) = setup();
        let mut server = RpcServer::new(
            state,
            mempool,
            executor,
            TORUS_CHAIN_ID,
            100,
            BlockNotifier::new(),
        );
        let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (evm_fwd_tx, _evm_fwd_rx) = tokio::sync::mpsc::unbounded_channel();
        server.set_leader_forwarding(
            own,
            Arc::new(move || Some(leader)),
            fwd_tx,
            evm_fwd_tx,
            false,
        );
        let (handle, addr) = server.start("127.0.0.1:0".parse().unwrap()).await.unwrap();
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let results: Vec<RpcSubmitResult> = client
            .request(
                "torus_submitNativeActions",
                jsonrpsee::rpc_params![vec![mk_payload(now_ms)]],
            )
            .await
            .unwrap();
        assert!(results[0].hash.is_some());
        assert!(
            fwd_rx.try_recv().is_err(),
            "no full-body leader forward when gossip pre-spread owns body delivery"
        );
        handle.stop().unwrap();

        // forward_bodies=true (gossip off): today's full-body forward, unchanged.
        let (_dir2, state2, mempool2, executor2) = setup();
        let mut server2 = RpcServer::new(
            state2,
            mempool2,
            executor2,
            TORUS_CHAIN_ID,
            100,
            BlockNotifier::new(),
        );
        let (fwd_tx2, mut fwd_rx2) = tokio::sync::mpsc::unbounded_channel();
        let (evm_fwd_tx2, _evm_fwd_rx2) = tokio::sync::mpsc::unbounded_channel();
        server2.set_leader_forwarding(
            own,
            Arc::new(move || Some(leader)),
            fwd_tx2,
            evm_fwd_tx2,
            true,
        );
        let (handle2, addr2) = server2.start("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let client2 = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr2}"))
            .unwrap();
        let results2: Vec<RpcSubmitResult> = client2
            .request(
                "torus_submitNativeActions",
                jsonrpsee::rpc_params![vec![mk_payload(now_ms + 1)]],
            )
            .await
            .unwrap();
        assert!(results2[0].hash.is_some());
        let (vk, payload) = fwd_rx2
            .try_recv()
            .expect("full-body forward must still flow in fallback mode");
        assert_eq!(vk, leader);
        assert!(
            payload.len() > 20,
            "payload is 20-byte sender prefix + action bytes"
        );
        handle2.stop().unwrap();
    }

    /// Option B (EVM tx dissemination): an EVM tx submitted to a NON-leader node must be
    /// forwarded to the current leader UNCONDITIONALLY — unlike native actions, EVM has no
    /// gossip pre-spread, so the `forward_bodies` gate does NOT apply. When this node IS the
    /// leader, nothing is forwarded (it proposes the tx itself). RED before
    /// `forward_evm_to_leader` + the `send_raw_transaction` call site exist.
    #[tokio::test]
    async fn evm_tx_forwarded_to_leader_unconditionally() {
        use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
        use alloy_primitives::{Bytes, Signature as AlloySig, TxKind};
        use alloy_rlp::Encodable;
        use jsonrpsee::core::client::ClientT;

        fn evm_addr(key: &k256::ecdsa::SigningKey) -> Address {
            let pt = key.verifying_key().to_encoded_point(false);
            Address::from_slice(&alloy_primitives::keccak256(&pt.as_bytes()[1..]).as_slice()[12..])
        }
        fn signed_eip1559(key: &k256::ecdsa::SigningKey, nonce: u64) -> Vec<u8> {
            let tx = TxEip1559 {
                chain_id: TORUS_CHAIN_ID,
                nonce,
                max_fee_per_gas: 1_000_000_000,
                max_priority_fee_per_gas: 1_000_000_000,
                gas_limit: 21_000,
                to: TxKind::Call(Address::ZERO),
                value: U256::from(1u64),
                input: Bytes::new(),
                access_list: Default::default(),
            };
            let sig_hash = tx.signature_hash();
            let (sig, recid) = key.sign_prehash_recoverable(sig_hash.as_slice()).unwrap();
            let r = U256::from_be_slice(sig.r().to_bytes().as_slice());
            let s = U256::from_be_slice(sig.s().to_bytes().as_slice());
            let signature = AlloySig::new(r, s, recid.is_y_odd());
            let mut buf = Vec::new();
            TxEnvelope::Eip1559(tx.into_signed(signature)).encode(&mut buf);
            buf
        }
        let fund = |state: &StateDb, addr: &Address| {
            state
                .put_account(
                    addr,
                    &AccountInfo {
                        balance: U256::from(10u128.pow(18)),
                        nonce: 0,
                        code_hash: B256::ZERO,
                        code: None,
                        account_id: None,
                    },
                )
                .unwrap();
        };

        let key = k256::ecdsa::SigningKey::from_slice(
            &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap(),
        )
        .unwrap();
        let sender = evm_addr(&key);
        let own = [1u8; 32];
        let leader = [2u8; 32];

        // --- Node is NOT the leader: EVM tx must be forwarded even with forward_bodies=false. ---
        let (_dir, state, mempool, executor) = setup();
        fund(&state, &sender);
        let raw = signed_eip1559(&key, 0);
        let mut server = RpcServer::new(
            state,
            mempool,
            executor,
            TORUS_CHAIN_ID,
            100,
            BlockNotifier::new(),
        );
        let (fwd_tx, _fwd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (evm_fwd_tx, mut evm_fwd_rx) = tokio::sync::mpsc::unbounded_channel();
        server.set_leader_forwarding(
            own,
            Arc::new(move || Some(leader)),
            fwd_tx,
            evm_fwd_tx,
            false,
        );
        let (handle, addr) = server.start("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let hash: String = client
            .request(
                "eth_sendRawTransaction",
                jsonrpsee::rpc_params![format!("0x{}", hex::encode(&raw))],
            )
            .await
            .unwrap();
        assert!(hash.starts_with("0x"));
        let (vk, payload) = evm_fwd_rx
            .try_recv()
            .expect("EVM tx must be forwarded to the leader even with forward_bodies=false");
        assert_eq!(vk, leader, "forwarded to the current leader");
        assert_eq!(
            payload, raw,
            "payload is the raw RLP, unmodified (no sender prefix)"
        );
        handle.stop().unwrap();

        // --- Node IS the leader: nothing forwarded (it proposes the tx itself). ---
        let (_dir2, state2, mempool2, executor2) = setup();
        fund(&state2, &sender);
        let raw2 = signed_eip1559(&key, 0);
        let mut server2 = RpcServer::new(
            state2,
            mempool2,
            executor2,
            TORUS_CHAIN_ID,
            100,
            BlockNotifier::new(),
        );
        let (fwd_tx2, _fwd_rx2) = tokio::sync::mpsc::unbounded_channel();
        let (evm_fwd_tx2, mut evm_fwd_rx2) = tokio::sync::mpsc::unbounded_channel();
        server2.set_leader_forwarding(
            own,
            Arc::new(move || Some(own)),
            fwd_tx2,
            evm_fwd_tx2,
            false,
        );
        let (handle2, addr2) = server2.start("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let client2 = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr2}"))
            .unwrap();
        let _: String = client2
            .request(
                "eth_sendRawTransaction",
                jsonrpsee::rpc_params![format!("0x{}", hex::encode(&raw2))],
            )
            .await
            .unwrap();
        assert!(
            evm_fwd_rx2.try_recv().is_err(),
            "no forward when this node is itself the leader"
        );
        handle2.stop().unwrap();
    }

    #[tokio::test]
    async fn web3_client_version() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        assert_eq!(
            client
                .request::<String, _>("web3_clientVersion", jsonrpsee::rpc_params![])
                .await
                .unwrap(),
            "torus/v0.1.0"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_chain_id() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        assert_eq!(
            client
                .request::<String, _>("eth_chainId", jsonrpsee::rpc_params![])
                .await
                .unwrap(),
            "0x1e62"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_block_number() {
        let (_dir, state, mempool, executor) = setup();
        for i in 0..5u64 {
            store_header(&state, &test_header(i, 0, 1_000_000_000));
        }
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        assert_eq!(
            client
                .request::<String, _>("eth_blockNumber", jsonrpsee::rpc_params![])
                .await
                .unwrap(),
            "0x4"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_balance() {
        let (_dir, state, mempool, executor) = setup();
        let addr_val = Address::from([0x11; 20]);
        let balance = U256::from(1_000_000_000_000_000_000u128);
        state
            .put_account(
                &addr_val,
                &AccountInfo {
                    balance,
                    nonce: 5,
                    code_hash: B256::ZERO,
                    code: None,
                    account_id: None,
                },
            )
            .unwrap();
        store_header(&state, &test_header(0, 0, 0));
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        assert_eq!(
            parse_u256(
                &client
                    .request::<String, _>(
                        "eth_getBalance",
                        jsonrpsee::rpc_params![hex_address(addr_val), "latest"]
                    )
                    .await
                    .unwrap()
            )
            .unwrap(),
            balance
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_block_by_number() {
        let (_dir, state, mempool, executor) = setup();
        let block_hash = store_header(&state, &test_header(0, 21000, 1_000_000_000));
        store_body(
            &state,
            0,
            &TorusBlockBody {
                native_actions: vec![],
                evm_transactions: vec![],
                core_writer_actions: vec![],
            },
        );
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let block: RpcBlock = client
            .request::<Option<RpcBlock>, _>(
                "eth_getBlockByNumber",
                jsonrpsee::rpc_params!["0x0", false],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(block.number, "0x0");
        assert_eq!(block.hash, hex_b256(block_hash));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_transaction_receipt() {
        let (_dir, state, mempool, executor) = setup();
        store_header(
            &state,
            &TorusBlockHeader {
                evm_tx_count: 1,
                ..test_header(0, 21000, 1_000_000_000)
            },
        );
        let tx_hash = B256::from([0xaa; 32]);
        store_receipt(
            &state,
            0,
            &Receipt {
                tx_hash,
                block_number: 0,
                block_hash: B256::ZERO,
                tx_index: 0,
                cumulative_gas_used: 21000,
                gas_used: 21000,
                contract_address: None,
                logs: vec![],
                logs_bloom: Bloom::ZERO,
                status: true,
                effective_gas_price: 1_000_000_000,
            },
        );
        state
            .put_cf_raw(
                torus_state::cf::CF_TX_HASH_TO_LOCATION,
                tx_hash.as_slice(),
                &[0u8; 12],
            )
            .unwrap();
        store_body(
            &state,
            0,
            &TorusBlockBody {
                native_actions: vec![],
                evm_transactions: vec![],
                core_writer_actions: vec![],
            },
        );
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let r: RpcReceipt = client
            .request::<Option<RpcReceipt>, _>(
                "eth_getTransactionReceipt",
                jsonrpsee::rpc_params![hex_b256(tx_hash)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.transaction_hash, hex_b256(tx_hash));
        assert_eq!(r.status, "0x1");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_call_simple_transfer() {
        let (_dir, state, mempool, executor) = setup();
        let sender = Address::from([0x11; 20]);
        state
            .put_account(
                &sender,
                &AccountInfo {
                    balance: U256::from(10u64.pow(18)),
                    nonce: 0,
                    code_hash: B256::ZERO,
                    code: None,
                    account_id: None,
                },
            )
            .unwrap();
        store_header(&state, &test_header(0, 0, 0));
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let call = serde_json::json!({ "from": hex_address(sender), "to": hex_address(Address::from([0x22; 20])), "value": "0x0" });
        assert_eq!(
            client
                .request::<String, _>("eth_call", jsonrpsee::rpc_params![call, "latest"])
                .await
                .unwrap(),
            "0x"
        );
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_logs_with_filter() {
        let (_dir, state, mempool, executor) = setup();
        let log_addr = Address::from([0x33; 20]);
        let topic0 = B256::from([0x44; 32]);
        store_header(
            &state,
            &TorusBlockHeader {
                evm_tx_count: 1,
                ..test_header(0, 21000, 1_000_000_000)
            },
        );
        store_receipt(
            &state,
            0,
            &Receipt {
                tx_hash: B256::from([0x55; 32]),
                block_number: 0,
                block_hash: B256::ZERO,
                tx_index: 0,
                cumulative_gas_used: 21000,
                gas_used: 21000,
                contract_address: None,
                logs: vec![torus_types::Log {
                    address: log_addr,
                    topics: vec![topic0],
                    data: vec![1, 2, 3],
                }],
                logs_bloom: Bloom::ZERO,
                status: true,
                effective_gas_price: 1_000_000_000,
            },
        );
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let filter = serde_json::json!({ "fromBlock": "0x0", "toBlock": "0x0", "address": hex_address(log_addr), "topics": [hex_b256(topic0)] });
        let logs: Vec<RpcLog> = client
            .request("eth_getLogs", jsonrpsee::rpc_params![filter])
            .await
            .unwrap();
        assert_eq!(logs.len(), 1);
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_fee_history() {
        let (_dir, state, mempool, executor) = setup();
        for i in 0..5u64 {
            store_header(&state, &test_header(i, i * 1000, 1_000_000_000 + i));
        }
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let result: FeeHistory = client
            .request(
                "eth_feeHistory",
                jsonrpsee::rpc_params!["0x3", "0x4", [25.0, 75.0]],
            )
            .await
            .unwrap();
        assert_eq!(result.oldest_block, hex_u64(2));
        assert_eq!(result.gas_used_ratio.len(), 3);
        assert_eq!(result.base_fee_per_gas.len(), 4);
        handle.stop().unwrap();
    }

    // ========================================================================
    // Torus namespace tests (2.9.1 + 2.9.2)
    // ========================================================================

    use borsh::BorshSerialize;
    use torus_core::position::{MarginType, NativeBalance, Position, PositionManager};
    use torus_core::precompiles::{write_order_book_snapshot, OrderBookSnapshot, PriceLevel};
    use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_TRADES};
    use torus_types::FixedPoint;

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    fn store_market(state: &StateDb, market_id: u64, base: &str, quote: &str) {
        let mut data = Vec::new();
        BorshSerialize::serialize(&base.to_string(), &mut data).unwrap();
        BorshSerialize::serialize(&quote.to_string(), &mut data).unwrap();
        let lot_raw: i128 = FixedPoint::SCALE; // 1.0
        let tick_raw: i128 = FixedPoint::SCALE / 100; // 0.01
        let margin_raw: i128 = FixedPoint::SCALE / 10; // 0.1
        BorshSerialize::serialize(&lot_raw, &mut data).unwrap();
        BorshSerialize::serialize(&tick_raw, &mut data).unwrap();
        BorshSerialize::serialize(&margin_raw, &mut data).unwrap();
        state
            .put_cf_raw(CF_NATIVE_MARKETS, &market_id.to_be_bytes(), &data)
            .unwrap();
    }

    fn store_trade(
        state: &StateDb,
        market_id: u64,
        trade_id: u128,
        price_raw: i128,
        qty_raw: i128,
        side: u8,
        block: u64,
        ts: u64,
        index: u32,
    ) {
        let mut key = Vec::with_capacity(20);
        key.extend_from_slice(&market_id.to_be_bytes());
        key.extend_from_slice(&block.to_be_bytes());
        key.extend_from_slice(&index.to_be_bytes());

        let mut data = Vec::new();
        BorshSerialize::serialize(&trade_id, &mut data).unwrap();
        BorshSerialize::serialize(&price_raw, &mut data).unwrap();
        BorshSerialize::serialize(&qty_raw, &mut data).unwrap();
        BorshSerialize::serialize(&side, &mut data).unwrap();
        BorshSerialize::serialize(&block, &mut data).unwrap();
        BorshSerialize::serialize(&ts, &mut data).unwrap();
        state.put_cf_raw(CF_NATIVE_TRADES, &key, &data).unwrap();
    }

    #[tokio::test]
    async fn torus_get_order_book_with_orders() {
        let (_dir, state, mempool, executor) = setup();
        let snapshot = OrderBookSnapshot {
            bids: vec![
                PriceLevel {
                    price: fp(50000),
                    quantity: fp(10),
                },
                PriceLevel {
                    price: fp(49900),
                    quantity: fp(5),
                },
            ],
            asks: vec![
                PriceLevel {
                    price: fp(50100),
                    quantity: fp(8),
                },
                PriceLevel {
                    price: fp(50200),
                    quantity: fp(3),
                },
            ],
        };
        write_order_book_snapshot(&state, 1, &snapshot).unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let book: RpcOrderBook = client
            .request("torus_getOrderBook", jsonrpsee::rpc_params!["0x1"])
            .await
            .unwrap();
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.asks.len(), 2);
        assert_eq!(book.bids[0].price, hex_fp(fp(50000)));
        assert_eq!(book.asks[0].price, hex_fp(fp(50100)));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_order_book_empty() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let book: RpcOrderBook = client
            .request("torus_getOrderBook", jsonrpsee::rpc_params!["0x99"])
            .await
            .unwrap();
        assert!(book.bids.is_empty());
        assert!(book.asks.is_empty());
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_position_open() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x11; 20]);
        let pm = PositionManager::new(state.clone());
        pm.put_position(&Position {
            trader,
            market_id: 1,
            is_long: true,
            size: fp(5),
            entry_price: fp(50000),
            realized_pnl: fp(100),
            isolated_margin: fp(2500),
            margin_type: MarginType::Isolated,
        })
        .unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let pos: Option<RpcPosition> = client
            .request(
                "torus_getPosition",
                jsonrpsee::rpc_params![hex_address(trader), "0x1"],
            )
            .await
            .unwrap();
        let pos = pos.unwrap();
        assert_eq!(pos.side, "long");
        assert_eq!(pos.size, hex_fp(fp(5)));
        assert_eq!(pos.entry_price, hex_fp(fp(50000)));
        assert_eq!(pos.realized_pnl, hex_fp(fp(100)));
        assert_eq!(pos.margin_mode, "isolated");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_position_none() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x22; 20]);
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let pos: Option<RpcPosition> = client
            .request(
                "torus_getPosition",
                jsonrpsee::rpc_params![hex_address(trader), "0x1"],
            )
            .await
            .unwrap();
        assert!(pos.is_none());
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_balances_basic() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x33; 20]);
        // Set native balance
        let pm = PositionManager::new(state.clone());
        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(10000),
                order_margin: fp(500),
            },
        )
        .unwrap();
        // Set EVM balance
        state
            .put_account(
                &trader,
                &AccountInfo {
                    balance: U256::from(2_000_000_000_000_000_000u128),
                    nonce: 0,
                    code_hash: B256::ZERO,
                    code: None,
                    account_id: None,
                },
            )
            .unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let bal: RpcBalances = client
            .request(
                "torus_getBalances",
                jsonrpsee::rpc_params![hex_address(trader)],
            )
            .await
            .unwrap();
        // native_balance = available + order_margin = 10500
        assert_eq!(bal.native_balance, hex_fp(fp(10000) + fp(500)));
        assert_eq!(
            bal.evm_balance,
            hex_u256(U256::from(2_000_000_000_000_000_000u128))
        );
        // total_margin_used = order_margin (no positions)
        assert_eq!(bal.total_margin_used, hex_fp(fp(500)));
        assert_eq!(bal.available_balance, hex_fp(fp(10000)));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_balances_with_margin() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x44; 20]);
        let pm = PositionManager::new(state.clone());
        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(8000),
                order_margin: fp(1000),
            },
        )
        .unwrap();
        // Open position with isolated margin
        pm.put_position(&Position {
            trader,
            market_id: 1,
            is_long: true,
            size: fp(2),
            entry_price: fp(50000),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: fp(500),
            margin_type: MarginType::Isolated,
        })
        .unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let bal: RpcBalances = client
            .request(
                "torus_getBalances",
                jsonrpsee::rpc_params![hex_address(trader)],
            )
            .await
            .unwrap();
        // total_margin_used = order_margin(1000) + isolated(500) = 1500
        assert_eq!(bal.total_margin_used, hex_fp(fp(1500)));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_markets() {
        let (_dir, state, mempool, executor) = setup();
        store_market(&state, 1, "BTC", "USD");
        store_market(&state, 2, "ETH", "USD");
        store_market(&state, 3, "SOL", "USD");
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let markets: Vec<RpcMarketInfo> = client
            .request("torus_getMarkets", jsonrpsee::rpc_params![])
            .await
            .unwrap();
        assert_eq!(markets.len(), 3);
        assert_eq!(markets[0].base_asset, "BTC");
        assert_eq!(markets[1].base_asset, "ETH");
        assert_eq!(markets[2].base_asset, "SOL");
        assert_eq!(markets[0].status, "active");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_trade_history() {
        let (_dir, state, mempool, executor) = setup();
        let price = 50000i64 as i128 * FixedPoint::SCALE;
        let qty = 1i128 * FixedPoint::SCALE;
        store_trade(&state, 1, 100, price, qty, 0, 10, 1700000010, 0);
        store_trade(
            &state,
            1,
            101,
            price + FixedPoint::SCALE,
            qty,
            1,
            11,
            1700000011,
            0,
        );
        store_trade(
            &state,
            1,
            102,
            price - FixedPoint::SCALE,
            qty,
            0,
            12,
            1700000012,
            0,
        );
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let trades: Vec<RpcTrade> = client
            .request(
                "torus_getTradeHistory",
                jsonrpsee::rpc_params!["0x1", 100u32],
            )
            .await
            .unwrap();
        assert_eq!(trades.len(), 3);
        // Most recent first
        assert_eq!(trades[0].trade_id, hex_u128(102));
        assert_eq!(trades[1].trade_id, hex_u128(101));
        assert_eq!(trades[2].trade_id, hex_u128(100));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_trade_history_with_limit() {
        let (_dir, state, mempool, executor) = setup();
        let price = 50000i64 as i128 * FixedPoint::SCALE;
        let qty = 1i128 * FixedPoint::SCALE;
        for i in 0..5u32 {
            store_trade(
                &state,
                1,
                i as u128,
                price,
                qty,
                0,
                i as u64,
                1700000000 + i as u64,
                0,
            );
        }
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let trades: Vec<RpcTrade> = client
            .request("torus_getTradeHistory", jsonrpsee::rpc_params!["0x1", 2u32])
            .await
            .unwrap();
        assert_eq!(trades.len(), 2);
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_invalid_market_id() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let result = client
            .request::<RpcOrderBook, _>("torus_getOrderBook", jsonrpsee::rpc_params!["not_hex"])
            .await;
        assert!(result.is_err());
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_invalid_trader_address() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let result = client
            .request::<Option<RpcPosition>, _>(
                "torus_getPosition",
                jsonrpsee::rpc_params!["invalid_addr", "0x1"],
            )
            .await;
        assert!(result.is_err());
        handle.stop().unwrap();
    }

    // ========================================================================
    // Trading app RPC tests
    // ========================================================================

    use torus_core::order_book::OrderBook;
    use torus_state::cf::{CF_NATIVE_ORDER_BOOKS, CF_NATIVE_USER_TRADES};
    use torus_types::{OrderType, PlaceOrderParams, Side, TimeInForce};

    fn store_order_book_with_orders(
        state: &StateDb,
        market_id: u64,
        orders: &[(Address, Side, i64, i64)],
    ) {
        let mut book = OrderBook::new(market_id, fp(1), fp(1));
        for (trader, side, price, qty) in orders {
            book.place_order(
                PlaceOrderParams {
                    market_id,
                    is_buy: *side == Side::Buy,
                    price: fp(*price),
                    quantity: fp(*qty),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: None,
                },
                *trader,
                1700000000,
            );
        }
        let data = borsh::to_vec(&book).unwrap();
        state
            .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &market_id.to_be_bytes(), &data)
            .unwrap();
    }

    fn store_user_trade(
        state: &StateDb,
        trader: &Address,
        trade_id: u128,
        market_id: u64,
        price_raw: i128,
        qty_raw: i128,
        side: u8,
        role: u8,
        block: u64,
        ts: u64,
        index: u32,
    ) {
        let desc_block = u64::MAX - block;
        let mut key = [0u8; 32];
        key[..20].copy_from_slice(trader.as_slice());
        key[20..28].copy_from_slice(&desc_block.to_be_bytes());
        key[28..32].copy_from_slice(&index.to_be_bytes());
        let mut data = Vec::with_capacity(74);
        data.extend_from_slice(&trade_id.to_le_bytes());
        data.extend_from_slice(&market_id.to_le_bytes());
        data.extend_from_slice(&price_raw.to_le_bytes());
        data.extend_from_slice(&qty_raw.to_le_bytes());
        data.push(side);
        data.push(role);
        data.extend_from_slice(&block.to_le_bytes());
        data.extend_from_slice(&ts.to_le_bytes());
        state
            .put_cf_raw(CF_NATIVE_USER_TRADES, &key, &data)
            .unwrap();
    }

    #[tokio::test]
    async fn torus_get_open_orders_empty() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let orders: Vec<RpcOpenOrder> = client
            .request(
                "torus_getOpenOrders",
                jsonrpsee::rpc_params![hex_address(Address::from([0xAA; 20]))],
            )
            .await
            .unwrap();
        assert!(orders.is_empty());
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_open_orders_single_market() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0xBB; 20]);
        store_order_book_with_orders(
            &state,
            1,
            &[
                (trader, Side::Buy, 49000, 2),
                (trader, Side::Buy, 48000, 3),
                (trader, Side::Sell, 51000, 1),
            ],
        );
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let orders: Vec<RpcOpenOrder> = client
            .request(
                "torus_getOpenOrders",
                jsonrpsee::rpc_params![hex_address(trader), "0x1"],
            )
            .await
            .unwrap();
        assert_eq!(orders.len(), 3);
        assert_eq!(orders[0].order_type, "limit");
        assert_eq!(orders[0].time_in_force, "gtc");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_open_orders_filter_by_market() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0xCC; 20]);
        store_order_book_with_orders(&state, 1, &[(trader, Side::Buy, 49000, 2)]);
        store_order_book_with_orders(
            &state,
            2,
            &[(trader, Side::Buy, 2900, 5), (trader, Side::Sell, 3100, 3)],
        );
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let orders: Vec<RpcOpenOrder> = client
            .request(
                "torus_getOpenOrders",
                jsonrpsee::rpc_params![hex_address(trader), "0x2"],
            )
            .await
            .unwrap();
        assert_eq!(orders.len(), 2);
        let all: Vec<RpcOpenOrder> = client
            .request(
                "torus_getOpenOrders",
                jsonrpsee::rpc_params![hex_address(trader)],
            )
            .await
            .unwrap();
        assert_eq!(all.len(), 3);
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_open_interest_basic() {
        let (_dir, state, mempool, executor) = setup();
        let pm = PositionManager::new(state.clone());
        pm.put_position(&Position {
            trader: Address::from([0x11; 20]),
            market_id: 1,
            is_long: true,
            size: fp(10),
            entry_price: fp(50000),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: fp(5000),
            margin_type: MarginType::Cross,
        })
        .unwrap();
        pm.put_position(&Position {
            trader: Address::from([0x22; 20]),
            market_id: 1,
            is_long: false,
            size: fp(7),
            entry_price: fp(50000),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: fp(3500),
            margin_type: MarginType::Cross,
        })
        .unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let oi: RpcOpenInterest = client
            .request("torus_getOpenInterest", jsonrpsee::rpc_params!["0x1"])
            .await
            .unwrap();
        assert_eq!(oi.long_oi, hex_fp(fp(10)));
        assert_eq!(oi.short_oi, hex_fp(fp(7)));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_mark_price_from_order_book() {
        let (_dir, state, mempool, executor) = setup();
        let mut book = OrderBook::new(1, fp(1), fp(1));
        book.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(50000),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            Address::from([0x11; 20]),
            1700000000,
        );
        book.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy: false,
                price: fp(50000),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            Address::from([0x22; 20]),
            1700000001,
        );
        state
            .put_cf_raw(
                CF_NATIVE_ORDER_BOOKS,
                &1u64.to_be_bytes(),
                &borsh::to_vec(&book).unwrap(),
            )
            .unwrap();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let mp: RpcMarkPrice = client
            .request("torus_getMarkPrice", jsonrpsee::rpc_params!["0x1"])
            .await
            .unwrap();
        assert_eq!(mp.last_trade_price, hex_fp(fp(50000)));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_user_trades_basic() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0xDD; 20]);
        let price = 50000i64 as i128 * FixedPoint::SCALE;
        let qty = 1i128 * FixedPoint::SCALE;
        store_user_trade(&state, &trader, 1, 1, price, qty, 0, 1, 10, 1700000010, 0);
        store_user_trade(&state, &trader, 2, 1, price, qty, 1, 0, 11, 1700000011, 0);
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let trades: Vec<RpcUserTrade> = client
            .request(
                "torus_getUserTrades",
                jsonrpsee::rpc_params![hex_address(trader)],
            )
            .await
            .unwrap();
        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].trade_id, hex_u128(2));
        assert_eq!(trades[1].trade_id, hex_u128(1));
        assert_eq!(trades[0].role, "maker");
        assert_eq!(trades[1].role, "taker");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn torus_get_user_trades_filter_market() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0xEE; 20]);
        let price = 50000i64 as i128 * FixedPoint::SCALE;
        let qty = 1i128 * FixedPoint::SCALE;
        store_user_trade(&state, &trader, 1, 1, price, qty, 0, 1, 10, 1700000010, 0);
        store_user_trade(&state, &trader, 2, 2, price, qty, 0, 1, 11, 1700000011, 0);
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let trades: Vec<RpcUserTrade> = client
            .request(
                "torus_getUserTrades",
                jsonrpsee::rpc_params![hex_address(trader), "0x1"],
            )
            .await
            .unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].market_id, hex_u64(1));
        handle.stop().unwrap();
    }
    // ========================================================================
    // 7 pending trading RPC tests (from writing-plan-trading-rpc-gaps.md)
    // ========================================================================

    // --- get_open_orders_multi_market ---
    // Unfiltered query returns orders spanning multiple markets.
    #[tokio::test]
    async fn get_open_orders_multi_market() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x11; 20]);
        // Place orders in three distinct markets.
        store_order_book_with_orders(&state, 1, &[(trader, Side::Buy, 49000, 2)]);
        store_order_book_with_orders(&state, 2, &[(trader, Side::Sell, 3100, 1)]);
        store_order_book_with_orders(
            &state,
            3,
            &[(trader, Side::Buy, 1900, 5), (trader, Side::Sell, 2100, 3)],
        );
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        // No market_id param → all markets.
        let orders: Vec<RpcOpenOrder> = client
            .request(
                "torus_getOpenOrders",
                jsonrpsee::rpc_params![hex_address(trader)],
            )
            .await
            .unwrap();
        assert_eq!(orders.len(), 4);
        // All must belong to the requesting trader; market IDs should span 1, 2, and 3.
        let mut market_ids: Vec<u64> = orders
            .iter()
            .map(|o| u64::from_str_radix(o.market_id.strip_prefix("0x").unwrap(), 16).unwrap())
            .collect();
        market_ids.sort_unstable();
        market_ids.dedup();
        assert_eq!(market_ids, vec![1, 2, 3]);
        handle.stop().unwrap();
    }

    // --- get_open_orders_after_cancel ---
    // A cancelled order must not appear in subsequent getOpenOrders responses.
    #[tokio::test]
    async fn get_open_orders_after_cancel() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x22; 20]);
        // Build a book with two buy orders.
        let mut book = OrderBook::new(1, fp(1), fp(1));
        let r1 = book.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(48000),
                quantity: fp(2),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            trader,
            1700000000,
        );
        book.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(47000),
                quantity: fp(3),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            trader,
            1700000001,
        );
        // Cancel the first order before persisting.
        book.cancel_order(r1.order_id).unwrap();
        let data = borsh::to_vec(&book).unwrap();
        state
            .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &1u64.to_be_bytes(), &data)
            .unwrap();

        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let orders: Vec<RpcOpenOrder> = client
            .request(
                "torus_getOpenOrders",
                jsonrpsee::rpc_params![hex_address(trader), "0x1"],
            )
            .await
            .unwrap();
        // Only the second order should remain.
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].price, hex_fp(fp(47000)));
        handle.stop().unwrap();
    }

    // --- get_open_orders_after_fill ---
    // A fully-filled order must not appear in getOpenOrders results.
    #[tokio::test]
    async fn get_open_orders_after_fill() {
        let (_dir, state, mempool, executor) = setup();
        let maker = Address::from([0x33; 20]);
        let taker = Address::from([0x44; 20]);
        // Build book: maker places a sell resting on the book first.
        let mut book = OrderBook::new(1, fp(1), fp(1));
        book.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy: false,
                price: fp(50000),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            maker,
            1700000000,
        );
        // Taker buy at the same price fully fills the maker's sell.
        book.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(50000),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            taker,
            1700000001,
        );
        // Persist the post-fill book state.
        let data = borsh::to_vec(&book).unwrap();
        state
            .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &1u64.to_be_bytes(), &data)
            .unwrap();

        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        // Neither party should have open orders after the complete fill.
        let maker_orders: Vec<RpcOpenOrder> = client
            .request(
                "torus_getOpenOrders",
                jsonrpsee::rpc_params![hex_address(maker), "0x1"],
            )
            .await
            .unwrap();
        assert!(
            maker_orders.is_empty(),
            "maker's filled sell should be gone"
        );
        let taker_orders: Vec<RpcOpenOrder> = client
            .request(
                "torus_getOpenOrders",
                jsonrpsee::rpc_params![hex_address(taker), "0x1"],
            )
            .await
            .unwrap();
        assert!(taker_orders.is_empty(), "taker's filled buy should be gone");
        handle.stop().unwrap();
    }

    // --- get_open_orders_limit_500 ---
    // The endpoint must not return more than 500 orders regardless of how many
    // are in the book.
    //
    // The order book enforces MAX_ORDERS_PER_TRADER_PER_MARKET = 200, so we
    // spread orders across three markets (200 + 200 + 101 = 501) to produce
    // more than 500 total and verify the RPC cap fires.
    #[tokio::test]
    async fn get_open_orders_limit_500() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0x55; 20]);

        // Markets 1 and 2: 200 buy orders each (hits per-market trader cap).
        for market_id in [1u64, 2u64] {
            let mut book = OrderBook::new(market_id, fp(1), fp(1));
            for i in 1u32..=200 {
                book.place_order(
                    PlaceOrderParams {
                        market_id,
                        is_buy: true,
                        price: fp(i as i64 * 100),
                        quantity: fp(1),
                        order_type: OrderType::Limit,
                        time_in_force: TimeInForce::GTC,
                        reduce_only: false,
                        client_order_id: None,
                    },
                    trader,
                    1_700_000_000 + i as u64,
                );
            }
            let data = borsh::to_vec(&book).unwrap();
            state
                .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &market_id.to_be_bytes(), &data)
                .unwrap();
        }

        // Market 3: 101 sell orders — brings the total to 501.
        let mut book3 = OrderBook::new(3, fp(1), fp(1));
        for i in 1u32..=101 {
            book3.place_order(
                PlaceOrderParams {
                    market_id: 3,
                    is_buy: false,
                    price: fp(i as i64 * 100 + 500_000),
                    quantity: fp(1),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: None,
                },
                trader,
                1_700_000_000 + i as u64,
            );
        }
        let data3 = borsh::to_vec(&book3).unwrap();
        state
            .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &3u64.to_be_bytes(), &data3)
            .unwrap();

        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let orders: Vec<RpcOpenOrder> = client
            .request(
                "torus_getOpenOrders",
                jsonrpsee::rpc_params![hex_address(trader)],
            )
            .await
            .unwrap();
        // Must be capped at 500 — never 501.
        assert_eq!(orders.len(), 500);
        handle.stop().unwrap();
    }

    // --- get_open_interest_no_positions ---
    // An empty market returns zero long OI and zero short OI.
    #[tokio::test]
    async fn get_open_interest_no_positions() {
        let (_dir, state, mempool, executor) = setup();
        // No positions written to state — market 99 is entirely empty.
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let oi: RpcOpenInterest = client
            .request("torus_getOpenInterest", jsonrpsee::rpc_params!["0x63"])
            .await
            .unwrap();
        assert_eq!(oi.long_oi, hex_fp(FixedPoint::ZERO));
        assert_eq!(oi.short_oi, hex_fp(FixedPoint::ZERO));
        handle.stop().unwrap();
    }

    // --- get_mark_price_from_oracle ---
    // When an aggregated oracle price exists in CF_NATIVE_ORACLE the endpoint
    // must return it as both mark_price and index_price.  This is distinct
    // from the order-book-last-trade path tested by torus_get_mark_price_from_order_book.
    #[tokio::test]
    async fn get_mark_price_from_oracle() {
        use torus_state::cf::CF_NATIVE_ORACLE;
        let (_dir, state, mempool, executor) = setup();
        let market_id: u64 = 7;
        let oracle_price = fp(42000);

        // Write a StoredAggregatedPrice entry for market 7.
        // Binary layout: price(i128 16 BE) + block_number(u64 8 BE) + num_reporters(u32 4 BE).
        let mut key = Vec::with_capacity(11);
        key.extend_from_slice(b"agg");
        key.extend_from_slice(&market_id.to_be_bytes());
        let mut value = Vec::with_capacity(28);
        value.extend_from_slice(&oracle_price.raw().to_be_bytes());
        value.extend_from_slice(&1u64.to_be_bytes()); // block_number
        value.extend_from_slice(&3u32.to_be_bytes()); // num_reporters
        state.put_cf_raw(CF_NATIVE_ORACLE, &key, &value).unwrap();

        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let mp: RpcMarkPrice = client
            .request("torus_getMarkPrice", jsonrpsee::rpc_params!["0x7"])
            .await
            .unwrap();
        // Oracle price must surface as mark_price and index_price.
        assert_eq!(mp.mark_price, hex_fp(oracle_price));
        assert_eq!(mp.index_price, hex_fp(oracle_price));
        // No trades occurred in this book so last_trade_price must be zero.
        assert_eq!(mp.last_trade_price, hex_fp(FixedPoint::ZERO));
        handle.stop().unwrap();
    }

    // --- get_user_trades_newest_first ---
    // Trades must be returned in descending block order (newest block first).
    #[tokio::test]
    async fn get_user_trades_newest_first() {
        let (_dir, state, mempool, executor) = setup();
        let trader = Address::from([0xFF; 20]);
        let price = 50000i64 as i128 * FixedPoint::SCALE;
        let qty = 1i128 * FixedPoint::SCALE;
        // Insert three trades at blocks 5, 20, and 10 (out of order).
        store_user_trade(
            &state,
            &trader,
            100,
            1,
            price,
            qty,
            0,
            1,
            5,
            1_700_000_005,
            0,
        );
        store_user_trade(
            &state,
            &trader,
            200,
            1,
            price,
            qty,
            1,
            0,
            20,
            1_700_000_020,
            0,
        );
        store_user_trade(
            &state,
            &trader,
            300,
            1,
            price,
            qty,
            0,
            1,
            10,
            1_700_000_010,
            0,
        );

        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let trades: Vec<RpcUserTrade> = client
            .request(
                "torus_getUserTrades",
                jsonrpsee::rpc_params![hex_address(trader)],
            )
            .await
            .unwrap();
        assert_eq!(trades.len(), 3);
        // Verify descending block order: block 20, then 10, then 5.
        let blocks: Vec<u64> = trades
            .iter()
            .map(|t| u64::from_str_radix(t.block_number.strip_prefix("0x").unwrap(), 16).unwrap())
            .collect();
        assert_eq!(blocks, vec![20, 10, 5]);
        // Verify trade IDs match the expected block ordering.
        assert_eq!(trades[0].trade_id, hex_u128(200));
        assert_eq!(trades[1].trade_id, hex_u128(300));
        assert_eq!(trades[2].trade_id, hex_u128(100));
        handle.stop().unwrap();
    }
}
#[cfg(test)]
mod submit_queue_tests {
    use super::*;

    #[tokio::test]
    async fn permit_granted_when_free() {
        let sem = Arc::new(tokio::sync::Semaphore::new(SUBMIT_PERMITS));
        assert!(acquire_submit_permit(&sem).await.is_some());
    }

    #[tokio::test]
    async fn permit_times_out_when_saturated() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let _held = sem.clone().acquire_owned().await.unwrap();
        let start = Instant::now();
        assert!(acquire_submit_permit(&sem).await.is_none());
        // Bounded queue: gives up around SUBMIT_QUEUE_TIMEOUT, not instantly, not forever.
        assert!(start.elapsed() >= SUBMIT_QUEUE_TIMEOUT);
        assert!(start.elapsed() < SUBMIT_QUEUE_TIMEOUT * 4);
    }
}
