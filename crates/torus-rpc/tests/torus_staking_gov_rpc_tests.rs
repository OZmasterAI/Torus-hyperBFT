//! Integration tests for torus_* staking, governance, and submission RPC endpoints.

use std::net::SocketAddr;
use std::sync::Arc;

use alloy_primitives::{Address, U256};
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClientBuilder;
use jsonrpsee::rpc_params;
use jsonrpsee::server::ServerHandle;
use revm::state::AccountInfo;
use tempfile::TempDir;

use torus_economics::governance::GovernanceManager;
use torus_economics::staking::StakingManager;
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_mempool::{Mempool, MempoolConfig};
use torus_rpc::types::*;
use torus_rpc::{BlockNotifier, RpcServer};
use torus_state::StateDb;

// ============================================================================
// Helpers
// ============================================================================

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn wei(tokens: u64) -> U256 {
    U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
}

fn fund(state: &StateDb, a: &Address, amount: U256) {
    let info = AccountInfo {
        balance: amount,
        ..Default::default()
    };
    state.put_account(a, &info).unwrap();
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

// ============================================================================
// Staking tests (2.9.3)
// ============================================================================

#[tokio::test]
async fn torus_get_validators_returns_registered() {
    let (_dir, state, mempool, executor) = setup();
    let staking = StakingManager::new(state.clone());
    let v1 = addr(1);
    let v2 = addr(2);
    let v3 = addr(3);

    fund(&state, &v1, wei(50_000));
    fund(&state, &v2, wei(100_000));
    fund(&state, &v3, wei(75_000));

    staking
        .register_validator(v1, [1u8; 32], 500, wei(10_000))
        .unwrap();
    staking
        .register_validator(v2, [2u8; 32], 300, wei(50_000))
        .unwrap();
    staking
        .register_validator(v3, [3u8; 32], 100, wei(25_000))
        .unwrap();

    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    let result: Vec<RpcValidatorInfo> = client
        .request("torus_getValidators", rpc_params![])
        .await
        .unwrap();

    assert_eq!(result.len(), 3);
    for v in &result {
        assert!(!v.address.is_empty());
        assert!(v.pubkey.starts_with("0x"));
        assert_eq!(v.status, "candidate");
    }

    handle.stop().unwrap();
}

#[tokio::test]
async fn torus_get_validators_empty() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    let result: Vec<RpcValidatorInfo> = client
        .request("torus_getValidators", rpc_params![])
        .await
        .unwrap();

    assert!(result.is_empty());
    handle.stop().unwrap();
}

#[tokio::test]
async fn torus_get_staking_info() {
    let (_dir, state, mempool, executor) = setup();
    let staking = StakingManager::new(state.clone());
    let validator = addr(1);
    let delegator = addr(2);

    fund(&state, &validator, wei(100_000));
    fund(&state, &delegator, wei(100_000));

    staking
        .register_validator(validator, [1u8; 32], 500, wei(10_000))
        .unwrap();
    staking.delegate(delegator, validator, wei(5_000)).unwrap();
    staking.permanent_stake(delegator, wei(3_000), 100).unwrap();
    staking.credit_rewards(delegator, wei(200)).unwrap();

    let (handle, saddr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{saddr}"))
        .unwrap();

    let delegator_hex = hex_address(delegator);
    let result: RpcStakingInfo = client
        .request("torus_getStakingInfo", rpc_params![delegator_hex])
        .await
        .unwrap();

    assert_eq!(result.delegated.len(), 1);
    assert_ne!(result.permanent_stake, "0x0");
    assert_ne!(result.pending_rewards, "0x0");
    assert!(result.unbonding.is_empty());

    handle.stop().unwrap();
}

#[tokio::test]
async fn torus_get_staking_info_no_staking() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, saddr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{saddr}"))
        .unwrap();

    let nobody = hex_address(addr(99));
    let result: RpcStakingInfo = client
        .request("torus_getStakingInfo", rpc_params![nobody])
        .await
        .unwrap();

    assert!(result.delegated.is_empty());
    assert_eq!(result.permanent_stake, "0x0");
    assert_eq!(result.pending_rewards, "0x0");
    assert!(result.unbonding.is_empty());

    handle.stop().unwrap();
}

