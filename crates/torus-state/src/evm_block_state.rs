//! Item 7 option 0, task 1.5 (`docs/plans/item7-option0-impl.md`): the EVM
//! part of a block reads through the block's layered overlay (own pending ->
//! parent -> L -> C -> DB), never the raw DB.
//!
//! - [`LayeredEvmDb`]: revm's [`revm::DatabaseRef`] over a
//!   [`NativeStateOverlay`]. Accounts (CF_ACCOUNTS), storage (CF_STORAGE),
//!   code (CF_CODE) and block hashes (CF_BLOCK_HEADERS, first 32 bytes) decode
//!   exactly as `DatabaseRef for StateDb` (the decoders are shared).
//! - [`EvmBlockState`]: that adapter plus the writer-precompile journal, a
//!   fresh overlay over the SAME layers (parent, L in its two parts, C) and
//!   the same DB, without the resident rows R (design point 2, H3). The
//!   journal moves into the executor and comes back as the block's
//!   `native_writes`, which the block overlay absorbs (task 1.2): its layer
//!   references are then released.

use alloy_primitives::{Address, B256};
use revm::bytecode::Bytecode;
use revm::state::AccountInfo;

use crate::backend::{NativeStateOverlay, StateBackend};
use crate::cf::{CF_ACCOUNTS, CF_BLOCK_HEADERS, CF_CODE, CF_STORAGE};
use crate::db::{
    code_hash_is_empty, decode_account_info, decode_block_hash, decode_code,
    decode_storage_value, storage_key, StateDb,
};
use crate::error::StateError;

/// revm reads through a block overlay (see the module docs).
#[derive(Clone, Copy)]
pub struct LayeredEvmDb<'a> {
    overlay: &'a NativeStateOverlay,
}

impl<'a> LayeredEvmDb<'a> {
    pub fn new(overlay: &'a NativeStateOverlay) -> Self {
        Self { overlay }
    }

    /// The overlay the reads go through.
    pub fn overlay(&self) -> &'a NativeStateOverlay {
        self.overlay
    }

    #[inline]
    fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        #[cfg(feature = "test-probes")]
        let _scope = crate::probes::scope(crate::probes::Reader::Revm);
        StateBackend::get_cf_raw(self.overlay, cf, key)
    }
}

impl revm::DatabaseRef for LayeredEvmDb<'_> {
    type Error = StateError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        match self.get(CF_ACCOUNTS, address.as_slice())? {
            Some(data) => Ok(Some(decode_account_info(&data)?)),
            None => Ok(None),
        }
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash_is_empty(&code_hash) {
            return Ok(Bytecode::default());
        }
        Ok(decode_code(self.get(CF_CODE, code_hash.as_slice())?))
    }

    fn storage_ref(
        &self,
        address: Address,
        index: revm::primitives::StorageKey,
    ) -> Result<revm::primitives::StorageValue, Self::Error> {
        let key = storage_key(&address, &index);
        decode_storage_value(self.get(CF_STORAGE, &key)?.as_deref())
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        Ok(decode_block_hash(self.get(CF_BLOCK_HEADERS, &number.to_be_bytes())?.as_deref())
            .unwrap_or(B256::ZERO))
    }
}

/// A block's EVM reads: the revm adapter and the writer-precompile journal
/// over the same layers (see the module docs).
pub struct EvmBlockState<'a> {
    pub db: LayeredEvmDb<'a>,
    pub journal: NativeStateOverlay,
}

impl<'a> EvmBlockState<'a> {
    /// Over `overlay`'s layers. The journal gets a pending set of its own and
    /// no R; `overlay` itself is only read.
    pub fn new(overlay: &'a NativeStateOverlay) -> Self {
        Self {
            db: LayeredEvmDb::new(overlay),
            journal: overlay.journal_over_layers(),
        }
    }

    /// Item 7 option 0 (task 1.4): a point read through the block overlay
    /// (own pending -> parent -> L -> C -> DB): the block's lane read (the
    /// senders' `EvmUserModify` rows), probe-labelled `Lane`.
    pub fn lane_read(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        #[cfg(feature = "test-probes")]
        let _scope = crate::probes::scope(crate::probes::Reader::Lane);
        StateBackend::get_cf_raw(self.db.overlay(), cf, key)
    }

    /// The DB under the layers (the serial EVM root compute reads it).
    pub fn state_db(&self) -> &'a StateDb {
        self.db.overlay().db()
    }
}
