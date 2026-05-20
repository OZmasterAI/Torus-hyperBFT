//! Copy-on-write state overlay for speculative block execution.

use std::collections::HashMap;

use alloy_primitives::{Address, Bytes, B256, U256};
use revm::bytecode::Bytecode;
use revm::database::BundleState;
use revm::state::AccountInfo;

use crate::db::{StateDb, KECCAK_EMPTY};
use crate::error::StateError;

/// In-memory write overlay on top of a `StateDb`.
///
/// Reads check the overlay first, then fall through to the base database.
/// Writes only affect the overlay. Call [`commit`](StateOverlay::commit) to persist
/// or simply drop to roll back.
pub struct StateOverlay {
    base: StateDb,
    accounts: HashMap<Address, Option<AccountInfo>>,
    storage: HashMap<Address, HashMap<U256, U256>>,
    code: HashMap<B256, Vec<u8>>,
}

impl StateOverlay {
    /// Create a new overlay on top of the given base database.
    pub fn new(base: StateDb) -> Self {
        Self {
            base,
            accounts: HashMap::new(),
            storage: HashMap::new(),
            code: HashMap::new(),
        }
    }

    /// Write an account to the overlay. `None` marks the account as deleted.
    pub fn set_account(&mut self, address: Address, info: Option<AccountInfo>) {
        self.accounts.insert(address, info);
    }

    /// Write a storage slot to the overlay.
    pub fn set_storage(&mut self, address: Address, slot: U256, value: U256) {
        self.storage.entry(address).or_default().insert(slot, value);
    }

    /// Write contract code to the overlay.
    pub fn set_code(&mut self, code_hash: B256, code: Vec<u8>) {
        self.code.insert(code_hash, code);
    }

    /// Persist all overlay changes to the base `StateDb`, consuming the overlay.
    pub fn commit(self) -> Result<(), StateError> {
        for (address, info) in &self.accounts {
            match info {
                Some(info) => self.base.put_account(address, info)?,
                None => self.base.delete_account(address)?,
            }
        }
        for (address, slots) in &self.storage {
            for (slot, value) in slots {
                self.base.put_storage(address, slot, value)?;
            }
        }
        for (hash, code) in &self.code {
            self.base.put_code(hash, code)?;
        }
        Ok(())
    }

    /// Number of modified accounts in the overlay.
    pub fn dirty_account_count(&self) -> usize {
        self.accounts.len()
    }

    /// Total number of modified storage slots in the overlay.
    pub fn dirty_storage_count(&self) -> usize {
        self.storage.values().map(|s| s.len()).sum()
    }

    /// Reference to the underlying base database.
    pub fn base(&self) -> &StateDb {
        &self.base
    }

    /// Create an overlay pre-populated with changes from a `BundleState`.
    pub fn from_bundle(base: StateDb, bundle: &BundleState) -> Self {
        let mut overlay = Self::new(base);
        overlay.apply_bundle(bundle);
        overlay
    }

    /// Apply all account, storage, and code changes from a `BundleState`.
    ///
    /// Later calls override earlier ones for the same key, so call in
    /// ancestor-first order when layering multiple bundles.
    pub fn apply_bundle(&mut self, bundle: &BundleState) {
        for (address, acct) in &bundle.state {
            self.accounts.insert(*address, acct.info.clone());
            for (slot, slot_val) in &acct.storage {
                self.storage
                    .entry(*address)
                    .or_default()
                    .insert(*slot, slot_val.present_value);
            }
        }
        for (hash, bytecode) in &bundle.contracts {
            self.code.insert(*hash, bytecode.bytes().to_vec());
        }
    }
}

impl revm::DatabaseRef for StateOverlay {
    type Error = StateError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        if let Some(info) = self.accounts.get(&address) {
            return Ok(info.clone());
        }
        self.base.get_account(&address)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash == KECCAK_EMPTY || code_hash == B256::ZERO {
            return Ok(Bytecode::default());
        }
        if let Some(code) = self.code.get(&code_hash) {
            return Ok(Bytecode::new_raw(Bytes::from(code.clone())));
        }
        revm::DatabaseRef::code_by_hash_ref(&self.base, code_hash)
    }

    fn storage_ref(
        &self,
        address: Address,
        index: revm::primitives::StorageKey,
    ) -> Result<revm::primitives::StorageValue, Self::Error> {
        if let Some(slots) = self.storage.get(&address) {
            if let Some(value) = slots.get(&index) {
                return Ok(*value);
            }
        }
        self.base.get_storage(&address, &index)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        revm::DatabaseRef::block_hash_ref(&self.base, number)
    }
}
