/*
    Copyright © 2024, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Cryptographic primitives.
//!
//! The definitions and re-exports in this module provide two categories of cryptographic primitives:
//! 1. **Cryptographic Hashes**: provided by the [`sha2`] crate.
//! 2. **Digital Signatures**: provided by the [`ed25519_dalek`] crate.

use super::data_types::SignatureBytes;

// re-exports below.
pub use sha2::Digest;
pub use sha2::Sha256 as CryptoHasher;

pub use ed25519_dalek::{Signature, SignatureError, Signer, SigningKey, Verifier, VerifyingKey};

/// A facade around [`SigningKey`] that implements method for [`sign`](Self::sign)-ing messages as well
/// as a getter for the [`public`](Self::public) key associated with the signing key.
#[derive(Clone)]
pub(crate) struct Keypair(pub(crate) SigningKey);

impl Keypair {
    /// Create a `Keypair` that wraps over `signing_key`.
    pub(crate) fn new(signing_key: SigningKey) -> Keypair {
        Keypair(signing_key)
    }

    /// Sign an arbitrary `message` with the `Keypair`.
    pub(crate) fn sign(&self, message: &[u8]) -> SignatureBytes {
        SignatureBytes::new(self.0.sign(message).to_bytes())
    }

    /// Get the `VerifyingKey` of this `Keypair`.
    pub(crate) fn public(&self) -> VerifyingKey {
        self.0.verifying_key()
    }
}

/// `verifying_key.verify(message, signature).is_ok()`: the one place consensus checks an
/// ed25519 signature (PCs, TCs, NECs, phase / timeout / NE votes, leader-signed headers and tips).
///
/// tc-liveness (s136): a `(key, signature, message)` that passed on this thread before is not
/// checked again (see [`PassedChecks`]); the answer is the one a fresh check gives.
pub(crate) fn verify_signature(verifying_key: &VerifyingKey, message: &[u8], signature: &Signature) -> bool {
    let entry = PassedChecks::entry(verifying_key, message, signature);
    if PASSED_CHECKS.with(|passed| passed.borrow().contains(&entry)) {
        return true;
    }
    SIGNATURE_CHECKS.with(|checks| checks.set(checks.get() + 1));
    let delay_us = SIGNATURE_CHECK_DELAY_US.load(std::sync::atomic::Ordering::Relaxed);
    if delay_us > 0 {
        std::thread::sleep(std::time::Duration::from_micros(delay_us));
    }
    if verifying_key.verify(message, signature).is_err() {
        return false;
    }
    PASSED_CHECKS.with(|passed| passed.borrow_mut().insert(entry));
    true
}

/// The signature checks that passed on this thread, most recent [`PASSED_CHECKS_CAPACITY`].
///
/// A replica meets the same signatures again and again, each time a full ed25519 check: every
/// timeout vote carries its sender's highest TC (the TC's signatures, high QC and high tip), the
/// honest votes of a view report the same tip and QC, a leader re-sends its header (leader
/// signature, justify QC, TC) every few hundred milliseconds, every validator re-broadcasts each
/// AdvanceView, and block-sync servers re-advertise the same QC. TC-required phases B and C
/// roughly doubled the checks per header and per timeout vote. At ~50 us per check (release) this
/// is noise; at tens of milliseconds per check (a debug build on a loaded host, 36 ms measured)
/// the algorithm thread fell seconds behind its queue and the cluster stopped committing.
///
/// Only passes are kept, keyed by the exact bytes `key ‖ signature ‖ message` (the key and the
/// signature have fixed lengths, so the concatenation is unambiguous): a hit means exactly this
/// check passed before, and a failed or new check always runs. Per thread, because each replica
/// checks on its own algorithm thread; oldest entry evicted first.
struct PassedChecks {
    entries: std::collections::HashSet<std::rc::Rc<[u8]>>,
    order: std::collections::VecDeque<std::rc::Rc<[u8]>>,
    capacity: usize,
}

/// How many passed checks a thread keeps (a few hundred bytes each): about 30 views of distinct
/// signatures at 4 validators.
const PASSED_CHECKS_CAPACITY: usize = 1024;

impl PassedChecks {
    fn new(capacity: usize) -> Self {
        Self {
            entries: std::collections::HashSet::new(),
            order: std::collections::VecDeque::new(),
            capacity,
        }
    }

    fn entry(verifying_key: &VerifyingKey, message: &[u8], signature: &Signature) -> Vec<u8> {
        let mut entry = Vec::with_capacity(32 + 64 + message.len());
        entry.extend_from_slice(verifying_key.as_bytes());
        entry.extend_from_slice(&signature.to_bytes());
        entry.extend_from_slice(message);
        entry
    }

    fn contains(&self, entry: &[u8]) -> bool {
        self.entries.contains(entry)
    }

    fn insert(&mut self, entry: Vec<u8>) {
        let entry: std::rc::Rc<[u8]> = entry.into();
        if !self.entries.insert(entry.clone()) {
            return;
        }
        self.order.push_back(entry);
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

thread_local! {
    static PASSED_CHECKS: std::cell::RefCell<PassedChecks> =
        std::cell::RefCell::new(PassedChecks::new(PASSED_CHECKS_CAPACITY));
}

thread_local! {
    static SIGNATURE_CHECKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test hook: per-check delay (microseconds) [`verify_signature`] adds to every ed25519 check.
static SIGNATURE_CHECK_DELAY_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many ed25519 checks [`verify_signature`] has run on this thread. Test hook.
#[doc(hidden)]
pub fn signature_checks_on_this_thread() -> u64 {
    SIGNATURE_CHECKS.with(|checks| checks.get())
}

/// Test hook: make every ed25519 check [`verify_signature`] runs take `delay` longer, process
/// wide, as a debug build on a loaded host does (tens of milliseconds per check, tc-liveness s136).
/// Zero (the default) adds nothing.
#[doc(hidden)]
pub fn set_signature_check_delay_for_tests(delay: std::time::Duration) {
    SIGNATURE_CHECK_DELAY_US.store(delay.as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
}

/// tc-liveness (s136): a signature this thread has already seen pass is not checked again, and the
/// answer is always the one a fresh check gives.
#[cfg(test)]
mod signature_check_tests {
    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn checks() -> u64 {
        signature_checks_on_this_thread()
    }

    #[test]
    fn a_passed_check_is_not_run_again() {
        let signer = key(11);
        let message = b"tc-liveness: passed once".to_vec();
        let signature = signer.sign(&message);
        let before = checks();
        assert!(verify_signature(&signer.verifying_key(), &message, &signature));
        assert!(verify_signature(&signer.verifying_key(), &message, &signature));
        assert!(verify_signature(&signer.verifying_key(), &message, &signature));
        assert_eq!(checks() - before, 1, "the same (key, signature, message) is checked once");
    }

    #[test]
    fn a_failed_check_is_never_remembered() {
        let signer = key(12);
        let message = b"tc-liveness: fails".to_vec();
        let mut forged = signer.sign(&message).to_bytes();
        forged[0] ^= 1;
        let forged = Signature::from_bytes(&forged);
        let before = checks();
        assert!(!verify_signature(&signer.verifying_key(), &message, &forged));
        assert!(!verify_signature(&signer.verifying_key(), &message, &forged));
        assert_eq!(checks() - before, 2, "a failed check runs every time");
        assert!(verify_signature(&signer.verifying_key(), &message, &signer.sign(&message)));
    }

    #[test]
    fn the_oldest_passed_check_is_evicted_first() {
        let mut passed = PassedChecks::new(2);
        let (a, b, c) = (b"a".to_vec(), b"b".to_vec(), b"c".to_vec());
        passed.insert(a.clone());
        passed.insert(b.clone());
        passed.insert(a.clone());
        passed.insert(c.clone());
        assert!(!passed.contains(&a) && passed.contains(&b) && passed.contains(&c));
        assert_eq!(passed.order.len(), 2);
    }

    #[test]
    fn a_passed_check_does_not_vouch_for_another_key_or_message() {
        let signer = key(13);
        let message = b"tc-liveness: one message".to_vec();
        let signature = signer.sign(&message);
        assert!(verify_signature(&signer.verifying_key(), &message, &signature));
        assert!(!verify_signature(&signer.verifying_key(), b"tc-liveness: another message", &signature));
        assert!(!verify_signature(&key(14).verifying_key(), &message, &signature));
    }
}
