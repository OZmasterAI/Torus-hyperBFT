//! Transaction mempool for the Torus-hyperBFT blockchain.
//!
//! Manages pending EVM transactions and native actions, providing:
//! - EVM tx pool with nonce tracking and gas price ordering
//! - Transaction validation: signature recovery, nonce/balance checks
//! - Size-limited pool with eviction and replacement-by-fee
//! - Drain interface for block proposers

pub mod error;
pub mod evm_pool;
pub mod native_pool;
pub mod validate;

use std::sync::RwLock;

use alloy_primitives::B256;
use torus_state::StateDb;
use torus_types::SignedNativeAction;

pub use crate::error::MempoolError;
pub use crate::evm_pool::EvmPoolEntry;

/// Mempool configuration.
#[derive(Clone, Debug)]
pub struct MempoolConfig {
    /// Maximum total EVM transactions in the pool.
    pub max_pool_size: usize,
    /// Maximum pending EVM transactions per sender.
    pub max_per_sender: usize,
    /// Minimum gas price bump percentage for replacement (e.g., 10 = 10%).
    pub replacement_bump_pct: u64,
    /// Chain ID for transaction validation.
    pub chain_id: u64,
    /// Block gas limit for validation.
    pub block_gas_limit: u64,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            max_pool_size: 4096,
            max_per_sender: 16,
            replacement_bump_pct: 10,
            chain_id: 7777,
            block_gas_limit: 30_000_000,
        }
    }
}

/// Thread-safe transaction mempool combining EVM and native action pools.
pub struct Mempool {
    evm: RwLock<evm_pool::EvmPool>,
    native: RwLock<native_pool::NativePool>,
    state: StateDb,
    config: MempoolConfig,
}

impl Mempool {
    /// Create a new mempool backed by the given state database.
    pub fn new(state: StateDb, config: MempoolConfig) -> Self {
        Self {
            evm: RwLock::new(evm_pool::EvmPool::new()),
            native: RwLock::new(native_pool::NativePool::new()),
            state,
            config,
        }
    }

    /// Submit a raw RLP-encoded EVM transaction.
    /// Decodes, validates, and inserts. Returns the transaction hash on success.
    pub fn add_evm_tx(&self, raw_rlp: Vec<u8>) -> Result<B256, MempoolError> {
        let entry = validate::validate_evm_tx(
            &raw_rlp,
            &self.state,
            self.config.chain_id,
            self.config.block_gas_limit,
        )?;

        let hash = entry.hash;
        let mut pool = self.evm.write().unwrap();

        if pool.contains(&hash) {
            return Err(MempoolError::DuplicateTx(hash));
        }

        pool.insert(
            entry,
            self.config.max_pool_size,
            self.config.max_per_sender,
            self.config.replacement_bump_pct,
        )?;

        tracing::debug!(tx_hash = %hash, "evm tx added to mempool");
        Ok(hash)
    }

    /// Re-insert previously drained EVM transactions (e.g., after block reorg).
    pub fn reinsert_evm(&self, raw_txs: Vec<Vec<u8>>) {
        for raw in raw_txs {
            if let Err(e) = self.add_evm_tx(raw) {
                tracing::trace!("reinsert skipped: {e}");
            }
        }
    }

    /// Drain EVM transactions for a block proposal.
    /// Selects highest-gas-price transactions that fit within `gas_limit`,
    /// respecting per-sender nonce ordering. Removes selected txs from the pool.
    pub fn drain_evm(&self, gas_limit: u64) -> Vec<Vec<u8>> {
        let mut pool = self.evm.write().unwrap();
        pool.drain(gas_limit)
    }

    /// Current EVM pool size.
    pub fn evm_pool_size(&self) -> usize {
        self.evm.read().unwrap().size()
    }

    /// Submit a signed native action.
    pub fn add_native_action(&self, action: SignedNativeAction) {
        let mut pool = self.native.write().unwrap();
        pool.insert(action);
    }

    /// Drain native actions for a block proposal.
    /// Returns up to `limit` actions in priority order (cancels first).
    pub fn drain_native(&self, limit: usize) -> Vec<SignedNativeAction> {
        let mut pool = self.native.write().unwrap();
        pool.drain(limit)
    }

    /// Re-insert previously drained native actions.
    pub fn reinsert_native(&self, actions: Vec<SignedNativeAction>) {
        let mut pool = self.native.write().unwrap();
        pool.reinsert(actions);
    }

    /// Current native pool size.
    pub fn native_pool_size(&self) -> usize {
        self.native.read().unwrap().size()
    }

