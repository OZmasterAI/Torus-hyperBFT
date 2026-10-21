//! s134 P1 (Codex): EIP-712 signature substitution between governance actions.
//!
//! Before the fix, a `ProposalAction` hashed its bare fields with no type
//! hash, so `ValidatorRegistration(0x..01)` and `DelistMarket(1)` had the
//! same `actionHash` and one signed proposal could be swapped for the other
//! without invalidating the signature.
//!
//! These tests check the encoding against an independent EIP-712
//! implementation (`alloy-sol-types` `sol!` structs): every struct is
//! `keccak(typeHash || encodeData)`, nested structs are hashed with their own
//! type hash, and every type hash is distinct. They also prove the
//! cross-variant substitution fails.

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::{sol, SolStruct};
use k256::ecdsa::SigningKey;

use torus_types::eip712::{eip712_struct_hash, sign_native_action_for_chain, TORUS_CHAIN_ID};
use torus_types::{
    FixedPoint, NativeAction, OracleSubmission, OrderType, PlaceOrderParams, Proposal,
    ProposalAction, PublicKey, SessionScope, TimeInForce, VoteOption,
};

const N: u64 = 1_700_000_000_000;

sol! {
    // ---- governance action variants (SubmitProposal.actionHash) ----
    struct ProposalUpdateMarketParams { uint64 marketId; MarketParams params; }
    struct ProposalListMarket { MarketListing listing; }
    struct ProposalDelistMarket { uint64 marketId; }
    struct ProposalParameterChange { string key; string value; }
    struct ProposalValidatorRegistration { address candidate; }
    struct ProposalSetDelistClosePrice { uint64 marketId; int128 price; }

    // ---- nested structs ----
    struct MarketParams { int128 tickSize; int128 lotSize; uint32 maxLeverage; uint32 maintenanceMarginBps; uint32 maxFundingRateBps; }
    struct MarketListing { string baseAsset; string quoteAsset; int128 tickSize; int128 lotSize; uint32 maxLeverage; uint32 maintenanceMarginBps; }

    // ---- top-level native actions ----
    struct PlaceOrder { uint64 marketId; bool isBuy; int128 price; int128 quantity; uint8 orderType; uint8 timeInForce; bool reduceOnly; uint128 clientOrderId; bool hasClientOrderId; uint64 nonce; }
    struct PlaceOrderItem { uint64 marketId; bool isBuy; int128 price; int128 quantity; uint8 orderType; uint8 timeInForce; bool reduceOnly; uint128 clientOrderId; bool hasClientOrderId; }
    struct PlaceOrderBatch { bytes32 ordersHash; uint64 count; uint64 nonce; }
    struct CancelOrder { uint128 orderId; uint64 nonce; }
    struct CancelAllOrders { uint64 marketId; bool hasMarketId; uint64 nonce; }
    struct ModifyOrder { uint128 orderId; int128 newPrice; bool hasNewPrice; int128 newQty; bool hasNewQty; uint64 nonce; }
    struct TransferToPerp { uint256 amount; uint64 nonce; }
    struct TransferToSpot { uint256 amount; uint64 nonce; }
    struct Withdraw { uint256 amount; address to; uint64 nonce; }
    struct Delegate { address validator; uint256 amount; uint64 nonce; }
    struct Undelegate { address validator; uint256 amount; uint64 nonce; }
    struct PermanentStake { uint256 amount; uint64 nonce; }
    struct ClaimRewards { uint64 nonce; }
    struct TopUpSelfStake { uint256 amount; uint64 nonce; }
    struct SubmitProposal { string title; string description; bytes32 actionHash; uint64 nonce; }
    struct Vote { uint64 proposalId; uint8 option; uint64 nonce; }
    struct SubmitOraclePrices { bytes32 pricesHash; uint64 timestamp; uint64 nonce; }
    struct RegisterValidator { bytes32 pubkey; uint16 commission; uint64 nonce; }
    struct UpdateCommission { uint16 newRate; uint64 nonce; }
    struct JailVote { address target; uint64 nonce; }
    struct UnjailSelf { uint64 nonce; }
    struct RotateValidatorKey { bytes32 newPubkey; uint64 nonce; }
    struct CreateSession { bytes32 sessionPubkey; uint64 expiry; uint8 scope; uint64 nonce; }
    struct RevokeSession { bytes32 sessionPubkey; uint64 nonce; }
    struct UpdateMarketParams { uint64 marketId; MarketParams params; uint64 nonce; }
    struct ListMarket { MarketListing listing; uint64 nonce; }
    struct DelistMarket { uint64 marketId; uint64 nonce; }
    struct AttestStateHash { uint64 height; bytes32 stateHash; uint64 nonce; }
    struct ClaimUnbonded { uint64 nonce; }
    struct SetOracleSigner { address signer; bytes32 proofHash; uint64 nonce; }
    struct CancelByCloid { uint64 marketId; uint128 cloid; uint64 nonce; }
    struct EvmUserModify { bool usingBigBlocks; uint64 nonce; }
}

