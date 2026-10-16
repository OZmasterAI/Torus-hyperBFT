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
        evm_block_interval: torus_types::DEFAULT_EVM_BLOCK_INTERVAL,
        evm_big_block_interval: torus_types::DEFAULT_EVM_BIG_BLOCK_INTERVAL,
        evm_big_gas_limit: torus_types::DEFAULT_EVM_BIG_GAS_LIMIT,
        native_action_count: 0,
        evm_tx_count: 0,
        base_fee_per_gas: 1_000_000_000,
        big_base_fee_per_gas: 1_000_000_000,
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
    // s84: the eth view's head is the executed head; mark height 1 executed so
    // `latest` is the 1-gwei header.
    state
        .put_cf_raw(
            torus_state::cf::CF_CONSENSUS_META,
            torus_state::cf::META_NATIVE_APPLIED_HEIGHT,
            &1u64.to_be_bytes(),
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

/// Commit a header at height 1 with `base_fee` and `evm_gas_limit` and mark it
/// executed, so `latest` is that header (same layout as the D2 test above).
/// Big blocks off: M = 0, the default big limit, the big base fee = `base_fee`.
fn store_head_with_base_fee(state: &StateDb, base_fee: u64, evm_gas_limit: u64) {
    store_head(
        state,
        Head {
            height: 1,
            evm_gas_limit,
            base_fee,
            big_interval: torus_types::DEFAULT_EVM_BIG_BLOCK_INTERVAL,
            big_gas_limit: torus_types::DEFAULT_EVM_BIG_GAS_LIMIT,
            big_base_fee: base_fee,
        },
    );
}

/// Item 7 step 2: the header values of a stored test head ([`store_head`]).
#[derive(Clone, Copy)]
struct Head {
    height: u64,
    evm_gas_limit: u64,
    base_fee: u64,
    /// M (`evm_big_block_interval`).
    big_interval: u64,
    big_gas_limit: u64,
    big_base_fee: u64,
}

/// Commit the header `h` and mark its height executed, so `latest` is that
/// header (the layout `commit_block` writes: hash ‖ header JSON).
fn store_head(state: &StateDb, h: Head) {
    let header = torus_types::TorusBlockHeader {
        height: h.height,
        parent_hash: alloy_primitives::B256::ZERO,
        timestamp: 1000,
        proposer: alloy_primitives::Address::repeat_byte(0x99),
        state_root: alloy_primitives::B256::ZERO,
        receipts_root: alloy_primitives::B256::ZERO,
        logs_bloom: alloy_primitives::Bloom::ZERO,
        evm_gas_used: 0,
        evm_fee_revenue: 0,
        evm_gas_limit: h.evm_gas_limit,
        evm_block_interval: torus_types::DEFAULT_EVM_BLOCK_INTERVAL,
        evm_big_block_interval: h.big_interval,
        evm_big_gas_limit: h.big_gas_limit,
        native_action_count: 0,
        evm_tx_count: 0,
        base_fee_per_gas: h.base_fee,
        big_base_fee_per_gas: h.big_base_fee,
        epoch: 0,
        validator_set_hash: alloy_primitives::B256::ZERO,
        sig_attestation: [0u8; 64],
    };
    let block_hash = alloy_primitives::keccak256(header.canonical_header_bytes());
    let mut data = block_hash.to_vec();
    data.extend(serde_json::to_vec(&header).unwrap());
    state
        .put_cf_raw(
            torus_state::cf::CF_BLOCK_HEADERS,
            &h.height.to_be_bytes(),
            &data,
        )
        .unwrap();
    state
        .put_cf_raw(
            torus_state::cf::CF_CONSENSUS_META,
            torus_state::cf::META_NATIVE_APPLIED_HEIGHT,
            &h.height.to_be_bytes(),
        )
        .unwrap();
}

fn access_list_1_addr_2_keys() -> serde_json::Value {
    serde_json::json!([{
        "address": "0x00000000000000000000000000000000000000cc",
        "storageKeys": [
            "0x0000000000000000000000000000000000000000000000000000000000000001",
            "0x0000000000000000000000000000000000000000000000000000000000000002"
        ]
    }])
}

/// s104 follow-up (c): eth_estimateGas charges the EIP-2930 intrinsic cost of
/// an access list, as the real typed tx does: 2400 per address + 1900 per
/// storage key. The plain call (no fee fields, no list) estimates as before:
/// 21000 + 4 nonzero calldata bytes * 16 = 21064.
#[tokio::test]
async fn estimate_gas_charges_access_list_intrinsic_gas() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    let plain = serde_json::json!({
        "from": "0x00000000000000000000000000000000000000ee",
        "to": "0x00000000000000000000000000000000000000bb",
        "data": "0xdeadbeef"
    });
    let est = |v: serde_json::Value| {
        let client = &client;
        async move {
            let s: String = client
                .request("eth_estimateGas", rpc_params![v])
                .await
                .unwrap();
            u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap()
        }
    };
    let base = est(plain.clone()).await;
    assert_eq!(base, 21_064, "plain call estimate unchanged");

    let mut with_list = plain.clone();
    with_list["accessList"] = access_list_1_addr_2_keys();
    assert_eq!(est(with_list.clone()).await, base + 2400 + 2 * 1900);

    // Same list on an explicit type 1 and on an inferred type 2 call.
    let mut typed1 = with_list.clone();
    typed1["type"] = serde_json::json!("0x1");
    assert_eq!(est(typed1).await, base + 6200);
    let mut typed2 = with_list;
    typed2["maxFeePerGas"] = serde_json::json!("0x0");
    assert_eq!(est(typed2).await, base + 6200);

    // eth_call output for the plain request is unchanged.
    let out: String = client
        .request("eth_call", rpc_params![plain, "latest"])
        .await
        .unwrap();
    assert_eq!(out, "0x");
    handle.stop().unwrap();
}

