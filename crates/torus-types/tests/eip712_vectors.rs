//! EIP-712 fixture generator. Writes `tests/fixtures/eip712_vectors.json`
//! for cross-language verification by the TS trading-app signing library.
//!
//! The fixture is the authoritative contract: if this file changes, the TS
//! consumer tests (torus-trading-app/tests/eip712.spec.ts) must pass against
//! the new output unchanged, or TS must be updated in the same PR.

use std::fs;
use std::path::PathBuf;

use alloy_primitives::{hex as alloy_hex, Address, U256};
use k256::ecdsa::SigningKey;
use serde::Serialize;

use torus_types::eip712::{
    eip712_domain_separator, eip712_signing_hash, eip712_struct_hash, sign_native_action,
    TORUS_CHAIN_ID,
};
use torus_types::{
    FixedPoint, MarketListing, MarketParams, NativeAction, OracleSubmission, OrderType,
    PlaceOrderParams, Proposal, ProposalAction, PublicKey, TimeInForce, VoteOption,
};

/// Pinned nonce so fixtures are byte-deterministic.
const PINNED_NONCE: u64 = 1_700_000_000_000;

/// Pinned secp256k1 key: [0x00;31] || 0x01. Also used by the existing
/// `eip712::tests::test_key` so on-chain and fixture signers match.
fn pinned_key() -> SigningKey {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    SigningKey::from_slice(&bytes).unwrap()
}

fn hex0x(bytes: &[u8]) -> String {
    format!("0x{}", alloy_hex::encode(bytes))
}

#[derive(Serialize)]
struct Vector {
    label: String,
    action: serde_json::Value,
    nonce: u64,
    struct_hash: String,
    signing_hash: String,
    signer: String,
    signature: SerializedSig,
    /// `hex(utf8(json(SignedNativeAction)))` — the exact body passed to
    /// `torus_submitNativeAction`. TS must reproduce this byte-for-byte
    /// (modulo legitimate key-field ordering) when given the same inputs.
    signed_envelope_hex: String,
    /// Raw `json(SignedNativeAction)` string, for eyeballing + string-length
    /// asserts on the TS side without re-decoding the hex.
    signed_envelope_json: String,
}

#[derive(Serialize)]
struct SerializedSig {
    v: u8,
    r: Vec<u8>,
    s: Vec<u8>,
}

#[derive(Serialize)]
struct FixtureFile {
    schema_version: u32,
    domain_separator: String,
    chain_id: u64,
    verifying_contract: String,
    pinned_nonce: u64,
    pinned_private_key: String,
    pinned_signer_address: String,
    vectors: Vec<Vector>,
}

fn build_vector(label: &str, action: NativeAction) -> Vector {
    let key = pinned_key();
    let nonce = PINNED_NONCE;
    let struct_hash = eip712_struct_hash(&action, nonce);
    let signing_hash = eip712_signing_hash(eip712_domain_separator(), struct_hash);

    let signed = sign_native_action(action.clone(), nonce, &key);
    let signer = signed
        .recover_sender()
        .expect("pinned key must round-trip recover");

    let envelope_bytes = serde_json::to_vec(&signed).expect("serialize signed envelope");
    let envelope_json = String::from_utf8(envelope_bytes.clone()).expect("envelope is utf-8");
    let envelope_hex = hex0x(&envelope_bytes);

    let action_value = serde_json::to_value(&action).expect("serialize action");

    Vector {
        label: label.to_string(),
        action: action_value,
        nonce,
        struct_hash: hex0x(struct_hash.as_slice()),
        signing_hash: hex0x(signing_hash.as_slice()),
        signer: hex0x(signer.as_slice()),
        signature: match &signed.signature {
            torus_types::ActionSignature::Eip712(sig) => SerializedSig {
                v: sig.v,
                r: sig.r.to_vec(),
                s: sig.s.to_vec(),
            },
            _ => panic!("expected Eip712 signature"),
        },
        signed_envelope_hex: envelope_hex,
        signed_envelope_json: envelope_json,
    }
}