/// The type hash `keccak(encodeType)` of a reference struct.
fn th<T: SolStruct>() -> B256 {
    keccak256(T::eip712_encode_type().as_bytes())
}

fn fp(raw: i128) -> FixedPoint {
    FixedPoint::from_raw(raw)
}

fn params() -> torus_types::MarketParams {
    torus_types::MarketParams {
        tick_size: fp(1_000),
        lot_size: fp(-7),
        max_leverage: 20,
        maintenance_margin_bps: 250,
        max_funding_rate_bps: 75,
    }
}

fn listing() -> torus_types::MarketListing {
    torus_types::MarketListing {
        base_asset: "BTC".into(),
        quote_asset: "USD".into(),
        tick_size: fp(100),
        lot_size: fp(1),
        max_leverage: 50,
        maintenance_margin_bps: 100,
    }
}

fn ref_params() -> MarketParams {
    MarketParams {
        tickSize: 1_000,
        lotSize: -7,
        maxLeverage: 20,
        maintenanceMarginBps: 250,
        maxFundingRateBps: 75,
    }
}

fn ref_listing() -> MarketListing {
    MarketListing {
        baseAsset: "BTC".into(),
        quoteAsset: "USD".into(),
        tickSize: 100,
        lotSize: 1,
        maxLeverage: 50,
        maintenanceMarginBps: 100,
    }
}

fn addr(last: u8) -> Address {
    let mut b = [0u8; 20];
    b[19] = last;
    Address::from(b)
}

fn submit(action: ProposalAction) -> NativeAction {
    NativeAction::SubmitProposal(Proposal {
        title: "t".into(),
        description: "d".into(),
        action,
    })
}

/// Every governance action variant with its reference `actionHash`.
fn proposal_cases() -> Vec<(&'static str, ProposalAction, B256)> {
    vec![
        (
            "UpdateMarketParams",
            ProposalAction::UpdateMarketParams { market_id: 3, params: params() },
            ProposalUpdateMarketParams { marketId: 3, params: ref_params() }.eip712_hash_struct(),
        ),
        (
            "ListMarket",
            ProposalAction::ListMarket(listing()),
            ProposalListMarket { listing: ref_listing() }.eip712_hash_struct(),
        ),
        (
            "DelistMarket",
            ProposalAction::DelistMarket { market_id: 1 },
            ProposalDelistMarket { marketId: 1 }.eip712_hash_struct(),
        ),
        (
            "ParameterChange",
            ProposalAction::ParameterChange { key: "k".into(), value: "v".into() },
            ProposalParameterChange { key: "k".into(), value: "v".into() }.eip712_hash_struct(),
        ),
        (
            "ValidatorRegistration",
            ProposalAction::ValidatorRegistration { candidate: addr(1) },
            ProposalValidatorRegistration { candidate: addr(1) }.eip712_hash_struct(),
        ),
        (
            "SetDelistClosePrice",
            ProposalAction::SetDelistClosePrice { market_id: 1, price: fp(-5) },
            ProposalSetDelistClosePrice { marketId: 1, price: -5 }.eip712_hash_struct(),
        ),
    ]
}

#[test]
fn every_proposal_action_matches_the_eip712_reference() {
    for (label, action, action_hash) in proposal_cases() {
        let ours = eip712_struct_hash(&submit(action), N);
        let reference = SubmitProposal {
            title: "t".into(),
            description: "d".into(),
            actionHash: action_hash,
            nonce: N,
        }
        .eip712_hash_struct();
        assert_eq!(ours, reference, "SubmitProposal({label}) differs from the EIP-712 reference");
    }
}