#[tokio::test]
async fn torus_get_epoch() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, addr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    let result: RpcEpochInfo = client
        .request("torus_getEpoch", rpc_params![])
        .await
        .unwrap();

    // latest_height defaults to 0, epoch_length is 100.
    assert!(!result.current_epoch.is_empty());
    assert!(!result.epoch_length.is_empty());

    handle.stop().unwrap();
}

#[tokio::test]
async fn torus_get_delegations() {
    let (_dir, state, mempool, executor) = setup();
    let staking = StakingManager::new(state.clone());
    let v1 = addr(1);
    let v2 = addr(2);
    let delegator = addr(3);

    fund(&state, &v1, wei(50_000));
    fund(&state, &v2, wei(50_000));
    fund(&state, &delegator, wei(100_000));

    staking
        .register_validator(v1, [1u8; 32], 500, wei(10_000))
        .unwrap();
    staking
        .register_validator(v2, [2u8; 32], 500, wei(10_000))
        .unwrap();
    staking.delegate(delegator, v1, wei(20_000)).unwrap();
    staking.delegate(delegator, v2, wei(30_000)).unwrap();

    let (handle, saddr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{saddr}"))
        .unwrap();

    let delegator_hex = hex_address(delegator);
    let result: Vec<RpcDelegation> = client
        .request("torus_getDelegations", rpc_params![delegator_hex])
        .await
        .unwrap();

    assert_eq!(result.len(), 2);

    handle.stop().unwrap();
}

// ============================================================================
// Submission tests (2.9.4)
// ============================================================================

#[tokio::test]
async fn torus_submit_native_action_valid() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, saddr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{saddr}"))
        .unwrap();

    // Create a signed native action using the EIP-712 signing facility.
    let key = k256::ecdsa::SigningKey::from_slice(&[1u8; 32]).unwrap();
    let action = torus_types::NativeAction::ClaimRewards;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let signed = torus_types::eip712::sign_native_action(action, nonce, &key);

    // Serialize to JSON bytes, then hex-encode.
    let json_bytes = serde_json::to_vec(&signed).unwrap();
    let hex_str = format!("0x{}", hex::encode(&json_bytes));

    let result: String = client
        .request("torus_submitNativeAction", rpc_params![hex_str])
        .await
        .unwrap();

    assert!(result.starts_with("0x"));
    assert_eq!(result.len(), 66); // "0x" + 64 hex chars = 32 byte hash

    handle.stop().unwrap();
}

#[tokio::test]
async fn torus_submit_native_action_invalid_hex() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, saddr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{saddr}"))
        .unwrap();

    let result = client
        .request::<String, _>("torus_submitNativeAction", rpc_params!["0xZZZZZZ"])
        .await;

    assert!(result.is_err());

    handle.stop().unwrap();
}

#[tokio::test]
async fn torus_submit_native_action_invalid_encoding() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, saddr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{saddr}"))
        .unwrap();

    // Valid hex but not valid JSON/action data.
    let result = client
        .request::<String, _>(
            "torus_submitNativeAction",
            rpc_params!["0xdeadbeefdeadbeef"],
        )
        .await;

    assert!(result.is_err());

    handle.stop().unwrap();
}

// ============================================================================
// Governance tests (2.9.5)
// ============================================================================

