//! Validator operations: register-validator, update-commission, jail-vote, unjail, rotate-key,
//! set-oracle-signer.

use std::path::Path;

use alloy_primitives::Address;
use k256::ecdsa::SigningKey;
use torus_types::NativeAction;

use crate::parse::{parse_address, parse_pubkey};
use crate::rpc::RpcClient;
use crate::keystore::{address_from_key, load_keystore};
use crate::sign::{load_signing_key, now_ms, prompt_passphrase, submit_native_action, submit_native_action_with_key};
use crate::Cli;

pub(crate) async fn cmd_register_validator(
    cli: &Cli,
    rpc: &RpcClient,
    pubkey: &str,
    commission_bps: u16,
) -> Result<(), String> {
    let pk = parse_pubkey(pubkey)?;
    submit_native_action(
        cli,
        rpc,
        NativeAction::RegisterValidator {
            pubkey: pk,
            commission: commission_bps,
        },
    )
    .await
}

pub(crate) async fn cmd_update_commission(
    cli: &Cli,
    rpc: &RpcClient,
    commission_bps: u16,
) -> Result<(), String> {
    submit_native_action(
        cli,
        rpc,
        NativeAction::UpdateCommission {
            new_rate: commission_bps,
        },
    )
    .await
}

pub(crate) async fn cmd_jail_vote(
    cli: &Cli,
    rpc: &RpcClient,
    validator: &str,
) -> Result<(), String> {
    let target = parse_address(validator)?;
    submit_native_action(cli, rpc, NativeAction::JailVote { target }).await
}

pub(crate) async fn cmd_unjail(cli: &Cli, rpc: &RpcClient) -> Result<(), String> {
    submit_native_action(cli, rpc, NativeAction::UnjailSelf).await
}

pub(crate) async fn cmd_rotate_key(
    cli: &Cli,
    rpc: &RpcClient,
    new_pubkey: &str,
) -> Result<(), String> {
    let pk = parse_pubkey(new_pubkey)?;
    submit_native_action(
        cli,
        rpc,
        NativeAction::RotateValidatorKey { new_pubkey: pk },
    )
    .await
}

/// `SetOracleSigner` for `validator`: with the signer key, set or rotate it
/// with the key's proof of possession over (validator, chain id, `nonce`)
/// (review M3); without, clear it (`Address::ZERO`, no proof needed).
pub(crate) fn build_set_oracle_signer(validator: &Address, signer_key: Option<&SigningKey>, nonce: u64) -> NativeAction {
    match signer_key {
        None => NativeAction::SetOracleSigner { signer: Address::ZERO, proof: None },
        Some(k) => NativeAction::SetOracleSigner {
            signer: address_from_key(k),
            proof: Some(torus_types::eip712::sign_oracle_signer_proof(validator, nonce, k)),
        },
    }
}

/// The signer key from exactly one of `--signer-keystore` (passphrase from
/// `--signer-passphrase-file` or a prompt), `--signer-key-file` (hex, mode
/// 0600 or stricter), or `None` for `--clear`.
pub(crate) fn resolve_signer_key(
    keystore: Option<&Path>,
    key_file: Option<&Path>,
    passphrase_file: Option<&Path>,
    clear: bool,
) -> Result<Option<SigningKey>, String> {
    match (keystore, key_file, clear) {
        (None, None, true) => Ok(None),
        (Some(ks), None, false) => {
            let pass = match passphrase_file {
                Some(p) => std::fs::read_to_string(p)
                    .map_err(|e| format!("{}: {e}", p.display()))?
                    .trim_end()
                    .to_string(),
                None => prompt_passphrase("Enter SIGNER keystore passphrase: "),
            };
            load_keystore(ks, &pass).map(Some).map_err(|e| format!("signer keystore: {e}"))
        }
        (None, Some(kf), false) => load_hex_key_file(kf).map(Some),
        _ => Err("give exactly one of --signer-keystore, --signer-key-file or --clear".into()),
    }
}

fn load_hex_key_file(path: &Path) -> Result<SigningKey, String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(format!("{}: mode {:o}; the key file must be 0600 or stricter", path.display(), mode & 0o777));
    }
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let s = s.trim();
    let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s)).map_err(|e| format!("{}: bad hex: {e}", path.display()))?;
    SigningKey::from_slice(&bytes).map_err(|e| format!("{}: invalid key: {e}", path.display()))
}

