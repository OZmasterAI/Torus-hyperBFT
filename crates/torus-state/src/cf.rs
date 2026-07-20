//! Column family definitions (tech-requirements.md section 6.1).

// EVM state
pub const CF_ACCOUNTS: &str = "cf_accounts";
pub const CF_STORAGE: &str = "cf_storage";
pub const CF_CODE: &str = "cf_code";

// Block data
pub const CF_BLOCK_HEADERS: &str = "cf_block_headers";
pub const CF_BLOCK_BODIES: &str = "cf_block_bodies";
pub const CF_BLOCK_HASH_TO_NUMBER: &str = "cf_block_hash_to_number";

/// Durable COMMIT MANIFEST keyed by 8-byte BE height. Value: the committed
/// consensus datum bytes verbatim (a `bincode(CompactBlock)` under compact
/// proposals, or `bincode(TorusBlock)` under full proposals).
///
/// Written as the FIRST durable action on every commit callback
/// (`on_committed_block`), BEFORE the body is reconstructed/persisted. Its sole
/// purpose is crash-recovery of the HEAL CHANNEL: a `CompactBlock` carries the
/// `native_action_hashes`, which a boot-parked body-hole needs to content-address
/// the missing bodies from PEERS (`/torus/native-da/1.0`). The persisted header
/// (`CF_BLOCK_HEADERS`) records only `native_action_count`, NOT the hashes, so
/// without this manifest a boot-parked hole cannot drive a peer pull and can only
/// (futilely) wait for a local durable body — the silent-stall root cause
/// (t12-r3-full: `execution stalled at height 1165 ... missing=0`). Pruned at
/// dispatch once the body is durable (`CF_BLOCK_BODIES`), so it stays bounded to
/// the committed-but-not-yet-executed window.
pub const CF_COMMIT_MANIFEST: &str = "cf_commit_manifest";

// Receipts and logs
pub const CF_RECEIPTS: &str = "cf_receipts";
/// Reserved for future eth_getLogs indexing. Currently unpopulated.
/// AUDIT: EVM-FIND-21 -- full log/bloom indexing deferred to post-launch optimisation.
pub const CF_LOGS: &str = "cf_logs";
/// Reserved for future eth_getLogs bloom indexing. Currently unpopulated.
/// AUDIT: EVM-FIND-21 -- full log/bloom indexing deferred to post-launch optimisation.
pub const CF_LOGS_BLOOM: &str = "cf_logs_bloom";
pub const CF_TX_HASH_TO_LOCATION: &str = "cf_tx_hash_to_location";

// Native exchange state
pub const CF_NATIVE_ORDERS: &str = "cf_native_orders";
pub const CF_NATIVE_POSITIONS: &str = "cf_native_positions";
pub const CF_NATIVE_BALANCES: &str = "cf_native_balances";
pub const CF_NATIVE_ORDER_BOOKS: &str = "cf_native_order_books";
pub const CF_NATIVE_MARKETS: &str = "cf_native_markets";
/// 3c (level-rows-as-authority, `TORUS_BOOK_ROWS=2`): NODE-LOCAL full order-row
/// store. Key/value schema = the C4 order row, byte-identical:
/// `market_id(8 BE) ‖ 0x01 ‖ order_id(16 BE)` -> `seq(8 BE) ‖ borsh(Order)`.
///
/// DETERMINISM RULE: this CF is a deterministic materialization of consensus
/// state (every validator holds identical bytes) but it is NODE-LOCAL by
/// classification — it MUST NOT be in `NATIVE_ROOT_CFS`, is never bucketed /
/// mirrored / hashed, and no reader may feed it into a root preimage except by
/// way of the boot-verified book rebuild (level-hash byte-compare). Written
/// through the same `NativeStateOverlay` atomic batch as the root CFs, trie,
/// mirror and applied-height marker, so a crash cannot split root state from
/// the order store.
pub const CF_BOOK_ORDER_ROWS: &str = "cf_book_order_rows";

// Staking
pub const CF_STAKING_VALIDATORS: &str = "cf_staking_validators";
pub const CF_STAKING_DELEGATIONS: &str = "cf_staking_delegations";
pub const CF_STAKING_PERMANENT: &str = "cf_staking_permanent";
pub const CF_STAKING_REWARDS: &str = "cf_staking_rewards";

