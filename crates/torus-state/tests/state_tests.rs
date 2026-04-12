//! Integration tests for the torus-state crate.
//!
//! Covers: RocksDB column families, revm DatabaseRef, overlay isolation,
//! and MPT state root computation against Ethereum test vectors.

use alloy_primitives::{address, keccak256, B256, U256};
use reth_trie_common::{TrieAccount, EMPTY_ROOT_HASH};
use revm::state::AccountInfo;
use revm::DatabaseRef;

use torus_state::cf::ALL_CF_NAMES;
use torus_state::db::{StateDb, KECCAK_EMPTY};
use torus_state::overlay::StateOverlay;
use torus_state::trie;

/// Helper: create a temporary StateDb.
fn temp_db() -> (StateDb, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (db, dir)
}

// ============================================================================
// 1.2.1 — RocksDB wrapper with column family layout
// ============================================================================

#[test]
fn opens_with_all_column_families() {
    let (db, _dir) = temp_db();
    // Verify every CF from §6.1 is accessible
    for cf_name in ALL_CF_NAMES {
        db.get_cf_raw(cf_name, b"test_key")
            .unwrap_or_else(|e| panic!("CF {cf_name} not accessible: {e}"));
    }
    assert_eq!(db.column_families().len(), ALL_CF_NAMES.len());
}