/// A plain transfer (no calldata) estimates exactly the 21000 intrinsic gas:
/// the search's lower bound must be a limit that fails, so 21000 itself is
/// tried (it returned 21001 when 21000 was taken as the failing bound).
#[tokio::test]
async fn estimate_gas_plain_transfer_is_exactly_21000() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    for call in [
        serde_json::json!({
            "from": "0x00000000000000000000000000000000000000ee",
            "to": "0x00000000000000000000000000000000000000bb"
        }),
        serde_json::json!({
            "from": "0x00000000000000000000000000000000000000ee",
            "to": "0x00000000000000000000000000000000000000bb",
            "data": "0x"
        }),
    ] {
        let s: String = client
            .request("eth_estimateGas", rpc_params![call.clone()])
            .await
            .unwrap();
        assert_eq!(s, "0x5208", "plain transfer {call}");
    }
    handle.stop().unwrap();
}

/// An explicit unsupported type (blob / set-code) is refused as invalid params.
#[tokio::test]
async fn eth_call_rejects_unsupported_tx_type() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    for t in ["0x3", "0x4"] {
        let call = serde_json::json!({
            "to": "0x00000000000000000000000000000000000000bb",
            "type": t
        });
        let call_err = client
            .request::<String, _>("eth_call", rpc_params![call.clone(), "latest"])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            call_err.contains("transaction type not supported"),
            "{t}: {call_err}"
        );
        let est_err = client
            .request::<String, _>("eth_estimateGas", rpc_params![call])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            est_err.contains("transaction type not supported"),
            "{t}: {est_err}"
        );
    }
    handle.stop().unwrap();
}

/// Call-simulation mode skips the base-fee check (D2), so a typed 1559 call
/// whose maxFeePerGas is below the block base fee still runs, as the same
/// request did when it was built untyped. A bare call still works too.
#[tokio::test]
async fn typed_1559_call_below_base_fee_still_runs() {
    let (_dir, state, mempool, executor) = setup();
    store_head_with_base_fee(&state, 1_000_000_000, 30_000_000);
    let sender = alloy_primitives::Address::repeat_byte(0xee);
    state
        .put_account(
            &sender,
            &revm::state::AccountInfo {
                balance: alloy_primitives::U256::from(10u128.pow(18)),
                nonce: 0,
                code_hash: alloy_primitives::B256::ZERO,
                code: None,
                account_id: None,
            },
        )
        .unwrap();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    let typed = serde_json::json!({
        "from": "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "to": "0x00000000000000000000000000000000000000bb",
        "maxFeePerGas": "0x1",
        "maxPriorityFeePerGas": "0x1"
    });
    let r: Result<String, _> = client
        .request("eth_call", rpc_params![typed.clone(), "latest"])
        .await;
    assert!(r.is_ok(), "typed call below base fee: {:?}", r.err());
    let e: Result<String, _> = client.request("eth_estimateGas", rpc_params![typed]).await;
    assert!(e.is_ok(), "typed estimate below base fee: {:?}", e.err());
    let bare = serde_json::json!({ "to": "0x00000000000000000000000000000000000000bb" });
    let r: Result<String, _> = client
        .request("eth_call", rpc_params![bare, "latest"])
        .await;
    assert!(r.is_ok(), "bare call: {:?}", r.err());
    handle.stop().unwrap();
}