// Governance
pub const CF_GOVERNANCE_PROPOSALS: &str = "cf_governance_proposals";
pub const CF_GOVERNANCE_VOTES: &str = "cf_governance_votes";
pub const CF_FEE_CONFIG: &str = "cf_fee_config";
pub const CF_TREASURY: &str = "cf_treasury";
pub const CF_DEV_POOL: &str = "cf_dev_pool";

// Oracle
pub const CF_NATIVE_ORACLE: &str = "cf_native_oracle";
pub const CF_NATIVE_TRADES: &str = "cf_native_trades";
pub const CF_NATIVE_USER_TRADES: &str = "cf_native_user_trades";

// Slashing & Jailing (Phase 3: 3.1)
pub const CF_SLASH_RECORDS: &str = "cf_slash_records";
pub const CF_JAIL_VOTES: &str = "cf_jail_votes";

// Replay protection (FIX ECON-FIND-03)
/// Consumed EIP-712 nonces. Key: sender(20) ++ nonce(8 BE). Value: block_height(8 BE).
pub const CF_NATIVE_NONCES: &str = "cf_native_nonces";

/// Build the [`CF_NATIVE_NONCES`] key for a `(sender, nonce)` pair: sender(20) ++ nonce(8 BE).
///
/// Single source of truth for the replay-protection key layout. Used by the
/// live-execution replay guard and nonce-write loop (torus-consensus app.rs) and the
/// sync/catchup validator (torus-bridge validator.rs); keep them in lock-step via this
/// helper so the key format can never silently drift between read and write paths.
pub fn native_nonce_key(sender: &alloy_primitives::Address, nonce: u64) -> [u8; 28] {
    let mut key = [0u8; 28];
    key[..20].copy_from_slice(sender.as_slice());
    key[20..28].copy_from_slice(&nonce.to_be_bytes());
    key
}

// Native-action data-availability (DA) body store (Phase C: native-action DA)
/// Durable native-action bodies keyed by 32-byte action-hash. Value: bincode(SignedNativeAction).
///
/// DECOUPLED from the 60s nonce-staleness gate (NONCE_WINDOW_MS) that gates mempool
/// admission, so a body referenced by a proposed/committed CompactBlock is always
/// reconstructable even after the nonce window expires or the process restarts
/// (livelock root cause, mem 28e1a821). Push-primary, fetch-rare: mirrored on every
/// ingest/produce/receive path; served by-hash on a miss via /torus/native-da/1.0.
pub const CF_NATIVE_PENDING: &str = "cf_native_pending";

/// Erasure-shard custody store (Sprint 5 T3.1, recovery-path scaffold).
/// Key: `body_hash(32) ++ shard_index(2 BE)` -> `bincode(StoredShard)` (shard
/// bytes + Merkle proof + erasure-root + `(k, n)` + `body_len`). Populated by
/// the shard-encode path and served over `/torus/native-da-shards/1.0`; a
/// reconstructing node verifies each shard's proof, rebuilds from any `k`, and
/// falls back to the whole-body `CF_NATIVE_PENDING` pull on `< k`. Additive:
/// the whole-body path is unchanged.
/// TODO(T3.1): the writer/serve wiring is deferred (see erasure.rs + codec.rs
/// scaffold); this CF is registered now so mixed-binary peers agree on the DB
/// layout ahead of the protocol landing.
pub const CF_NATIVE_SHARDS: &str = "cf_native_shards";

// Session keys
/// Session key storage. Key: ed25519 pubkey (32 bytes). Value: JSON-encoded SessionData.
pub const CF_SESSIONS: &str = "cf_sessions";

// Other
pub const CF_CORE_WRITER_QUEUE: &str = "cf_core_writer_queue";
pub const CF_CONSENSUS_META: &str = "cf_consensus_meta";

/// Key in CF_CONSENSUS_META: last block height where native post-commit completed.
pub const META_NATIVE_APPLIED_HEIGHT: &[u8] = b"native_applied_height";

