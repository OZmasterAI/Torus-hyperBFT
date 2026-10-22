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
pub(crate) fn verify_signature(verifying_key: &VerifyingKey, message: &[u8], signature: &Signature) -> bool {
    SIGNATURE_CHECKS.with(|checks| checks.set(checks.get() + 1));
    let delay_us = SIGNATURE_CHECK_DELAY_US.load(std::sync::atomic::Ordering::Relaxed);
    if delay_us > 0 {
        std::thread::sleep(std::time::Duration::from_micros(delay_us));
    }
    verifying_key.verify(message, signature).is_ok()
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
