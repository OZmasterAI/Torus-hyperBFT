//! Item 7 option 0, task 1.6 (`docs/plans/item7-option0-impl.md`): the
//! raw-DB tripwire. While a thread holds an [`Armed`] guard ([`arm`]), every
//! read entry point of [`crate::StateDb`] panics on that thread: `get_cf_raw`,
//! `multi_get_cf_raw`, `get_cf_len`, the iterators, `prefix_exists`, the revm
//! `DatabaseRef` (through `get_account`, `get_storage`, `get_code`,
//! `get_block_hash`), `inner()` and `cf_handle`. The overlay's own
//! fall-through to the DB (the bottom of its layered read: own pending -> R ->
//! parent -> L -> C -> DB) holds an exemption token around the call, so a read
//! through the block overlay never trips it; a read that bypasses the overlay
//! does. Step 3 arms it around the fast-path EVM section.
//!
//! Compiled only with `debug_assertions` or the `test-probes` feature; a
//! release build gets no-op stand-ins (no thread-local, no check).

#[cfg(any(debug_assertions, feature = "test-probes"))]
mod imp {
    use std::cell::Cell;

    thread_local! {
        static ARMED: Cell<u32> = const { Cell::new(0) };
        static EXEMPT: Cell<u32> = const { Cell::new(0) };
    }

    /// Disarms on drop (nests).
    pub struct Armed(());

    impl Drop for Armed {
        fn drop(&mut self) {
            ARMED.with(|a| a.set(a.get() - 1));
        }
    }

    /// Arm the tripwire on this thread until the guard drops.
    pub fn arm() -> Armed {
        ARMED.with(|a| a.set(a.get() + 1));
        Armed(())
    }

    /// Whether the tripwire is armed on this thread.
    pub fn armed() -> bool {
        ARMED.with(|a| a.get() > 0)
    }

    /// Ends the exemption on drop (nests).
    pub struct Exempt(());

    impl Drop for Exempt {
        fn drop(&mut self) {
            EXEMPT.with(|e| e.set(e.get() - 1));
        }
    }

    /// A read through the overlay (its DB fall-through) or a test probe's
    /// re-read: the raw entry points below it do not trip.
    pub fn exempt() -> Exempt {
        EXEMPT.with(|e| e.set(e.get() + 1));
        Exempt(())
    }

    /// Called by every `StateDb` read entry point.
    #[inline]
    pub fn check(entry: &'static str) {
        if ARMED.with(|a| a.get()) > 0 && EXEMPT.with(|e| e.get()) == 0 {
            panic!(
                "raw-DB tripwire: StateDb::{entry} while armed (a reader bypassed the block \
                 overlay; read through it instead)"
            );
        }
    }
}

#[cfg(not(any(debug_assertions, feature = "test-probes")))]
mod imp {
    /// Release stand-in: no state, no check.
    pub struct Armed(());

    #[inline(always)]
    pub fn arm() -> Armed {
        Armed(())
    }

    #[inline(always)]
    pub fn armed() -> bool {
        false
    }

    /// Release stand-in.
    pub struct Exempt(());

    #[inline(always)]
    pub fn exempt() -> Exempt {
        Exempt(())
    }

    #[inline(always)]
    pub fn check(_entry: &'static str) {}
}

pub use imp::{arm, armed, Armed};
pub(crate) use imp::exempt;
pub(crate) use imp::check;

#[cfg(all(test, any(debug_assertions, feature = "test-probes")))]
mod tests {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use alloy_primitives::{Address, B256, U256};
    use revm::DatabaseRef;

    use super::*;
    use crate::cf::{CF_ACCOUNTS, CF_BLOCK_HEADERS, CF_CODE, CF_NATIVE_BALANCES, CF_STORAGE};
    use crate::{EvmBlockState, NativeStateOverlay, StateBackend, StateDb};

    fn db() -> (StateDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        (db, dir)
    }

    fn trips(f: impl FnOnce()) -> bool {
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(()) => false,
            Err(e) => {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                assert!(msg.contains("raw-DB tripwire"), "another panic: {msg}");
                true
            }
        }
    }