// Trie (MPT state root)
pub const CF_TRIE_NODES: &str = "cf_trie_nodes";
/// Account-trie branch nodes for reth's `StateRoot`. Key: path nibbles (1 byte/nibble),
/// value: encoded `BranchNodeCompact`. Phase A (incremental state root).
pub const CF_TRIE_ACCOUNTS: &str = "cf_trie_accounts";
/// Storage-trie branch nodes. Key: keccak(address)(32) ++ path nibbles, value: branch node.
/// The hashed-address prefix isolates each account's storage trie. Phase A.
pub const CF_TRIE_STORAGE: &str = "cf_trie_storage";

// Hashed-state mirrors (keccak-ordered) — back reth's `StateRoot` hashed cursors (Phase A).
/// keccak(address)(32) -> reth `Account` (nonce, balance, bytecode_hash). Keccak-ordered so a
/// RocksDB iterator yields accounts in Ethereum state-trie order.
pub const CF_HASHED_ACCOUNTS: &str = "cf_hashed_accounts";
/// keccak(address)(32) ++ keccak(slot)(32) -> 32-byte big-endian storage value. Keccak-ordered.
pub const CF_HASHED_STORAGE: &str = "cf_hashed_storage";

// Native incremental state root — bucketed Merkle tree (Phase A, Stage A2).
/// Persisted native bucketed-Merkle tree nodes. Keys: leaf `0x00 ++ bucket(2 BE)`, internal
/// `0x01 ++ level(1) ++ index(2 BE)`, root marker `0x02`. Only non-default nodes are stored.
/// Replaces the O(total) flat keccak over the 6 native CFs with an O(changed)/block root.
pub const CF_NATIVE_TRIE: &str = "cf_native_trie";
/// Bucket-ordered mirror of the 6 native-root CFs (analog of `CF_HASHED_*` for the EVM trie).
/// Key: `bucket_id(2 BE) ++ cf_tag(1) ++ native_key` -> `keccak256(native value)` (32 B —
/// hash-only mirror, 3c preimage round). A prefix-scan on a 2-byte bucket id yields that bucket's
/// members in `(cf_tag, key)` order, so a changed bucket re-hashes in O(bucket) instead of
/// O(total). Phase A.
pub const CF_NATIVE_HASHED: &str = "cf_native_hashed";

/// All column family names. RocksDB requires these at open time.
pub const ALL_CF_NAMES: &[&str] = &[
    CF_ACCOUNTS,
    CF_STORAGE,
    CF_CODE,
    CF_BLOCK_HEADERS,
    CF_BLOCK_BODIES,
    CF_COMMIT_MANIFEST,
    CF_BLOCK_HASH_TO_NUMBER,
    CF_RECEIPTS,
    CF_LOGS,
    CF_LOGS_BLOOM,
    CF_TX_HASH_TO_LOCATION,
    CF_NATIVE_ORDERS,
    CF_NATIVE_POSITIONS,
    CF_NATIVE_BALANCES,
    CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_MARKETS,
    CF_STAKING_VALIDATORS,
    CF_STAKING_DELEGATIONS,
    CF_STAKING_PERMANENT,
    CF_STAKING_REWARDS,
    CF_GOVERNANCE_PROPOSALS,
    CF_GOVERNANCE_VOTES,
    CF_FEE_CONFIG,
    CF_TREASURY,
    CF_DEV_POOL,
    CF_NATIVE_ORACLE,
    CF_NATIVE_TRADES,
    CF_NATIVE_USER_TRADES,
    CF_SLASH_RECORDS,
    CF_JAIL_VOTES,
    CF_NATIVE_NONCES,
    CF_NATIVE_PENDING,
    CF_NATIVE_SHARDS,
    CF_SESSIONS,
    CF_CORE_WRITER_QUEUE,
    CF_CONSENSUS_META,
    CF_TRIE_NODES,
    CF_TRIE_ACCOUNTS,
    CF_TRIE_STORAGE,
    CF_HASHED_ACCOUNTS,
    CF_HASHED_STORAGE,
    CF_NATIVE_TRIE,
    CF_NATIVE_HASHED,
    CF_BOOK_ORDER_ROWS,
];