#[tokio::test]
async fn torus_get_proposal_exists() {
    let (_dir, state, mempool, executor) = setup();
    let gov = GovernanceManager::new(state.clone());
    let staking = StakingManager::new(state.clone());
    // FIX MED-NEW-15: params must be initialized before submit_proposal.
    gov.set_governance_params(&torus_economics::governance::GovernanceParams::defaults(
        Address::ZERO,
    ))
    .unwrap();

    let proposer = addr(1);
    let validator = addr(2);

    // Proposer needs stake to submit proposal.
    fund(&state, &proposer, wei(100_000));
    fund(&state, &validator, wei(100_000));
    staking
        .register_validator(validator, [1u8; 32], 500, wei(10_000))
        .unwrap();
    staking.delegate(proposer, validator, wei(5_000)).unwrap();

    let id = gov
        .submit_proposal(
            proposer,
            "Test Proposal".to_string(),
            "This is a test proposal.".to_string(),
            None,
            100,
        )
        .unwrap();

    let (handle, saddr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{saddr}"))
        .unwrap();

    let result: Option<RpcProposal> = client
        .request("torus_getProposal", rpc_params![id])
        .await
        .unwrap();

    let p = result.unwrap();
    assert_eq!(p.id, id);
    assert_eq!(p.title, "Test Proposal");
    assert_eq!(p.description, "This is a test proposal.");
    assert_eq!(p.status, "Active");
    assert_eq!(p.proposal_type, "TextProposal");
    assert_eq!(p.proposer, hex_address(proposer));

    handle.stop().unwrap();
}

#[tokio::test]
async fn torus_get_proposal_not_found() {
    let (_dir, state, mempool, executor) = setup();
    let (handle, saddr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{saddr}"))
        .unwrap();

    let result: Option<RpcProposal> = client
        .request("torus_getProposal", rpc_params![999u64])
        .await
        .unwrap();

    assert!(result.is_none());

    handle.stop().unwrap();
}

#[tokio::test]
async fn torus_get_proposals_by_status_and_all() {
    let (_dir, state, mempool, executor) = setup();
    let gov = GovernanceManager::new(state.clone());
    let staking = StakingManager::new(state.clone());
    // FIX MED-NEW-15: params must be initialized before submit_proposal.
    gov.set_governance_params(&torus_economics::governance::GovernanceParams::defaults(
        Address::ZERO,
    ))
    .unwrap();

    let proposer = addr(1);
    let validator = addr(2);

    fund(&state, &proposer, wei(100_000));
    fund(&state, &validator, wei(100_000));
    staking
        .register_validator(validator, [1u8; 32], 500, wei(10_000))
        .unwrap();
    staking.delegate(proposer, validator, wei(5_000)).unwrap();

    // Create 3 proposals (all start as Active).
    gov.submit_proposal(
        proposer,
        "Proposal A".to_string(),
        "Desc A".to_string(),
        None,
        100,
    )
    .unwrap();
    gov.submit_proposal(
        proposer,
        "Proposal B".to_string(),
        "Desc B".to_string(),
        None,
        100,
    )
    .unwrap();
    gov.submit_proposal(
        proposer,
        "Proposal C".to_string(),
        "Desc C".to_string(),
        None,
        100,
    )
    .unwrap();

    let (handle, saddr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{saddr}"))
        .unwrap();

    // Get all proposals (no status filter).
    let all: Vec<RpcProposal> = client
        .request("torus_getProposals", rpc_params![Option::<String>::None])
        .await
        .unwrap();
    assert_eq!(all.len(), 3);

    // Get by status "Active".
    let active: Vec<RpcProposal> = client
        .request("torus_getProposals", rpc_params!["Active"])
        .await
        .unwrap();
    assert_eq!(active.len(), 3);

    // Get by status "Passed" — none should match.
    let passed: Vec<RpcProposal> = client
        .request("torus_getProposals", rpc_params!["Passed"])
        .await
        .unwrap();
    assert!(passed.is_empty());

    handle.stop().unwrap();
}

#[tokio::test]
async fn torus_get_governance_params() {
    let (_dir, state, mempool, executor) = setup();

    // FIX MED-NEW-15: Governance params must be initialized before querying.
    let gov = GovernanceManager::new(state.clone());
    gov.set_governance_params(&torus_economics::governance::GovernanceParams::defaults(
        Address::ZERO,
    ))
    .unwrap();

    let (handle, saddr) = start_server(state, mempool, executor).await;
    let client = HttpClientBuilder::default()
        .build(format!("http://{saddr}"))
        .unwrap();

    let result: RpcGovernanceParams = client
        .request("torus_getGovernanceParams", rpc_params![])
        .await
        .unwrap();

    assert!(!result.voting_period_blocks.is_empty());
    assert!(!result.quorum_bps.is_empty());
    assert!(!result.min_proposal_stake.is_empty());
    assert_eq!(result.permanent_weight_multiplier, "3/2");

    handle.stop().unwrap();
}
