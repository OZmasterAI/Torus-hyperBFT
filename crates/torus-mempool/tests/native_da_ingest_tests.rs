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
    //
    //    P3 Round-2 (scope 1): the DA mirror was hoisted AHEAD of this verify
    //    FIFO into the off-loop mirror worker (`mirror_native_to_da` at network
    //    receipt). This test models that pipeline: mirror stage FIRST, then the
    //    admit/verify stage — the stale body is DA-resident before admission runs
    //    and stays so even though admission rejects it (mem 28e1a821).
    let stale = SignedNativeAction {
        action: NativeAction::CancelOrder { order_id: 9 },
        nonce: now.saturating_sub(120_000), // ~2 min old -> past the 60s window
        signature: sig(),
    };
    let stale_hash = compute_action_hash(&stale);
    // mirror-worker stage (network receipt): body becomes DA-resident up front.
    pool.mirror_native_to_da(std::slice::from_ref(&stale));
    assert!(
        pool.get_native_da(&stale_hash).is_some(),
        "stale body must be DA-resident from the receipt mirror stage, BEFORE verify"
    );
    // verify-worker stage: admission rejects the stale body (spam gate intact).
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
    // mirror-worker stage, then admit stage.
    pool.mirror_native_to_da(std::slice::from_ref(&g));
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

/// P3 Round-2 scope 1 (mirror worker + decode off-loop) invariant: a gossiped
/// body is DA-readable (`get_native_da`) the moment the off-loop mirror worker
/// mirrors it AT RECEIPT — BEFORE the (slow) verify FIFO ever runs on it. This
/// is the availability-starvation fix: replicas can reconstruct a
/// block-referenced body out-of-band even while the ingest verify queue is
/// deeply backed up. Models the two-stage worker pipeline (mirror stage → verify
/// stage) at the mempool API level. Availability ≠ validity: the mirror never
/// admits the body to the pool without the crypto verify.
#[test]
fn gossip_body_da_resident_before_verify() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = StateDb::open(dir.path()).expect("open db");
    let pool = Mempool::new(state, MempoolConfig::default());
    let now = now_ms();
    let sender = Address::repeat_byte(0xCD);

    let body = SignedNativeAction {
        action: NativeAction::ClaimRewards,
        nonce: now,
        signature: sig(),
    };
    let hash = compute_action_hash(&body);

    // STAGE 1 — mirror worker (network receipt): mirror the raw decoded body.
    // No verify has run yet; the body is NOT in the pool.
    pool.mirror_native_to_da(std::slice::from_ref(&body));
    assert!(
        pool.get_native_da(&hash).is_some(),
        "body must be DA-reconstructable immediately after the receipt mirror, \
         BEFORE any verify runs"
    );
    assert!(
        pool.get_native_by_hash(&hash).is_none(),
        "availability != validity: the mirror must NOT admit the body to the pool"
    );

    // STAGE 2 — verify worker: only now does crypto verify + pool admission run.
    // (Uses the trusted path to keep the unit test signature-agnostic; the point
    // is that DA-residence was already established in stage 1, decoupled.)
    pool.add_native_action_from_gossip_trusted(sender, body)
        .expect("fresh body admits after verify stage");
    assert!(
        pool.get_native_da(&hash).is_some(),
        "body stays DA-resident across the verify stage"
    );
    assert!(
        pool.get_native_by_hash(&hash).is_some(),
        "verified fresh body is now in the pool too"
    );
}
