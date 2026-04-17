//! Encrypted keystore for secp256k1 private keys.
//!
//! Format: AES-256-GCM encrypted key, derived via Argon2id.
//! Mirrors the validator keystore in torus-node but for secp256k1 (EVM/native signing) keys.

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use alloy_primitives::Address;
use argon2::Argon2;
use k256::ecdsa::{SigningKey, VerifyingKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct KeystoreFile {
    pub version: u32,
    pub address: String,
    pub crypto: CryptoParams,
}

#[derive(Serialize, Deserialize)]
pub struct CryptoParams {
    pub cipher: String,
    pub ciphertext: String,
    pub nonce: String,
    pub kdf: String,
    pub kdf_salt: String,
}

pub fn address_from_key(key: &SigningKey) -> Address {
    let vk = VerifyingKey::from(key);
    let uncompressed = vk.to_encoded_point(false);
    let hash = alloy_primitives::keccak256(&uncompressed.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

pub fn generate_keystore(
    output_path: &Path,
    passphrase: &str,
) -> Result<(SigningKey, Address), Box<dyn std::error::Error>> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let key = SigningKey::from_slice(&seed).map_err(|e| format!("key gen: {e}"))?;
    let addr = address_from_key(&key);
    write_keystore(output_path, &key, passphrase)?;
    Ok((key, addr))
}

pub fn write_keystore(
    output_path: &Path,
    key: &SigningKey,
    passphrase: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut salt = [0u8; 32];
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce_bytes);

    let mut derived_key = [0u8; 32];
    let argon2 = Argon2::default();
    argon2
        .hash_password_into(passphrase.as_bytes(), &salt, &mut derived_key)
        .map_err(|e| format!("argon2: {e}"))?;

    let cipher = Aes256Gcm::new_from_slice(&derived_key)
        .map_err(|e| format!("cipher: {e}"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, key.to_bytes().as_ref())
        .map_err(|e| format!("encrypt: {e}"))?;

    let addr = address_from_key(key);
    let ks = KeystoreFile {
        version: 1,
        address: format!("0x{}", hex::encode(addr)),
        crypto: CryptoParams {
            cipher: "aes-256-gcm".to_string(),
            ciphertext: hex::encode(&ciphertext),
            nonce: hex::encode(nonce_bytes),
            kdf: "argon2id".to_string(),
            kdf_salt: hex::encode(salt),
        },
    };

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, serde_json::to_string_pretty(&ks)?)?;
    Ok(())
}

pub fn load_keystore(
    path: &Path,
    passphrase: &str,
) -> Result<SigningKey, Box<dyn std::error::Error>> {
    let json = std::fs::read_to_string(path)?;
    let ks: KeystoreFile = serde_json::from_str(&json)?;

    if ks.version != 1 {
        return Err(format!("unsupported keystore version: {}", ks.version).into());
    }

    let salt = hex::decode(&ks.crypto.kdf_salt)?;
    let nonce_bytes = hex::decode(&ks.crypto.nonce)?;
    let ciphertext = hex::decode(&ks.crypto.ciphertext)?;

    let mut derived_key = [0u8; 32];
    let argon2 = Argon2::default();
    argon2
        .hash_password_into(passphrase.as_bytes(), &salt, &mut derived_key)
        .map_err(|e| format!("argon2: {e}"))?;

    let cipher = Aes256Gcm::new_from_slice(&derived_key)?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| "decryption failed: wrong passphrase")?;

    let key = SigningKey::from_slice(&plaintext)
        .map_err(|e| format!("invalid key bytes: {e}"))?;

    // Verify address matches
    let addr = address_from_key(&key);
    let expected = format!("0x{}", hex::encode(addr));
    if expected != ks.address {
        return Err("decrypted key does not match stored address".into());
    }

    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::test_utils::test_signing_key;

    #[test]
    fn keygen_and_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.keystore");
        let (key, addr) = generate_keystore(&path, "testpass").unwrap();
        let loaded = load_keystore(&path, "testpass").unwrap();
        assert_eq!(key.to_bytes(), loaded.to_bytes());
        assert_eq!(address_from_key(&loaded), addr);
    }

    #[test]
    fn wrong_passphrase_fails() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test2.keystore");
        generate_keystore(&path, "correct").unwrap();
        assert!(load_keystore(&path, "wrong").is_err());
    }

    #[test]
    fn address_derivation() {
        let key = test_signing_key();
        let addr = address_from_key(&key);
        assert_ne!(addr, Address::ZERO);
        assert_eq!(addr.len(), 20);
    }
}