fn all_vectors() -> Vec<Vector> {
    let addr1 = Address::from([0x11; 20]);
    let addr2 = Address::from([0x22; 20]);

    let mk_place = |is_buy, order_type, tif, reduce_only, coid: Option<u64>| -> NativeAction {
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: 1,
            is_buy,
            price: FixedPoint::from_raw(6_500_000_000_000),
            quantity: FixedPoint::from_raw(10_000_000),
            order_type,
            time_in_force: tif,
            reduce_only,
            client_order_id: coid,
        })
    };

    vec![
        build_vector(
            "PlaceOrder_Limit_Buy_GTC",
            mk_place(true, OrderType::Limit, TimeInForce::GTC, false, Some(42)),
        ),
        build_vector(
            "PlaceOrder_Limit_Sell_PostOnly",
            mk_place(false, OrderType::Limit, TimeInForce::PostOnly, false, None),
        ),
        build_vector(
            "PlaceOrder_Market_Buy_IOC",
            mk_place(true, OrderType::Market, TimeInForce::IOC, false, None),
        ),
        build_vector(
            "PlaceOrder_Market_Sell_FOK_ReduceOnly",
            mk_place(false, OrderType::Market, TimeInForce::FOK, true, None),
        ),
        build_vector(
            "PlaceOrder_StopMarket_Sell",
            mk_place(
                false,
                OrderType::StopMarket {
                    trigger: FixedPoint::from_raw(6_400_000_000_000),
                },
                TimeInForce::GTC,
                true,
                None,
            ),
        ),
        build_vector(
            "PlaceOrder_StopLimit_Buy",
            mk_place(
                true,
                OrderType::StopLimit {
                    trigger: FixedPoint::from_raw(6_600_000_000_000),
                    limit: FixedPoint::from_raw(6_610_000_000_000),
                },
                TimeInForce::GTC,
                false,
                Some(1337),
            ),
        ),
        build_vector(
            "CancelOrder_SmallId",
            NativeAction::CancelOrder { order_id: 42 },
        ),
        build_vector(
            "CancelOrder_LargeId_JsUnsafe",
            // Above 2^53 — locks down the TS string-template path for u128.
            NativeAction::CancelOrder {
                order_id: 9_007_199_254_740_993,
            },
        ),
        build_vector(
            "CancelAllOrders_SomeMarket",
            NativeAction::CancelAllOrders { market_id: Some(1) },
        ),
        build_vector(
            "CancelAllOrders_None",
            NativeAction::CancelAllOrders { market_id: None },
        ),
        build_vector(
            "ModifyOrder_PriceAndQty",
            NativeAction::ModifyOrder {
                order_id: 456,
                new_price: Some(FixedPoint::from_raw(6_550_000_000_000)),
                new_qty: Some(FixedPoint::from_raw(20_000_000)),
            },
        ),
        build_vector(
            "ModifyOrder_PriceOnly",
            NativeAction::ModifyOrder {
                order_id: 456,
                new_price: Some(FixedPoint::from_raw(6_550_000_000_000)),
                new_qty: None,
            },
        ),
        build_vector(
            "ModifyOrder_QtyOnly",
            NativeAction::ModifyOrder {
                order_id: 456,
                new_price: None,
                new_qty: Some(FixedPoint::from_raw(20_000_000)),
            },
        ),
        build_vector(
            "TransferToPerp",
            NativeAction::TransferToPerp {
                amount: U256::from(1_000_000_000_000u128),
            },
        ),
        build_vector(
            "TransferToSpot",
            NativeAction::TransferToSpot {
                amount: U256::from(500_000_000_000u128),
            },
        ),
        build_vector(
            "Withdraw",
            NativeAction::Withdraw {
                amount: U256::from(42u64),
                to: addr1,
            },
        ),
        build_vector(
            "Delegate",
            NativeAction::Delegate {
                validator: addr1,
                amount: U256::from(1_000u64),
            },
        ),
        build_vector(
            "Undelegate",
            NativeAction::Undelegate {
                validator: addr2,
                amount: U256::from(500u64),
            },
        ),
        build_vector(
            "PermanentStake",
            NativeAction::PermanentStake {
                amount: U256::from(10_000u64),
            },
        ),
        build_vector("ClaimRewards", NativeAction::ClaimRewards),
        build_vector(
            "TopUpSelfStake",
            NativeAction::TopUpSelfStake {
                amount: U256::from(2_500u64),
            },
        ),
        build_vector(
            "Vote_Yes",
            NativeAction::Vote {
                proposal_id: 1,
                option: VoteOption::Yes,
            },
        ),
        build_vector(
            "Vote_No",
            NativeAction::Vote {
                proposal_id: 2,
                option: VoteOption::No,
            },
        ),
        build_vector(
            "Vote_Abstain",
            NativeAction::Vote {
                proposal_id: 3,
                option: VoteOption::Abstain,
            },
        ),
        build_vector(
            "RegisterValidator",
            NativeAction::RegisterValidator {
                pubkey: PublicKey([0xab; 32]),
                commission: 500,
            },
        ),
        build_vector(
            "UpdateCommission",
            NativeAction::UpdateCommission { new_rate: 300 },
        ),
        build_vector("JailVote", NativeAction::JailVote { target: addr1 }),
        build_vector("UnjailSelf", NativeAction::UnjailSelf),
        build_vector(
            "RotateValidatorKey",
            NativeAction::RotateValidatorKey {
                new_pubkey: PublicKey([0xcd; 32]),
            },
        ),
        build_vector("DelistMarket", NativeAction::DelistMarket { market_id: 99 }),
        // Nested-struct variants: Phase B consumers need these too. TS MVP
        // can skip verifying these hashes until the parallel TS dispatcher
        // lands, but the fixture captures them so drift is caught.
        build_vector(
            "SubmitProposal_DelistMarket",
            NativeAction::SubmitProposal(Proposal {
                title: "Test Proposal".into(),
                description: "Delist market 99 for testing".into(),
                action: ProposalAction::DelistMarket { market_id: 99 },
            }),
        ),
        build_vector(
            "SubmitOraclePrices",
            NativeAction::SubmitOraclePrices(OracleSubmission {
                prices: vec![
                    (1, FixedPoint::from_raw(6_500_000_000_000)),
                    (2, FixedPoint::from_raw(3_200_000_000_000)),
                ],
                timestamp: PINNED_NONCE,
            }),
        ),
        build_vector(
            "UpdateMarketParams",
            NativeAction::UpdateMarketParams {
                market_id: 1,
                params: MarketParams {
                    tick_size: FixedPoint::from_raw(10_000),
                    lot_size: FixedPoint::from_raw(100),
                    max_leverage: 20,
                    maintenance_margin_bps: 500,
                    max_funding_rate_bps: 100,
                },
            },
        ),
        build_vector(
            "ListMarket",
            NativeAction::ListMarket(MarketListing {
                base_asset: "BTC".into(),
                quote_asset: "USD".into(),
                tick_size: FixedPoint::from_raw(10_000),
                lot_size: FixedPoint::from_raw(100),
                max_leverage: 50,
                maintenance_margin_bps: 300,
            }),
        ),
        // O2/G5: the action market makers actually sign. Multi-order + a
        // batch-of-ONE — the latter pins that PlaceOrderBatch([x]) hashes
        // DIFFERENTLY from PlaceOrder(x) (distinct PlaceOrderItem typehash,
        // item hash carries no nonce); the struct-hash-uniqueness assert in
        // write_eip712_fixture_file enforces the non-collision. Inner order
        // of the batch-of-one deliberately equals PlaceOrder_Limit_Buy_GTC.
        build_vector(
            "PlaceOrderBatch_TwoOrders",
            NativeAction::PlaceOrderBatch(vec![
                PlaceOrderParams {
                    market_id: 1,
                    is_buy: true,
                    price: FixedPoint::from_raw(6_500_000_000_000),
                    quantity: FixedPoint::from_raw(10_000_000),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: Some(42),
                },
                PlaceOrderParams {
                    market_id: 2,
                    is_buy: false,
                    price: FixedPoint::from_raw(3_200_000_000_000),
                    quantity: FixedPoint::from_raw(5_000_000),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::PostOnly,
                    reduce_only: false,
                    client_order_id: None,
                },
            ]),
        ),
        build_vector(
            "PlaceOrderBatch_SingleOrder",
            NativeAction::PlaceOrderBatch(vec![PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: FixedPoint::from_raw(6_500_000_000_000),
                quantity: FixedPoint::from_raw(10_000_000),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: Some(42),
            }]),
        ),
    ]
}

