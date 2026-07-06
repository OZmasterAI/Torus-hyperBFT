//! Key management commands: keygen, import, address.

use std::path::PathBuf;

use k256::ecdsa::SigningKey;

use crate::keystore::{self, address_from_key, generate_keystore, load_keystore};
use crate::sign::prompt_passphrase;

pub(crate) async fn cmd_keygen(output: PathBuf) -> Result<(), String> {
    let pass = prompt_passphrase("Enter passphrase for new keystore: ");
    let pass2 = prompt_passphrase("Confirm passphrase: ");
    if pass != pass2 {
        return Err("passphrases do not match".into());
    }

    let (key, addr) =
        generate_keystore(&output, &pass).map_err(|e| format!("keygen failed: {e}"))?;

    let private_key_hex = hex::encode(key.to_bytes());
    println!("Address:     0x{}", hex::encode(addr));
    println!("Private key: 0x{private_key_hex}");
    println!();
    println!("Keystore saved to: {}", output.display());
    println!("IMPORTANT: The private key above will NEVER be shown again.");
    println!("           Store it securely or keep the keystore file safe.");

    Ok(())
}

pub(crate) async fn cmd_import(key_hex: String, output: PathBuf) -> Result<(), String> {
    let hex_str = key_hex.strip_prefix("0x").unwrap_or(&key_hex);
    let bytes = hex::decode(hex_str).map_err(|e| format!("invalid hex: {e}"))?;
    let key = SigningKey::from_slice(&bytes).map_err(|e| format!("invalid key: {e}"))?;

    let pass = prompt_passphrase("Enter passphrase for keystore: ");
    let pass2 = prompt_passphrase("Confirm passphrase: ");
    if pass != pass2 {
        return Err("passphrases do not match".into());
    }

    keystore::write_keystore(&output, &key, &pass).map_err(|e| format!("write failed: {e}"))?;

    let addr = address_from_key(&key);
    println!("Imported address: 0x{}", hex::encode(addr));
    println!("Keystore saved to: {}", output.display());
    Ok(())
}

pub(crate) async fn cmd_address(keystore_path: PathBuf) -> Result<(), String> {
    let pass = prompt_passphrase("Enter keystore passphrase: ");
    let key = load_keystore(&keystore_path, &pass).map_err(|e| format!("{e}"))?;
    let addr = address_from_key(&key);
    println!("0x{}", hex::encode(addr));
    Ok(())
}
