//! Integration tests for the durable native-action data-availability (DA) store.
//!
//! Verifies the store is keyed by action-hash, durable across a reopen, and
//! DECOUPLED from the 60s nonce-staleness gate: a body referenced by a proposed
//! or committed CompactBlock must always be reconstructable, even after the nonce
//! window expires or the process restarts (livelock root cause, mem 28e1a821).

use alloy_primitives::B256;

use torus_state::db::StateDb;
use torus_state::NativeDaStore;
use torus_types::{
    compute_action_hash, ActionSignature, NativeAction, Signature, SignedNativeAction,
};

fn sig() -> ActionSignature {
    ActionSignature::Eip712(Signature {
        v: 27,
        r: [0u8; 32],
        s: [0u8; 32],
    })
}

fn make_action(nonce: u64, action: NativeAction) -> SignedNativeAction {
    SignedNativeAction {
        action,
        nonce,
        signature: sig(),
    }
}

#[test]
fn native_da_store_roundtrip() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    let store = NativeDaStore::new(db);

    let action = make_action(1, NativeAction::ClaimRewards);
    let hash = compute_action_hash(&action);

    // Absent before insert.
    assert!(store.get(&hash).expect("get").is_none());

    // put -> get returns an identical body (round-trips to the same hash).
    store.put(&action).expect("put");
    let got = store.get(&hash).expect("get").expect("present after put");
    assert_eq!(got.nonce, action.nonce);
    assert_eq!(
        compute_action_hash(&got),
        hash,
        "round-tripped body must hash identically"
    );

    // Unknown hash -> None.
    assert!(store.get(&B256::ZERO).expect("get").is_none());
}

#[test]
fn native_da_store_roundtrip_stores_nonce_stale_body() {
    // The DA store is DECOUPLED from the 60s nonce gate: a body whose
    // millisecond-timestamp nonce is far in the past (mempool admission would
    // reject it) must still store + fetch, so a block-referenced body is always
    // reconstructable.
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    let store = NativeDaStore::new(db);

    let ancient_nonce = 1u64; // ms-timestamp ~at epoch -> decades stale
    let action = make_action(ancient_nonce, NativeAction::ClaimRewards);
    let hash = compute_action_hash(&action);

    store.put(&action).expect("put stale");
    assert!(
        store.get(&hash).expect("get").is_some(),
        "nonce-stale body must remain fetchable (gate decoupling)"
    );
}

#[test]
fn native_da_store_roundtrip_survives_reopen() {
    // Durability: a body survives a process restart (CF-backed, not RAM-only).
    let dir = tempfile::tempdir().expect("create tempdir");
    let action = make_action(42, NativeAction::ClaimRewards);
    let hash = compute_action_hash(&action);

    {
        let db = StateDb::open(dir.path()).expect("open db");
        let store = NativeDaStore::new(db);
        store.put(&action).expect("put");
    } // db dropped -> simulates a process restart

    let db = StateDb::open(dir.path()).expect("reopen db");
    let store = NativeDaStore::new(db);
    assert!(
        store.get(&hash).expect("get").is_some(),
        "body must survive a store reopen"
    );
}

#[test]
fn native_da_store_roundtrip_remove_deletes() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    let store = NativeDaStore::new(db);

    let a1 = make_action(1, NativeAction::ClaimRewards);
    let a2 = make_action(2, NativeAction::CancelOrder { order_id: 7 });
    let h1 = compute_action_hash(&a1);
    let h2 = compute_action_hash(&a2);
    store.put(&a1).expect("put a1");
    store.put(&a2).expect("put a2");

    store.remove(&[h1]).expect("remove h1");
    assert!(store.get(&h1).expect("get").is_none(), "removed body gone");
    assert!(
        store.get(&h2).expect("get").is_some(),
        "untouched body remains"
    );

    // Removing an absent hash is a no-op (idempotent).
    store.remove(&[B256::ZERO]).expect("remove absent is ok");
}