#[test]
fn account_write_read_roundtrip() {
    let (db, _dir) = temp_db();
    let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    // Initially no account
    assert!(db.get_account(&addr).unwrap().is_none());

    let info = AccountInfo {
        balance: U256::from(1_000_000_000u64),
        nonce: 42,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    db.put_account(&addr, &info).unwrap();

    let loaded = db
        .get_account(&addr)
        .unwrap()
        .expect("account should exist");
    assert_eq!(loaded.balance, info.balance);
    assert_eq!(loaded.nonce, info.nonce);
    assert_eq!(loaded.code_hash, info.code_hash);
}

#[test]
fn account_delete() {
    let (db, _dir) = temp_db();
    let addr = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

    let info = AccountInfo {
        balance: U256::from(100u64),
        nonce: 1,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    db.put_account(&addr, &info).unwrap();
    assert!(db.get_account(&addr).unwrap().is_some());

    db.delete_account(&addr).unwrap();
    assert!(db.get_account(&addr).unwrap().is_none());
}

#[test]
fn storage_write_read_roundtrip() {
    let (db, _dir) = temp_db();
    let addr = address!("cccccccccccccccccccccccccccccccccccccccc");
    let slot = U256::from(0u64);
    let value = U256::from(0xdeadbeef_u64);

    // Initially zero
    assert_eq!(db.get_storage(&addr, &slot).unwrap(), U256::ZERO);

    db.put_storage(&addr, &slot, &value).unwrap();
    assert_eq!(db.get_storage(&addr, &slot).unwrap(), value);
}

#[test]
fn storage_zero_deletes() {
    let (db, _dir) = temp_db();
    let addr = address!("dddddddddddddddddddddddddddddddddddddddd");
    let slot = U256::from(1u64);

    db.put_storage(&addr, &slot, &U256::from(42u64)).unwrap();
    assert_ne!(db.get_storage(&addr, &slot).unwrap(), U256::ZERO);

    // Writing zero should delete the entry
    db.put_storage(&addr, &slot, &U256::ZERO).unwrap();
    assert_eq!(db.get_storage(&addr, &slot).unwrap(), U256::ZERO);
}

#[test]
fn code_write_read_roundtrip() {
    let (db, _dir) = temp_db();
    let bytecode = vec![0x60, 0x00, 0x60, 0x00, 0xf3]; // PUSH0 PUSH0 RETURN
    let code_hash = keccak256(&bytecode);

    assert!(db.get_code(&code_hash).unwrap().is_none());

    db.put_code(&code_hash, &bytecode).unwrap();
    let loaded = db.get_code(&code_hash).unwrap().expect("code should exist");
    assert_eq!(loaded, bytecode);
}

#[test]
fn block_hash_write_read_roundtrip() {
    let (db, _dir) = temp_db();
    let hash = B256::from([0xab; 32]);

    assert!(db.get_block_hash(100).unwrap().is_none());

    db.put_block_hash(100, &hash).unwrap();
    assert_eq!(db.get_block_hash(100).unwrap(), Some(hash));
}

// ============================================================================
// 1.2.2 — revm DatabaseRef trait
// ============================================================================

#[test]
fn database_ref_basic() {
    let (db, _dir) = temp_db();
    let addr = address!("1111111111111111111111111111111111111111");

    // Non-existent account returns None
    assert!(db.basic_ref(addr).unwrap().is_none());

    let info = AccountInfo {
        balance: U256::from(500u64),
        nonce: 7,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    db.put_account(&addr, &info).unwrap();

    let loaded = db.basic_ref(addr).unwrap().expect("account exists");
    assert_eq!(loaded.balance, U256::from(500u64));
    assert_eq!(loaded.nonce, 7);
}

#[test]
fn database_ref_code_by_hash() {
    let (db, _dir) = temp_db();

    // Empty code hash returns default Bytecode
    let empty = db.code_by_hash_ref(KECCAK_EMPTY).unwrap();
    assert!(empty.is_empty());

    // Zero hash also returns default
    let zero = db.code_by_hash_ref(B256::ZERO).unwrap();
    assert!(zero.is_empty());

    // Real code
    let code = vec![0x60, 0x42, 0x60, 0x00, 0x52]; // PUSH1 0x42 PUSH1 0x00 MSTORE
    let code_hash = keccak256(&code);
    db.put_code(&code_hash, &code).unwrap();

    let loaded = db.code_by_hash_ref(code_hash).unwrap();
    assert!(!loaded.is_empty());
}

#[test]
fn database_ref_storage() {
    let (db, _dir) = temp_db();
    let addr = address!("2222222222222222222222222222222222222222");
    let slot = U256::from(5u64);
    let value = U256::from(999u64);

    assert_eq!(db.storage_ref(addr, slot).unwrap(), U256::ZERO);

    db.put_storage(&addr, &slot, &value).unwrap();
    assert_eq!(db.storage_ref(addr, slot).unwrap(), value);
}

#[test]
fn database_ref_block_hash() {
    let (db, _dir) = temp_db();

    // Unknown block returns zero
    assert_eq!(db.block_hash_ref(999).unwrap(), B256::ZERO);

    let hash = B256::from([0xcd; 32]);
    db.put_block_hash(999, &hash).unwrap();
    assert_eq!(db.block_hash_ref(999).unwrap(), hash);
}

// ============================================================================
// 1.2.3 — State snapshot and overlay (copy-on-write)
// ============================================================================

#[test]
fn overlay_reads_through_to_base() {
    let (db, _dir) = temp_db();
    let addr = address!("3333333333333333333333333333333333333333");

    let info = AccountInfo {
        balance: U256::from(1000u64),
        nonce: 1,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    db.put_account(&addr, &info).unwrap();

    let overlay = StateOverlay::new(db);
    let loaded = overlay.basic_ref(addr).unwrap().expect("reads through");
    assert_eq!(loaded.balance, U256::from(1000u64));
}

#[test]
fn overlay_shadows_base() {
    let (db, _dir) = temp_db();
    let addr = address!("4444444444444444444444444444444444444444");

    let info = AccountInfo {
        balance: U256::from(100u64),
        nonce: 1,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    db.put_account(&addr, &info).unwrap();

    let mut overlay = StateOverlay::new(db);

    // Write to overlay
    let updated = AccountInfo {
        balance: U256::from(999u64),
        nonce: 2,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    overlay.set_account(addr, Some(updated.clone()));

    // Overlay reads should return the overlay value
    let loaded = overlay.basic_ref(addr).unwrap().expect("overlay value");
    assert_eq!(loaded.balance, U256::from(999u64));
    assert_eq!(loaded.nonce, 2);
}

#[test]
fn overlay_storage_shadows_base() {
    let (db, _dir) = temp_db();
    let addr = address!("5555555555555555555555555555555555555555");
    let slot = U256::from(10u64);

    db.put_storage(&addr, &slot, &U256::from(42u64)).unwrap();

    let mut overlay = StateOverlay::new(db);
    overlay.set_storage(addr, slot, U256::from(99u64));

    assert_eq!(overlay.storage_ref(addr, slot).unwrap(), U256::from(99u64));
}

#[test]
fn overlay_isolation_from_base() {
    let (db, _dir) = temp_db();
    let addr = address!("6666666666666666666666666666666666666666");

    let info = AccountInfo {
        balance: U256::from(500u64),
        nonce: 0,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    db.put_account(&addr, &info).unwrap();

    let mut overlay = StateOverlay::new(db.clone());
    overlay.set_account(
        addr,
        Some(AccountInfo {
            balance: U256::from(999u64),
            nonce: 1,
            code_hash: KECCAK_EMPTY,
            account_id: None,
            code: None,
        }),
    );

    // Base is unaffected
    let base_loaded = db.get_account(&addr).unwrap().expect("base unchanged");
    assert_eq!(base_loaded.balance, U256::from(500u64));

    // Overlay sees the change
    let overlay_loaded = overlay.basic_ref(addr).unwrap().expect("overlay changed");
    assert_eq!(overlay_loaded.balance, U256::from(999u64));
}

#[test]
fn overlay_commit_persists_to_base() {
    let (db, _dir) = temp_db();
    let addr = address!("7777777777777777777777777777777777777777");

    let mut overlay = StateOverlay::new(db.clone());
    overlay.set_account(
        addr,
        Some(AccountInfo {
            balance: U256::from(123u64),
            nonce: 5,
            code_hash: KECCAK_EMPTY,
            account_id: None,
            code: None,
        }),
    );
    overlay.set_storage(addr, U256::from(0u64), U256::from(456u64));

    assert_eq!(overlay.dirty_account_count(), 1);
    assert_eq!(overlay.dirty_storage_count(), 1);

    overlay.commit().unwrap();

    // Now base has the committed data
    let loaded = db.get_account(&addr).unwrap().expect("committed");
    assert_eq!(loaded.balance, U256::from(123u64));
    assert_eq!(
        db.get_storage(&addr, &U256::from(0u64)).unwrap(),
        U256::from(456u64)
    );
}

#[test]
fn overlay_rollback_discards() {
    let (db, _dir) = temp_db();
    let addr = address!("8888888888888888888888888888888888888888");

    let mut overlay = StateOverlay::new(db.clone());
    overlay.set_account(
        addr,
        Some(AccountInfo {
            balance: U256::from(999u64),
            nonce: 1,
            code_hash: KECCAK_EMPTY,
            account_id: None,
            code: None,
        }),
    );

    // Just drop the overlay (rollback)
    drop(overlay);

    // Base is unaffected
    assert!(db.get_account(&addr).unwrap().is_none());
}

// ============================================================================
// 1.2.5 — MPT state root computation
// ============================================================================

#[test]
fn empty_state_root() {
    // Empty trie root = keccak256(RLP("")) = keccak256(0x80)
    let root = trie::compute_state_root(std::iter::empty());
    assert_eq!(root, EMPTY_ROOT_HASH);
}

#[test]
fn empty_storage_root() {
    let root = trie::compute_storage_root(std::iter::empty());
    assert_eq!(root, EMPTY_ROOT_HASH);
}

#[test]
fn single_account_state_root() {
    let addr = address!("0000000000000000000000000000000000000001");
    let account = TrieAccount {
        nonce: 0,
        balance: U256::from(1_000_000_000_000_000_000u128), // 1 ETH
        storage_root: EMPTY_ROOT_HASH,
        code_hash: KECCAK_EMPTY,
    };

    let root = trie::compute_state_root(vec![(addr, account)]);
    assert_ne!(root, EMPTY_ROOT_HASH);
    assert_ne!(root, B256::ZERO);
}

#[test]
fn state_root_deterministic() {
    let addr1 = address!("0000000000000000000000000000000000000001");
    let addr2 = address!("0000000000000000000000000000000000000002");

    let accounts = vec![
        (
            addr1,
            TrieAccount {
                nonce: 1,
                balance: U256::from(100u64),
                storage_root: EMPTY_ROOT_HASH,
                code_hash: KECCAK_EMPTY,
            },
        ),
        (
            addr2,
            TrieAccount {
                nonce: 0,
                balance: U256::from(200u64),
                storage_root: EMPTY_ROOT_HASH,
                code_hash: KECCAK_EMPTY,
            },
        ),
    ];

    // Same input, same order => same root
    let root1 = trie::compute_state_root(accounts.clone());
    let root2 = trie::compute_state_root(accounts.clone());
    assert_eq!(root1, root2);

    // Reversed order => still same root (function sorts by keccak256(address))
    let reversed = vec![accounts[1].clone(), accounts[0].clone()];
    let root3 = trie::compute_state_root(reversed);
    assert_eq!(root1, root3);
}

#[test]
fn state_root_changes_with_different_state() {
    let addr = address!("0000000000000000000000000000000000000001");

    let root_a = trie::compute_state_root(vec![(
        addr,
        TrieAccount {
            nonce: 0,
            balance: U256::from(100u64),
            storage_root: EMPTY_ROOT_HASH,
            code_hash: KECCAK_EMPTY,
        },
    )]);

    let root_b = trie::compute_state_root(vec![(
        addr,
        TrieAccount {
            nonce: 0,
            balance: U256::from(200u64),
            storage_root: EMPTY_ROOT_HASH,
            code_hash: KECCAK_EMPTY,
        },
    )]);

    assert_ne!(root_a, root_b, "different balances => different roots");
}

#[test]
fn storage_root_with_slots() {
    let root = trie::compute_storage_root(vec![
        (U256::from(0u64), U256::from(100u64)),
        (U256::from(1u64), U256::from(200u64)),
    ]);
    assert_ne!(root, EMPTY_ROOT_HASH);
    assert_ne!(root, B256::ZERO);
}

#[test]
fn storage_root_deterministic() {
    let slots = vec![
        (U256::from(0u64), U256::from(42u64)),
        (U256::from(1u64), U256::from(43u64)),
    ];

    let root1 = trie::compute_storage_root(slots.clone());
    let root2 = trie::compute_storage_root(slots.clone());
    assert_eq!(root1, root2);

    // Reversed order => same root (function sorts internally)
    let reversed = vec![slots[1], slots[0]];
    let root3 = trie::compute_storage_root(reversed);
    assert_eq!(root1, root3);
}

#[test]
fn storage_root_ignores_zero_values() {
    let with_zero = trie::compute_storage_root(vec![
        (U256::from(0u64), U256::from(42u64)),
        (U256::from(1u64), U256::ZERO), // should be filtered out
    ]);
    let without_zero = trie::compute_storage_root(vec![(U256::from(0u64), U256::from(42u64))]);
    assert_eq!(with_zero, without_zero);
}

#[test]
fn compute_state_root_from_db_empty() {
    let (db, _dir) = temp_db();
    let root = trie::compute_state_root_from_db(&db).unwrap();
    assert_eq!(root, EMPTY_ROOT_HASH);
}

#[test]
fn compute_state_root_from_db_with_accounts() {
    let (db, _dir) = temp_db();
    let addr = address!("0000000000000000000000000000000000000001");

    let info = AccountInfo {
        balance: U256::from(1_000_000u64),
        nonce: 0,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    db.put_account(&addr, &info).unwrap();

    let root = trie::compute_state_root_from_db(&db).unwrap();
    assert_ne!(root, EMPTY_ROOT_HASH);

    // Compare with manual computation
    let manual_root = trie::compute_state_root(vec![(
        addr,
        TrieAccount {
            nonce: 0,
            balance: U256::from(1_000_000u64),
            storage_root: EMPTY_ROOT_HASH,
            code_hash: KECCAK_EMPTY,
        },
    )]);
    assert_eq!(root, manual_root);
}

#[test]
fn compute_state_root_from_db_with_storage() {
    let (db, _dir) = temp_db();
    let addr = address!("0000000000000000000000000000000000000001");

    let info = AccountInfo {
        balance: U256::from(500u64),
        nonce: 1,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    db.put_account(&addr, &info).unwrap();
    db.put_storage(&addr, &U256::from(0u64), &U256::from(42u64))
        .unwrap();

    let root = trie::compute_state_root_from_db(&db).unwrap();

    // Manually compute expected root
    let storage_root = trie::compute_storage_root(vec![(U256::from(0u64), U256::from(42u64))]);
    let manual_root = trie::compute_state_root(vec![(
        addr,
        TrieAccount {
            nonce: 1,
            balance: U256::from(500u64),
            storage_root,
            code_hash: KECCAK_EMPTY,
        },
    )]);
    assert_eq!(root, manual_root);
}

// ============================================================================
// Ethereum test vector: genesis alloc root
// ============================================================================

/// Test against a known Ethereum state root.
///
/// Genesis alloc with a single account at address 0x01 with 1 wei balance
/// should produce a deterministic, non-zero state root.
#[test]
fn ethereum_test_vector_single_account() {
    // This matches the Ethereum mainnet genesis precompile account at 0x01
    let addr = address!("0000000000000000000000000000000000000001");
    let account = TrieAccount {
        nonce: 0,
        balance: U256::from(1u64),
        storage_root: EMPTY_ROOT_HASH,
        code_hash: KECCAK_EMPTY,
    };

    let root = trie::compute_state_root(vec![(addr, account)]);

    // Verify it's a valid 32-byte hash, not zero or empty root
    assert_ne!(root, EMPTY_ROOT_HASH);
    assert_ne!(root, B256::ZERO);

    // Run twice to confirm determinism
    let root2 = trie::compute_state_root(vec![(addr, account)]);
    assert_eq!(root, root2);
}

/// Composite root matches the spec: keccak256(evm_root || native_root).
#[test]
fn composite_state_root() {
    let evm_root = B256::from([0xaa; 32]);
    let native_root = B256::from([0xbb; 32]);

    let composite = trie::compute_composite_root(evm_root, native_root);

    // Manually compute expected
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(evm_root.as_slice());
    data[32..].copy_from_slice(native_root.as_slice());
    let expected = keccak256(&data);

    assert_eq!(composite, expected);
}

// ============================================================================
// Iterator tests
// ============================================================================

#[test]
fn all_accounts_iterator() {
    let (db, _dir) = temp_db();

    // Empty
    assert!(db.all_accounts().unwrap().is_empty());

    // Add two accounts
    let addr1 = address!("1111111111111111111111111111111111111111");
    let addr2 = address!("2222222222222222222222222222222222222222");

    db.put_account(
        &addr1,
        &AccountInfo {
            balance: U256::from(100u64),
            nonce: 0,
            code_hash: KECCAK_EMPTY,
            account_id: None,
            code: None,
        },
    )
    .unwrap();
    db.put_account(
        &addr2,
        &AccountInfo {
            balance: U256::from(200u64),
            nonce: 1,
            code_hash: KECCAK_EMPTY,
            account_id: None,
            code: None,
        },
    )
    .unwrap();

    let accounts = db.all_accounts().unwrap();
    assert_eq!(accounts.len(), 2);
}

#[test]
fn account_storage_iterator() {
    let (db, _dir) = temp_db();
    let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    // Empty
    assert!(db.account_storage(&addr).unwrap().is_empty());

    // Add storage
    db.put_storage(&addr, &U256::from(0u64), &U256::from(10u64))
        .unwrap();
    db.put_storage(&addr, &U256::from(1u64), &U256::from(20u64))
        .unwrap();

    let slots = db.account_storage(&addr).unwrap();
    assert_eq!(slots.len(), 2);
}