    /// Drain both pools for a block proposal.
    pub fn drain_for_block(
        &self,
        native_limit: usize,
        evm_gas_limit: u64,
    ) -> (Vec<SignedNativeAction>, Vec<Vec<u8>>) {
        let native = self.drain_native(native_limit);
        let evm = self.drain_evm(evm_gas_limit);
        (native, evm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
    use alloy_primitives::{Address, Bytes, Signature as AlloySig, TxKind, B256, U256};
    use alloy_rlp::Encodable;
    use k256::ecdsa::SigningKey;
    use revm::state::AccountInfo;
    use sha3::{Digest, Keccak256};
    use tempfile::TempDir;

    fn address_from_key(key: &SigningKey) -> Address {
        let vk = key.verifying_key();
        let pubkey = vk.to_encoded_point(false);
        let hash = Keccak256::digest(&pubkey.as_bytes()[1..]);
        Address::from_slice(&hash[12..])
    }

    fn create_eip1559_tx(
        key: &SigningKey,
        nonce: u64,
        max_fee: u128,
        priority_fee: u128,
        gas_limit: u64,
        value: U256,
    ) -> Vec<u8> {
        let tx = TxEip1559 {
            chain_id: 7777,
            nonce,
            max_fee_per_gas: max_fee,
            max_priority_fee_per_gas: priority_fee,
            gas_limit,
            to: TxKind::Call(Address::ZERO),
            value,
            input: Bytes::new(),
            access_list: Default::default(),
        };

        let sig_hash = tx.signature_hash();
        let (sig, recid) = key.sign_prehash_recoverable(sig_hash.as_slice()).unwrap();
        let r_bytes = sig.r().to_bytes();
        let s_bytes = sig.s().to_bytes();
        let r = U256::from_be_slice(r_bytes.as_slice());
        let s = U256::from_be_slice(s_bytes.as_slice());
        let v = recid.is_y_odd();
        let signature = AlloySig::new(r, s, v);

        let signed = tx.into_signed(signature);
        let envelope = TxEnvelope::Eip1559(signed);
        let mut buf = Vec::new();
        envelope.encode(&mut buf);
        buf
    }

    fn setup() -> (TempDir, StateDb) {
        let dir = TempDir::new().unwrap();
        let state = StateDb::open(dir.path()).unwrap();
        (dir, state)
    }

    fn fund(state: &StateDb, addr: &Address, balance: U256, nonce: u64) {
        state
            .put_account(
                addr,
                &AccountInfo {
                    balance,
                    nonce,
                    code_hash: B256::ZERO,
                    code: None,
                    account_id: None,
                },
            )
            .unwrap();
    }

    fn key(seed: u8) -> SigningKey {
        let mut secret = [0u8; 32];
        secret[31] = seed;
        secret[0] = 1;
        SigningKey::from_slice(&secret).unwrap()
    }

    #[test]
    fn insert_and_retrieve() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(1);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        let raw = create_eip1559_tx(&k, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO);
        let hash = pool.add_evm_tx(raw).unwrap();
        assert_eq!(pool.evm_pool_size(), 1);
        assert!(!hash.is_zero());
    }

    #[test]
    fn nonce_ordering_per_sender() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(2);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            2,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            3_000_000_000,
            300_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            1,
            2_000_000_000,
            200_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.evm_pool_size(), 3);