pub(crate) async fn cmd_set_oracle_signer(
    cli: &Cli,
    rpc: &RpcClient,
    signer_keystore: Option<&Path>,
    signer_key_file: Option<&Path>,
    signer_passphrase_file: Option<&Path>,
    clear: bool,
) -> Result<(), String> {
    let signer = resolve_signer_key(signer_keystore, signer_key_file, signer_passphrase_file, clear)?;
    let key = load_signing_key(cli)?; // the VALIDATOR key
    let action = build_set_oracle_signer(&address_from_key(&key), signer.as_ref(), now_ms());
    submit_native_action_with_key(cli, rpc, action, &key).await
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use torus_types::eip712::sign_native_action;
    use torus_types::{NativeAction, PublicKey};

    use crate::keystore::address_from_key;
    use crate::test_utils::test_signing_key;

    fn roundtrip(action: NativeAction) {
        let key = test_signing_key();
        let signed = sign_native_action(action, 1_700_000_000_000u64, &key);
        assert_eq!(signed.recover_sender().unwrap(), address_from_key(&key));
    }

    #[test]
    fn test_register_validator_roundtrip() {
        roundtrip(NativeAction::RegisterValidator {
            pubkey: PublicKey([7u8; 32]),
            commission: 500,
        });
    }

    #[test]
    fn test_update_commission_roundtrip() {
        roundtrip(NativeAction::UpdateCommission { new_rate: 1000 });
    }

    #[test]
    fn test_jail_vote_roundtrip() {
        roundtrip(NativeAction::JailVote {
            target: Address::from_slice(&[0xDE; 20]),
        });
    }

    #[test]
    fn test_unjail_roundtrip() {
        roundtrip(NativeAction::UnjailSelf);
    }

    #[test]
    fn test_rotate_key_roundtrip() {
        roundtrip(NativeAction::RotateValidatorKey {
            new_pubkey: PublicKey([0xAB; 32]),
        });
    }

    /// s517 review M3: `set-oracle-signer --signer-keystore <path> |
    /// --signer-key-file <path> | --clear`. The action carries the signer
    /// key's proof of possession bound to the validator.
    #[test]
    fn set_oracle_signer_action() {
        use super::{build_set_oracle_signer, resolve_signer_key};
        use std::os::unix::fs::PermissionsExt;
        let validator = Address::repeat_byte(0x11);
        let signer_key = k256::ecdsa::SigningKey::from_slice(&[0x32; 32]).unwrap();
        let a = build_set_oracle_signer(&validator, Some(&signer_key), 42);
        match &a {
            NativeAction::SetOracleSigner { signer, proof: Some(p) } => {
                assert_eq!(*signer, address_from_key(&signer_key));
                assert_eq!(p.nonce, 42);
                let who = torus_types::eip712::recover_oracle_signer_proof(&validator, p).unwrap();
                assert_eq!(who, *signer, "proof made by the signer key for this validator");
            }
            other => panic!("{other:?}"),
        }
        let c = build_set_oracle_signer(&validator, None, 42);
        assert!(matches!(c, NativeAction::SetOracleSigner { signer, proof: None } if signer == Address::ZERO));
        let back: NativeAction = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(back.canonical_bytes(), a.canonical_bytes());
        assert!(torus_types::eip712::requires_eip712(&a));
        roundtrip(a);

        // Signer key sources: exactly one of keystore / key file / --clear.
        let d = tempfile::tempdir().unwrap();
        let kf = d.path().join("signer.key");
        std::fs::write(&kf, format!("0x{}\n", hex::encode(signer_key.to_bytes()))).unwrap();
        std::fs::set_permissions(&kf, std::fs::Permissions::from_mode(0o600)).unwrap();
        let got = resolve_signer_key(None, Some(&kf), None, false).unwrap().unwrap();
        assert_eq!(got.to_bytes(), signer_key.to_bytes());
        assert!(resolve_signer_key(None, None, None, true).unwrap().is_none());
        for (ks, k, clear) in [(None, None, false), (None, Some(kf.as_path()), true)] {
            let e = resolve_signer_key(ks, k, None, clear).unwrap_err();
            assert!(e.contains("exactly one"), "{e}");
        }
        std::fs::set_permissions(&kf, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(resolve_signer_key(None, Some(&kf), None, false).unwrap_err().contains("0600"));
        let ks = d.path().join("signer.keystore");
        crate::keystore::write_keystore(&ks, &signer_key, "pw").unwrap();
        let pf = d.path().join("pw");
        std::fs::write(&pf, "pw\n").unwrap();
        let got = resolve_signer_key(Some(&ks), None, Some(&pf), false).unwrap().unwrap();
        assert_eq!(got.to_bytes(), signer_key.to_bytes());
    }
}
