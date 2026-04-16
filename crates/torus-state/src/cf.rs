//! Column family definitions (tech-requirements.md section 6.1).

// EVM state
pub const CF_ACCOUNTS: &str = "cf_accounts";
pub const CF_STORAGE: &str = "cf_storage";
pub const CF_CODE: &str = "cf_code";

// Block data
pub const CF_BLOCK_HEADERS: &str = "cf_block_headers";
pub const CF_BLOCK_BODIES: &str = "cf_block_bodies";
pub const CF_BLOCK_HASH_TO_NUMBER: &str = "cf_block_hash_to_number";

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

// Other
pub const CF_CORE_WRITER_QUEUE: &str = "cf_core_writer_queue";
pub const CF_CONSENSUS_META: &str = "cf_consensus_meta";

// Trie (MPT state root)
pub const CF_TRIE_NODES: &str = "cf_trie_nodes";
pub const CF_TRIE_ACCOUNTS: &str = "cf_trie_accounts";
pub const CF_TRIE_STORAGE: &str = "cf_trie_storage";

/// All column family names. RocksDB requires these at open time.
pub const ALL_CF_NAMES: &[&str] = &[
    CF_ACCOUNTS,
    CF_STORAGE,
    CF_CODE,
    CF_BLOCK_HEADERS,
    CF_BLOCK_BODIES,
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
    CF_CORE_WRITER_QUEUE,
    CF_CONSENSUS_META,
    CF_TRIE_NODES,
    CF_TRIE_ACCOUNTS,
    CF_TRIE_STORAGE,
];