#[test]
fn proposal_action_type_hashes_are_distinct() {
    let ths = [
        th::<ProposalUpdateMarketParams>(),
        th::<ProposalListMarket>(),
        th::<ProposalDelistMarket>(),
        th::<ProposalParameterChange>(),
        th::<ProposalValidatorRegistration>(),
        th::<ProposalSetDelistClosePrice>(),
    ];
    for i in 0..ths.len() {
        for j in i + 1..ths.len() {
            assert_ne!(ths[i], ths[j], "type hash {i} == {j}");
        }
    }
}

/// The Codex pair: before the fix both action hashes were
/// `keccak(0x00..01)`.
#[test]
fn codex_pair_validator_registration_and_delist_market_do_not_collide() {
    let reg = submit(ProposalAction::ValidatorRegistration { candidate: addr(1) });
    let delist = submit(ProposalAction::DelistMarket { market_id: 1 });
    assert_ne!(eip712_struct_hash(&reg, N), eip712_struct_hash(&delist, N));
}

/// Field values chosen so the untyped encodings would line up: zero and one
/// words, empty strings, zero structs.
fn edge_actions() -> Vec<ProposalAction> {
    let zero_params = torus_types::MarketParams {
        tick_size: fp(0),
        lot_size: fp(0),
        max_leverage: 0,
        maintenance_margin_bps: 0,
        max_funding_rate_bps: 0,
    };
    let zero_listing = torus_types::MarketListing {
        base_asset: String::new(),
        quote_asset: String::new(),
        tick_size: fp(0),
        lot_size: fp(0),
        max_leverage: 0,
        maintenance_margin_bps: 0,
    };
    let mut v = Vec::new();
    for w in [0u8, 1] {
        v.push(ProposalAction::UpdateMarketParams { market_id: w as u64, params: zero_params.clone() });
        v.push(ProposalAction::DelistMarket { market_id: w as u64 });
        v.push(ProposalAction::ValidatorRegistration { candidate: addr(w) });
        v.push(ProposalAction::SetDelistClosePrice { market_id: w as u64, price: fp(0) });
        v.push(ProposalAction::SetDelistClosePrice { market_id: 0, price: fp(w as i128) });
    }
    v.push(ProposalAction::ListMarket(zero_listing));
    v.push(ProposalAction::ParameterChange { key: String::new(), value: String::new() });
    v
}

#[test]
fn edge_value_proposal_actions_never_share_a_struct_hash() {
    let actions = edge_actions();
    let hashes: Vec<B256> = actions.iter().map(|a| eip712_struct_hash(&submit(a.clone()), N)).collect();
    for i in 0..hashes.len() {
        for j in i + 1..hashes.len() {
            // Two entries describe the same action (SetDelistClosePrice 0/0).
            let same = format!("{:?}", actions[i]) == format!("{:?}", actions[j]);
            if !same {
                assert_ne!(hashes[i], hashes[j], "{:?} vs {:?}", actions[i], actions[j]);
            }
        }
    }
}

/// A signature over one governance action never recovers its signer for any
/// other variant (the substitution of the finding).
#[test]
fn substituted_governance_action_does_not_recover_the_signer() {
    let mut kb = [0u8; 32];
    kb[31] = 1;
    let key = SigningKey::from_slice(&kb).unwrap();
    let mut actions: Vec<ProposalAction> = proposal_cases().into_iter().map(|c| c.1).collect();
    actions.extend(edge_actions());
    for signed_action in &actions {
        let signed = sign_native_action_for_chain(TORUS_CHAIN_ID, submit(signed_action.clone()), N, &key);
        let signer = signed.recover_sender_for_chain(TORUS_CHAIN_ID).unwrap();
        for other in &actions {
            if format!("{signed_action:?}") == format!("{other:?}") {
                continue;
            }
            let mut swapped = signed.clone();
            swapped.action = submit(other.clone());
            let rec = swapped.recover_sender_for_chain(TORUS_CHAIN_ID);
            assert!(
                rec.map_or(true, |a| a != signer),
                "signature over {signed_action:?} recovers its signer for {other:?}"
            );
        }
    }
}

fn order(market: u64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: market,
        is_buy: true,
        price: fp(-12),
        quantity: fp(34),
        order_type: OrderType::Market,
        time_in_force: TimeInForce::IOC,
        reduce_only: true,
        client_order_id: Some(77),
    }
}