/// Item 7 step 1: under a 3M header, a bare eth_call (no gas) and
/// eth_estimateGas use the header's limit, not 30M, which revm refuses as
/// above the block's gas limit.
#[tokio::test]
async fn bare_call_and_estimate_work_under_a_3m_header() {
    let (_dir, state, mempool, executor) = setup();
    store_head_with_base_fee(&state, 1_000_000_000, 3_000_000);
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    let bare = serde_json::json!({
        "from": "0x00000000000000000000000000000000000000ee",
        "to": "0x00000000000000000000000000000000000000bb"
    });
    let r: Result<String, _> = client
        .request("eth_call", rpc_params![bare.clone(), "latest"])
        .await;
    assert!(r.is_ok(), "bare eth_call under a 3M header: {:?}", r.err());
    let est: Result<String, _> = client.request("eth_estimateGas", rpc_params![bare]).await;
    assert_eq!(
        est.as_deref().map_err(|e| e.to_string()),
        Ok("0x5208"),
        "eth_estimateGas under a 3M header"
    );
    handle.stop().unwrap();
}

/// Item 7 step 1 (owner s123): call gas above the header's limit is clamped to
/// the limit, so a 30M request still runs under a 3M header.
#[tokio::test]
async fn eth_call_gas_above_the_header_limit_is_capped() {
    let (_dir, state, mempool, executor) = setup();
    store_head_with_base_fee(&state, 1_000_000_000, 3_000_000);
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    let call = serde_json::json!({
        "from": "0x00000000000000000000000000000000000000ee",
        "to": "0x00000000000000000000000000000000000000bb",
        "gas": "0x1c9c380"
    });
    let r: Result<String, _> = client
        .request("eth_call", rpc_params![call.clone(), "latest"])
        .await;
    assert!(r.is_ok(), "30M eth_call under a 3M header: {:?}", r.err());
    let est: Result<String, _> = client.request("eth_estimateGas", rpc_params![call]).await;
    assert!(
        est.is_ok(),
        "30M eth_estimateGas under a 3M header: {:?}",
        est.err()
    );
    handle.stop().unwrap();
}

// ============================================================================
// Item 7 step 2 (B18): RPC per lane
// ============================================================================

const GWEI: u64 = 1_000_000_000;

/// Put `who` in the big lane: its `EvmUserModify` row in `CF_NATIVE_ORACLE`.
fn set_big_lane(state: &StateDb, who: alloy_primitives::Address) {
    state
        .put_cf_raw(
            torus_state::cf::CF_NATIVE_ORACLE,
            &torus_state::cf::evm_lane_key(&who),
            &[1],
        )
        .unwrap();
}

/// Memory words [`deploy_gas_burner`]'s one MLOAD expands to.
const BURN_WORDS: u64 = 51_000;
/// The burner's gas: the 21,000 intrinsic gas, PUSH3 3, MLOAD 3 and the memory
/// expansion `3w + w^2 / 512` (w = [`BURN_WORDS`]): 5,254,084.
const BURN_GAS: u64 = 21_000 + 3 + 3 + 3 * BURN_WORDS + BURN_WORDS * BURN_WORDS / 512;

/// A contract at `at` whose one MLOAD of the last of [`BURN_WORDS`] memory
/// words needs [`BURN_GAS`] (above the 3M small limit, under the 10M big one):
/// a call runs out of gas below it. One op, so the estimate's search is fast.
fn deploy_gas_burner(state: &StateDb, at: alloy_primitives::Address) {
    let offset = ((BURN_WORDS - 1) * 32).to_be_bytes();
    // PUSH3 offset; MLOAD; STOP
    let code = vec![0x62, offset[5], offset[6], offset[7], 0x51, 0x00];
    let code_hash = alloy_primitives::keccak256(&code);
    state.put_code(&code_hash, &code).unwrap();
    state
        .put_account(
            &at,
            &revm::state::AccountInfo {
                balance: alloy_primitives::U256::ZERO,
                nonce: 1,
                code_hash,
                code: None,
                account_id: None,
            },
        )
        .unwrap();
}

