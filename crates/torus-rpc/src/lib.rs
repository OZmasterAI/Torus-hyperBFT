//! JSON-RPC server for the Torus-hyperBFT blockchain.
//!
//! Implements Ethereum-compatible `eth_*`, `net_*`, and `web3_*` namespaces
//! using jsonrpsee 0.26 with WebSocket subscription support.

pub mod error;
pub mod eth;
pub mod net;
pub mod types;
pub mod web3;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use alloy_primitives::B256;
use jsonrpsee::server::{ServerBuilder, ServerHandle};
use tokio::sync::broadcast;
use torus_evm::EvmExecutor;
use torus_mempool::Mempool;
use torus_state::cf::CF_BLOCK_HEADERS;
use torus_state::StateDb;

use crate::eth::EthApiServer;
use crate::net::NetApiServer;
use crate::web3::Web3ApiServer;

/// Broadcast channels for WebSocket subscriptions.
#[derive(Clone)]
pub struct BlockNotifier {
    pub new_heads: broadcast::Sender<serde_json::Value>,
    pub new_logs: broadcast::Sender<Vec<serde_json::Value>>,
    pub pending_txs: broadcast::Sender<B256>,
}

impl BlockNotifier {
    pub fn new() -> Self {
        let (new_heads, _) = broadcast::channel(256);
        let (new_logs, _) = broadcast::channel(256);
        let (pending_txs, _) = broadcast::channel(1024);
        Self { new_heads, new_logs, pending_txs }
    }

    /// Notify subscribers of a new block head.
    pub fn notify_new_block(&self, head: serde_json::Value) {
        let _ = self.new_heads.send(head);
    }

    /// Notify subscribers of a pending transaction.
    pub fn notify_pending_tx(&self, hash: B256) {
        let _ = self.pending_txs.send(hash);
    }
}

impl Default for BlockNotifier {
    fn default() -> Self { Self::new() }
}

/// Shared state for all RPC handlers.
#[derive(Clone)]
pub struct RpcState {
    pub(crate) state: StateDb,
    pub(crate) mempool: Arc<Mempool>,
    pub(crate) executor: Arc<EvmExecutor>,
    pub(crate) chain_id: u64,
    pub(crate) latest_height: Arc<AtomicU64>,
    pub(crate) notifier: BlockNotifier,
}

/// JSON-RPC server combining eth, net, and web3 namespaces.
pub struct RpcServer {
    state: RpcState,
}

impl RpcServer {
    /// Create a new RPC server. Scans the DB to find the latest block height.
    pub fn new(
        state_db: StateDb, mempool: Arc<Mempool>, executor: Arc<EvmExecutor>,
        chain_id: u64, notifier: BlockNotifier,
    ) -> Self {
        let latest = find_latest_height(&state_db);
        Self { state: RpcState { state: state_db, mempool, executor, chain_id, latest_height: Arc::new(AtomicU64::new(latest)), notifier } }
    }

    /// Start the RPC server on the given address.
    pub async fn start(self, addr: SocketAddr) -> Result<(ServerHandle, SocketAddr), Box<dyn std::error::Error + Send + Sync>> {
        let server = ServerBuilder::default().build(addr).await?;
        let local_addr = server.local_addr()?;
        let mut module = jsonrpsee::RpcModule::new(());
        module.merge(EthApiServer::into_rpc(self.state.clone()))?;
        module.merge(NetApiServer::into_rpc(self.state.clone()))?;
        module.merge(Web3ApiServer::into_rpc(self.state.clone()))?;
        let handle = server.start(module);
        Ok((handle, local_addr))
    }

    /// Get a reference to the RPC state.
    pub fn state(&self) -> &RpcState { &self.state }
}

/// Scan CF_BLOCK_HEADERS to find the highest consecutive block.
pub fn find_latest_height(state: &StateDb) -> u64 {
    let mut height: u64 = 0;
    loop {
        match state.get_cf_raw(CF_BLOCK_HEADERS, &height.to_be_bytes()) {
            Ok(Some(_)) => height += 1,
            _ => break,
        }
    }
    height.saturating_sub(1)
}