    /// Task 1.6: every raw read entry point of `StateDb` panics while the
    /// tripwire is armed on this thread, and none does before arming, after
    /// the guard drops, or on another thread.
    #[test]
    fn tripwire_fires_on_raw_read_in_armed_scope() {
        let (db, _d) = db();
        let code = B256::repeat_byte(7);
        let entries: Vec<(&str, Box<dyn Fn(&StateDb) + Sync>)> = vec![
            ("get_cf_raw", Box::new(|db| drop(db.get_cf_raw(CF_ACCOUNTS, b"k")))),
            ("StateBackend::get_cf_raw", Box::new(|db| drop(StateBackend::get_cf_raw(db, CF_ACCOUNTS, b"k")))),
            ("multi_get_cf_raw", Box::new(|db| drop(db.multi_get_cf_raw(CF_ACCOUNTS, &[b"k".as_slice()])))),
            ("get_cf_len", Box::new(|db| drop(StateBackend::get_cf_len(db, CF_ACCOUNTS, b"k")))),
            ("iterate_cf", Box::new(|db| drop(StateBackend::iterate_cf(db, CF_ACCOUNTS, None)))),
            ("iterate_cf_from", Box::new(|db| drop(StateBackend::iterate_cf_from(db, CF_ACCOUNTS, b"", 4)))),
            ("iterate_cf_prefix_from", Box::new(|db| drop(StateBackend::iterate_cf_prefix_from(db, CF_ACCOUNTS, b"k", b"k", 4)))),
            ("prefix_exists", Box::new(|db| drop(StateBackend::prefix_exists(db, CF_ACCOUNTS, b"k")))),
            ("basic_ref", Box::new(|db| drop(db.basic_ref(Address::ZERO)))),
            ("storage_ref", Box::new(|db| drop(db.storage_ref(Address::ZERO, U256::from(1))))),
            ("code_by_hash_ref", Box::new(move |db| drop(db.code_by_hash_ref(code)))),
            ("block_hash_ref", Box::new(|db| drop(db.block_hash_ref(1)))),
            ("get_account", Box::new(|db| drop(db.get_account(&Address::ZERO)))),
            ("get_block_hash", Box::new(|db| drop(db.get_block_hash(1)))),
            ("get_storage", Box::new(|db| drop(db.get_storage(&Address::ZERO, &U256::from(1))))),
            ("get_code", Box::new(move |db| drop(db.get_code(&code)))),
            ("exists_cf_raw", Box::new(|db| drop(db.exists_cf_raw(CF_ACCOUNTS, b"k")))),
            ("all_accounts", Box::new(|db| drop(db.all_accounts()))),
            ("account_storage", Box::new(|db| drop(db.account_storage(&Address::ZERO)))),
            ("inner", Box::new(|db| {
                let _ = db.inner();
            })),
            ("cf_handle", Box::new(|db| {
                let _ = db.cf_handle(CF_ACCOUNTS);
            })),
        ];
        for (name, read) in &entries {
            assert!(!trips(|| read(&db)), "{name}: unarmed");
            {
                let _armed = arm();
                assert!(armed());
                assert!(trips(|| read(&db)), "{name}: armed");
                // Armed on this thread only.
                std::thread::scope(|s| {
                    s.spawn(|| assert!(!trips(|| read(&db)), "{name}: other thread"));
                });
            }
            assert!(!armed());
            assert!(!trips(|| read(&db)), "{name}: after the guard");
        }
    }

    /// Task 1.6: reads through the block overlay (own pending -> parent -> L
    /// -> C -> DB), the revm adapter and the journal over its layers never
    /// trip while armed, and answer from the DB at the bottom; the exemption
    /// ends with the overlay call (a raw read right after still trips).
    #[test]
    fn tripwire_allows_overlay_bottom_reads() {
        let (db, _d) = db();
        let addr = Address::repeat_byte(0x11);
        let account = crate::db::encode_account_info(&revm::state::AccountInfo {
            balance: U256::from(5),
            nonce: 1,
            code_hash: B256::repeat_byte(9),
            code: None,
            account_id: None,
        });
        db.put_cf_raw(CF_ACCOUNTS, addr.as_slice(), &account).unwrap();
        db.put_cf_raw(CF_CODE, B256::repeat_byte(9).as_slice(), &[0x60, 0x00]).unwrap();
        db.put_cf_raw(
            CF_STORAGE,
            &crate::db::storage_key(&addr, &U256::from(3)),
            &U256::from(4).to_be_bytes::<32>(),
        )
        .unwrap();
        db.put_cf_raw(CF_BLOCK_HEADERS, &7u64.to_be_bytes(), B256::repeat_byte(2).as_slice())
            .unwrap();
        db.put_cf_raw(CF_NATIVE_BALANCES, b"bal", b"v").unwrap();
        let parent = {
            let ov = NativeStateOverlay::new(db.clone());
            ov.put_cf_raw(CF_NATIVE_BALANCES, b"parent", b"p").unwrap();
            ov.freeze(1)
        };
        let ov = NativeStateOverlay::with_layers(db.clone(), Some(parent), None, None);
        let _armed = arm();
        assert_eq!(ov.get_cf_raw(CF_NATIVE_BALANCES, b"bal").unwrap(), Some(b"v".to_vec()));
        assert_eq!(ov.get_cf_len(CF_NATIVE_BALANCES, b"bal").unwrap(), Some(1));
        assert_eq!(ov.iterate_cf(CF_NATIVE_BALANCES, None).unwrap().len(), 2);
        assert_eq!(ov.iterate_cf_from(CF_NATIVE_BALANCES, b"", 8).unwrap().len(), 2);
        assert_eq!(ov.iterate_cf_prefix_from(CF_NATIVE_BALANCES, b"b", b"b", 8).unwrap().len(), 1);
        assert!(ov.prefix_exists(CF_NATIVE_BALANCES, b"ba").unwrap());
        let state = EvmBlockState::new(&ov);
        assert_eq!(state.db.basic_ref(addr).unwrap().map(|a| a.nonce), Some(1));
        assert_eq!(state.db.storage_ref(addr, U256::from(3)).unwrap(), U256::from(4));
        assert_eq!(state.db.code_by_hash_ref(B256::repeat_byte(9)).unwrap().original_byte_slice(), &[0x60, 0x00]);
        assert_eq!(state.db.block_hash_ref(7).unwrap(), B256::repeat_byte(2));
        assert_eq!(state.journal.get_cf_raw(CF_NATIVE_BALANCES, b"parent").unwrap(), Some(b"p".to_vec()));
        assert_eq!(state.journal.get_cf_raw(CF_NATIVE_BALANCES, b"bal").unwrap(), Some(b"v".to_vec()));
        // Not a blanket exemption: the raw DB still trips right after.
        assert!(trips(|| drop(db.get_cf_raw(CF_NATIVE_BALANCES, b"bal"))));
    }
}