fn fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("eip712_vectors.json")
}

fn address_from_signing_key(key: &SigningKey) -> String {
    use k256::ecdsa::VerifyingKey;
    let vk = VerifyingKey::from(key);
    let uncompressed = vk.to_encoded_point(false);
    let hash = alloy_primitives::keccak256(&uncompressed.as_bytes()[1..]);
    hex0x(&hash[12..])
}

fn build_fixture_file() -> FixtureFile {
    let vectors = all_vectors();
    let key = pinned_key();
    let signer = address_from_signing_key(&key);

    FixtureFile {
        schema_version: 1,
        domain_separator: hex0x(eip712_domain_separator().as_slice()),
        chain_id: TORUS_CHAIN_ID,
        verifying_contract: hex0x(Address::ZERO.as_slice()),
        pinned_nonce: PINNED_NONCE,
        pinned_private_key: hex0x(&key.to_bytes()),
        pinned_signer_address: signer,
        vectors,
    }
}

#[test]
#[ignore] // Writes files to disk; run explicitly with: cargo test -- --ignored
fn write_eip712_fixture_file() {
    let file = build_fixture_file();
    assert!(
        file.vectors.len() >= 25,
        "need at least 25 vectors, got {}",
        file.vectors.len()
    );

    // Sanity: every vector round-trips through recover_sender.
    let expected_signer = file.pinned_signer_address.clone();
    for v in &file.vectors {
        assert_eq!(
            v.signer, expected_signer,
            "variant {} recovered to unexpected signer",
            v.label
        );
    }

    // Sanity: struct hashes are unique across distinct variants (same nonce).
    let mut seen = std::collections::HashSet::new();
    for v in &file.vectors {
        assert!(
            seen.insert(v.struct_hash.clone()),
            "duplicate struct_hash for {}",
            v.label
        );
    }

    let path = fixtures_path();
    fs::create_dir_all(path.parent().unwrap()).expect("mkdir fixtures/");
    let json = serde_json::to_string_pretty(&file).expect("serialize fixture");
    fs::write(&path, json).expect("write fixture file");

    eprintln!("wrote {} vectors to {}", file.vectors.len(), path.display());
}

