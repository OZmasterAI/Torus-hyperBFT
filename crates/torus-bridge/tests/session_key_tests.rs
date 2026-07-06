//! Tests for ed25519 session key lifecycle and authorization.

use alloy_primitives::{Address, U256};
use ed25519_dalek::Signer;
use ed25519_dalek::SigningKey as Ed25519SigningKey;
use k256::ecdsa::SigningKey;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_state::StateDb;
use torus_types::eip712::{
    eip712_domain_separator, eip712_signing_hash, eip712_struct_hash, sign_native_action,
    MAX_SESSIONS_PER_ADDRESS, MAX_SESSION_EXPIRY_MS,
};
use torus_types::{
    ActionSignature, Ed25519Sig, FixedPoint, NativeAction, OrderType, PlaceOrderParams,
    SessionData, SessionScope, SignedNativeAction, TimeInForce,
};

// ---- Test helpers ----

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn make_ctx(state_db: StateDb, timestamp: u64) -> NativeExecContext {
    NativeExecContext::new(
        state_db,
        1,         // block_height
        timestamp, // timestamp
        0,         // epoch
        100,       // epoch_length
        10,        // max_validators
        addr(99),  // proposer
        addr(200), // treasury
        addr(201), // dev_pool
    )
}

fn test_ecdsa_key() -> SigningKey {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    SigningKey::from_slice(&bytes).unwrap()
}

fn make_ed25519_key() -> Ed25519SigningKey {
    let mut bytes = [0u8; 32];
    bytes[31] = 42;
    Ed25519SigningKey::from_bytes(&bytes)
}

fn sign_with_session(
    action: NativeAction,
    nonce: u64,
    session_key: &Ed25519SigningKey,
) -> SignedNativeAction {
    let domain = eip712_domain_separator();
    let struct_hash = eip712_struct_hash(&action, nonce);
    let signing_hash = eip712_signing_hash(domain, struct_hash);

    let sig = session_key.sign(signing_hash.as_slice());
    let pubkey = session_key.verifying_key().to_bytes();

    SignedNativeAction {
        action,
        nonce,
        signature: ActionSignature::Session {
            session_pubkey: pubkey,
            sig: Ed25519Sig(sig.to_bytes()),
        },
    }
}

fn place_order_action() -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: FixedPoint::from_raw(1000 * FixedPoint::SCALE),
        quantity: FixedPoint::from_raw(10 * FixedPoint::SCALE),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

// ---- Tests ----

#[test]
fn create_session_with_ecdsa_succeeds() {
    let (_dir, state_db) = open_test_db();
    let owner = addr(1);
    let session_key = make_ed25519_key();
    let pubkey = session_key.verifying_key().to_bytes();
    // Block timestamp is SECONDS in production (header.timestamp = .as_secs());
    // expiry is MILLISECONDS (matches order-time current_time_ms / the nonce window).
    let block_ts_secs = 1_700_000_000u64;
    let expiry = block_ts_secs * 1000 + 3_600_000; // 1 hour, ms

    let mut ctx = make_ctx(state_db.clone(), block_ts_secs);
    let action = NativeAction::CreateSession {
        session_pubkey: pubkey,
        expiry,
        scope: SessionScope::Trading,
    };

    let result = NativeExecutor::execute(&mut ctx, &owner, &action);
    assert!(result.success, "create_session failed: {:?}", result.error);

    // Verify session stored in state
    let session = state_db.get_session(&pubkey).unwrap().unwrap();
    assert_eq!(session.owner, owner);
    assert_eq!(session.expiry, expiry);
    assert_eq!(session.scope, SessionScope::Trading);
    assert_eq!(session.created_at, block_ts_secs * 1000);
}

#[test]
fn create_session_seconds_block_ts_then_resolve_ms_order() {
    // Regression for the seconds/ms expiry bug: production builds the block
    // timestamp in SECONDS (header.timestamp = .as_secs()), but expiry and order
    // validation (resolve_sender / current_time_ms) are MILLISECONDS. A session
    // created via the executor under a realistic seconds block ts MUST be usable by
    // an order validated against wall-clock ms. Before the fix, create_session
    // rejected the ms expiry as "exceeds 24h maximum", the session was never written,
    // and every order failed "session key not found".
    let (_dir, state_db) = open_test_db();
    let owner = addr(1);
    let session_key = make_ed25519_key();
    let pubkey = session_key.verifying_key().to_bytes();

    let block_ts_secs = 1_700_000_000u64; // SECONDS, as a block header carries
    let now_ms = block_ts_secs * 1000; // wall-clock ms at order time
    let expiry_ms = now_ms + 3_600_000; // 1h, ms (what a client sends)

    let mut ctx = make_ctx(state_db.clone(), block_ts_secs);
    let create = NativeAction::CreateSession {
        session_pubkey: pubkey,
        expiry: expiry_ms,
        scope: SessionScope::Full,
    };
    let result = NativeExecutor::execute(&mut ctx, &owner, &create);
    assert!(
        result.success,
        "create_session rejected a valid ms expiry under a seconds block ts: {:?}",
        result.error
    );

    // The session must be usable: resolve an order against wall-clock ms.
    let order = place_order_action();
    let signed = sign_with_session(order, now_ms, &session_key);
    let resolved = signed.resolve_sender(now_ms, |pk| state_db.get_session(pk).ok().flatten());
    assert_eq!(
        resolved.unwrap(),
        owner,
        "session created under a seconds block ts must resolve at ms order time"
    );
}