        let drained = pool.drain_evm(30_000_000);
        assert_eq!(drained.len(), 3);
        assert_eq!(pool.evm_pool_size(), 0);
    }

    #[test]
    fn reject_wrong_chain_id() {
        let (_dir, state) = setup();
        let k = key(3);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        let tx = TxEip1559 {
            chain_id: 9999,
            nonce: 0,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 100_000_000,
            gas_limit: 21_000,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: Default::default(),
        };
        let sig_hash = tx.signature_hash();
        let (sig, recid) = k.sign_prehash_recoverable(sig_hash.as_slice()).unwrap();
        let signature = AlloySig::new(
            U256::from_be_slice(sig.r().to_bytes().as_slice()),
            U256::from_be_slice(sig.s().to_bytes().as_slice()),
            recid.is_y_odd(),
        );
        let envelope = TxEnvelope::Eip1559(tx.into_signed(signature));
        let mut raw = Vec::new();
        envelope.encode(&mut raw);

        let pool = Mempool::new(state, MempoolConfig::default());
        let err = pool.add_evm_tx(raw).unwrap_err();
        assert!(matches!(err, MempoolError::InvalidChainId { .. }));
    }

    #[test]
    fn reject_nonce_too_low() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(4);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 5);

        let raw = create_eip1559_tx(&k, 3, 1_000_000_000, 100_000_000, 21_000, U256::ZERO);
        let err = pool.add_evm_tx(raw).unwrap_err();
        assert!(matches!(err, MempoolError::NonceTooLow { .. }));
    }

    #[test]
    fn reject_insufficient_balance() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(5);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(1000u64), 0);

        let raw = create_eip1559_tx(&k, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO);
        let err = pool.add_evm_tx(raw).unwrap_err();
        assert!(matches!(err, MempoolError::InsufficientBalance { .. }));
    }

    #[test]
    fn reject_gas_limit_exceeded() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(6);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        let raw = create_eip1559_tx(&k, 0, 1_000_000_000, 100_000_000, 31_000_000, U256::ZERO);
        let err = pool.add_evm_tx(raw).unwrap_err();
        assert!(matches!(err, MempoolError::GasLimitExceeded { .. }));
    }

    #[test]
    fn replacement_by_fee() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(7);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.evm_pool_size(), 1);

        let err = pool
            .add_evm_tx(create_eip1559_tx(
                &k,
                0,
                1_050_000_000,
                105_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap_err();
        assert!(matches!(err, MempoolError::ReplacementUnderpriced { .. }));

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_200_000_000,
            120_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.evm_pool_size(), 1);
    }

    #[test]
    fn pool_size_limit_eviction() {
        let (_dir, state) = setup();
        let config = MempoolConfig {
            max_pool_size: 3,
            max_per_sender: 16,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state.clone(), config);

        for i in 1..=3u8 {
            let k = key(10 + i);
            let addr = address_from_key(&k);
            fund(&state, &addr, U256::from(10u64.pow(18)), 0);
            pool.add_evm_tx(create_eip1559_tx(
                &k,
                0,
                (i as u128) * 1_000_000_000,
                (i as u128) * 100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap();
        }
        assert_eq!(pool.evm_pool_size(), 3);

        let k4 = key(14);
        fund(&state, &address_from_key(&k4), U256::from(10u64.pow(18)), 0);
        pool.add_evm_tx(create_eip1559_tx(
            &k4,
            0,
            5_000_000_000,
            500_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.evm_pool_size(), 3);

        let k5 = key(15);
        fund(&state, &address_from_key(&k5), U256::from(10u64.pow(18)), 0);
        let err = pool
            .add_evm_tx(create_eip1559_tx(
                &k5,
                0,
                500_000_000,
                50_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap_err();
        assert!(matches!(err, MempoolError::PoolFull));
    }

    #[test]
    fn per_sender_limit() {
        let (_dir, state) = setup();
        let config = MempoolConfig {
            max_per_sender: 2,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state.clone(), config);

        let k = key(20);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            1,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        let err = pool
            .add_evm_tx(create_eip1559_tx(
                &k,
                2,
                1_000_000_000,
                100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap_err();
        assert!(matches!(err, MempoolError::PoolFull));
    }

    #[test]
    fn drain_respects_gas_budget() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(30);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        for i in 0..3u64 {
            pool.add_evm_tx(create_eip1559_tx(
                &k,
                i,
                1_000_000_000,
                100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap();
        }

        let drained = pool.drain_evm(42_000);
        assert_eq!(drained.len(), 2);
        assert_eq!(pool.evm_pool_size(), 1);
    }

    #[test]
    fn drain_gas_price_priority_across_senders() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let ka = key(40);
        fund(&state, &address_from_key(&ka), U256::from(10u64.pow(18)), 0);
        pool.add_evm_tx(create_eip1559_tx(
            &ka,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();

        let kb = key(41);
        fund(&state, &address_from_key(&kb), U256::from(10u64.pow(18)), 0);
        pool.add_evm_tx(create_eip1559_tx(
            &kb,
            0,
            5_000_000_000,
            500_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();

        let drained = pool.drain_evm(21_000);
        assert_eq!(drained.len(), 1);
        assert_eq!(pool.evm_pool_size(), 1);

        let rest = pool.drain_evm(30_000_000);
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn drain_native_priority() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());

        use torus_types::{NativeAction, Signature};
        let sig = Signature {
            v: 27,
            r: [0u8; 32],
            s: [0u8; 32],
        };

        pool.add_native_action(SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 1,
            signature: sig.clone(),
        });
        pool.add_native_action(SignedNativeAction {
            action: NativeAction::CancelOrder { order_id: 42 },
            nonce: 2,
            signature: sig.clone(),
        });
        pool.add_native_action(SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 3,
            signature: sig,
        });

        let drained = pool.drain_native(2);
        assert_eq!(drained.len(), 2);
        assert!(matches!(
            drained[0].action,
            NativeAction::CancelOrder { .. }
        ));
        assert_eq!(pool.native_pool_size(), 1);
    }

    #[test]
    fn reinsert_evm_after_drain() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(50);
        fund(&state, &address_from_key(&k), U256::from(10u64.pow(18)), 0);
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();

        let drained = pool.drain_evm(30_000_000);
        assert_eq!(pool.evm_pool_size(), 0);

        pool.reinsert_evm(drained);
        assert_eq!(pool.evm_pool_size(), 1);
    }

    #[test]
    fn drain_for_block_combined() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(60);
        fund(&state, &address_from_key(&k), U256::from(10u64.pow(18)), 0);
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();

        pool.add_native_action(SignedNativeAction {
            action: torus_types::NativeAction::ClaimRewards,
            nonce: 1,
            signature: torus_types::Signature {
                v: 27,
                r: [0; 32],
                s: [0; 32],
            },
        });

        let (native, evm) = pool.drain_for_block(10, 30_000_000);
        assert_eq!(evm.len(), 1);
        assert_eq!(native.len(), 1);
        assert_eq!(pool.evm_pool_size(), 0);
        assert_eq!(pool.native_pool_size(), 0);
    }
}