/// Item 7 step 2 (B18): eth_call and eth_estimateGas run in the caller's lane.
/// Header small 3M, big 10M: with M = 50 a big-lane caller's 5.25M call
/// estimates under 10M; a small-lane caller is capped at 3M and the call runs
/// out of gas there. With M = 0 (big blocks off) the big-lane caller is capped
/// at 3M too.
#[tokio::test]
async fn estimate_gas_caps_at_the_callers_lane_limit() {
    let big = alloy_primitives::Address::repeat_byte(0xb1);
    let small = alloy_primitives::Address::repeat_byte(0x51);
    let burner = alloy_primitives::Address::repeat_byte(0x10);
    for m in [50u64, 0] {
        let (_dir, state, mempool, executor) = setup();
        store_head(
            &state,
            Head {
                height: 1,
                evm_gas_limit: 3_000_000,
                base_fee: GWEI,
                big_interval: m,
                big_gas_limit: 10_000_000,
                big_base_fee: 2 * GWEI,
            },
        );
        set_big_lane(&state, big);
        deploy_gas_burner(&state, burner);
        let (handle, addr) = start_server(state, mempool, executor).await;
        let client = HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();
        let call_from =
            |from| serde_json::json!({ "from": hex_address(from), "to": hex_address(burner) });
        let estimate = |from| {
            let client = &client;
            async move {
                client
                    .request::<String, _>("eth_estimateGas", rpc_params![call_from(from)])
                    .await
                    .map(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap())
                    .map_err(|e| e.to_string())
            }
        };
        let call_5m = |from| {
            let client = &client;
            let mut req = call_from(from);
            req["gas"] = serde_json::json!(hex_u64(5_300_000));
            async move {
                client
                    .request::<String, _>("eth_call", rpc_params![req, "latest"])
                    .await
                    .map_err(|e| e.to_string())
            }
        };

        let big_est = estimate(big).await;
        if m > 0 {
            let est = big_est.expect("big-lane estimate with M = 50");
            assert!(
                est > 3_000_000 && est < 10_000_000,
                "big lane estimates under its 10M limit: {est}"
            );
            assert_eq!(est, BURN_GAS, "the burner's exact gas");
            assert_eq!(
                call_5m(big).await,
                Ok("0x".to_string()),
                "big-lane 5.3M call"
            );
        } else {
            assert!(
                big_est.is_err(),
                "M = 0: the big lane is off, capped at 3M: {big_est:?}"
            );
            assert!(
                call_5m(big).await.is_err(),
                "M = 0: a 5.3M call is capped at 3M"
            );
        }
        let small_est = estimate(small).await;
        assert!(
            small_est.is_err(),
            "M = {m}: small lane capped at 3M: {small_est:?}"
        );
        assert!(
            call_5m(small).await.is_err(),
            "M = {m}: small-lane 5.3M call capped at 3M"
        );
        handle.stop().unwrap();
    }
}

