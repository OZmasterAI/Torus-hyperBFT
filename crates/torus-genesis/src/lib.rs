//! Genesis configuration parser and state initializer for Torus-hyperBFT.
//!
//! Parses a genesis JSON file matching the schema in technical-requirements §12,
//! then seeds the state database with pre-funded accounts, contract code,
//! storage, and validator stakes.

use std::collections::HashMap;
use std::path::Path;

use alloy_primitives::{keccak256, Address, B256, U256};
use borsh::BorshSerialize;
use ed25519_dalek::VerifyingKey;
use hotstuff_rs::types::data_types::Power;
use hotstuff_rs::types::update_sets::AppStateUpdates;
use hotstuff_rs::types::validator_set::{ValidatorSet as HsValidatorSet, ValidatorSetState};
use revm::state::AccountInfo;
use serde::Deserialize;
use tracing::info;

use torus_core::position::NativeBalance;
use torus_economics::types::{ValidatorState, ValidatorStatus};
use torus_economics::{EpochManager, GovernanceManager, GovernanceParams, StakingManager};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_STAKING_VALIDATORS};
use torus_state::db::KECCAK_EMPTY;
use torus_state::trie::compute_state_root_from_db;
use torus_state::{StateDb, StateError};
use torus_types::{
    ChainConfig, FixedPoint, PublicKey, ValidatorInfo, ValidatorSet as TorusValidatorSet,
};

pub mod digest;
pub use digest::GenesisDigest;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum GenesisError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("state error: {0}")]
    State(#[from] StateError),
    #[error("invalid hex string: {0}")]
    InvalidHex(String),
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("invalid pubkey: {0}")]
    InvalidPubkey(String),
    #[error("invalid balance/amount: {0}")]
    InvalidAmount(String),
    #[error("invalid market: {0}")]
    InvalidMarket(String),
    #[error("invalid fee_split: {0}")]
    InvalidFeeSplit(String),
    #[error("invalid evm section: {0}")]
    InvalidEvm(String),
    /// P0.0: the activation schedule does not merge (alias or built-in conflict).
    #[error("invalid activations: {0}")]
    InvalidActivations(String),
    /// R19: two spellings of one canonical key (address case / 0x prefix,
    /// storage slot "0" vs "0x0"). Refused instead of letting HashMap order
    /// pick the value.
    #[error("duplicate genesis key: {0}")]
    DuplicateKey(String),
    /// A `consensus` parameter out of its range (dead-leader skip K / j cap).
    #[error("invalid consensus section: {0}")]
    InvalidConsensus(String),
}

// ---------------------------------------------------------------------------
// Genesis JSON structures (§12)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Genesis {
    /// The network's chain id: EVM, consensus, and the EIP-712 domain every
    /// native action is signed for (so a signature never replays across
    /// networks). Absent = devnet, 7778. Testnet and mainnet need their own.
    #[serde(default = "default_chain_id")]
    pub chain_id: u64,
    pub chain_name: String,
    pub timestamp: u64,
    pub consensus: ConsensusConfig,
    pub evm: EvmConfig,
    pub economics: EconomicsConfig,
    pub validators: Vec<GenesisValidator>,
    #[serde(default)]
    pub accounts: Vec<GenesisAccount>,
    #[serde(default)]
    pub evm_alloc: HashMap<String, EvmAllocEntry>,
    #[serde(default)]
    pub markets: Vec<GenesisMarket>,
    #[serde(default)]
    pub precompiles: HashMap<String, String>,
    #[serde(default)]
    pub permanent_stakes: Vec<GenesisPermanentStake>,
    /// Native (perp-side) balances to pre-fund. Seeds `CF_NATIVE_BALANCES` so that
    /// accounts can place margined orders at genesis without a prior deposit — used
    /// by the throughput bench's market-maker senders.
    #[serde(default)]
    pub native_balances: Vec<GenesisNativeBalance>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConsensusConfig {
    pub protocol: String,
    pub epoch_length: u64,
    pub timeout_base_ms: u64,
    pub timeout_max_ms: u64,
    /// Base of the multiplicative view-timeout backoff (Task A). Consensus-
    /// liveness critical: identical on all validators. Absent in genesis → 2.
    #[serde(default = "default_backoff_factor")]
    pub backoff_factor: u32,
    /// Exponent cap of the view-timeout backoff; 0 disables backoff (runtime
    /// kill switch). Absent in genesis → 8.
    #[serde(default = "default_backoff_cap")]
    pub backoff_cap: u32,
    /// S470: exponent cap of the COMMIT-LAG view-timeout backoff (stretches
    /// deadlines while the QC frontier outruns the commit frontier — the
    /// header-pipeline commit wedge). 0 disables (kill switch). Absent in
    /// genesis → 0 (off; deadline schedule byte-identical to pre-S470).
    /// FLEET-UNIFORM: identical on all validators or liveness degrades.
    #[serde(default)]
    pub commit_lag_backoff_cap: u32,
    /// MonadBFT B3 reputation-weighted leader selection (consensus-critical:
    /// identical on all validators). Absent in genesis TOML → disabled.
    #[serde(default)]
    pub reputation_leader_selection: bool,
    /// Running state hash activation height (CHAIN-WIDE: the first height
    /// every node hashes). Absent = running hash disabled.
    #[serde(default)]
    pub state_hash_activation_height: Option<u64>,
    /// Order-book storage mode (owner s131), CHAIN-WIDE and fixed for the
    /// chain's life: `"classic"`, `"order_rows"`, `"level_authority"`,
    /// `"level_authority_chunked"` (or 0-3). Absent = Classic; any other value
    /// refuses the genesis. The node takes its mode from here.
    #[serde(default)]
    pub book_mode: torus_types::book_mode::BookMode,
    /// P0.0: `{ "<feature_name>": height }`, when each gated feature switches
    /// on (0 or 1 = from block 1). An unknown name refuses to start;
    /// `state_hash_activation_height` is the alias of `running_state_hash`.
    /// Absent = none, the same as `{}` (Codex P1, s136: an existing genesis
    /// file keeps its meaning, its digest binds the data dir). Owner s136's
    /// "hash on at genesis" is written explicitly by the generators
    /// ([`torus_types::DEFAULT_GENESIS_ACTIVATIONS`], `gen-3val-genesis.sh`).
    #[serde(default)]
    pub activations: Option<std::collections::BTreeMap<torus_types::Feature, u64>>,
    /// L6.1 window-committed Classic order books (CHAIN-WIDE, consensus-
    /// changing; docs/plans/l61-window-committed-books.md). P0.0: the alias
    /// of `activations.book_window_commit` at the running hash's height
    /// (needs the hash; an explicit other height refuses). Absent = off.
    #[serde(default)]
    pub book_window_commit: bool,
    /// Dead-leader skip (docs/plans/dead-leader-skip.md, b1): K, the run of missed leader slots
    /// that gets a validator excluded from leading at the next epoch plan. 0 = off; else
    /// 2..=64. Absent = 0 (off, owner s138). CHAIN-WIDE.
    #[serde(default = "default_leader_miss_threshold")]
    pub leader_miss_threshold: u32,
    /// Dead-leader skip: cap of the exclusion count j (re-admission floor `L * 2^j`), 0..=10.
    /// Absent = 6. CHAIN-WIDE.
    #[serde(default = "default_leader_exclusion_cap")]
    pub leader_exclusion_cap: u32,
}

fn default_leader_miss_threshold() -> u32 {
    torus_types::DEFAULT_LEADER_MISS_THRESHOLD
}

fn default_leader_exclusion_cap() -> u32 {
    torus_types::DEFAULT_LEADER_EXCLUSION_CAP
}

fn default_chain_id() -> u64 {
    torus_types::eip712::TORUS_CHAIN_ID
}

fn default_backoff_factor() -> u32 {
    2
}