#[test]
fn session_key_place_order_resolves_to_owner() {
    let (_dir, state_db) = open_test_db();
    let owner = addr(1);
    let session_key = make_ed25519_key();
    let pubkey = session_key.verifying_key().to_bytes();
    let timestamp = 1_700_000_000_000u64;

    // Store session in state
    let session_data = SessionData {
        owner,
        expiry: timestamp + 3_600_000,
        scope: SessionScope::Trading,
        created_at: timestamp,
    };
    state_db.put_session(&pubkey, &session_data).unwrap();

    // Sign PlaceOrder with session key
    let action = place_order_action();
    let signed = sign_with_session(action, timestamp, &session_key);

    // Resolve sender
    let resolved = signed.resolve_sender(timestamp, |pk| state_db.get_session(pk).ok().flatten());
    assert_eq!(resolved.unwrap(), owner);
}

#[test]
fn session_key_withdraw_rejected_scope_violation() {
    let (_dir, state_db) = open_test_db();
    let owner = addr(1);
    let session_key = make_ed25519_key();
    let pubkey = session_key.verifying_key().to_bytes();
    let timestamp = 1_700_000_000_000u64;

    let session_data = SessionData {
        owner,
        expiry: timestamp + 3_600_000,
        scope: SessionScope::Trading,
        created_at: timestamp,
    };
    state_db.put_session(&pubkey, &session_data).unwrap();

    // Try Withdraw with session key — should fail (requires EIP-712)
    let action = NativeAction::Withdraw {
        amount: U256::from(100),
        to: addr(2),
    };
    let signed = sign_with_session(action, timestamp, &session_key);

    let result = signed.resolve_sender(timestamp, |pk| state_db.get_session(pk).ok().flatten());
    assert_eq!(
        result.unwrap_err(),
        torus_types::eip712::Eip712Error::RequiresEip712
    );
}

#[test]
fn expired_session_key_rejected() {
    let (_dir, state_db) = open_test_db();
    let owner = addr(1);
    let session_key = make_ed25519_key();
    let pubkey = session_key.verifying_key().to_bytes();
    let created_at = 1_700_000_000_000u64;
    let expiry = created_at + 3_600_000;
    let current_time = expiry + 1; // 1ms after expiry

    let session_data = SessionData {
        owner,
        expiry,
        scope: SessionScope::Trading,
        created_at,
    };
    state_db.put_session(&pubkey, &session_data).unwrap();

    let action = place_order_action();
    let signed = sign_with_session(action, current_time, &session_key);

    let result = signed.resolve_sender(current_time, |pk| state_db.get_session(pk).ok().flatten());
    assert_eq!(
        result.unwrap_err(),
        torus_types::eip712::Eip712Error::SessionExpired
    );
}

#[test]
fn create_6th_session_rejected() {
    let (_dir, state_db) = open_test_db();
    let owner = addr(1);
    let block_ts_secs = 1_700_000_000u64; // SECONDS (block header)
    let expiry = block_ts_secs * 1000 + 3_600_000; // ms

    // Create 5 sessions
    for i in 0..5u8 {
        let mut key_bytes = [0u8; 32];
        key_bytes[0] = i + 10;
        let data = SessionData {
            owner,
            expiry,
            scope: SessionScope::Trading,
            created_at: block_ts_secs * 1000,
        };
        state_db.put_session(&key_bytes, &data).unwrap();
    }

    // Attempt 6th
    let session_key = make_ed25519_key();
    let pubkey = session_key.verifying_key().to_bytes();
    let action = NativeAction::CreateSession {
        session_pubkey: pubkey,
        expiry,
        scope: SessionScope::Trading,
    };

    let mut ctx = make_ctx(state_db, block_ts_secs);
    let result = NativeExecutor::execute(&mut ctx, &owner, &action);
    assert!(!result.success);
    assert!(result.error.unwrap().contains("max 5"));
}

