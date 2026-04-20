//! Genesis configuration parser and state initializer for Torus-hyperBFT.
//!
//! Parses a genesis JSON file matching the schema in technical-requirements §12,
//! then seeds the state database with pre-funded accounts, contract code,
//! storage, and validator stakes.

use std::collections::HashMap;
use std::path::Path;

use alloy_primitives::{keccak256, Address, B256, U256};
use ed25519_dalek::VerifyingKey;
use hotstuff_rs::types::data_types::Power;
use hotstuff_rs::types::update_sets::AppStateUpdates;
use hotstuff_rs::types::validator_set::{
    ValidatorSet as HsValidatorSet, ValidatorSetState,
};
use revm::state::AccountInfo;
use serde::Deserialize;
use tracing::info;

use torus_economics::types::{ValidatorState, ValidatorStatus};
use torus_state::cf::CF_STAKING_VALIDATORS;
use torus_state::db::KECCAK_EMPTY;
use torus_state::trie::compute_state_root_from_db;
use torus_state::{StateDb, StateError};
use torus_types::{
    ChainConfig, PublicKey, ValidatorInfo,
    ValidatorSet as TorusValidatorSet,
};

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
}

// ---------------------------------------------------------------------------
// Genesis JSON structures (§12)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Genesis {
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
    pub markets: Vec<serde_json::Value>,
    #[serde(default)]
    pub precompiles: HashMap<String, String>,
    #[serde(default)]
    pub permanent_stakes: Vec<GenesisPermanentStake>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConsensusConfig {
    pub protocol: String,
    pub epoch_length: u64,
    pub timeout_base_ms: u64,
    pub timeout_max_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvmConfig {
    pub spec_id: String,
    pub gas_limit: u64,
    pub base_fee_initial: u64,
    pub chain_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EconomicsConfig {
    #[serde(default)]
    pub treasury_address: Option<String>,
    #[serde(default)]
    pub dev_pool_address: Option<String>,
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

fn parse_u256(s: &str) -> Result<U256, GenesisError> {
    let s = s.trim();
    if s.starts_with("0x") || s.starts_with("0X") {
        U256::from_str_radix(&s[2..], 16)
            .map_err(|_| GenesisError::InvalidAmount(s.to_string()))
    } else {
        U256::from_str_radix(s, 10)
            .map_err(|_| GenesisError::InvalidAmount(s.to_string()))
    }
}

fn decode_hex(s: &str) -> Result<Vec<u8>, GenesisError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let s = s.strip_prefix("0X").unwrap_or(s);
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| GenesisError::InvalidHex(s.to_string()))
        })
        .collect()
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
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content).map_err(GenesisError::Json)
    }

    /// Parse a genesis configuration from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, GenesisError> {
        serde_json::from_str(json).map_err(GenesisError::Json)
    }

    /// Initialize the state database with genesis data. Returns the state root.
    ///
    /// 1. Seeds EVM accounts from `accounts` and `evm_alloc`
    /// 2. Seeds contract code and storage from `evm_alloc`
    /// 3. Seeds validator stakes into `CF_STAKING_VALIDATORS`
    /// 4. Computes and returns the state root
    pub fn initialize(&self, state_db: &StateDb) -> Result<B256, GenesisError> {
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
            };
            let data = borsh::to_vec(&state).map_err(|e| {
                GenesisError::InvalidHex(format!("borsh encode validator: {e}"))
            })?;
            state_db.put_cf_raw(CF_STAKING_VALIDATORS, address.as_slice(), &data)?;
            info!(%address, stake = %validator.stake, "seeded genesis validator");
        }

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

        // 5. Compute state root
        let state_root = compute_state_root_from_db(state_db)?;
        info!(%state_root, "genesis state root computed");
        Ok(state_root)
    }

    /// Build a [`ChainConfig`] from the genesis parameters.
    pub fn chain_config(&self) -> ChainConfig {
        ChainConfig {
            chain_id: self.chain_id,
            chain_name: self.chain_name.clone(),
            evm_gas_limit: self.evm.gas_limit,
            base_fee_per_gas: self.evm.base_fee_initial,
            epoch_length: self.consensus.epoch_length,
            max_validators: self.economics.validator.max_validators,
            min_stake: parse_u256(&self.economics.validator.min_self_delegation)
                .unwrap_or_default(),
            fee_burn_bps: self.economics.fee_split.start.burn,
            fee_validator_bps: self.economics.fee_split.start.validator,
            fee_treasury_bps: self.economics.fee_split.start.treasury,
            fee_dev_pool_bps: self.economics.fee_split.start.dev_pool,
            treasury_address: self
                .economics
                .treasury_address
                .as_deref()
                .and_then(|s| parse_address(s).ok())
                .unwrap_or(Address::ZERO),
            dev_pool_address: self
                .economics
                .dev_pool_address
                .as_deref()
                .and_then(|s| parse_address(s).ok())
                .unwrap_or(Address::ZERO),
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
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse::<Address>().unwrap()
        );
        assert_eq!(
            config.dev_pool_address,
            "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".parse::<Address>().unwrap()
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
        let treasury: Address = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
        let acct = state_db.get_account(&treasury).unwrap().expect("treasury exists");
        assert_eq!(
            acct.balance,
            U256::from_str_radix("1000000000000000000000000000", 10).unwrap()
        );

        // Verify evm_alloc contract
        let contract: Address = "0xcccccccccccccccccccccccccccccccccccccccc".parse().unwrap();
        let acct = state_db.get_account(&contract).unwrap().expect("contract exists");
        assert_ne!(acct.code_hash, KECCAK_EMPTY);
        assert!(state_db.get_code(&acct.code_hash).unwrap().is_some());

        // Verify storage
        let val = state_db.get_storage(&contract, &U256::ZERO).unwrap();
        assert_eq!(val, U256::from(1));

        // Verify validator in CF_STAKING_VALIDATORS
        let vaddr: Address = "0x1111111111111111111111111111111111111111".parse().unwrap();
        let raw = state_db.get_cf_raw(CF_STAKING_VALIDATORS, vaddr.as_slice()).unwrap();
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
}
