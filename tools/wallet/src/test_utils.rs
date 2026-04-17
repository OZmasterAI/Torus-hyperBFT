//! Shared test helpers for the torus-wallet crate.

use k256::ecdsa::SigningKey;

/// Canonical test signing key: `[0x00; 31] || 0x01`.
///
/// Used across all wallet test modules for deterministic signing roundtrips.
/// Matches the pinned key in `torus-types/tests/eip712_vectors.rs`.
pub(crate) fn test_signing_key() -> SigningKey {
    let mut b = [0u8; 32];
    b[31] = 1;
    SigningKey::from_slice(&b).unwrap()
}
