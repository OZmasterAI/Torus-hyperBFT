//! O5 cross-crate size-ladder invariants (torus-network × torus-mempool).
//!
//! The per-crate ladder lives in `torus_network::caps`; these tests cover the
//! relations that cross crate boundaries: what the mempool may SELECT into a
//! block versus what the network gates will actually let DISSEMINATE. A
//! violation here is a liveness trap, not a fork risk — `validate_block`
//! rejects on none of these — but the failure mode is nasty: the receive gate
//! drops the message AND penalizes the author (swarm.rs oversized-consensus
//! path), so a proposer that legally exceeds a cap livelocks the view.

use torus_mempool::rate_limit::{
    evm_block_gas_budget, native_total_block_cap, NATIVE_ORDERS_PER_BATCH_CAP,
};
use torus_network::config::NetworkConfig;

/// EVM calldata costs >= 16 gas/byte (EIP-2028 non-zero floor), so the
/// per-block gas budget bounds selectable EVM bytes. A compact proposal
/// carries those bytes INLINE (`CompactBlock.evm_transactions`) — the worst
/// legal selection must fit the consensus accept gate, or every receiver
/// drops the proposal and penalizes the proposer while the next leader
/// re-selects the same mempool: a livelock by construction.
#[test]
fn worst_case_compact_proposal_fits_consensus_accept_gate() {
    let evm_worst = (evm_block_gas_budget() / 16) as usize; // 5M gas / 16 = 312.5 KB
    let manifest = native_total_block_cap() * 32; // 32B action hashes
    let header_slack = 4 * 1024; // header + bincode framing margin
    let worst = evm_worst + manifest + header_slack;
    let accept = NetworkConfig::default().max_consensus_message_size;
    assert!(
        worst <= accept,
        "worst-case compact proposal {worst}B exceeds consensus accept gate {accept}B \
         — receivers drop+penalize, proposer livelocks (O5)"
    );
}

/// Worst single pull chunk — `NATIVE_DA_FETCH_CHUNK` bodies, each a maximal
/// `PlaceOrderBatch` — must fit the `/torus/native-da` response codec, or the
/// pull fallback (the recovery path for everything the push path gave up on)
/// collapses into responses the receiver rejects.
#[test]
fn max_pull_chunk_fits_native_da_codec() {
    use torus_types::{
        ActionSignature, FixedPoint, NativeAction, OrderType, PlaceOrderParams, Signature,
        SignedNativeAction, TimeInForce,
    };
    let order = PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: FixedPoint::from_raw(6_000_000_000_000),
        quantity: FixedPoint::from_raw(FixedPoint::SCALE),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let batch = SignedNativeAction {
        action: NativeAction::PlaceOrderBatch(vec![order; NATIVE_ORDERS_PER_BATCH_CAP]),
        nonce: 0,
        signature: ActionSignature::Eip712(Signature {
            v: 27,
            r: [0u8; 32],
            s: [0u8; 32],
        }),
    };
    let one = bincode::serialize(&batch)
        .expect("serialize max batch")
        .len();
    let envelope_slack = 64; // per-body framing in the response
    assert!(
        torus_network::bridge::NATIVE_DA_FETCH_CHUNK * (one + envelope_slack)
            <= torus_network::caps::MAX_NATIVE_DA_MSG_SIZE,
        "a full pull chunk ({} bodies x {}B) exceeds MAX_NATIVE_DA_MSG_SIZE {}",
        torus_network::bridge::NATIVE_DA_FETCH_CHUNK,
        one,
        torus_network::caps::MAX_NATIVE_DA_MSG_SIZE
    );
}

/// r4 (block-cap-200 default + direct-push floor): the mempool's compiled
/// per-block BODY budget must stay carriable by every recovery rung of the
/// network ladder, and the shard read cap must admit the worst-case k=2 shard
/// of such a body. `torus_network::caps` pins the mempool value as a literal
/// (it cannot dep the mempool); this is the cross-crate equality that keeps
/// that mirror honest.
#[test]
fn compiled_block_bytes_cap_fits_the_dissemination_ladder() {
    use torus_mempool::rate_limit::NATIVE_BLOCK_BYTES_CAP;
    use torus_network::caps::{
        direct_push_body_bytes, DIRECT_PUSH_BODY_BYTES, MAX_BLOCK_DATA_MSG_SIZE,
        MAX_DIRECT_MSG_SIZE, MAX_NATIVE_DA_SHARDS_MSG_SIZE,
    };
    // caps.rs `shard_cap_admits_worst_case_shard` mirrors this literal.
    assert_eq!(NATIVE_BLOCK_BYTES_CAP, 12_000_000, "update the caps.rs mirror literal too");
    // A full body must sync (block-data codec) — the last-resort rung.
    assert!(NATIVE_BLOCK_BYTES_CAP + 64 * 1024 <= MAX_BLOCK_DATA_MSG_SIZE);
    // Worst-case single shard (k = f+1 = 2 at n=3) + proof + header fits the shard cap.
    assert!(NATIVE_BLOCK_BYTES_CAP / 2 + 8 * 32 + 64 <= MAX_NATIVE_DA_SHARDS_MSG_SIZE);
    // The direct-push floor (default and effective) leaves framing headroom
    // under our own direct codec cap, so a body set exactly at the floor is
    // still readable by every same-build peer.
    assert!(DIRECT_PUSH_BODY_BYTES < MAX_DIRECT_MSG_SIZE);
    assert!(direct_push_body_bytes() < MAX_DIRECT_MSG_SIZE);
}