/// Public helper to update latest height after committing a new block.
pub fn set_latest_height(state: &RpcState, height: u64) {
    state.latest_height.store(height, Ordering::Relaxed);
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
        TorusBlockHeader { height, timestamp: 1_700_000_000 + height, proposer: Address::ZERO, state_root: B256::ZERO, receipts_root: B256::ZERO, logs_bloom: Bloom::ZERO, evm_gas_used: gas_used, evm_gas_limit: 30_000_000, native_action_count: 0, evm_tx_count: 0, base_fee_per_gas: base_fee, epoch: 0, validator_set_hash: B256::ZERO }
    }

    fn store_header(state: &StateDb, header: &TorusBlockHeader) -> B256 {
        let bytes = serde_json::to_vec(header).unwrap();
        let hash = alloy_primitives::keccak256(&bytes);
        state.put_cf_raw(CF_BLOCK_HEADERS, &header.height.to_be_bytes(), &bytes).unwrap();
        state.put_cf_raw(torus_state::cf::CF_BLOCK_HASH_TO_NUMBER, hash.as_slice(), &header.height.to_be_bytes()).unwrap();
        hash
    }

    fn store_body(state: &StateDb, height: u64, body: &TorusBlockBody) {
        state.put_cf_raw(CF_BLOCK_BODIES, &height.to_be_bytes(), &serde_json::to_vec(body).unwrap()).unwrap();
    }

    fn store_receipt(state: &StateDb, height: u64, receipt: &Receipt) {
        let mut key = [0u8; 12];
        key[..8].copy_from_slice(&height.to_be_bytes());
        key[8..12].copy_from_slice(&receipt.tx_index.to_be_bytes());
        state.put_cf_raw(CF_RECEIPTS, &key, &serde_json::to_vec(receipt).unwrap()).unwrap();
    }

    fn setup() -> (TempDir, StateDb, Arc<Mempool>, Arc<EvmExecutor>) {
        let dir = TempDir::new().unwrap();
        let state = StateDb::open(dir.path()).unwrap();
        (dir, state.clone(), Arc::new(Mempool::new(state.clone(), MempoolConfig::default())), Arc::new(EvmExecutor::new(TORUS_CHAIN_ID)))
    }

    async fn start_server(state: StateDb, mempool: Arc<Mempool>, executor: Arc<EvmExecutor>) -> (ServerHandle, SocketAddr) {
        RpcServer::new(state, mempool, executor, TORUS_CHAIN_ID, BlockNotifier::new()).start("127.0.0.1:0".parse().unwrap()).await.unwrap()
    }

    #[test]
    fn hex_encoding_roundtrip() {
        assert_eq!(hex_u64(0), "0x0");
        assert_eq!(hex_u64(255), "0xff");
        assert_eq!(hex_u64(7777), "0x1e61");
        assert_eq!(U256::from(42u64), parse_u256(&hex_u256(U256::from(42u64))).unwrap());
        assert_eq!(Address::from([0xab; 20]), parse_address(&hex_address(Address::from([0xab; 20]))).unwrap());
    }

    #[test]
    fn block_tag_parsing() {
        assert_eq!(resolve_block_tag("latest", 100).unwrap(), 100);
        assert_eq!(resolve_block_tag("earliest", 100).unwrap(), 0);
        assert_eq!(resolve_block_tag("pending", 100).unwrap(), 100);
        assert_eq!(resolve_block_tag("0xa", 100).unwrap(), 10);
    }

    #[tokio::test]
    async fn web3_client_version() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default().build(format!("http://{addr}")).unwrap();
        assert_eq!(client.request::<String, _>("web3_clientVersion", jsonrpsee::rpc_params![]).await.unwrap(), "torus/v0.1.0");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_chain_id() {
        let (_dir, state, mempool, executor) = setup();
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default().build(format!("http://{addr}")).unwrap();
        assert_eq!(client.request::<String, _>("eth_chainId", jsonrpsee::rpc_params![]).await.unwrap(), "0x1e61");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_block_number() {
        let (_dir, state, mempool, executor) = setup();
        for i in 0..5u64 { store_header(&state, &test_header(i, 0, 1_000_000_000)); }
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default().build(format!("http://{addr}")).unwrap();
        assert_eq!(client.request::<String, _>("eth_blockNumber", jsonrpsee::rpc_params![]).await.unwrap(), "0x4");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_balance() {
        let (_dir, state, mempool, executor) = setup();
        let addr_val = Address::from([0x11; 20]);
        let balance = U256::from(1_000_000_000_000_000_000u128);
        state.put_account(&addr_val, &AccountInfo { balance, nonce: 5, code_hash: B256::ZERO, code: None, account_id: None }).unwrap();
        store_header(&state, &test_header(0, 0, 0));
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default().build(format!("http://{addr}")).unwrap();
        assert_eq!(parse_u256(&client.request::<String, _>("eth_getBalance", jsonrpsee::rpc_params![hex_address(addr_val), "latest"]).await.unwrap()).unwrap(), balance);
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_block_by_number() {
        let (_dir, state, mempool, executor) = setup();
        let block_hash = store_header(&state, &test_header(0, 21000, 1_000_000_000));
        store_body(&state, 0, &TorusBlockBody { native_actions: vec![], evm_transactions: vec![], core_writer_actions: vec![] });
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default().build(format!("http://{addr}")).unwrap();
        let block: RpcBlock = client.request::<Option<RpcBlock>, _>("eth_getBlockByNumber", jsonrpsee::rpc_params!["0x0", false]).await.unwrap().unwrap();
        assert_eq!(block.number, "0x0");
        assert_eq!(block.hash, hex_b256(block_hash));
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_transaction_receipt() {
        let (_dir, state, mempool, executor) = setup();
        store_header(&state, &TorusBlockHeader { evm_tx_count: 1, ..test_header(0, 21000, 1_000_000_000) });
        let tx_hash = B256::from([0xaa; 32]);
        store_receipt(&state, 0, &Receipt { tx_hash, block_number: 0, block_hash: B256::ZERO, tx_index: 0, cumulative_gas_used: 21000, gas_used: 21000, contract_address: None, logs: vec![], logs_bloom: Bloom::ZERO, status: true, effective_gas_price: 1_000_000_000 });
        state.put_cf_raw(torus_state::cf::CF_TX_HASH_TO_LOCATION, tx_hash.as_slice(), &[0u8; 12]).unwrap();
        store_body(&state, 0, &TorusBlockBody { native_actions: vec![], evm_transactions: vec![], core_writer_actions: vec![] });
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default().build(format!("http://{addr}")).unwrap();
        let r: RpcReceipt = client.request::<Option<RpcReceipt>, _>("eth_getTransactionReceipt", jsonrpsee::rpc_params![hex_b256(tx_hash)]).await.unwrap().unwrap();
        assert_eq!(r.transaction_hash, hex_b256(tx_hash));
        assert_eq!(r.status, "0x1");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_call_simple_transfer() {
        let (_dir, state, mempool, executor) = setup();
        let sender = Address::from([0x11; 20]);
        state.put_account(&sender, &AccountInfo { balance: U256::from(10u64.pow(18)), nonce: 0, code_hash: B256::ZERO, code: None, account_id: None }).unwrap();
        store_header(&state, &test_header(0, 0, 0));
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default().build(format!("http://{addr}")).unwrap();
        let call = serde_json::json!({ "from": hex_address(sender), "to": hex_address(Address::from([0x22; 20])), "value": "0x0" });
        assert_eq!(client.request::<String, _>("eth_call", jsonrpsee::rpc_params![call, "latest"]).await.unwrap(), "0x");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_logs_with_filter() {
        let (_dir, state, mempool, executor) = setup();
        let log_addr = Address::from([0x33; 20]);
        let topic0 = B256::from([0x44; 32]);
        store_header(&state, &TorusBlockHeader { evm_tx_count: 1, ..test_header(0, 21000, 1_000_000_000) });
        store_receipt(&state, 0, &Receipt { tx_hash: B256::from([0x55; 32]), block_number: 0, block_hash: B256::ZERO, tx_index: 0, cumulative_gas_used: 21000, gas_used: 21000, contract_address: None, logs: vec![torus_types::Log { address: log_addr, topics: vec![topic0], data: vec![1, 2, 3] }], logs_bloom: Bloom::ZERO, status: true, effective_gas_price: 1_000_000_000 });
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default().build(format!("http://{addr}")).unwrap();
        let filter = serde_json::json!({ "fromBlock": "0x0", "toBlock": "0x0", "address": hex_address(log_addr), "topics": [hex_b256(topic0)] });
        let logs: Vec<RpcLog> = client.request("eth_getLogs", jsonrpsee::rpc_params![filter]).await.unwrap();
        assert_eq!(logs.len(), 1);
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_fee_history() {
        let (_dir, state, mempool, executor) = setup();
        for i in 0..5u64 { store_header(&state, &test_header(i, i * 1000, 1_000_000_000 + i)); }
        let (handle, addr) = start_server(state, mempool, executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default().build(format!("http://{addr}")).unwrap();
        let result: FeeHistory = client.request("eth_feeHistory", jsonrpsee::rpc_params!["0x3", "0x4", [25.0, 75.0]]).await.unwrap();
        assert_eq!(result.oldest_block, hex_u64(2));
        assert_eq!(result.gas_used_ratio.len(), 3);
        assert_eq!(result.base_fee_per_gas.len(), 4);
        handle.stop().unwrap();
    }
}
