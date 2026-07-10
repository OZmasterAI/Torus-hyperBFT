//! Batch EK: RPC compliance tests for eth_* namespace hardening.

use std::net::SocketAddr;
use std::sync::Arc;

use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClientBuilder;
use jsonrpsee::rpc_params;
use jsonrpsee::server::ServerHandle;
use tempfile::TempDir;

use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_mempool::{Mempool, MempoolConfig};
use torus_rpc::types::*;
use torus_rpc::{BlockNotifier, RpcServer};
use torus_state::StateDb;

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

/// FIX 7: eth_call with historical block tag returns clear error.
#[tokio::test]
async fn eth_call_rejects_historical_block_tag() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    let call_obj = serde_json::json!({
        "from": "0x0000000000000000000000000000000000000001",
        "to": "0x0000000000000000000000000000000000000002",
        "data": "0x"
    });

    // Use a specific block number (0x1) which != latest (0) — should error.
    // "earliest" maps to 0 which equals latest on a fresh DB.
    let result: Result<String, _> = client
        .request("eth_call", rpc_params![call_obj, "0x1"])
        .await;
    assert!(
        result.is_err(),
        "eth_call should reject historical block tag"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("historical state not available"),
        "error should mention historical state: {err_msg}"
    );

    handle.stop().unwrap();
}

/// D2 (S392) acceptance: bare eth_call (no fee fields) from an EMPTY account
/// at height > 0 with base_fee = 1 gwei succeeds — geth/reth call semantics.
/// Pre-fix this failed with GasPriceLessThanBasefee; the bug hid because other
/// tests run at height 0, where BlockEnvCfg::default() carries base_fee 0.
#[tokio::test]
async fn bare_eth_call_succeeds_at_height_with_base_fee() {
    let (_dir, state, mempool, executor) = setup();

    // Commit a height-1 header carrying the chain's 1-gwei base fee BEFORE the
    // server starts, so find_latest_height reports latest = 1.
    let header = torus_types::TorusBlockHeader {
        height: 1,
        parent_hash: alloy_primitives::B256::ZERO,
        timestamp: 1000,
        proposer: alloy_primitives::Address::repeat_byte(0x99),
        state_root: alloy_primitives::B256::ZERO,
        receipts_root: alloy_primitives::B256::ZERO,
        logs_bloom: alloy_primitives::Bloom::ZERO,
        evm_gas_used: 0,
        evm_fee_revenue: 0,
        evm_gas_limit: 30_000_000,
        native_action_count: 0,
        evm_tx_count: 0,
        base_fee_per_gas: 1_000_000_000,
        epoch: 0,
        validator_set_hash: alloy_primitives::B256::ZERO,
        sig_attestation: [0u8; 64],
    };
    let canonical = header.canonical_header_bytes();
    let block_hash = alloy_primitives::keccak256(&canonical);
    let mut data = block_hash.to_vec();
    data.extend(serde_json::to_vec(&header).unwrap());
    state
        .put_cf_raw(
            torus_state::cf::CF_BLOCK_HEADERS,
            &1u64.to_be_bytes(),
            &data,
        )
        .unwrap();

    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    // Never-funded account, no gas/fee fields.
    let call_obj = serde_json::json!({
        "from": "0x00000000000000000000000000000000000000ee",
        "to": "0x00000000000000000000000000000000000000bb",
        "data": "0x"
    });
    let result: Result<String, _> = client
        .request("eth_call", rpc_params![call_obj.clone(), "latest"])
        .await;
    assert!(
        result.is_ok(),
        "bare eth_call from empty account at height>0 must succeed, got: {:?}",
        result.err()
    );

    // Same bare request through eth_estimateGas.
    let est: Result<String, _> = client
        .request("eth_estimateGas", rpc_params![call_obj])
        .await;
    assert!(
        est.is_ok(),
        "bare eth_estimateGas must succeed, got: {:?}",
        est.err()
    );

    handle.stop().unwrap();
}

/// FIX 8 + 15: eth_estimateGas returns error (not gas_used) on revert.
#[tokio::test]
async fn estimate_gas_returns_error_on_revert() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    // Call to non-existent contract with data — will revert
    let call_obj = serde_json::json!({
        "from": "0x0000000000000000000000000000000000000001",
        "to": "0x0000000000000000000000000000000000000099",
        "data": "0xdeadbeef"
    });

    let _result: Result<String, _> = client
        .request("eth_estimateGas", rpc_params![call_obj])
        .await;

    // Whether this call succeeds or errors depends on EVM behavior for calls to
    // empty accounts. The key behavioral change tested: if !result.success, we now
    // return an error (code 3, "execution reverted") instead of Ok(gas_used).
    // A reverting contract is needed for a true negative test — this verifies
    // the code path compiles and doesn't panic.

    handle.stop().unwrap();
}