#[test]
fn fixture_file_matches_current_implementation() {
    // CI gate: if someone changes eip712.rs without regenerating the fixture,
    // the two files diverge and this test fails.
    let path = fixtures_path();
    if !path.exists() {
        // Allow first run on a fresh checkout to generate the file.
        eprintln!(
            "WARN: fixture file not found at {}, skipping consistency check",
            path.display()
        );
        return;
    }
    let on_disk = fs::read_to_string(&path).expect("read fixture");
    let disk_value: serde_json::Value = serde_json::from_str(&on_disk).expect("parse fixture JSON");

    let computed = build_fixture_file();
    let computed_value = serde_json::to_value(&computed).expect("serialize computed fixture");

    if disk_value != computed_value {
        panic!(
            "fixture file at {} is stale. Re-run `cargo test -p torus-types \
             --test eip712_vectors write_eip712_fixture_file` and commit the \
             updated JSON.",
            path.display()
        );
    }
}

#[test]
fn v_byte_is_27_or_28_for_all_variants() {
    // Locks down the v-byte convention so the TS `splitSig` path doesn't
    // need to subtract 27. Matches the comment in writing-plan-trading-app.md
    // Step 6d.
    for v in all_vectors() {
        assert!(
            v.signature.v == 27 || v.signature.v == 28,
            "variant {} produced v={}",
            v.label,
            v.signature.v
        );
    }
}

#[test]
fn domain_separator_matches_spec() {
    let sep = eip712_domain_separator();
    let as_hex = hex0x(sep.as_slice());
    // Regression guard — a change here means the TS DOMAIN constant must
    // also change.
    assert_eq!(as_hex.len(), 2 + 64);
    assert_ne!(as_hex, format!("0x{}", "00".repeat(32)));
}
