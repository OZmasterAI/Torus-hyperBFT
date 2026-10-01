//! Signer key sources: the wallet keystore (Argon2id + AES-GCM) or a hex key
//! file that must be mode 0600 or stricter.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use alloy_primitives::Address;
use k256::ecdsa::SigningKey;

use crate::config::Config;

fn create_0600(path: &Path) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => format!("{} already exists; refusing to overwrite", path.display()),
            _ => format!("create {}: {e}", path.display()),
        })
}

/// Write `key` as hex to a NEW file with mode 0600.
pub fn write_key_file(path: &Path, key: &SigningKey) -> Result<(), String> {
    let mut f = create_0600(path)?;
    writeln!(f, "{}", hex::encode(key.to_bytes())).map_err(|e| e.to_string())
}

/// Load a hex key file (optional `0x`, surrounding whitespace ok). The file
/// must not be readable by group or others.
pub fn load_key_file(path: &Path) -> Result<SigningKey, String> {
    let mode = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "{}: mode {:o} is too open; the key file must be 0600 or stricter",
            path.display(),
            mode & 0o777
        ));
    }
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let s = s.trim();
    let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s)).map_err(|e| format!("{}: bad hex: {e}", path.display()))?;
    if bytes.len() != 32 {
        return Err(format!("{}: key must be 32 bytes, got {}", path.display(), bytes.len()));
    }
    SigningKey::from_slice(&bytes).map_err(|e| format!("{}: invalid key: {e}", path.display()))
}

/// Create a NEW signer keystore (wallet format, mode 0600) and return its address.
pub fn keygen(path: &Path, passphrase: &str) -> Result<Address, String> {
    if path.exists() {
        return Err(format!("{} already exists; refusing to overwrite", path.display()));
    }
    // The wallet writes with `fs::write`, which keeps an existing file's mode:
    // pre-create the temp file 0600 so the keystore is never group/world-readable,
    // then hard-link it into place (fails instead of overwriting a racing file).
    let tmp = path.with_extension("keygen-tmp");
    drop(create_0600(&tmp)?);
    let r = torus_wallet::keystore::generate_keystore(&tmp, passphrase)
        .map_err(|e| e.to_string())
        .and_then(|(_, addr)| {
            std::fs::hard_link(&tmp, path).map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => format!("{} already exists; refusing to overwrite", path.display()),
                _ => format!("{}: {e}", path.display()),
            })?;
            Ok(addr)
        });
    let _ = std::fs::remove_file(&tmp);
    r
}

pub fn read_passphrase(path: &Path) -> Result<String, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(s.trim_end().to_string())
}

/// The signer key named by the config (keystore + passphrase file, or key file).
pub fn load_signer(cfg: &Config) -> Result<SigningKey, String> {
    match (&cfg.signer_keystore, &cfg.signer_key_file) {
        (Some(ks), None) => {
            let pf = cfg.passphrase_file.as_ref().ok_or("signer_keystore needs passphrase_file")?;
            torus_wallet::keystore::load_keystore(ks, &read_passphrase(pf)?)
                .map_err(|e| format!("{}: {e}", ks.display()))
        }
        (None, Some(kf)) => load_key_file(kf),
        _ => Err("set exactly one of signer_keystore or signer_key_file".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn key() -> SigningKey {
        SigningKey::from_slice(&[7u8; 32]).unwrap()
    }

    #[test]
    fn hex_key_file_roundtrip_mode_0600() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("signer.key");
        write_key_file(&p, &key()).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load_key_file(&p).unwrap().to_bytes(), key().to_bytes());
    }

    #[test]
    fn load_refuses_group_or_world_readable() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("signer.key");
        write_key_file(&p, &key()).unwrap();
        for mode in [0o644, 0o640, 0o604] {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            let e = load_key_file(&p).unwrap_err();
            assert!(e.contains("0600"), "{mode:o}: {e}");
        }
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert!(load_key_file(&p).is_ok(), "0400 is stricter than 0600");
    }

    #[test]
    fn load_rejects_bad_hex_or_length() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("k");
        for body in ["zz", "0x1234", "", &"00".repeat(32), &"ab".repeat(33)] {
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(load_key_file(&p).is_err(), "{body:?}");
        }
        std::fs::write(&p, format!("0x{}\n", hex::encode(key().to_bytes()))).unwrap();
        assert_eq!(load_key_file(&p).unwrap().to_bytes(), key().to_bytes(), "0x prefix + newline ok");
    }

    #[test]
    fn keygen_refuses_to_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("signer.keystore");
        let addr = keygen(&p, "pw").unwrap();
        let loaded = torus_wallet::keystore::load_keystore(&p, "pw").unwrap();
        assert_eq!(torus_wallet::keystore::address_from_key(&loaded), addr);
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let before = std::fs::read(&p).unwrap();
        let e = keygen(&p, "pw").unwrap_err();
        assert!(e.contains("exists"), "{e}");
        assert_eq!(std::fs::read(&p).unwrap(), before);
        let kf = d.path().join("k");
        write_key_file(&kf, &key()).unwrap();
        assert!(write_key_file(&kf, &key()).unwrap_err().contains("exists"));
    }

    #[test]
    fn load_signer_from_config() {
        let d = tempfile::tempdir().unwrap();
        let ks = d.path().join("s.keystore");
        let pw = d.path().join("pw");
        std::fs::write(&pw, "secret\n").unwrap();
        let addr = keygen(&ks, "secret").unwrap();
        let toml = format!(
            "rpc_url = \"x\"\nsigner_keystore = \"{}\"\npassphrase_file = \"{}\"\nvalidator_address = \"0x1111111111111111111111111111111111111111\"\n[[markets]]\nmarket_id = 1\nbase_asset = \"B\"\nsymbols = {{ binance = \"a\", okx = \"b\", bybit = \"c\" }}\n",
            ks.display(),
            pw.display()
        );
        let cfg = crate::config::Config::parse(&toml).unwrap();
        let k = load_signer(&cfg).unwrap();
        assert_eq!(torus_wallet::keystore::address_from_key(&k), addr);
    }
}