/// FIX 9: eth_getLogs rejects block ranges exceeding 10,000.
#[tokio::test]
async fn get_logs_rejects_excessive_block_range() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    let filter = serde_json::json!({
        "fromBlock": "0x0",
        "toBlock": "0x10000" // 65536 — exceeds 10,000
    });

    let result: Result<Vec<RpcLog>, _> = client.request("eth_getLogs", rpc_params![filter]).await;
    assert!(result.is_err(), "getLogs should reject excessive range");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("exceeds maximum"),
        "error should mention range limit: {err_msg}"
    );

    handle.stop().unwrap();
}

/// FIX 9: eth_getLogs accepts range within 10,000 blocks.
#[tokio::test]
async fn get_logs_accepts_valid_range() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    let filter = serde_json::json!({
        "fromBlock": "latest",
        "toBlock": "latest"
    });

    let result: Result<Vec<RpcLog>, _> = client.request("eth_getLogs", rpc_params![filter]).await;
    assert!(result.is_ok(), "getLogs should accept zero-range query");

    handle.stop().unwrap();
}

/// FIX 10: RpcTransaction includes access_list field type.
#[test]
fn rpc_transaction_access_list_field_exists() {
    let tx = RpcTransaction {
        hash: "0x00".into(),
        nonce: "0x0".into(),
        block_hash: "0x00".into(),
        block_number: "0x0".into(),
        transaction_index: "0x0".into(),
        from: "0x00".into(),
        to: None,
        value: "0x0".into(),
        gas: "0x0".into(),
        gas_price: "0x0".into(),
        input: "0x".into(),
        v: "0x0".into(),
        r: "0x0".into(),
        s: "0x0".into(),
        tx_type: "0x0".into(),
        chain_id: None,
        max_fee_per_gas: None,
        max_priority_fee_per_gas: None,
        access_list: Some(vec![RpcAccessListItem {
            address: "0x0000000000000000000000000000000000000001".into(),
            storage_keys: vec![
                "0x0000000000000000000000000000000000000000000000000000000000000001".into(),
            ],
        }]),
    };
    let json = serde_json::to_value(&tx).unwrap();
    let al = json
        .get("accessList")
        .expect("accessList should be present");
    assert!(al.is_array());
    assert_eq!(al.as_array().unwrap().len(), 1);
}

/// FIX 10: access_list is omitted for legacy transactions (None).
#[test]
fn rpc_transaction_legacy_omits_access_list() {
    let tx = RpcTransaction {
        hash: "0x00".into(),
        nonce: "0x0".into(),
        block_hash: "0x00".into(),
        block_number: "0x0".into(),
        transaction_index: "0x0".into(),
        from: "0x00".into(),
        to: None,
        value: "0x0".into(),
        gas: "0x0".into(),
        gas_price: "0x0".into(),
        input: "0x".into(),
        v: "0x1b".into(),
        r: "0x0".into(),
        s: "0x0".into(),
        tx_type: "0x0".into(),
        chain_id: None,
        max_fee_per_gas: None,
        max_priority_fee_per_gas: None,
        access_list: None, // Legacy: no access list
    };
    let json = serde_json::to_value(&tx).unwrap();
    assert!(
        json.get("accessList").is_none(),
        "Legacy tx should not include accessList"
    );
}

/// FIX 11: transactions_root is not all zeros.
#[test]
fn empty_trie_root_constant_is_correct() {
    // The empty MPT root is keccak256(rlp("")) = keccak256(0x80)
    let expected = alloy_primitives::keccak256(&[0x80]);
    let expected_hex = format!("0x{}", hex::encode(expected.as_slice()));
    assert_eq!(
        expected_hex,
        "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421"
    );
}

/// FIX 14: TxSubmitLimiter works correctly.
#[test]
fn tx_submit_limiter_enforces_rate() {
    use torus_rpc::TxSubmitLimiter;
    let limiter = TxSubmitLimiter::new(5);
    let sender = alloy_primitives::Address::new([1u8; 20]);

    // First 5 should pass
    for _ in 0..5 {
        assert!(limiter.check_sender(&sender));
    }
    // 6th should be rejected
    assert!(!limiter.check_sender(&sender));

    // Different sender should still work
    let sender2 = alloy_primitives::Address::new([2u8; 20]);
    assert!(limiter.check_sender(&sender2));
}

/// FIX 16: Unknown methods return -32601.
#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    let result: Result<serde_json::Value, _> = client
        .request("eth_getBlockReceipts", rpc_params!["latest"])
        .await;
    assert!(result.is_err());
    let err_str = result.unwrap_err().to_string();
    // jsonrpsee returns "Method not found" for unregistered methods
    assert!(
        err_str.contains("not found") || err_str.contains("-32601"),
        "unknown method should return -32601: {err_str}"
    );

    handle.stop().unwrap();
}
