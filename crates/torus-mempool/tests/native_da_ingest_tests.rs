//! Task 2: the durable DA store is populated on every native-action ingest path,
//! INCLUDING nonce-stale gossip/push bodies that mempool admission rejects.
//!
//! Decoupling the DA store from the 60s nonce gate is the core of the livelock
//! fix: a body referenced by a proposed/committed CompactBlock must always be
//! reconstructable, even when it is too stale to (re)enter the mempool (root
//! cause, mem 28e1a821). The spam gate on mempool admission stays intact.

use alloy_primitives::Address;
use torus_mempool::{Mempool, MempoolConfig};
use torus_state::StateDb;
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[test]
fn da_store_populated_on_ingest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = StateDb::open(dir.path()).expect("open db");
    let pool = Mempool::new(state, MempoolConfig::default());
    let now = now_ms();
    let sender = Address::repeat_byte(0xAB);

    // 1. Admission path (session-presigned, fresh nonce) -> body mirrored to DA.
    let fresh = SignedNativeAction {
        action: NativeAction::ClaimRewards,
        nonce: now,
        signature: sig(),
    };
    let fresh_hash = compute_action_hash(&fresh);
    pool.add_native_action_presigned(sender, fresh)
        .expect("presigned admit");
    assert!(
        pool.get_native_da(&fresh_hash).is_some(),
        "fresh admitted body must be mirrored to the DA store"
    );

    // 2. Gossip/push receive with a NONCE-STALE body: mempool admission is
    //    rejected (spam gate intact), but the body is STILL mirrored to the DA
    //    store (decoupled) so the referencing block stays reconstructable.
    let stale = SignedNativeAction {
        action: NativeAction::CancelOrder { order_id: 9 },
        nonce: now.saturating_sub(120_000), // ~2 min old -> past the 60s window
        signature: sig(),
    };
    let stale_hash = compute_action_hash(&stale);
    let res = pool.add_native_action_from_gossip_trusted(sender, stale);
    assert!(
        res.is_err(),
        "nonce-stale gossip body is rejected from mempool admission"
    );
    assert!(
        pool.get_native_da(&stale_hash).is_some(),
        "nonce-stale body must STILL be in the DA store (nonce-gate decoupling)"
    );
    assert!(
        pool.get_native_by_hash(&stale_hash).is_none(),
        "stale body must NOT be admitted to the in-memory pool"
    );

    // 3. Gossip/push receive with a fresh body -> DA store AND admitted to pool.
    let g = SignedNativeAction {
        action: NativeAction::ClaimRewards,
        nonce: now + 1,
        signature: sig(),
    };
    let g_hash = compute_action_hash(&g);
    pool.add_native_action_from_gossip_trusted(sender, g)
        .expect("fresh gossip admit");
    assert!(
        pool.get_native_da(&g_hash).is_some(),
        "fresh gossip body in DA store"
    );
    assert!(
        pool.get_native_by_hash(&g_hash).is_some(),
        "fresh gossip body admitted to pool"
    );
}