#[test]
fn revoke_session_then_use_fails() {
    let (_dir, state_db) = open_test_db();
    let owner = addr(1);
    let session_key = make_ed25519_key();
    let pubkey = session_key.verifying_key().to_bytes();
    let timestamp = 1_700_000_000_000u64;

    // Create session
    let session_data = SessionData {
        owner,
        expiry: timestamp + 3_600_000,
        scope: SessionScope::Trading,
        created_at: timestamp,
    };
    state_db.put_session(&pubkey, &session_data).unwrap();

    // Revoke it
    let mut ctx = make_ctx(state_db.clone(), timestamp);
    let revoke_action = NativeAction::RevokeSession {
        session_pubkey: pubkey,
    };
    let result = NativeExecutor::execute(&mut ctx, &owner, &revoke_action);
    assert!(result.success, "revoke failed: {:?}", result.error);

    // Attempt to use revoked session key
    let action = place_order_action();
    let signed = sign_with_session(action, timestamp, &session_key);
    let result = signed.resolve_sender(timestamp, |pk| state_db.get_session(pk).ok().flatten());
    assert_eq!(
        result.unwrap_err(),
        torus_types::eip712::Eip712Error::SessionNotFound
    );
}

#[test]
fn session_key_cannot_create_session() {
    let (_dir, state_db) = open_test_db();
    let owner = addr(1);
    let session_key = make_ed25519_key();
    let pubkey = session_key.verifying_key().to_bytes();
    let timestamp = 1_700_000_000_000u64;

    let session_data = SessionData {
        owner,
        expiry: timestamp + 3_600_000,
        scope: SessionScope::Full,
        created_at: timestamp,
    };
    state_db.put_session(&pubkey, &session_data).unwrap();

    // Try CreateSession with session key — requires_eip712 rejects it
    let new_key = [99u8; 32];
    let action = NativeAction::CreateSession {
        session_pubkey: new_key,
        expiry: timestamp + 1_000_000,
        scope: SessionScope::Trading,
    };
    let signed = sign_with_session(action, timestamp, &session_key);

    let result = signed.resolve_sender(timestamp, |pk| state_db.get_session(pk).ok().flatten());
    assert_eq!(
        result.unwrap_err(),
        torus_types::eip712::Eip712Error::RequiresEip712
    );
}

#[test]
fn invalid_ed25519_signature_rejected() {
    let (_dir, state_db) = open_test_db();
    let owner = addr(1);
    let session_key = make_ed25519_key();
    let pubkey = session_key.verifying_key().to_bytes();
    let timestamp = 1_700_000_000_000u64;

    let session_data = SessionData {
        owner,
        expiry: timestamp + 3_600_000,
        scope: SessionScope::Trading,
        created_at: timestamp,
    };
    state_db.put_session(&pubkey, &session_data).unwrap();

    // Create action with garbage signature
    let action = place_order_action();
    let signed = SignedNativeAction {
        action,
        nonce: timestamp,
        signature: ActionSignature::Session {
            session_pubkey: pubkey,
            sig: Ed25519Sig([0xAA; 64]),
        },
    };

    let result = signed.resolve_sender(timestamp, |pk| state_db.get_session(pk).ok().flatten());
    assert_eq!(
        result.unwrap_err(),
        torus_types::eip712::Eip712Error::SessionSignatureInvalid
    );
}

#[test]
fn session_scope_transfers_only_blocks_trading() {
    let (_dir, state_db) = open_test_db();
    let owner = addr(1);
    let session_key = make_ed25519_key();
    let pubkey = session_key.verifying_key().to_bytes();
    let timestamp = 1_700_000_000_000u64;

    let session_data = SessionData {
        owner,
        expiry: timestamp + 3_600_000,
        scope: SessionScope::TransfersOnly,
        created_at: timestamp,
    };
    state_db.put_session(&pubkey, &session_data).unwrap();

    // TransferToPerp should work
    let transfer = NativeAction::TransferToPerp {
        amount: U256::from(100),
    };
    let signed = sign_with_session(transfer, timestamp, &session_key);
    let result = signed.resolve_sender(timestamp, |pk| state_db.get_session(pk).ok().flatten());
    assert_eq!(result.unwrap(), owner);

    // PlaceOrder should fail
    let order = place_order_action();
    let signed = sign_with_session(order, timestamp + 1, &session_key);
    let result = signed.resolve_sender(timestamp, |pk| state_db.get_session(pk).ok().flatten());
    assert_eq!(
        result.unwrap_err(),
        torus_types::eip712::Eip712Error::SessionScopeViolation
    );
}
