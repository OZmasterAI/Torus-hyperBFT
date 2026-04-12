//! RocksDB storage + MPT state root computation.

// Re-export reth trie types used for Ethereum-compatible state root computation.
pub use reth_trie_common::HashedPostState;

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    #[test]
    fn reth_trie_types_importable() {
        // Verify HashedPostState is constructible
        let state = HashedPostState::default();
        assert!(state.accounts.is_empty());
        assert!(state.storages.is_empty());
    }

    #[test]
    fn alloy_primitives_compatible() {
        // Verify alloy types work across crate boundaries
        let hash = B256::ZERO;
        assert_eq!(hash, B256::default());
    }
}