fn default_backoff_cap() -> u32 {
    8
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvmConfig {
    pub spec_id: String,
    pub gas_limit: u64,
    pub base_fee_initial: u64,
    pub chain_id: u64,
    /// Item 7 step 1: N, EVM txs only in blocks with `height % N == 0` (the
    /// genesis parent header's `evm_block_interval`). Absent = 11
    /// (`DEFAULT_EVM_BLOCK_INTERVAL`); 0 is refused.
    #[serde(default = "default_evm_block_interval")]
    pub block_interval: u64,
    /// Item 7 step 2: M, big EVM blocks at `height % M == 0`; absent = 0 (off).
    #[serde(default)]
    pub big_block_interval: u64,
    /// Item 7 step 2: the big lane's gas limit; absent = 10M.
    #[serde(default = "default_evm_big_gas_limit")]
    pub big_gas_limit: u64,
}

fn default_evm_block_interval() -> u64 {
    torus_types::DEFAULT_EVM_BLOCK_INTERVAL
}

fn default_evm_big_gas_limit() -> u64 {
    torus_types::DEFAULT_EVM_BIG_GAS_LIMIT
}

#[derive(Debug, Clone, Deserialize)]
pub struct EconomicsConfig {
    #[serde(default)]
    pub treasury_address: Option<String>,
    #[serde(default)]
    pub dev_pool_address: Option<String>,
    /// The chain's gas-fee split (`ChainConfig::fee_schedule`, s140): start
    /// and end each sum to 10_000 bps. Absent = the built-in split
    /// ([`torus_types::BUILTIN_FEE_SCHEDULE`]).
    #[serde(default)]
    pub fee_split: FeeSplitConfig,
    pub permanent_staking: PermanentStakingConfig,
    pub validator: ValidatorConstraints,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeeSplitConfig {
    pub start: FeeSplit,
    pub end: FeeSplit,
    pub transition_epochs: u64,
}

impl Default for FeeSplitConfig {
    /// The built-in split ([`torus_types::BUILTIN_FEE_SCHEDULE`]).
    fn default() -> Self {
        let b = torus_types::BUILTIN_FEE_SCHEDULE;
        let shares = |s: torus_types::FeeShares| FeeSplit {
            burn: s.burn.into(),
            validator: s.validator.into(),
            treasury: s.treasury.into(),
            dev_pool: s.dev_pool.into(),
        };
        Self {
            start: shares(b.start),
            end: shares(b.end),
            transition_epochs: b.transition_epochs,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeeSplit {
    pub burn: u32,
    pub validator: u32,
    pub treasury: u32,
    pub dev_pool: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PermanentStakingConfig {
    pub annual_rate_bps: u32,
    pub governance_multiplier_bps: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidatorConstraints {
    pub min_self_delegation: String,
    pub max_validators: u32,
    pub unbonding_period_blocks: u64,
    pub max_commission_bps: u32,
    pub max_commission_change_bps: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenesisValidator {
    pub address: String,
    pub pubkey: String,
    pub stake: String,
    pub commission_bps: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenesisAccount {
    pub address: String,
    pub balance: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenesisPermanentStake {
    pub address: String,
    pub amount: String,
}

/// A perpetual market to seed into `CF_NATIVE_MARKETS` at genesis.
///
/// Stored in the exact borsh layout the matching engine + RPC reader expect:
/// `base_asset(String) + quote_asset(String) + lot_size(i128) + tick_size(i128) + initial_margin(i128)`,
/// keyed by `market_id` (8-byte big-endian). `CF_NATIVE_MARKETS` is OFF the consensus
/// native state root, so seeding it cannot diverge validators.
#[derive(Debug, Clone, Deserialize)]
pub struct GenesisMarket {
    pub market_id: u64,
    pub base_asset: String,
    pub quote_asset: String,
    /// Minimum order size, as a decimal string (e.g. "1.0"). Scaled by 10^8 (`FixedPoint`).
    pub lot_size: String,
    /// Minimum price increment, as a decimal string (e.g. "0.01"). Scaled by 10^8.
    pub tick_size: String,
    /// Initial margin requirement, as a decimal string (e.g. "5.0"). Scaled by 10^8.
    pub initial_margin: String,
}

/// A native (perp-side) balance to pre-fund into `CF_NATIVE_BALANCES`.
#[derive(Debug, Clone, Deserialize)]
pub struct GenesisNativeBalance {
    pub address: String,
    /// Available collateral, as a decimal string (e.g. "1000000000.0"). Scaled by 10^8.
    pub available: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvmAllocEntry {
    pub balance: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub storage: Option<HashMap<String, String>>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_address(s: &str) -> Result<Address, GenesisError> {
    let s = s.trim();
    s.parse::<Address>()
        .map_err(|_| GenesisError::InvalidAddress(s.to_string()))
}

/// s140 (owner): an `economics` address. Absent = the zero address (as
/// before); present but malformed = a load error naming the field, never the
/// zero address (fees and the governance treasury would go to 0x0).
fn economics_address(field: &str, value: Option<&str>) -> Result<Address, GenesisError> {
    match value {
        None => Ok(Address::ZERO),
        Some(s) => parse_address(s)
            .map_err(|_| GenesisError::InvalidAddress(format!("economics.{field}: {s:?}"))),
    }
}

fn parse_u256(s: &str) -> Result<U256, GenesisError> {
    let s = s.trim();
    if s.starts_with("0x") || s.starts_with("0X") {
        U256::from_str_radix(&s[2..], 16).map_err(|_| GenesisError::InvalidAmount(s.to_string()))
    } else {
        U256::from_str_radix(s, 10).map_err(|_| GenesisError::InvalidAmount(s.to_string()))
    }
}

/// Parse a decimal string (e.g. `"1.0"`, `"0.01"`, `"1000000000"`) into a `FixedPoint`
/// raw value scaled by `FixedPoint::SCALE` (10^8). Uses exact integer math — no floating
/// point — so every validator parsing the same genesis string derives a byte-identical
/// raw value. Rejects more than 8 fractional digits and on overflow.
fn parse_fixedpoint_raw(s: &str) -> Result<i128, GenesisError> {
    let trimmed = s.trim();
    let (neg, body) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    let (int_str, frac_str) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    if frac_str.len() > 8 {
        return Err(GenesisError::InvalidAmount(format!(
            "{s}: more than 8 fractional digits"
        )));
    }
    let parse_digits = |d: &str| -> Result<i128, GenesisError> {
        if d.is_empty() {
            Ok(0)
        } else if d.bytes().all(|b| b.is_ascii_digit()) {
            d.parse::<i128>()
                .map_err(|_| GenesisError::InvalidAmount(s.to_string()))
        } else {
            Err(GenesisError::InvalidAmount(s.to_string()))
        }
    };
    let int_val = parse_digits(int_str)?;
    // Right-pad the fraction to 8 digits: "5" -> "50000000" = 0.5 * 10^8.
    let frac_val = parse_digits(&format!("{frac_str:0<8}"))?;
    let raw = int_val
        .checked_mul(FixedPoint::SCALE)
        .and_then(|v| v.checked_add(frac_val))
        .ok_or_else(|| GenesisError::InvalidAmount(format!("{s}: overflow")))?;
    Ok(if neg { -raw } else { raw })
}

/// Decode `0x`/`0X`-prefixed or bare hex. R19: an odd digit count or a
/// non-hex (incl. non-ASCII) character is `InvalidHex`, never a slice panic.
fn decode_hex(s: &str) -> Result<Vec<u8>, GenesisError> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if !digits.len().is_multiple_of(2) {
        return Err(GenesisError::InvalidHex(format!(
            "{s}: odd number of hex digits"
        )));
    }
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(GenesisError::InvalidHex(s.to_string()));
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&digits[i..i + 2], 16)
                .map_err(|_| GenesisError::InvalidHex(s.to_string()))
        })
        .collect()
}

/// R19: `keys` mapped to their canonical form must be distinct. Keys are
/// visited in sorted order, so the same input always names the same pair.
fn check_unique_keys<'a, K: Ord>(
    what: &str,
    keys: impl Iterator<Item = &'a String>,
    canon: impl Fn(&str) -> Result<K, GenesisError>,
) -> Result<(), GenesisError> {
    let mut keys: Vec<&String> = keys.collect();
    keys.sort();
    let mut seen = std::collections::BTreeMap::new();
    for key in keys {
        if let Some(prev) = seen.insert(canon(key)?, key) {
            return Err(GenesisError::DuplicateKey(format!(
                "{what}: \"{prev}\" and \"{key}\" are the same key"
            )));
        }
    }
    Ok(())
}

/// Convert a stake amount (U256 in wei) to a consensus power value (u64).
/// Divides by 10^18 to get whole TRS, with a minimum power of 1.
fn stake_to_power(stake: U256) -> u64 {
    let trs = stake / U256::from(10u64.pow(18));
    let power: u64 = trs.try_into().unwrap_or(u64::MAX);
    power.max(1)
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl Genesis {
    /// Parse a genesis configuration from a JSON file.
    pub fn from_file(path: &Path) -> Result<Self, GenesisError> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    /// P0.0 phase 4: read the file once and return the parsed genesis and
    /// the digest of the same text ([`GenesisDigest`]).
    pub fn from_file_with_digest(path: &Path) -> Result<(Self, GenesisDigest), GenesisError> {
        let text = std::fs::read_to_string(path)?;
        Ok((Self::from_json(&text)?, GenesisDigest::of_json(&text)?))
    }

    /// Parse a genesis configuration from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, GenesisError> {
        let genesis: Self = serde_json::from_str(json)?;
        genesis.check_fee_split()?;
        if genesis.evm.block_interval == 0 {
            return Err(GenesisError::InvalidEvm(
                "evm.block_interval must be >= 1".into(),
            ));
        }
        // s138: the node runs on `chain_id`; EVM signers read `evm.chain_id`.
        if genesis.evm.chain_id != genesis.chain_id {
            return Err(GenesisError::InvalidEvm(format!(
                "evm.chain_id {} differs from chain_id {} (absent chain_id = {}); \
                 EVM transactions must be signed with chain_id, so the two must match",
                genesis.evm.chain_id,
                genesis.chain_id,
                default_chain_id()
            )));
        }
        genesis.check_keys_and_hex()?;
        genesis.activation_schedule()?;
        torus_types::DeadLeaderParams::check(
            genesis.consensus.leader_miss_threshold,
            genesis.consensus.leader_exclusion_cap,
        )
        .map_err(GenesisError::InvalidConsensus)?;
        Ok(genesis)
    }

    /// P0.0: the activation schedule of this genesis: `consensus.activations`
    /// plus the old `state_hash_activation_height` (the alias of
    /// `running_state_hash`), united with the built-in table for `chain_id`.
    /// Both alias spellings with different heights, or a built-in height that
    /// differs from the genesis one, refuse to start.
    pub fn activation_schedule(&self) -> Result<torus_types::ActivationSchedule, GenesisError> {
        use torus_types::Feature::{BookWindowCommit, RunningStateHash};
        // Codex P1 (s136): absent = none (an existing genesis file keeps its
        // meaning: the same digest must mean the same chain). The default
        // (hash + L6.1 from block 1) is written by the generators.
        let mut activations = self.consensus.activations.clone().unwrap_or_default();
        if let Some(alias) = self.consensus.state_hash_activation_height {
            if alias == 0 {
                return Err(GenesisError::InvalidActivations(
                    "state_hash_activation_height must be >= 1".into(),
                ));
            }
            match activations.get(&RunningStateHash) {
                // 0 and 1 are both block 1.
                Some(&h) if h.max(1) != alias => {
                    return Err(GenesisError::InvalidActivations(format!(
                        "state_hash_activation_height = {alias} but activations.{} = {h}",
                        RunningStateHash.name()
                    )));
                }
                Some(_) => {}
                None => {
                    activations.insert(RunningStateHash, alias);
                }
            }
        }
        // L6.1: the bool is the alias of `book_window_commit` from the
        // running hash's height (what CL21 measured and the bench harness's
        // BOOK_WINDOW_COMMIT=1 writes).
        if self.consensus.book_window_commit {
            let Some(&hash) = activations.get(&RunningStateHash) else {
                return Err(GenesisError::InvalidActivations(format!(
                    "book_window_commit = true needs the running state hash \
                     (activations.{} or state_hash_activation_height)",
                    RunningStateHash.name()
                )));
            };
            match activations.get(&BookWindowCommit) {
                Some(&h) if h.max(1) != hash.max(1) => {
                    return Err(GenesisError::InvalidActivations(format!(
                        "book_window_commit = true (from the running hash's height {hash}) but \
                         activations.{} = {h}",
                        BookWindowCommit.name()
                    )));
                }
                Some(_) => {}
                None => {
                    activations.insert(BookWindowCommit, hash);
                }
            }
        }
        if activations.contains_key(&BookWindowCommit) && !self.consensus.book_mode.is_classic() {
            return Err(GenesisError::InvalidActivations(format!(
                "{} (L6.1) needs book_mode classic, not {}",
                BookWindowCommit.name(),
                self.consensus.book_mode.name()
            )));
        }
        torus_types::ActivationSchedule::for_chain(self.chain_id, &activations)
            .map_err(GenesisError::InvalidActivations)
    }

    /// R19: every hex field decodes and every map key is canonical-unique, so
    /// a bad genesis fails at load (the node refuses to start) and
    /// `initialize` never depends on HashMap iteration order.
    fn check_keys_and_hex(&self) -> Result<(), GenesisError> {
        self.treasury_address()?;
        self.dev_pool_address()?;
        check_unique_keys("evm_alloc address", self.evm_alloc.keys(), parse_address)?;
        for (addr_str, entry) in &self.evm_alloc {
            parse_u256(&entry.balance)?;
            if let Some(code_hex) = &entry.code {
                decode_hex(code_hex)?;
            }
            if let Some(storage) = &entry.storage {
                check_unique_keys(
                    &format!("evm_alloc[{addr_str}] storage slot"),
                    storage.keys(),
                    parse_u256,
                )?;
                for value in storage.values() {
                    parse_u256(value)?;
                }
            }
        }
        // R19 follow-up (owner s112): an address in both accounts[] and
        // evm_alloc is refused instead of evm_alloc overwriting the account.
        let alloc: std::collections::BTreeMap<Address, &String> = self
            .evm_alloc
            .keys()
            .map(|k| Ok((parse_address(k)?, k)))
            .collect::<Result<_, GenesisError>>()?;
        for (i, account) in self.accounts.iter().enumerate() {
            let address = parse_address(&account.address)?;
            if let Some(alloc_key) = alloc.get(&address) {
                return Err(GenesisError::DuplicateKey(format!(
                    "address {address:#x} is in both accounts[{i}] (\"{}\") and evm_alloc \
                     (\"{alloc_key}\")",
                    account.address
                )));
            }
        }
        // Validators are keyed by address (CF_STAKING_VALIDATORS) and by
        // consensus key (the hotstuff set): both must be unique.
        let mut by_address = std::collections::BTreeMap::new();
        let mut by_pubkey = std::collections::BTreeMap::new();
        for (i, validator) in self.validators.iter().enumerate() {
            let address = parse_address(&validator.address)?;
            let pubkey = decode_hex(&validator.pubkey)?;
            if pubkey.len() != 32 {
                return Err(GenesisError::InvalidPubkey(validator.pubkey.clone()));
            }
            if let Some(j) = by_address.insert(address, i) {
                return Err(GenesisError::DuplicateKey(format!(
                    "validator address {address:#x} is in validators[{j}] and validators[{i}]"
                )));
            }
            if let Some(j) = by_pubkey.insert(pubkey.clone(), i) {
                return Err(GenesisError::DuplicateKey(format!(
                    "validator pubkey 0x{} is in validators[{j}] and validators[{i}]",
                    alloy_primitives::hex::encode(&pubkey)
                )));
            }
        }
        Ok(())
    }

    /// `economics.treasury_address` (absent = the zero address; malformed =
    /// an error, s140).
    pub fn treasury_address(&self) -> Result<Address, GenesisError> {
        economics_address(
            "treasury_address",
            self.economics.treasury_address.as_deref(),
        )
    }

    /// `economics.dev_pool_address` (absent = the zero address; malformed =
    /// an error, s140).
    pub fn dev_pool_address(&self) -> Result<Address, GenesisError> {
        economics_address(
            "dev_pool_address",
            self.economics.dev_pool_address.as_deref(),
        )
    }

    /// s140: the chain's gas-fee split from `economics.fee_split` (absent =
    /// the built-in split): every share 0..=10_000 bps, start and end each
    /// summing to exactly 10_000 ([`torus_types::FeeSchedule::check`]); any
    /// `transition_epochs`. Was row 80 option D (s100): only the built-in
    /// split loaded.
    pub fn fee_schedule(&self) -> Result<torus_types::FeeSchedule, GenesisError> {
        let fs = &self.economics.fee_split;
        let shares = |end: &str, s: &FeeSplit| -> Result<torus_types::FeeShares, GenesisError> {
            let bps = |field: &str, v: u32| {
                u16::try_from(v).ok().filter(|v| *v <= 10_000).ok_or_else(|| {
                    GenesisError::InvalidFeeSplit(format!(
                        "fee_split.{end}.{field} {v} bps is over 10000"
                    ))
                })
            };
            Ok(torus_types::FeeShares {
                burn: bps("burn", s.burn)?,
                validator: bps("validator", s.validator)?,
                treasury: bps("treasury", s.treasury)?,
                dev_pool: bps("dev_pool", s.dev_pool)?,
            })
        };
        let schedule = torus_types::FeeSchedule {
            start: shares("start", &fs.start)?,
            end: shares("end", &fs.end)?,
            transition_epochs: fs.transition_epochs,
        };
        schedule.check().map_err(GenesisError::InvalidFeeSplit)?;
        Ok(schedule)
    }

    fn check_fee_split(&self) -> Result<(), GenesisError> {
        self.fee_schedule().map(|_| ())
    }

    /// Initialize the state database with genesis data. Returns the state root.
    ///
    /// 1. Seeds EVM accounts from `accounts` and `evm_alloc`
    /// 2. Seeds contract code and storage from `evm_alloc`
    /// 3. Seeds validator stakes into `CF_STAKING_VALIDATORS`
    /// 4. Computes and returns the state root
    pub fn initialize(&self, state_db: &StateDb) -> Result<B256, GenesisError> {
        // R19: keys and hex are checked before any write (also for a Genesis
        // edited after from_json).
        self.check_keys_and_hex()?;

        // 0. Row 43: every market needs tick_size > 0 and lot_size > 0 (books are
        //    built from the market row). Checked before any write.
        for market in &self.markets {
            for (field, value) in [("tick_size", &market.tick_size), ("lot_size", &market.lot_size)] {
                if parse_fixedpoint_raw(value)? <= 0 {
                    return Err(GenesisError::InvalidMarket(format!(
                        "market {} {field} must be > 0",
                        market.market_id
                    )));
                }
            }
        }

        // 1. Seed plain accounts
        for account in &self.accounts {
            let address = parse_address(&account.address)?;
            let balance = parse_u256(&account.balance)?;
            state_db.put_account(
                &address,
                &AccountInfo {
                    balance,
                    nonce: 0,
                    code_hash: KECCAK_EMPTY,
                    code: None,
                    account_id: None,
                },
            )?;
            info!(
                %address, %balance,
                note = account.note.as_deref().unwrap_or(""),
                "seeded genesis account"
            );
        }

        // 2. Seed evm_alloc entries (contracts with code/storage)
        for (addr_str, entry) in &self.evm_alloc {
            let address = parse_address(addr_str)?;
            let balance = parse_u256(&entry.balance)?;

            let code_hash = if let Some(code_hex) = &entry.code {
                let code_bytes = decode_hex(code_hex)?;
                let hash = keccak256(&code_bytes);
                state_db.put_code(&hash, &code_bytes)?;
                hash
            } else {
                KECCAK_EMPTY
            };

            state_db.put_account(
                &address,
                &AccountInfo {
                    balance,
                    nonce: 0,
                    code_hash,
                    code: None,
                    account_id: None,
                },
            )?;

            if let Some(storage) = &entry.storage {
                for (key_str, value_str) in storage {
                    let key = parse_u256(key_str)?;
                    let value = parse_u256(value_str)?;
                    state_db.put_storage(&address, &key, &value)?;
                }
            }

            info!(%address, has_code = entry.code.is_some(), "seeded evm_alloc entry");
        }

        // 3. Seed validator stakes into CF_STAKING_VALIDATORS (borsh-encoded)
        for validator in &self.validators {
            let address = parse_address(&validator.address)?;
            let pubkey_bytes = decode_hex(&validator.pubkey)?;
            let pubkey: [u8; 32] = pubkey_bytes
                .try_into()
                .map_err(|_| GenesisError::InvalidPubkey(validator.pubkey.clone()))?;
            let stake = parse_u256(&validator.stake)?;
            let state = ValidatorState {
                address,
                pubkey,
                commission_bps: validator.commission_bps,
                self_stake: stake,
                total_delegated: U256::ZERO,
                status: ValidatorStatus::Active,
                jailed_until: None,
                last_commission_change_block: None,
                oracle_signer: None,
            };
            let data = borsh::to_vec(&state)
                .map_err(|e| GenesisError::InvalidHex(format!("borsh encode validator: {e}")))?;
            state_db.put_cf_raw(CF_STAKING_VALIDATORS, address.as_slice(), &data)?;
            info!(%address, stake = %validator.stake, "seeded genesis validator");
        }
        // The hotstuff genesis set is the installed epoch set (all genesis
        // validators, ranked like every execution-computed plan): the base of
        // the first plan (consensus bug (b), torus_economics::epoch_plan).
        let staking = StakingManager::new(state_db.clone());
        EpochManager::compute_new_validator_set(&staking, u32::MAX, 0)
            .and_then(|set| staking.record_genesis_epoch_set(&set))
            .map_err(|e| GenesisError::InvalidHex(format!("genesis epoch set: {e}")))?;

        // 4. Seed permanent stakes into CF_STAKING_PERMANENT
        for ps in &self.permanent_stakes {
            let address = parse_address(&ps.address)?;
            let amount = parse_u256(&ps.amount)?;
            // Borsh-encode PermanentStakeInfo: address(20) + amount(32 BE) + locked_at_block(8 LE)
            let mut data = Vec::with_capacity(60);
            data.extend_from_slice(address.as_slice()); // 20 bytes
            data.extend_from_slice(&amount.to_be_bytes::<32>()); // 32 bytes
            data.extend_from_slice(&0u64.to_le_bytes()); // 8 bytes, block 0
            state_db.put_cf_raw(
                torus_state::cf::CF_STAKING_PERMANENT,
                address.as_slice(),
                &data,
            )?;
            info!(%address, %amount, "seeded genesis permanent stake");
        }

        // 5. Seed native perpetual markets into CF_NATIVE_MARKETS. This CF is OFF the
        //    consensus native state root, so seeding it cannot diverge validators; it
        //    drives RPC market listings and order/margin validation.
        for market in &self.markets {
            let lot_size = parse_fixedpoint_raw(&market.lot_size)?;
            let tick_size = parse_fixedpoint_raw(&market.tick_size)?;
            let initial_margin = parse_fixedpoint_raw(&market.initial_margin)?;

            // Borsh layout MUST match torus-rpc `StoredMarket` + governance `MarketListing`:
            // base(String) + quote(String) + lot_size(i128) + tick_size(i128) + initial_margin(i128).
            let mut data = Vec::new();
            BorshSerialize::serialize(&market.base_asset, &mut data)
                .map_err(|e| GenesisError::InvalidHex(format!("borsh market base: {e}")))?;
            BorshSerialize::serialize(&market.quote_asset, &mut data)
                .map_err(|e| GenesisError::InvalidHex(format!("borsh market quote: {e}")))?;
            BorshSerialize::serialize(&lot_size, &mut data)
                .map_err(|e| GenesisError::InvalidHex(format!("borsh market lot: {e}")))?;
            BorshSerialize::serialize(&tick_size, &mut data)
                .map_err(|e| GenesisError::InvalidHex(format!("borsh market tick: {e}")))?;
            BorshSerialize::serialize(&initial_margin, &mut data)
                .map_err(|e| GenesisError::InvalidHex(format!("borsh market margin: {e}")))?;

            state_db.put_cf_raw(CF_NATIVE_MARKETS, &market.market_id.to_be_bytes(), &data)?;
            info!(
                market_id = market.market_id,
                base = %market.base_asset,
                quote = %market.quote_asset,
                "seeded genesis market"
            );
        }

        // 6. Seed native (perp-side) balances into CF_NATIVE_BALANCES so pre-funded
        //    accounts can place margined orders at genesis without a prior deposit.
        //    CF_NATIVE_BALANCES IS part of the native root, but the genesis root is only
        //    logged (not checkpointed) and block 1 recomputes the composite root from the
        //    full DB — identical genesis.json => identical roots across validators.
        for nb in &self.native_balances {
            let address = parse_address(&nb.address)?;
            let available = FixedPoint::from_raw(parse_fixedpoint_raw(&nb.available)?);
            let balance = NativeBalance {
                available,
                order_margin: FixedPoint::ZERO,
            };
            let data = borsh::to_vec(&balance)
                .map_err(|e| GenesisError::InvalidHex(format!("borsh native balance: {e}")))?;
            state_db.put_cf_raw(CF_NATIVE_BALANCES, address.as_slice(), &data)?;
            info!(%address, %available, "seeded genesis native balance");
        }

        // 7. Seed governance params
        let treasury_address = self.treasury_address()?;
        let gov = GovernanceManager::new(state_db.clone());
        gov.set_governance_params(&GovernanceParams::defaults(treasury_address))
            .map_err(|e| GenesisError::InvalidHex(format!("governance params: {e}")))?;
        info!(%treasury_address, "seeded governance params");

        // 8. Compute state root (EVM-only; informational — see step 6 note).
        let state_root = compute_state_root_from_db(state_db)?;
        info!(%state_root, "genesis state root computed");
        Ok(state_root)
    }

    /// Build a [`ChainConfig`] from the genesis parameters.
    ///
    /// Panics on an activation schedule that does not merge, a malformed
    /// economics address or an invalid fee split; `from_json` already
    /// refused such a genesis (the node does not start).
    pub fn chain_config(&self) -> ChainConfig {
        let activations = self
            .activation_schedule()
            .unwrap_or_else(|e| panic!("genesis not loaded through from_json: {e}"));
        let [treasury_address, dev_pool_address] =
            [self.treasury_address(), self.dev_pool_address()]
                .map(|a| a.unwrap_or_else(|e| panic!("genesis not loaded through from_json: {e}")));
        ChainConfig {
            chain_id: self.chain_id,
            chain_name: self.chain_name.clone(),
            evm_gas_limit: self.evm.gas_limit,
            evm_block_interval: self.evm.block_interval,
            evm_big_block_interval: self.evm.big_block_interval,
            evm_big_gas_limit: self.evm.big_gas_limit,
            base_fee_per_gas: self.evm.base_fee_initial,
            epoch_length: self.consensus.epoch_length,
            max_validators: self.economics.validator.max_validators,
            min_stake: parse_u256(&self.economics.validator.min_self_delegation)
                .unwrap_or_default(),
            fee_burn_bps: self.economics.fee_split.start.burn,
            fee_validator_bps: self.economics.fee_split.start.validator,
            fee_treasury_bps: self.economics.fee_split.start.treasury,
            fee_dev_pool_bps: self.economics.fee_split.start.dev_pool,
            fee_schedule: self
                .fee_schedule()
                .unwrap_or_else(|e| panic!("genesis not loaded through from_json: {e}")),
            treasury_address,
            dev_pool_address,
            timeout_base_ms: self.consensus.timeout_base_ms,
            backoff_factor: self.consensus.backoff_factor,
            backoff_cap: self.consensus.backoff_cap,
            commit_lag_backoff_cap: self.consensus.commit_lag_backoff_cap,
            reputation_leader_selection: self.consensus.reputation_leader_selection,
            // Node-local perf toggle; never sourced from genesis. Enabled per-node
            // via the `--exec-trust-cache` CLI flag.
            exec_trust_cache: false,
            activations,
            book_mode: self.consensus.book_mode,
            leader_miss_threshold: self.consensus.leader_miss_threshold,
            leader_exclusion_cap: self.consensus.leader_exclusion_cap,
        }
    }

    /// Build a [`TorusValidatorSet`] from the genesis validators.
    pub fn validator_set(&self) -> Result<TorusValidatorSet, GenesisError> {
        let mut validators = Vec::with_capacity(self.validators.len());
        for v in &self.validators {
            let address = parse_address(&v.address)?;
            let pubkey_bytes = decode_hex(&v.pubkey)?;
            let pubkey: [u8; 32] = pubkey_bytes
                .try_into()
                .map_err(|_| GenesisError::InvalidPubkey(v.pubkey.clone()))?;
            let stake = parse_u256(&v.stake)?;
            validators.push(ValidatorInfo {
                address,
                pubkey: PublicKey(pubkey),
                power: stake_to_power(stake),
                commission_bps: v.commission_bps,
            });
        }
        Ok(TorusValidatorSet {
            validators,
            epoch: 0,
        })
    }

    /// Build the hotstuff_rs genesis state for [`Replica::initialize`].
    ///
    /// Returns `(AppStateUpdates, ValidatorSetState)` where the app state is
    /// empty (state lives in RocksDB CFs, not the consensus KV store) and the
    /// validator set contains all genesis validators with stake-derived power.
    pub fn to_hotstuff_genesis(
        &self,
    ) -> Result<(AppStateUpdates, ValidatorSetState), GenesisError> {
        let mut vs = HsValidatorSet::new();
        for validator in &self.validators {
            let pubkey_bytes = decode_hex(&validator.pubkey)?;
            let pubkey_arr: [u8; 32] = pubkey_bytes
                .try_into()
                .map_err(|_| GenesisError::InvalidPubkey(validator.pubkey.clone()))?;
            let vk = VerifyingKey::from_bytes(&pubkey_arr)
                .map_err(|_| GenesisError::InvalidPubkey(validator.pubkey.clone()))?;
            let stake = parse_u256(&validator.stake)?;
            vs.put(&vk, Power::new(stake_to_power(stake)));
        }

        let vs_state = ValidatorSetState::new(vs.clone(), vs, None, true);
        let app_state = AppStateUpdates::new();
        Ok((app_state, vs_state))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_genesis_json() -> String {
        serde_json::json!({
            "chain_id": 7778,
            "chain_name": "torus-devnet",
            "timestamp": 1700000000u64,
            "consensus": {
                "protocol": "hotstuff",
                "epoch_length": 100,
                "timeout_base_ms": 500,
                "timeout_max_ms": 30000
            },
            "evm": {
                "spec_id": "cancun",
                "gas_limit": 30000000,
                "base_fee_initial": 1000000000u64,
                "chain_id": 7778
            },
            "economics": {
                "treasury_address": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "dev_pool_address": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "fee_split": {
                    "start": { "burn": 1000, "validator": 0, "treasury": 4500, "dev_pool": 4500 },
                    "end": { "burn": 2500, "validator": 2500, "treasury": 2500, "dev_pool": 2500 },
                    "transition_epochs": 1825
                },
                "permanent_staking": {
                    "annual_rate_bps": 500,
                    "governance_multiplier_bps": 15000
                },
                "validator": {
                    "min_self_delegation": "10000000000000000000000",
                    "max_validators": 21,
                    "unbonding_period_blocks": 604800,
                    "max_commission_bps": 5000,
                    "max_commission_change_bps": 100
                }
            },
            "validators": [
                {
                    "address": "0x1111111111111111111111111111111111111111",
                    "pubkey": "0xd75a980182b10ab7d54bfed3c964073a0ee172f3daa3f4a18446b7e8f5f7a1d6",
                    "stake": "1000000000000000000000000",
                    "commission_bps": 500
                }
            ],
            "accounts": [
                {
                    "address": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "balance": "1000000000000000000000000000",
                    "note": "treasury"
                },
                {
                    "address": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "balance": "500000000000000000000000000",
                    "note": "dev_pool"
                }
            ],
            "evm_alloc": {
                "0xcccccccccccccccccccccccccccccccccccccccc": {
                    "balance": "0x0",
                    "code": "0x6080604052",
                    "storage": {
                        "0x0": "0x1"
                    }
                }
            }
        })
        .to_string()
    }

    /// Regression guard for the shipped testnet genesis BASE.
    ///
    /// Guards `testnet/genesis-weighted-base.json`, which is the human-editable
    /// source of truth: `testnet/gen-weighted-genesis.sh` expands it into
    /// `genesis-weighted-full.json` (gitignored, ~28MB of bulk bench accounts),
    /// and THAT is what nodes boot. The old `testnet/genesis.json` was a second
    /// lean base serving the same role; it was deleted because two plausible
    /// bases produce DIFFERENT state roots — hence mutually unjoinable chains —
    /// and only one can be canonical.
    ///
    /// The set seeds 100 perp markets (ids 1..=100: 1-10 named pairs, 11-100
    /// synthetic S11..S100 for throughput sweeps) and 10 permanent stakers
    /// (hardhat #0..#9) so governance market-listing stays testable. If this file
    /// is edited in a way that breaks the struct shape or drops a market/staker,
    /// this test fails.
    #[test]
    fn parse_real_testnet_genesis() {
        let json = include_str!("../../../testnet/genesis-weighted-base.json");
        let genesis =
            Genesis::from_json(json).expect("real testnet/genesis-weighted-base.json must parse");

        assert_eq!(genesis.chain_id, 7778, "testnet chain_id");
        assert_eq!(genesis.validators.len(), 4, "4-validator set (seed/18c/val2/val3)");

        // 100 markets, ids exactly 1..=100.
        let mut ids: Vec<u64> = genesis.markets.iter().map(|m| m.market_id).collect();
        ids.sort_unstable();
        assert_eq!(ids, (1..=100).collect::<Vec<_>>(), "markets 1..=100 seeded");

        // 10 permanent stakers, each with non-zero stake so they carry voting weight.
        assert_eq!(genesis.permanent_stakes.len(), 10, "10 governance stakers");
        for ps in &genesis.permanent_stakes {
            assert!(
                parse_u256(&ps.amount).unwrap() > U256::ZERO,
                "staker {} must have non-zero permanent stake",
                ps.address
            );
        }

        // Every staker must also be a signable EVM account (has a genesis balance)
        // so it can EIP-712-sign and submit its own Vote native action.
        for ps in &genesis.permanent_stakes {
            assert!(
                genesis
                    .accounts
                    .iter()
                    .any(|a| a.address.eq_ignore_ascii_case(&ps.address)),
                "staker {} must have an EVM account balance to sign votes",
                ps.address
            );
        }
    }

    /// s69: the shipped genesis files (devnet, A/B harness, and the testnet
    /// base the matched bench derives from) use the 1200 ms view-timeout base.
    #[test]
    fn shipped_genesis_files_use_1200ms_timeout_base() {
        for (name, json) in [
            ("devnet/genesis.json", include_str!("../../../devnet/genesis.json")),
            (
                "devnet/ab-harness/devnet-genesis.json",
                include_str!("../../../devnet/ab-harness/devnet-genesis.json"),
            ),
            (
                "testnet/genesis-weighted-base.json",
                include_str!("../../../testnet/genesis-weighted-base.json"),
            ),
        ] {
            let genesis = Genesis::from_json(json).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(genesis.consensus.timeout_base_ms, 1200, "{name}");
            assert_eq!(genesis.chain_config().timeout_base_ms, 1200, "{name}");
        }
    }

    /// Item 7 step 1: `evm.block_interval` (N) defaults to 11 (s123 owner,
    /// was 5) and feeds `ChainConfig`; 0 is refused at load. Genesis may set
    /// N = 1 (the 2..=100 range binds governance only).
    #[test]
    fn evm_block_interval_defaults_and_feeds_chain_config() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        assert_eq!(genesis.evm.block_interval, 11);
        assert_eq!(genesis.chain_config().evm_block_interval, 11);
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["evm"]["block_interval"] = 7.into();
        let genesis = Genesis::from_json(&json.to_string()).unwrap();
        assert_eq!(genesis.chain_config().evm_block_interval, 7);
        json["evm"]["block_interval"] = 1.into();
        let genesis = Genesis::from_json(&json.to_string()).unwrap();
        assert_eq!(genesis.chain_config().evm_block_interval, 1);
        json["evm"]["block_interval"] = 0.into();
        let err = Genesis::from_json(&json.to_string()).unwrap_err();
        assert!(matches!(err, GenesisError::InvalidEvm(_)), "{err}");
    }

    /// Item 7 step 2: `evm.big_block_interval` (M) defaults to 0 (off) and
    /// `evm.big_gas_limit` to 10M; both feed `ChainConfig`.
    #[test]
    fn big_block_values_default_off_and_feed_chain_config() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        assert_eq!(
            (genesis.evm.big_block_interval, genesis.evm.big_gas_limit),
            (0, 10_000_000)
        );
        let config = genesis.chain_config();
        assert_eq!(
            (config.evm_big_block_interval, config.evm_big_gas_limit),
            (0, 10_000_000)
        );
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["evm"]["big_block_interval"] = 190.into();
        json["evm"]["big_gas_limit"] = 15_000_000u64.into();
        let config = Genesis::from_json(&json.to_string())
            .unwrap()
            .chain_config();
        assert_eq!(
            (config.evm_big_block_interval, config.evm_big_gas_limit),
            (190, 15_000_000)
        );
    }

    /// Item 7 step 1: every tracked genesis file ships the 3M EVM budget and
    /// N = 11 (s123 owner, was 5), spelled out in the file (not left to the
    /// serde default).
    #[test]
    fn shipped_genesis_files_use_the_item7_evm_values() {
        for (name, json) in [
            (
                "devnet/genesis.json",
                include_str!("../../../devnet/genesis.json"),
            ),
            (
                "devnet/ab-harness/devnet-genesis.json",
                include_str!("../../../devnet/ab-harness/devnet-genesis.json"),
            ),
            (
                "testnet/genesis-weighted-base.json",
                include_str!("../../../testnet/genesis-weighted-base.json"),
            ),
            (
                "testnet/genesis-weighted-base-setup2b.json",
                include_str!("../../../testnet/genesis-weighted-base-setup2b.json"),
            ),
            (
                "testnet/p3-testnet/devnet-ladder/genesis-10mkt.json",
                include_str!("../../../testnet/p3-testnet/devnet-ladder/genesis-10mkt.json"),
            ),
        ] {
            let genesis = Genesis::from_json(json).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(genesis.evm.gas_limit, 3_000_000, "{name}");
            assert_eq!(genesis.evm.block_interval, 11, "{name}");
            assert_eq!(genesis.chain_config().evm_gas_limit, 3_000_000, "{name}");
            assert_eq!(genesis.chain_config().evm_block_interval, 11, "{name}");
            let raw: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(
                raw["evm"]["block_interval"], 11,
                "{name}: field spelled out"
            );
            // Item 7 step 2: big blocks off (M = 0) and a 10M big lane, spelled out too.
            assert_eq!(
                raw["evm"]["big_block_interval"], 0,
                "{name}: field spelled out"
            );
            assert_eq!(
                raw["evm"]["big_gas_limit"], 10_000_000,
                "{name}: field spelled out"
            );
            let config = genesis.chain_config();
            assert_eq!(
                (config.evm_big_block_interval, config.evm_big_gas_limit),
                (0, 10_000_000),
                "{name}"
            );
        }
    }

    /// S470 knob default: a genesis whose consensus section omits
    /// `commit_lag_backoff_cap` (every genesis written before S470) must map
    /// to a ChainConfig with the commit-lag backoff OFF — the exact pre-S470
    /// deadline schedule.
    /// Running state hash (review finding 1): the activation height is a
    /// CHAIN-WIDE genesis field; absent (every existing genesis) = disabled.
    #[test]
    fn genesis_state_hash_activation_height_is_chain_config() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        assert_eq!(genesis.consensus.state_hash_activation_height, None);
        // Codex P1 (s136): a genesis naming no activation keeps its legacy
        // meaning, off (generators write the default explicitly).
        assert_eq!(genesis.chain_config().state_hash_activation_height(), None);
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["consensus"]["state_hash_activation_height"] = serde_json::json!(5000);
        let genesis = Genesis::from_json(&json.to_string()).unwrap();
        assert_eq!(genesis.chain_config().state_hash_activation_height(), Some(5000));
        assert!(genesis.chain_config().validate().is_ok());
        // P0.0 phase 3: derived from the schedule only; 0 there is block 1.
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["consensus"]["activations"] = serde_json::json!({"running_state_hash": 0});
        let genesis = Genesis::from_json(&json.to_string()).unwrap();
        assert_eq!(genesis.chain_config().state_hash_activation_height(), Some(1));
    }

    /// P0.0: `consensus.activations` names features (an unknown name refuses
    /// to start) and lands in `ChainConfig.activations`; the old
    /// `state_hash_activation_height` is the alias of `running_state_hash`
    /// (both set with different heights refuses; 0 and 1 are the same height).
    /// RED before phase 1: the alias was not in the schedule and a conflict loaded.
    #[test]
    fn genesis_activations_and_the_running_hash_alias() {
        use torus_types::Feature::RunningStateHash;
        let with = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
            f(&mut json);
            Genesis::from_json(&json.to_string())
        };
        let none = with(&|j| j["consensus"]["activations"] = serde_json::json!({}))
            .unwrap()
            .chain_config();
        assert_eq!(none.activations, torus_types::ActivationSchedule::default());

        let g = with(&|j| {
            j["consensus"]["activations"] = serde_json::json!({"running_state_hash": 300})
        })
        .unwrap();
        let config = g.chain_config();
        assert_eq!(config.activations.height_of(RunningStateHash), Some(300));
        assert_eq!(config.activations.active_at(299), 0);
        assert_eq!(config.activations.active_at(300), RunningStateHash.bit());
        assert_eq!(
            config.state_hash_activation_height(),
            Some(300),
            "derived from the schedule"
        );

        let err = with(&|j| j["consensus"]["activations"] = serde_json::json!({"warp_drive": 3}))
            .expect_err("unknown feature name refuses to start");
        assert!(err.to_string().contains("warp_drive"), "{err}");

        // The alias alone schedules the feature.
        let g = with(&|j| j["consensus"]["state_hash_activation_height"] = serde_json::json!(40))
            .unwrap();
        assert_eq!(
            g.chain_config().activations.height_of(RunningStateHash),
            Some(40)
        );
        assert_eq!(g.chain_config().state_hash_activation_height(), Some(40));

        // Both, same height: fine; different: refused.
        let both = |alias: u64, new: u64| {
            with(&move |j| {
                j["consensus"]["state_hash_activation_height"] = serde_json::json!(alias);
                j["consensus"]["activations"] = serde_json::json!({"running_state_hash": new});
            })
        };
        assert!(both(40, 40).is_ok());
        assert!(both(1, 0).is_ok(), "0 and 1 are both block 1");
        let err = both(40, 41).expect_err("alias conflict refuses to start");
        assert!(matches!(err, GenesisError::InvalidActivations(_)), "{err}");
        // Height 0 through the old field stays refused (it never meant block 1).
        assert!(
            with(&|j| j["consensus"]["state_hash_activation_height"] = serde_json::json!(0))
                .is_err()
        );
    }

    /// Owner s131: the order-book storage mode is CHAIN-WIDE genesis field
    /// `consensus.book_mode`; absent (every existing genesis, incl. the
    /// testnet's and the devnet's) = Classic.
    #[test]
    fn genesis_without_book_mode_is_classic() {
        use torus_types::book_mode::BookMode;
        let sample = Genesis::from_json(&sample_genesis_json()).unwrap();
        assert_eq!(sample.consensus.book_mode, BookMode::Classic);
        assert_eq!(sample.chain_config().book_mode, BookMode::Classic);
        for (name, json) in [
            (
                "testnet base",
                include_str!("../../../testnet/genesis-weighted-base.json"),
            ),
            (
                "testnet setup2b",
                include_str!("../../../testnet/genesis-weighted-base-setup2b.json"),
            ),
            ("devnet", include_str!("../../../devnet/genesis.json")),
        ] {
            let g = Genesis::from_json(json).unwrap();
            assert_eq!(g.chain_config().book_mode, BookMode::Classic, "{name}");
        }
    }

    /// Names and their 0-3 numbers both parse; anything else refuses the
    /// genesis (never a silent Classic).
    #[test]
    fn genesis_book_mode_maps_to_chain_config() {
        use torus_types::book_mode::BookMode;
        let base: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        let with = |v: serde_json::Value| {
            let mut json = base.clone();
            json["consensus"]["book_mode"] = v;
            Genesis::from_json(&json.to_string())
        };
        for (v, want) in [
            (serde_json::json!("classic"), BookMode::Classic),
            (serde_json::json!("order_rows"), BookMode::OrderRows),
            (
                serde_json::json!("level_authority"),
                BookMode::LevelAuthority,
            ),
            (
                serde_json::json!("level_authority_chunked"),
                BookMode::LevelAuthorityChunked,
            ),
            (serde_json::json!(0), BookMode::Classic),
            (serde_json::json!(1), BookMode::OrderRows),
            (serde_json::json!(2), BookMode::LevelAuthority),
            (serde_json::json!(3), BookMode::LevelAuthorityChunked),
        ] {
            let g = with(v.clone()).unwrap_or_else(|e| panic!("{v}: {e}"));
            assert_eq!(g.consensus.book_mode, want, "{v}");
            let config = g.chain_config();
            assert_eq!(config.book_mode, want, "{v}");
            assert!(config.validate().is_ok());
        }
        for v in [
            serde_json::json!(4),
            serde_json::json!(-1),
            serde_json::json!("3"),
            serde_json::json!("Classic"),
            serde_json::json!("mode3"),
            serde_json::json!(null),
            serde_json::json!(true),
        ] {
            assert!(with(v.clone()).is_err(), "{v} must refuse the genesis");
        }
    }

    /// Codex P1 (s136 review): a genesis file naming no activation (no
    /// `consensus.activations`, no `state_hash_activation_height`, no
    /// `book_window_commit`) keeps its legacy meaning: no feature. Its file
    /// digest (the data dir binding) is unchanged, so reading it as "hash +
    /// L6.1 from block 1" would silently change the genesis parent header
    /// (`features = active_at(0)`) and block 1 of an existing chain. The
    /// sample and the checked-in files (devnet, ab-harness, devnet ladder)
    /// all decode to the empty schedule, equal to an explicit `{}`.
    /// RED before the fix: the absent field meant the defaults.
    #[test]
    fn an_old_genesis_without_activations_keeps_its_meaning_off() {
        let empty = |text: &str| {
            let mut json: serde_json::Value = serde_json::from_str(text).unwrap();
            json["consensus"]["activations"] = serde_json::json!({});
            Genesis::from_json(&json.to_string()).unwrap().chain_config()
        };
        let sample = sample_genesis_json();
        for (name, text) in [
            ("sample", sample.as_str()),
            ("devnet/genesis.json", include_str!("../../../devnet/genesis.json")),
            (
                "devnet/ab-harness/devnet-genesis.json",
                include_str!("../../../devnet/ab-harness/devnet-genesis.json"),
            ),
            (
                "testnet/p3-testnet/devnet-ladder/genesis-10mkt.json",
                include_str!("../../../testnet/p3-testnet/devnet-ladder/genesis-10mkt.json"),
            ),
        ] {
            let json: serde_json::Value = serde_json::from_str(text).unwrap();
            let consensus = json.get("consensus").cloned().unwrap_or_default();
            for field in ["activations", "state_hash_activation_height", "book_window_commit"] {
                assert!(consensus.get(field).is_none(), "{name}: names no {field}");
            }
            let config = Genesis::from_json(text)
                .unwrap_or_else(|e| panic!("{name}: {e}"))
                .chain_config();
            assert_eq!(config.state_hash_activation_height(), None, "{name}: hash off");
            assert_eq!(config.book_window_activation(), None, "{name}: L6.1 off");
            assert_eq!(config.activations.active_at(0), 0, "{name}: genesis parent bits");
            assert_eq!(config.activations, empty(text).activations, "{name}: = an explicit {{}}");
            // The data dir binding is the file digest: unchanged by the parser.
            assert_eq!(
                crate::digest::GenesisDigest::of_json(text).unwrap(),
                crate::digest::GenesisDigest::of_value(&json),
                "{name}"
            );
        }
    }

    /// Owner s136: the hash and L6.1 are ON in every NEWLY generated genesis,
    /// written explicitly (`gen-3val-genesis.sh` with STATE_HASH_ACTIVATION
    /// unset writes [`torus_types::DEFAULT_GENESIS_ACTIVATIONS`] as
    /// `consensus.activations`): both from block 1, the genesis parent header
    /// carries both bits. A row book mode gets the hash only.
    #[test]
    fn a_generated_genesis_names_the_hash_and_window_books_from_block_1() {
        use torus_types::Feature::{BookWindowCommit, RunningStateHash};
        let written: serde_json::Map<String, serde_json::Value> = torus_types::DEFAULT_GENESIS_ACTIVATIONS
            .iter()
            .map(|(f, h)| (f.name().to_string(), serde_json::json!(h)))
            .collect();
        assert_eq!(
            serde_json::Value::Object(written.clone()),
            serde_json::json!({"running_state_hash": 1, "book_window_commit": 1}),
            "what gen-3val-genesis.sh writes"
        );
        let with = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
            f(&mut json);
            Genesis::from_json(&json.to_string()).unwrap().chain_config()
        };
        let fresh = with(&|j| j["consensus"]["activations"] = serde_json::Value::Object(written.clone()));
        assert_eq!(fresh.state_hash_activation_height(), Some(1));
        assert_eq!(fresh.book_window_activation(), Some(1));
        assert_eq!(
            fresh.activations.active_at(0),
            RunningStateHash.bit() | BookWindowCommit.bit(),
            "the genesis parent header carries both"
        );
        assert!(fresh.validate().is_ok());
        let rows = with(&|j| {
            j["consensus"]["activations"] = serde_json::json!({"running_state_hash": 1});
            j["consensus"]["book_mode"] = serde_json::json!("level_authority_chunked");
        });
        assert_eq!(rows.state_hash_activation_height(), Some(1));
        assert_eq!(rows.book_window_activation(), None, "L6.1 is Classic-only");
        let alias = with(&|j| j["consensus"]["state_hash_activation_height"] = serde_json::json!(40));
        assert_eq!(alias.state_hash_activation_height(), Some(40));
        assert_eq!(alias.book_window_activation(), None, "explicit hash-only config");
    }

    /// L6.1 under P0.0: window books are the feature `book_window_commit`.
    /// `consensus.activations.book_window_commit` schedules it at its own
    /// height; the bool `consensus.book_window_commit` (what the bench
    /// harness writes) is its alias: from the running hash's height. Refused:
    /// L6.1 without the running hash, the alias next to an explicit height
    /// other than the hash's, and a row book mode.
    /// RED before the wiring: the bool stayed outside the schedule and the
    /// refusals loaded.
    #[test]
    fn genesis_book_window_commit_is_the_book_window_feature() {
        use torus_types::Feature::{BookWindowCommit, RunningStateHash};
        let with = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
            f(&mut json);
            Genesis::from_json(&json.to_string())
        };
        let alias = with(&|j| {
            j["consensus"]["state_hash_activation_height"] = serde_json::json!(7);
            j["consensus"]["book_window_commit"] = serde_json::json!(true);
        })
        .unwrap()
        .chain_config();
        assert_eq!(alias.activations.height_of(RunningStateHash), Some(7));
        assert_eq!(alias.activations.height_of(BookWindowCommit), Some(7));
        assert_eq!(alias.book_window_activation(), Some(7));
        assert_eq!(
            alias.activations.active_at(7),
            RunningStateHash.bit() | BookWindowCommit.bit(),
            "the header bits carry it"
        );
        let own = with(&|j| {
            j["consensus"]["activations"] =
                serde_json::json!({"running_state_hash": 1, "book_window_commit": 300});
        })
        .unwrap()
        .chain_config();
        assert_eq!(own.book_window_activation(), Some(300));
        assert_eq!(own.activations.active_at(299) & BookWindowCommit.bit(), 0);
        // The alias with the same explicit height is fine.
        assert!(with(&|j| {
            j["consensus"]["activations"] =
                serde_json::json!({"running_state_hash": 5, "book_window_commit": 5});
            j["consensus"]["book_window_commit"] = serde_json::json!(true);
        })
        .is_ok());
        let refused = [
            (
                "alias without the running hash",
                with(&|j| {
                    j["consensus"]["activations"] = serde_json::json!({});
                    j["consensus"]["book_window_commit"] = serde_json::json!(true);
                }),
            ),
            (
                "feature without the running hash",
                with(&|j| {
                    j["consensus"]["activations"] = serde_json::json!({"book_window_commit": 3});
                }),
            ),
            (
                "alias next to another height",
                with(&|j| {
                    j["consensus"]["activations"] =
                        serde_json::json!({"running_state_hash": 1, "book_window_commit": 300});
                    j["consensus"]["book_window_commit"] = serde_json::json!(true);
                }),
            ),
            (
                "row book mode",
                with(&|j| {
                    j["consensus"]["activations"] =
                        serde_json::json!({"running_state_hash": 1, "book_window_commit": 1});
                    j["consensus"]["book_mode"] = serde_json::json!("level_authority_chunked");
                }),
            ),
        ];
        for (what, got) in refused {
            let err = got.expect_err(what);
            assert!(matches!(err, GenesisError::InvalidActivations(_)), "{what}: {err}");
            assert!(err.to_string().contains("book_window_commit"), "{what}: {err}");
        }
    }

    /// L6.1: `consensus.book_window_commit` is a CHAIN-WIDE genesis field
    /// (the alias of the feature); on needs the running hash.
    #[test]
    fn genesis_book_window_commit_is_chain_config() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        assert!(!genesis.consensus.book_window_commit);
        // Codex P1 (s136): a genesis naming no activation keeps its legacy
        // meaning (no L6.1); generators write it explicitly.
        assert_eq!(genesis.chain_config().book_window_activation(), None, "absent = off");
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["consensus"]["book_window_commit"] = serde_json::json!(true);
        json["consensus"]["state_hash_activation_height"] = serde_json::json!(1);
        let mut config = Genesis::from_json(&json.to_string())
            .unwrap()
            .chain_config();
        assert_eq!(config.book_window_activation(), Some(1));
        assert!(config.validate().is_ok());
        config.activations = Default::default();
        assert!(config.validate().is_ok(), "nothing scheduled");
        config.activations = torus_types::ActivationSchedule::merge(
            &[(torus_types::Feature::BookWindowCommit, 1)].into_iter().collect(),
            &[],
        )
        .unwrap();
        assert!(config.validate().is_err(), "L6.1 without the running hash");
    }


    /// Consensus bug (b): genesis records the hotstuff genesis set as the
    /// installed epoch set, ranked exactly like a computed plan, so the first
    /// plan diffs against it (an unchanged set is no hotstuff update).
    #[test]
    fn initialize_records_the_genesis_set_as_the_installed_epoch_set() {
        use torus_economics::{EpochManager, StakingManager};
        let json = include_str!("../../../testnet/genesis-weighted-base.json");
        let genesis = Genesis::from_json(json).unwrap();
        let dir = TempDir::new().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        genesis.initialize(&db).unwrap();
        let staking = StakingManager::new(db);
        let config = genesis.chain_config();
        let plan = EpochManager::plan_rotation(
            &staking,
            config.max_validators,
            2 * config.epoch_length,
            config.epoch_length,
        )
        .unwrap();
        assert!(!plan.changed, "nothing moved since genesis: no hotstuff update");
        let installed = staking.current_epoch_validator_set().unwrap().expect("recorded at genesis");
        let hotstuff = genesis.validator_set().unwrap();
        let mut keys: Vec<_> = installed.validators.iter().map(|v| (v.pubkey.0, v.power)).collect();
        let mut expected: Vec<_> = hotstuff.validators.iter().map(|v| (v.pubkey.0, v.power)).collect();
        keys.sort();
        expected.sort();
        assert_eq!(keys, expected, "the hotstuff genesis set");
    }

    /// Dead-leader skip (owner s134): K and the j cap are genesis parameters, defaults 0 (off,
    /// owner s138) and 6 when absent, mapped to the chain config; out-of-range values refuse the genesis
    /// (K: 0 = off or 2..=64; cap: 0..=10).
    #[test]
    fn genesis_dead_leader_params_default_and_range_check() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        assert!(
            !sample_genesis_json().contains("leader_miss_threshold"),
            "fixture: K absent"
        );
        assert_eq!(
            genesis.consensus.leader_miss_threshold, 0,
            "absent K: off (owner s138)"
        );
        assert_eq!(genesis.consensus.leader_exclusion_cap, 6);
        let config = genesis.chain_config();
        assert_eq!(
            (config.leader_miss_threshold, config.leader_exclusion_cap),
            (0, 6)
        );
        assert!(config.validate().is_ok());

        let with = |k: serde_json::Value, cap: serde_json::Value| {
            let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
            json["consensus"]["leader_miss_threshold"] = k;
            json["consensus"]["leader_exclusion_cap"] = cap;
            Genesis::from_json(&json.to_string())
        };
        for (k, cap) in [(0, 6), (2, 0), (3, 6), (64, 10)] {
            let g = with(k.into(), cap.into()).unwrap_or_else(|e| panic!("K {k} cap {cap}: {e}"));
            assert_eq!(g.chain_config().leader_miss_threshold, k);
            assert_eq!(g.chain_config().leader_exclusion_cap, cap);
        }
        for (k, cap) in [(1, 6), (65, 6), (3, 11), (1000, 6)] {
            let err = with(k.into(), cap.into()).expect_err(&format!("K {k} cap {cap} accepted"));
            assert!(matches!(err, GenesisError::InvalidConsensus(_)), "{err}");
        }
        assert!(
            with((-1).into(), 6.into()).is_err(),
            "a negative K is no u32"
        );
    }

    #[test]
    fn genesis_without_commit_lag_backoff_cap_defaults_off() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        assert_eq!(genesis.consensus.commit_lag_backoff_cap, 0);
        assert_eq!(genesis.chain_config().commit_lag_backoff_cap, 0);
        // Pre-existing Task A defaults are untouched by S470.
        assert_eq!(genesis.chain_config().backoff_factor, 2);
        assert_eq!(genesis.chain_config().backoff_cap, 8);
    }

    /// Commit 4: the network chain id (EIP-712 domain, EVM, consensus) comes
    /// from genesis `chain_id`; a genesis without it is devnet, 7778.
    #[test]
    fn genesis_chain_id_defaults_to_devnet_7778() {
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json.as_object_mut().unwrap().remove("chain_id");
        let genesis = Genesis::from_json(&json.to_string()).unwrap();
        assert_eq!(genesis.chain_config().chain_id, torus_types::eip712::TORUS_CHAIN_ID);
        json["chain_id"] = 7779.into();
        json["evm"]["chain_id"] = 7779.into();
        let genesis = Genesis::from_json(&json.to_string()).unwrap();
        assert_eq!(genesis.chain_config().chain_id, 7779);
    }

    /// s138 audit: the top-level `chain_id` drives the node, but EVM signers
    /// (tools/matched-bench/run-cell.sh) read `evm.chain_id`; a genesis whose
    /// two ids differ is refused at load, naming both values.
    #[test]
    fn genesis_refuses_evm_chain_id_mismatch() {
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["chain_id"] = 7779.into();
        json["evm"]["chain_id"] = 7778.into();
        let err = Genesis::from_json(&json.to_string()).unwrap_err();
        assert!(matches!(err, GenesisError::InvalidEvm(_)), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("evm.chain_id 7778"), "{msg}");
        assert!(msg.contains("chain_id 7779"), "{msg}");
        // A genesis without top-level chain_id means devnet 7778.
        json.as_object_mut().unwrap().remove("chain_id");
        json["evm"]["chain_id"] = 1.into();
        let err = Genesis::from_json(&json.to_string()).unwrap_err();
        assert!(matches!(err, GenesisError::InvalidEvm(_)), "{err}");
    }

    #[test]
    fn genesis_accepts_matching_evm_chain_id() {
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["chain_id"] = 7780.into();
        json["evm"]["chain_id"] = 7780.into();
        let genesis = Genesis::from_json(&json.to_string()).unwrap();
        assert_eq!(genesis.chain_id, 7780);
        assert_eq!(genesis.evm.chain_id, 7780);
        json.as_object_mut().unwrap().remove("chain_id");
        json["evm"]["chain_id"] = torus_types::eip712::TORUS_CHAIN_ID.into();
        Genesis::from_json(&json.to_string()).unwrap();
    }

    /// s140 (owner): `economics.fee_split` is the chain's fee split
    /// (`ChainConfig::fee_schedule`): any split whose start and end each sum
    /// to 10_000 bps loads, with any `transition_epochs`; a dev share of 0
    /// is valid. Was row 80 option D (s100): only the built-in split loaded.
    #[test]
    fn genesis_fee_split_any_valid_split_reaches_the_chain_config() {
        use torus_types::{FeeSchedule, FeeShares};
        let base: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        let config = Genesis::from_json(&base.to_string()).unwrap().chain_config();
        assert_eq!(config.fee_schedule, torus_types::BUILTIN_FEE_SCHEDULE);
        let mut json = base.clone();
        json["economics"]["fee_split"] = serde_json::json!({
            "start": { "burn": 1000, "validator": 0, "treasury": 9000, "dev_pool": 0 },
            "end": { "burn": 2000, "validator": 3000, "treasury": 5000, "dev_pool": 0 },
            "transition_epochs": 365
        });
        let config = Genesis::from_json(&json.to_string()).unwrap().chain_config();
        assert_eq!(
            config.fee_schedule,
            FeeSchedule {
                start: FeeShares { burn: 1000, validator: 0, treasury: 9000, dev_pool: 0 },
                end: FeeShares { burn: 2000, validator: 3000, treasury: 5000, dev_pool: 0 },
                transition_epochs: 365,
            }
        );
        assert!(config.validate().is_ok());
        assert_eq!(
            (config.fee_burn_bps, config.fee_validator_bps, config.fee_treasury_bps, config.fee_dev_pool_bps),
            (1000, 0, 9000, 0),
            "the informational fields are the start shares"
        );
        json["economics"]["fee_split"]["transition_epochs"] = 0.into();
        assert_eq!(
            Genesis::from_json(&json.to_string()).unwrap().chain_config().fee_schedule.transition_epochs,
            0
        );
    }

    /// s140: a split whose start or end does not sum to exactly 10_000 bps
    /// (or a share that does not fit) refuses to load.
    #[test]
    fn genesis_fee_split_must_sum_to_10000() {
        for (path, value) in [
            ("/economics/fee_split/start/burn", 2000),
            ("/economics/fee_split/start/validator", 1),
            ("/economics/fee_split/start/treasury", 4000),
            ("/economics/fee_split/start/dev_pool", 5000),
            ("/economics/fee_split/end/burn", 0),
            ("/economics/fee_split/end/validator", 5000),
            ("/economics/fee_split/end/treasury", 2400),
            ("/economics/fee_split/end/dev_pool", 2600),
            ("/economics/fee_split/start/burn", 10001),
            // 65536 + 1000 would wrap a u16 back to the built-in 1000.
            ("/economics/fee_split/start/burn", 66536),
        ] {
            let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
            *json.pointer_mut(path).unwrap() = value.into();
            let err = Genesis::from_json(&json.to_string()).unwrap_err();
            assert!(
                matches!(err, GenesisError::InvalidFeeSplit(_)),
                "{path}={value}: {err}"
            );
        }
        // A share of 10_000 with the others 0 is valid.
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["economics"]["fee_split"]["start"] =
            serde_json::json!({ "burn": 10000, "validator": 0, "treasury": 0, "dev_pool": 0 });
        Genesis::from_json(&json.to_string()).unwrap();
    }

    /// s140 determinism: a genesis without `economics.fee_split` (and one
    /// with the built-in values, the sample) splits by the built-in schedule;
    /// a dev share of 0 loads without a `dev_pool_address` (absent = the zero
    /// address, as before s140; it is then never credited).
    #[test]
    fn genesis_without_fee_split_is_the_builtin_split() {
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["economics"].as_object_mut().unwrap().remove("fee_split");
        let config = Genesis::from_json(&json.to_string()).unwrap().chain_config();
        assert_eq!(config.fee_schedule, torus_types::BUILTIN_FEE_SCHEDULE);
        assert_eq!(
            (config.fee_burn_bps, config.fee_validator_bps, config.fee_treasury_bps, config.fee_dev_pool_bps),
            (1000, 0, 4500, 4500)
        );
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["economics"].as_object_mut().unwrap().remove("dev_pool_address");
        json["economics"]["fee_split"]["start"] =
            serde_json::json!({ "burn": 1000, "validator": 0, "treasury": 9000, "dev_pool": 0 });
        json["economics"]["fee_split"]["end"] =
            serde_json::json!({ "burn": 2500, "validator": 2500, "treasury": 5000, "dev_pool": 0 });
        let config = Genesis::from_json(&json.to_string()).unwrap().chain_config();
        assert_eq!(config.dev_pool_address, Address::ZERO);
        assert!(!config.fee_schedule.pays_dev_pool());
    }

    /// The file loader (the node's path) applies the same check.
    #[test]
    fn genesis_file_with_an_invalid_fee_split_is_refused() {
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["economics"]["fee_split"]["start"]["burn"] = 2500.into();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("genesis.json");
        std::fs::write(&path, json.to_string()).unwrap();
        assert!(matches!(
            Genesis::from_file(&path),
            Err(GenesisError::InvalidFeeSplit(_))
        ));
    }

    #[test]
    fn parse_genesis_json() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        assert_eq!(genesis.chain_id, 7778);
        assert_eq!(genesis.chain_name, "torus-devnet");
        assert_eq!(genesis.validators.len(), 1);
        assert_eq!(genesis.accounts.len(), 2);
        assert_eq!(genesis.evm_alloc.len(), 1);
        assert_eq!(genesis.consensus.epoch_length, 100);
        assert_eq!(genesis.evm.gas_limit, 30_000_000);
    }

    #[test]
    fn chain_config_from_genesis() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        let config = genesis.chain_config();
        assert_eq!(config.chain_id, 7778);
        assert_eq!(config.evm_gas_limit, 30_000_000);
        assert_eq!(config.base_fee_per_gas, 1_000_000_000);
        assert_eq!(config.epoch_length, 100);
        assert_eq!(config.fee_burn_bps, 1000);
        assert_eq!(config.fee_treasury_bps, 4500);
        assert_eq!(
            config.treasury_address,
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(
            config.dev_pool_address,
            "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .parse::<Address>()
                .unwrap()
        );
    }

    #[test]
    fn validator_set_from_genesis() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        let vs = genesis.validator_set().unwrap();
        assert_eq!(vs.validators.len(), 1);
        assert_eq!(vs.epoch, 0);
        // 1_000_000 TRS stake -> power = 1_000_000
        assert_eq!(vs.validators[0].power, 1_000_000);
        assert_eq!(vs.validators[0].commission_bps, 500);
    }

    #[test]
    fn initialize_state_db() {
        let dir = TempDir::new().unwrap();
        let state_db = StateDb::open(dir.path()).unwrap();
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();

        let state_root = genesis.initialize(&state_db).unwrap();
        assert_ne!(state_root, B256::ZERO);

        // Verify accounts exist
        let treasury: Address = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .parse()
            .unwrap();
        let acct = state_db
            .get_account(&treasury)
            .unwrap()
            .expect("treasury exists");
        assert_eq!(
            acct.balance,
            U256::from_str_radix("1000000000000000000000000000", 10).unwrap()
        );

        // Verify evm_alloc contract
        let contract: Address = "0xcccccccccccccccccccccccccccccccccccccccc"
            .parse()
            .unwrap();
        let acct = state_db
            .get_account(&contract)
            .unwrap()
            .expect("contract exists");
        assert_ne!(acct.code_hash, KECCAK_EMPTY);
        assert!(state_db.get_code(&acct.code_hash).unwrap().is_some());

        // Verify storage
        let val = state_db.get_storage(&contract, &U256::ZERO).unwrap();
        assert_eq!(val, U256::from(1));

        // Verify validator in CF_STAKING_VALIDATORS
        let vaddr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let raw = state_db
            .get_cf_raw(CF_STAKING_VALIDATORS, vaddr.as_slice())
            .unwrap();
        assert!(raw.is_some());
    }

    #[test]
    fn state_root_is_deterministic() {
        let json = sample_genesis_json();

        let dir1 = TempDir::new().unwrap();
        let db1 = StateDb::open(dir1.path()).unwrap();
        let root1 = Genesis::from_json(&json).unwrap().initialize(&db1).unwrap();

        let dir2 = TempDir::new().unwrap();
        let db2 = StateDb::open(dir2.path()).unwrap();
        let root2 = Genesis::from_json(&json).unwrap().initialize(&db2).unwrap();

        assert_eq!(root1, root2, "same genesis must produce same state root");
    }

    #[test]
    fn hotstuff_genesis_roundtrip() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        let (app_state, vs_state) = genesis.to_hotstuff_genesis().unwrap();
        // App state is empty (we use RocksDB CFs, not consensus KV)
        assert_eq!(app_state.inserts().count(), 0);
        // Validator set has our one validator
        let committed = vs_state.committed_validator_set();
        assert!(committed.len() > 0);
    }

    #[test]
    fn from_file_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("genesis.json");
        std::fs::write(&path, sample_genesis_json()).unwrap();
        let genesis = Genesis::from_file(&path).unwrap();
        assert_eq!(genesis.chain_id, 7778);
    }

    #[test]
    fn parse_u256_decimal_and_hex() {
        assert_eq!(parse_u256("1000").unwrap(), U256::from(1000u64));
        assert_eq!(parse_u256("0xff").unwrap(), U256::from(255u64));
        assert_eq!(parse_u256("0x0").unwrap(), U256::ZERO);
    }

    #[test]
    fn parse_fixedpoint_raw_exact_decimals() {
        assert_eq!(parse_fixedpoint_raw("1").unwrap(), FixedPoint::SCALE);
        assert_eq!(parse_fixedpoint_raw("1.0").unwrap(), FixedPoint::SCALE);
        assert_eq!(parse_fixedpoint_raw("0.01").unwrap(), 1_000_000);
        assert_eq!(parse_fixedpoint_raw("5.0").unwrap(), 5 * FixedPoint::SCALE);
        assert_eq!(parse_fixedpoint_raw("0.5").unwrap(), FixedPoint::SCALE / 2);
        assert_eq!(parse_fixedpoint_raw(".25").unwrap(), 25_000_000);
        assert_eq!(parse_fixedpoint_raw("0.00000001").unwrap(), 1); // 1 raw unit (10^-8)
        assert_eq!(
            parse_fixedpoint_raw("1000000000").unwrap(),
            1_000_000_000i128 * FixedPoint::SCALE
        );
        assert_eq!(
            parse_fixedpoint_raw("-2.5").unwrap(),
            -(5 * FixedPoint::SCALE / 2)
        );
        // Rejections: >8 fractional digits, non-numeric, multiple dots.
        assert!(parse_fixedpoint_raw("0.000000001").is_err());
        assert!(parse_fixedpoint_raw("abc").is_err());
        assert!(parse_fixedpoint_raw("1.2.3").is_err());
    }

    #[test]
    fn initialize_seeds_market_and_native_balance() {
        use borsh::BorshDeserialize;

        // Start from the valid sample genesis and inject a market + a funded balance.
        let mut v: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        v["markets"] = serde_json::json!([{
            "market_id": 1,
            "base_asset": "BTC",
            "quote_asset": "USD",
            "lot_size": "1.0",
            "tick_size": "0.01",
            "initial_margin": "5.0"
        }]);
        v["native_balances"] = serde_json::json!([{
            "address": "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266",
            "available": "1000000000.0"
        }]);

        let genesis = Genesis::from_json(&v.to_string()).unwrap();
        assert_eq!(genesis.markets.len(), 1);
        assert_eq!(genesis.native_balances.len(), 1);

        let dir = TempDir::new().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        genesis.initialize(&db).unwrap();

        // Market round-trips in the EXACT borsh layout the RPC `StoredMarket` reader expects:
        // base(String) + quote(String) + lot(i128) + tick(i128) + initial_margin(i128).
        let raw = db
            .get_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes())
            .unwrap()
            .expect("market 1 should be seeded");
        let (base, quote, lot, tick, margin): (String, String, i128, i128, i128) =
            BorshDeserialize::try_from_slice(&raw).unwrap();
        assert_eq!(base, "BTC");
        assert_eq!(quote, "USD");
        assert_eq!(lot, FixedPoint::SCALE);
        assert_eq!(tick, 1_000_000);
        assert_eq!(margin, 5 * FixedPoint::SCALE);

        // Native balance is readable through the production PositionManager path —
        // this is exactly what exec_place_order's margin check reads.
        let trader: Address = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
            .parse()
            .unwrap();
        let positions = torus_core::position::PositionManager::new(db.clone());
        let bal = positions.get_native_balance(&trader).unwrap();
        assert_eq!(
            bal.available,
            FixedPoint::from_raw(1_000_000_000i128 * FixedPoint::SCALE)
        );
        assert_eq!(bal.order_margin, FixedPoint::ZERO);
    }

    /// Row 43: a genesis market with tick_size or lot_size <= 0 is refused
    /// before anything is written.
    #[test]
    fn initialize_refuses_market_with_zero_tick_or_lot() {
        use torus_state::StateBackend;
        for (tick, lot, field) in [
            ("0", "1.0", "tick_size"),
            ("0.01", "0.0", "lot_size"),
            ("-0.01", "1.0", "tick_size"),
        ] {
            let mut v: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
            v["markets"] = serde_json::json!([
                { "market_id": 1, "base_asset": "BTC", "quote_asset": "USD",
                  "lot_size": "1.0", "tick_size": "1.0", "initial_margin": "5.0" },
                { "market_id": 2, "base_asset": "ETH", "quote_asset": "USD",
                  "lot_size": lot, "tick_size": tick, "initial_margin": "5.0" }
            ]);
            let genesis = Genesis::from_json(&v.to_string()).unwrap();
            let dir = TempDir::new().unwrap();
            let db = StateDb::open(dir.path()).unwrap();
            let err = genesis.initialize(&db).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("invalid market: market 2 {field} must be > 0"),
                "tick {tick} lot {lot}"
            );
            assert!(
                db.iterate_cf(CF_NATIVE_MARKETS, None).unwrap().is_empty(),
                "nothing seeded"
            );
            assert!(
                db.iterate_cf(CF_STAKING_VALIDATORS, None).unwrap().is_empty(),
                "refused before any write"
            );
        }
    }

    // -----------------------------------------------------------------------
    // R19: malformed hex is an error, not a panic; alias keys are refused.
    // -----------------------------------------------------------------------

    fn genesis_with(edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut v: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        edit(&mut v);
        v.to_string()
    }

    /// R19: an odd number of hex digits (or a non-ASCII byte) returns
    /// InvalidHex instead of slicing past the end / mid-character.
    #[test]
    fn decode_hex_odd_length_or_non_ascii_is_an_error_not_a_panic() {
        for bad in ["abc", "0x6", "0X123", "0xé1", "é"] {
            let r = std::panic::catch_unwind(|| decode_hex(bad));
            let r = r.unwrap_or_else(|_| panic!("decode_hex({bad:?}) panicked"));
            assert!(
                matches!(r, Err(GenesisError::InvalidHex(_))),
                "decode_hex({bad:?}) = {r:?}"
            );
        }
        assert_eq!(decode_hex("0x").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_hex("0XaBcD").unwrap(), vec![0xab, 0xcd]);
        assert_eq!(decode_hex("00ff").unwrap(), vec![0x00, 0xff]);
    }

    /// R19: odd-length validator pubkey / contract code is refused at load,
    /// so the node never starts on it (and validator_set() errors, not panics).
    #[test]
    fn genesis_with_odd_length_hex_is_refused_at_load() {
        let json = genesis_with(|v| v["validators"][0]["pubkey"] = "0xabc".into());
        let err = Genesis::from_json(&json).unwrap_err();
        assert!(matches!(err, GenesisError::InvalidHex(_)), "{err}");

        let json = genesis_with(|v| {
            v["evm_alloc"]["0xcccccccccccccccccccccccccccccccccccccccc"]["code"] = "0x608".into()
        });
        let err = Genesis::from_json(&json).unwrap_err();
        assert!(matches!(err, GenesisError::InvalidHex(_)), "{err}");
    }

    /// R19: storage slot keys "0", "0x0", "0X00" are one slot. Two spellings
    /// in one map are refused (naming both), at load and before any write,
    /// instead of HashMap order choosing the value.
    #[test]
    fn evm_alloc_storage_slot_aliases_are_refused() {
        for (a, b) in [("0", "0x0"), ("0x0", "0X00"), ("16", "0x10")] {
            let json = genesis_with(|v| {
                v["evm_alloc"]["0xcccccccccccccccccccccccccccccccccccccccc"]["storage"] =
                    serde_json::json!({ a: "0x1", b: "0x2" })
            });
            let err = Genesis::from_json(&json).unwrap_err().to_string();
            assert!(err.contains("duplicate"), "{a} / {b}: {err}");
            assert!(
                err.contains(&format!("\"{a}\"")) && err.contains(&format!("\"{b}\"")),
                "error names both spellings: {err}"
            );
        }
    }

    /// R19: evm_alloc addresses differing only by case (or 0x prefix) are one
    /// account; two spellings are refused, naming both.
    #[test]
    fn evm_alloc_address_aliases_are_refused() {
        let lower = "0xcccccccccccccccccccccccccccccccccccccccc";
        for other in [
            "0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
            "cccccccccccccccccccccccccccccccccccccccc",
        ] {
            let json =
                genesis_with(|v| v["evm_alloc"][other] = serde_json::json!({ "balance": "0x5" }));
            let err = Genesis::from_json(&json).unwrap_err().to_string();
            assert!(err.contains("duplicate"), "{other}: {err}");
            assert!(
                err.contains(&format!("\"{lower}\"")) && err.contains(&format!("\"{other}\"")),
                "error names both spellings: {err}"
            );
        }
    }

    /// R19 follow-up (owner s112): an address in both accounts[] and
    /// evm_alloc (any spelling) is refused at load, naming the address,
    /// instead of evm_alloc silently overwriting the account.
    #[test]
    fn address_in_both_accounts_and_evm_alloc_is_refused() {
        let alloc = "0xcccccccccccccccccccccccccccccccccccccccc";
        for spelling in [
            alloc,
            "0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
            "cccccccccccccccccccccccccccccccccccccccc",
        ] {
            let json = genesis_with(|v| {
                v["accounts"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({ "address": spelling, "balance": "7" }))
            });
            let err = Genesis::from_json(&json).unwrap_err().to_string();
            assert!(err.contains("duplicate"), "{spelling}: {err}");
            assert!(err.contains(alloc), "names the address: {err}");
            assert!(
                err.contains("accounts") && err.contains("evm_alloc"),
                "names both lists: {err}"
            );
        }
    }

    /// R19 follow-up (owner s112): two validators with the same address (any
    /// spelling; CF_STAKING_VALIDATORS is keyed by it) are refused at load,
    /// naming the address.
    #[test]
    fn duplicate_validator_address_is_refused() {
        let addr = "0x1111111111111111111111111111111111111111";
        for spelling in [addr, "1111111111111111111111111111111111111111"] {
            let json = genesis_with(|v| {
                let vals = v["validators"].as_array_mut().unwrap();
                let mut dup = vals[0].clone();
                dup["address"] = spelling.into();
                dup["pubkey"] =
                    "0x3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29".into();
                vals.push(dup);
            });
            let err = Genesis::from_json(&json).unwrap_err().to_string();
            assert!(err.contains("duplicate"), "{spelling}: {err}");
            assert!(err.contains("validator"), "{err}");
            assert!(err.contains(addr), "names the address: {err}");
        }
    }

    /// R19 follow-up (owner s112): two validators with the same consensus key
    /// (any hex case) are refused at load, naming the key.
    #[test]
    fn duplicate_validator_pubkey_is_refused() {
        let key = "0xd75a980182b10ab7d54bfed3c964073a0ee172f3daa3f4a18446b7e8f5f7a1d6";
        for spelling in [
            key.to_string(),
            format!("0x{}", key[2..].to_uppercase()),
            key[2..].to_string(),
        ] {
            let json = genesis_with(|v| {
                let vals = v["validators"].as_array_mut().unwrap();
                let mut dup = vals[0].clone();
                dup["address"] = "0x2222222222222222222222222222222222222222".into();
                dup["pubkey"] = spelling.clone().into();
                vals.push(dup);
            });
            let err = Genesis::from_json(&json).unwrap_err().to_string();
            assert!(err.contains("duplicate"), "{spelling}: {err}");
            assert!(err.contains("validator"), "{err}");
            assert!(err.contains(key), "names the key: {err}");
        }
    }

    /// R19 follow-up: initialize() refuses an edited Genesis with a
    /// duplicate validator or an accounts/evm_alloc overlap before any write.
    #[test]
    fn initialize_refuses_duplicate_validator_or_overlap_before_any_write() {
        let base = Genesis::from_json(&sample_genesis_json()).unwrap();
        let mut dup_validator = base.clone();
        let mut v = dup_validator.validators[0].clone();
        v.pubkey = "0x3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29".into();
        dup_validator.validators.push(v);
        let mut overlap = base.clone();
        overlap.accounts.push(GenesisAccount {
            address: "0xcccccccccccccccccccccccccccccccccccccccc".into(),
            balance: "1".into(),
            note: None,
        });
        for genesis in [dup_validator, overlap] {
            let dir = TempDir::new().unwrap();
            let db = StateDb::open(dir.path()).unwrap();
            let err = genesis.initialize(&db).unwrap_err().to_string();
            assert!(err.contains("duplicate"), "{err}");
            assert!(
                db.all_accounts().unwrap().is_empty(),
                "refused before any write"
            );
        }
    }

    /// R19: a Genesis built without from_json (fields edited after load) is
    /// still checked by initialize() before the first write.
    #[test]
    fn initialize_refuses_alias_keys_before_any_write() {
        use torus_state::StateBackend;
        let mut genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        let entry = genesis
            .evm_alloc
            .get_mut("0xcccccccccccccccccccccccccccccccccccccccc")
            .unwrap();
        entry
            .storage
            .as_mut()
            .unwrap()
            .insert("0".to_string(), "0x2".to_string());
        let dir = TempDir::new().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let err = genesis.initialize(&db).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
        assert!(
            db.all_accounts().unwrap().is_empty(),
            "refused before any write"
        );
        assert!(
            db.iterate_cf(CF_STAKING_VALIDATORS, None)
                .unwrap()
                .is_empty(),
            "refused before any write"
        );
    }

    /// s140 (owner): a present but malformed `economics.treasury_address` /
    /// `dev_pool_address` refuses to load, naming the field, instead of
    /// becoming the zero address (fees and the governance treasury would be
    /// credited to 0x0).
    #[test]
    fn malformed_economics_address_is_refused_at_load() {
        let base: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        for field in ["treasury_address", "dev_pool_address"] {
            for bad in [
                "FILL:treasury_address",
                "",
                "0x1234",
                "0xzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
                "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ] {
                let mut json = base.clone();
                json["economics"][field] = bad.into();
                let err = Genesis::from_json(&json.to_string())
                    .expect_err(&format!("economics.{field} = {bad:?} must be refused"))
                    .to_string();
                assert!(err.contains("invalid address"), "{field} {bad:?}: {err}");
                assert!(
                    err.contains(&format!("economics.{field}")),
                    "names the field: {err}"
                );
            }
        }
    }

    /// s140: an ABSENT economics address keeps its meaning (the zero
    /// address), so existing genesis files load as before.
    #[test]
    fn absent_economics_address_is_still_the_zero_address() {
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        let economics = json["economics"].as_object_mut().unwrap();
        economics.remove("treasury_address");
        economics.remove("dev_pool_address");
        let genesis = Genesis::from_json(&json.to_string()).expect("absent addresses load");
        let config = genesis.chain_config();
        assert_eq!(config.treasury_address, Address::ZERO);
        assert_eq!(config.dev_pool_address, Address::ZERO);
        let dir = TempDir::new().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        genesis
            .initialize(&db)
            .expect("absent addresses initialize");
        let params = GovernanceManager::new(db.clone())
            .get_governance_params()
            .unwrap();
        assert_eq!(params.treasury_address, Address::ZERO);
    }

    /// s140: initialize() refuses a Genesis whose economics address was
    /// made malformed after from_json, before any write.
    #[test]
    fn initialize_refuses_malformed_economics_address_before_any_write() {
        let base = Genesis::from_json(&sample_genesis_json()).unwrap();
        let mut bad_treasury = base.clone();
        bad_treasury.economics.treasury_address = Some("FILL:treasury_address".into());
        let mut bad_dev_pool = base;
        bad_dev_pool.economics.dev_pool_address = Some("0x1234".into());
        for (genesis, field) in [
            (bad_treasury, "treasury_address"),
            (bad_dev_pool, "dev_pool_address"),
        ] {
            let dir = TempDir::new().unwrap();
            let db = StateDb::open(dir.path()).unwrap();
            let err = genesis.initialize(&db).unwrap_err().to_string();
            assert!(err.contains(&format!("economics.{field}")), "{err}");
            assert!(
                db.all_accounts().unwrap().is_empty(),
                "refused before any write"
            );
        }
    }
}
