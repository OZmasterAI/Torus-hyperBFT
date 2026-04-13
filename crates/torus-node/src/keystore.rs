//! Encrypted keystore for validator ed25519 keys (Phase 3: 3.1.8).
//!
//! Format: JSON file with AES-256-GCM encrypted private key, derived via Argon2id.

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::Argon2;
use ed25519_dalek::SigningKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// JSON keystore format.
#[derive(Serialize, Deserialize)]
pub struct KeystoreFile {
    pub version: u32,
    pub crypto: CryptoParams,
    pub pubkey_hex: String,
}

#[derive(Serialize, Deserialize)]
pub struct CryptoParams {
    pub cipher: String,
    pub ciphertext: String,
    pub nonce: String,
    pub kdf: String,
    pub kdf_salt: String,
    pub kdf_ops_limit: u32,
    pub kdf_mem_limit: u32,
    pub kdf_parallelism: u32,
}

/// Generate a new keypair and write an encrypted keystore file.
pub fn generate_keystore(
    output_path: &Path,
    passphrase: &str,
) -> Result<SigningKey, Box<dyn std::error::Error>> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let signing_key = SigningKey::from_bytes(&seed);

    write_keystore(output_path, &signing_key, passphrase)?;
    Ok(signing_key)
}

/// Encrypt and write a signing key to a keystore file.
pub fn write_keystore(
    output_path: &Path,
    key: &SigningKey,
    passphrase: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Generate salt and nonce
    let mut salt = [0u8; 32];
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce_bytes);

    // Derive encryption key via Argon2id
    let mut derived_key = [0u8; 32];
    let argon2 = Argon2::default();
    argon2
        .hash_password_into(passphrase.as_bytes(), &salt, &mut derived_key)
        .map_err(|e| format!("argon2 key derivation failed: {e}"))?;

    // Encrypt the private key with AES-256-GCM
    let cipher = Aes256Gcm::new_from_slice(&derived_key)
        .map_err(|e| format!("cipher init failed: {e}"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, key.as_bytes().as_ref())
        .map_err(|e| format!("encryption failed: {e}"))?;

    let verifying_key = key.verifying_key();

    let keystore = KeystoreFile {
        version: 1,
        crypto: CryptoParams {
            cipher: "aes-256-gcm".to_string(),
            ciphertext: hex::encode(&ciphertext),
            nonce: hex::encode(nonce_bytes),
            kdf: "argon2id".to_string(),
            kdf_salt: hex::encode(salt),
            kdf_ops_limit: 3,
            kdf_mem_limit: 65536,
            kdf_parallelism: 1,
        },
        pubkey_hex: hex::encode(verifying_key.as_bytes()),
    };

    let json = serde_json::to_string_pretty(&keystore)?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, json)?;

    Ok(())
}

/// Load and decrypt a signing key from a keystore file.
pub fn load_keystore(
    path: &Path,
    passphrase: &str,
) -> Result<SigningKey, Box<dyn std::error::Error>> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read keystore file {}: {e}", path.display()))?;
    let keystore: KeystoreFile = serde_json::from_str(&json)
        .map_err(|e| format!("invalid keystore JSON: {e}"))?;

    if keystore.version != 1 {
        return Err(format!("unsupported keystore version: {}", keystore.version).into());
    }
    if keystore.crypto.cipher != "aes-256-gcm" {
        return Err(format!("unsupported cipher: {}", keystore.crypto.cipher).into());
    }
    if keystore.crypto.kdf != "argon2id" {
        return Err(format!("unsupported KDF: {}", keystore.crypto.kdf).into());
    }

    let salt = hex::decode(&keystore.crypto.kdf_salt)
        .map_err(|e| format!("invalid salt hex: {e}"))?;
    let nonce_bytes = hex::decode(&keystore.crypto.nonce)
        .map_err(|e| format!("invalid nonce hex: {e}"))?;
    let ciphertext = hex::decode(&keystore.crypto.ciphertext)
        .map_err(|e| format!("invalid ciphertext hex: {e}"))?;

    // Derive key via Argon2id
    let mut derived_key = [0u8; 32];
    let argon2 = Argon2::default();
    argon2
        .hash_password_into(passphrase.as_bytes(), &salt, &mut derived_key)
        .map_err(|e| format!("argon2 key derivation failed: {e}"))?;

    // Decrypt
    let cipher = Aes256Gcm::new_from_slice(&derived_key)
        .map_err(|e| format!("cipher init failed: {e}"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| "decryption failed: wrong passphrase or corrupted keystore")?;

    if plaintext.len() != 32 {
        return Err(format!("decrypted key length {} != 32", plaintext.len()).into());
    }

    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&plaintext);
    let signing_key = SigningKey::from_bytes(&key_bytes);

    // Verify the public key matches
    let expected_pubkey = hex::encode(signing_key.verifying_key().as_bytes());
    if expected_pubkey != keystore.pubkey_hex {
        return Err("decrypted key does not match stored public key".into());
    }

    Ok(signing_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn keygen_write_and_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.keystore");
        let passphrase = "test-passphrase-123";

        let key = generate_keystore(&path, passphrase).unwrap();
        let loaded = load_keystore(&path, passphrase).unwrap();

        assert_eq!(key.as_bytes(), loaded.as_bytes());
        assert_eq!(key.verifying_key(), loaded.verifying_key());
    }

    #[test]
    fn wrong_passphrase_fails() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test2.keystore");

        generate_keystore(&path, "correct").unwrap();
        let result = load_keystore(&path, "wrong");
        assert!(result.is_err());
    }

    #[test]
    fn keystore_file_is_valid_json() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test3.keystore");
        generate_keystore(&path, "pass").unwrap();

        let json = std::fs::read_to_string(&path).unwrap();
        let parsed: KeystoreFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.crypto.cipher, "aes-256-gcm");
        assert_eq!(parsed.crypto.kdf, "argon2id");
        assert!(!parsed.pubkey_hex.is_empty());
    }

    #[test]
    fn write_existing_key_to_keystore() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test4.keystore");

        // Create a known key
        let key = SigningKey::from_bytes(&[42u8; 32]);
        write_keystore(&path, &key, "mypass").unwrap();

        let loaded = load_keystore(&path, "mypass").unwrap();
        assert_eq!(key.as_bytes(), loaded.as_bytes());
    }
}