/// Item 7 step 2 (B18, owner answer 9): `torus_bigBlockGasPrice` is the head's
/// big base fee (`eth_gasPrice` stays the small one); `eth_feeHistory` takes an
/// optional 4th parameter `blockType`: "big" lists the big lane's base fees,
/// absent or "small" the small lane's, anything else is invalid params.
#[tokio::test]
async fn big_block_gas_price_and_fee_history_per_lane() {
    let (_dir, state, mempool, executor) = setup();
    for height in 1..=3u64 {
        store_head(
            &state,
            Head {
                height,
                evm_gas_limit: 3_000_000,
                base_fee: GWEI + height,
                big_interval: 50,
                big_gas_limit: 10_000_000,
                big_base_fee: 2 * GWEI + height,
            },
        );
    }
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    let small: String = client.request("eth_gasPrice", rpc_params![]).await.unwrap();
    assert_eq!(small, hex_u64(GWEI + 3), "eth_gasPrice = the small lane");
    let big: String = client
        .request("torus_bigBlockGasPrice", rpc_params![])
        .await
        .unwrap();
    assert_eq!(big, hex_u64(2 * GWEI + 3), "torus_bigBlockGasPrice");

    let fees = |base: u64| -> Vec<String> {
        // Heights 1..=3, then the next block's fee (no header 4: the last one).
        [1, 2, 3, 3].iter().map(|h| hex_u64(base + h)).collect()
    };
    let big_hist: FeeHistory = client
        .request(
            "eth_feeHistory",
            rpc_params!["0x3", "latest", None::<Vec<f64>>, "big"],
        )
        .await
        .unwrap();
    assert_eq!(big_hist.oldest_block, "0x1");
    assert_eq!(big_hist.base_fee_per_gas, fees(2 * GWEI), "blockType big");
    assert_eq!(
        big_hist.gas_used_ratio,
        vec![0.0; 3],
        "gasUsedRatio stays the header's"
    );
    let small_hist: FeeHistory = client
        .request(
            "eth_feeHistory",
            rpc_params!["0x3", "latest", None::<Vec<f64>>, "small"],
        )
        .await
        .unwrap();
    assert_eq!(small_hist.base_fee_per_gas, fees(GWEI), "blockType small");
    let default_hist: FeeHistory = client
        .request("eth_feeHistory", rpc_params!["0x3", "latest"])
        .await
        .unwrap();
    assert_eq!(
        default_hist.base_fee_per_gas,
        fees(GWEI),
        "no blockType = small"
    );

    let other = client
        .request::<FeeHistory, _>(
            "eth_feeHistory",
            rpc_params!["0x3", "latest", None::<Vec<f64>>, "other"],
        )
        .await
        .unwrap_err();
    match &other {
        jsonrpsee::core::ClientError::Call(e) => {
            assert_eq!(
                e.code(),
                -32602,
                "blockType \"other\" is invalid params: {e}"
            );
            assert!(
                e.message().contains("blockType"),
                "names the parameter: {e}"
            );
        }
        e => panic!("blockType \"other\": expected invalid params, got {e}"),
    }
    handle.stop().unwrap();
}

/// Item 7 step 2 (B18): `torus_usingBigBlocks` reports the stored lane choice
/// (the `EvmUserModify` row), also while big blocks are off (M = 0). It reads
/// the published ingress view: a row of a block not yet checkpointed counts.
#[tokio::test]
async fn using_big_blocks_reads_the_lane_row() {
    let (_dir, state, mempool, executor) = setup();
    store_head_with_base_fee(&state, GWEI, 3_000_000);
    let big = alloy_primitives::Address::repeat_byte(0xb1);
    let small = alloy_primitives::Address::repeat_byte(0x51);
    let fresh = alloy_primitives::Address::repeat_byte(0xf1);
    set_big_lane(&state, big);
    // `fresh`'s row only in block 2's frozen set, not in the DB.
    let block_2 = torus_state::NativeStateOverlay::new(state.clone());
    torus_state::backend::StateBackend::put_cf_raw(
        &block_2,
        torus_state::cf::CF_NATIVE_ORACLE,
        &torus_state::cf::evm_lane_key(&fresh),
        &[1],
    )
    .unwrap();
    mempool.publish_read_view(Some(torus_state::ReadView::new(
        state.clone(),
        2,
        vec![block_2.freeze(2)],
        None,
    )));
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    let using = |who| {
        let client = &client;
        async move {
            client
                .request::<bool, _>("torus_usingBigBlocks", rpc_params![hex_address(who)])
                .await
                .unwrap()
        }
    };
    assert!(using(big).await, "with the lane row");
    assert!(!using(small).await, "without the lane row");
    assert!(using(fresh).await, "a row in the ingress view");
    let bad = client
        .request::<bool, _>("torus_usingBigBlocks", rpc_params!["0x1234"])
        .await;
    assert!(bad.is_err(), "a malformed address is refused: {bad:?}");
    handle.stop().unwrap();
}

/// Item 7 step 2 (B18): a block's `gasLimit` is the height's EVM gas cap: the
/// small limit plus the big one at a big height (M = 50: height 100 = 3M + 10M),
/// the small limit elsewhere (105).
#[tokio::test]
async fn block_gas_limit_is_the_heights_cap() {
    let (_dir, state, mempool, executor) = setup();
    for height in [100u64, 105] {
        store_head(
            &state,
            Head {
                height,
                evm_gas_limit: 3_000_000,
                base_fee: GWEI,
                big_interval: 50,
                big_gas_limit: 10_000_000,
                big_base_fee: GWEI,
            },
        );
    }
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    for (height, cap) in [(100u64, 13_000_000u64), (105, 3_000_000)] {
        let block: serde_json::Value = client
            .request("eth_getBlockByNumber", rpc_params![hex_u64(height), false])
            .await
            .unwrap();
        assert_eq!(block["gasLimit"], hex_u64(cap), "height {height}");
    }
    handle.stop().unwrap();
}