/// Every top-level native action against the reference, and every top-level
/// type hash distinct.
#[test]
fn every_native_action_matches_the_eip712_reference() {
    let amt = U256::from(123_456u64);
    let pk = [9u8; 32];
    let item = |m: u64| PlaceOrderItem {
        marketId: m,
        isBuy: true,
        price: -12,
        quantity: 34,
        orderType: 1,
        timeInForce: TimeInForce::IOC as u8,
        reduceOnly: true,
        clientOrderId: 77,
        hasClientOrderId: true,
    };
    let mut orders_acc = Vec::new();
    orders_acc.extend_from_slice(&item(1).eip712_hash_struct().0);
    orders_acc.extend_from_slice(&item(2).eip712_hash_struct().0);
    let mut prices_acc = Vec::new();
    for (m, p) in [(1u64, 5i128), (2, -6)] {
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&m.to_be_bytes());
        prices_acc.extend_from_slice(&w);
        let mut w = [if p < 0 { 0xff } else { 0 }; 32];
        w[16..].copy_from_slice(&p.to_be_bytes());
        prices_acc.extend_from_slice(&w);
    }
    let cases: Vec<(&str, NativeAction, B256, B256)> = vec![
        ("PlaceOrder", NativeAction::PlaceOrder(order(1)), {
            let r = PlaceOrder { marketId: 1, isBuy: true, price: -12, quantity: 34, orderType: 1, timeInForce: TimeInForce::IOC as u8, reduceOnly: true, clientOrderId: 77, hasClientOrderId: true, nonce: N };
            r.eip712_hash_struct()
        }, th::<PlaceOrder>()),
        ("PlaceOrderBatch", NativeAction::PlaceOrderBatch(vec![order(1), order(2)]),
            PlaceOrderBatch { ordersHash: keccak256(&orders_acc), count: 2, nonce: N }.eip712_hash_struct(),
            th::<PlaceOrderBatch>()),
        ("CancelOrder", NativeAction::CancelOrder { order_id: 5 },
            CancelOrder { orderId: 5, nonce: N }.eip712_hash_struct(), th::<CancelOrder>()),
        ("CancelAllOrders", NativeAction::CancelAllOrders { market_id: Some(4) },
            CancelAllOrders { marketId: 4, hasMarketId: true, nonce: N }.eip712_hash_struct(), th::<CancelAllOrders>()),
        ("ModifyOrder", NativeAction::ModifyOrder { order_id: 5, new_price: Some(fp(-3)), new_qty: None },
            ModifyOrder { orderId: 5, newPrice: -3, hasNewPrice: true, newQty: 0, hasNewQty: false, nonce: N }.eip712_hash_struct(),
            th::<ModifyOrder>()),
        ("TransferToPerp", NativeAction::TransferToPerp { amount: amt },
            TransferToPerp { amount: amt, nonce: N }.eip712_hash_struct(), th::<TransferToPerp>()),
        ("TransferToSpot", NativeAction::TransferToSpot { amount: amt },
            TransferToSpot { amount: amt, nonce: N }.eip712_hash_struct(), th::<TransferToSpot>()),
        ("Withdraw", NativeAction::Withdraw { amount: amt, to: addr(3) },
            Withdraw { amount: amt, to: addr(3), nonce: N }.eip712_hash_struct(), th::<Withdraw>()),
        ("Delegate", NativeAction::Delegate { validator: addr(3), amount: amt },
            Delegate { validator: addr(3), amount: amt, nonce: N }.eip712_hash_struct(), th::<Delegate>()),
        ("Undelegate", NativeAction::Undelegate { validator: addr(3), amount: amt },
            Undelegate { validator: addr(3), amount: amt, nonce: N }.eip712_hash_struct(), th::<Undelegate>()),
        ("PermanentStake", NativeAction::PermanentStake { amount: amt },
            PermanentStake { amount: amt, nonce: N }.eip712_hash_struct(), th::<PermanentStake>()),
        ("ClaimRewards", NativeAction::ClaimRewards,
            ClaimRewards { nonce: N }.eip712_hash_struct(), th::<ClaimRewards>()),
        ("TopUpSelfStake", NativeAction::TopUpSelfStake { amount: amt },
            TopUpSelfStake { amount: amt, nonce: N }.eip712_hash_struct(), th::<TopUpSelfStake>()),
        ("SubmitProposal", submit(ProposalAction::DelistMarket { market_id: 9 }),
            SubmitProposal { title: "t".into(), description: "d".into(), actionHash: ProposalDelistMarket { marketId: 9 }.eip712_hash_struct(), nonce: N }.eip712_hash_struct(),
            th::<SubmitProposal>()),
        ("Vote", NativeAction::Vote { proposal_id: 8, option: VoteOption::Abstain },
            Vote { proposalId: 8, option: 2, nonce: N }.eip712_hash_struct(), th::<Vote>()),
        ("SubmitOraclePrices", NativeAction::SubmitOraclePrices(OracleSubmission { prices: vec![(1, fp(5)), (2, fp(-6))], timestamp: 42 }),
            SubmitOraclePrices { pricesHash: keccak256(&prices_acc), timestamp: 42, nonce: N }.eip712_hash_struct(),
            th::<SubmitOraclePrices>()),
        ("RegisterValidator", NativeAction::RegisterValidator { pubkey: PublicKey(pk), commission: 500 },
            RegisterValidator { pubkey: B256::from(pk), commission: 500, nonce: N }.eip712_hash_struct(), th::<RegisterValidator>()),
        ("UpdateCommission", NativeAction::UpdateCommission { new_rate: 300 },
            UpdateCommission { newRate: 300, nonce: N }.eip712_hash_struct(), th::<UpdateCommission>()),
        ("JailVote", NativeAction::JailVote { target: addr(4) },
            JailVote { target: addr(4), nonce: N }.eip712_hash_struct(), th::<JailVote>()),
        ("UnjailSelf", NativeAction::UnjailSelf,
            UnjailSelf { nonce: N }.eip712_hash_struct(), th::<UnjailSelf>()),
        ("RotateValidatorKey", NativeAction::RotateValidatorKey { new_pubkey: PublicKey(pk) },
            RotateValidatorKey { newPubkey: B256::from(pk), nonce: N }.eip712_hash_struct(), th::<RotateValidatorKey>()),
        ("CreateSession", NativeAction::CreateSession { session_pubkey: pk, expiry: 99, scope: SessionScope::Trading },
            CreateSession { sessionPubkey: B256::from(pk), expiry: 99, scope: SessionScope::Trading as u8, nonce: N }.eip712_hash_struct(),
            th::<CreateSession>()),
        ("RevokeSession", NativeAction::RevokeSession { session_pubkey: pk },
            RevokeSession { sessionPubkey: B256::from(pk), nonce: N }.eip712_hash_struct(), th::<RevokeSession>()),
        ("UpdateMarketParams", NativeAction::UpdateMarketParams { market_id: 3, params: params() },
            UpdateMarketParams { marketId: 3, params: ref_params(), nonce: N }.eip712_hash_struct(), th::<UpdateMarketParams>()),
        ("ListMarket", NativeAction::ListMarket(listing()),
            ListMarket { listing: ref_listing(), nonce: N }.eip712_hash_struct(), th::<ListMarket>()),
        ("DelistMarket", NativeAction::DelistMarket { market_id: 1 },
            DelistMarket { marketId: 1, nonce: N }.eip712_hash_struct(), th::<DelistMarket>()),
        ("AttestStateHash", NativeAction::AttestStateHash { height: 10, hash: B256::from(pk) },
            AttestStateHash { height: 10, stateHash: B256::from(pk), nonce: N }.eip712_hash_struct(), th::<AttestStateHash>()),
        ("ClaimUnbonded", NativeAction::ClaimUnbonded,
            ClaimUnbonded { nonce: N }.eip712_hash_struct(), th::<ClaimUnbonded>()),
        ("SetOracleSigner", NativeAction::SetOracleSigner { signer: addr(6), proof: None },
            SetOracleSigner { signer: addr(6), proofHash: B256::ZERO, nonce: N }.eip712_hash_struct(), th::<SetOracleSigner>()),
        ("CancelByCloid", NativeAction::CancelByCloid { market_id: 2, cloid: 0xabc },
            CancelByCloid { marketId: 2, cloid: 0xabc, nonce: N }.eip712_hash_struct(), th::<CancelByCloid>()),
        ("EvmUserModify", NativeAction::EvmUserModify { using_big_blocks: true },
            EvmUserModify { usingBigBlocks: true, nonce: N }.eip712_hash_struct(), th::<EvmUserModify>()),
    ];
    let mut bad = Vec::new();
    for (label, action, reference, _) in &cases {
        if eip712_struct_hash(action, N) != *reference {
            bad.push(*label);
        }
    }
    assert!(bad.is_empty(), "differ from the EIP-712 reference: {bad:?}");
    for i in 0..cases.len() {
        for j in i + 1..cases.len() {
            assert_ne!(cases[i].3, cases[j].3, "type hash {} == {}", cases[i].0, cases[j].0);
        }
    }
}
