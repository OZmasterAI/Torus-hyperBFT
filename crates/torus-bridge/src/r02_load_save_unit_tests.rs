//! R02 branch 7: the applied-marker half of the resident staleness guards
//! (`begin_resident`'s rows slot and the constructor's books holder share
//! `resident_marker_ok`). A failed marker read means "stale" (rebuild),
//! never "no marker" (reuse). `begin_resident` takes a concrete
//! `NativeStateOverlay`, so the rule is pinned here on the shared helper;
//! the constructor's path is driven end to end in
//! `tests/r02_load_save_tests.rs`.

use super::*;
use torus_state::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
use torus_state::{AtomicWriteOp, StateError};

/// Point reads of the applied marker fail when `fail` is set.
#[derive(Clone)]
struct FailMarker {
    inner: StateDb,
    fail: bool,
}

impl StateBackend for FailMarker {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if self.fail && cf == CF_CONSENSUS_META && key == META_NATIVE_APPLIED_HEIGHT {
            return Err(StateError::Io(std::io::Error::other(
                "injected marker read failure",
            )));
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

fn backend(marker: Option<&[u8]>, fail: bool) -> (tempfile::TempDir, FailMarker) {
    let dir = tempfile::tempdir().unwrap();
    let inner = StateDb::open(dir.path()).unwrap();
    if let Some(m) = marker {
        inner
            .put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, m)
            .unwrap();
    }
    (dir, FailMarker { inner, fail })
}

/// A failed marker read is the `Err` and fails the guard (rebuild).
#[test]
fn r02_applied_marker_read_fault_is_stale() {
    let (_d, b) = backend(Some(&5u64.to_be_bytes()), true);
    let marker = applied_marker(&b);
    assert!(marker.is_err(), "{marker:?}");
    assert!(
        !resident_marker_ok(&marker, 5),
        "a fault never passes the guard"
    );
}

/// Unchanged: no row and a wrong-length row read as absent (the guard
/// passes on the height check alone); a row equal to the held height
/// passes, any other fails.
#[test]
fn r02_applied_marker_without_fault_unchanged() {
    let (_d, none) = backend(None, false);
    assert_eq!(applied_marker(&none).unwrap(), None);
    assert!(resident_marker_ok(&applied_marker(&none), 5));

    let (_d, short) = backend(Some(b"short"), false);
    assert_eq!(
        applied_marker(&short).unwrap(),
        None,
        "wrong length: absent"
    );
    assert!(resident_marker_ok(&applied_marker(&short), 5));

    let (_d, five) = backend(Some(&5u64.to_be_bytes()), false);
    assert_eq!(applied_marker(&five).unwrap(), Some(5));
    assert!(resident_marker_ok(&applied_marker(&five), 5));
    assert!(!resident_marker_ok(&applied_marker(&five), 4));
}
