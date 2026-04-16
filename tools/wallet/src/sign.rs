//! Signing helpers: key loading, passphrase handling, EIP-712 native action submit,
//! and EIP-1559 EVM transaction construction.

use std::time::{SystemTime, UNIX_EPOCH};

use alloy_consensus::TxEip1559;
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, Bytes, Signature as AlloySig, TxKind, U256};
use alloy_rlp::Encodable;
use k256::ecdsa::SigningKey;
use torus_types::eip712::sign_native_action;
use torus_types::NativeAction;

use crate::keystore::{address_from_key, load_keystore};
use crate::rpc::RpcClient;
use crate::Cli;

pub(crate) fn load_signing_key(cli: &Cli) -> Result<SigningKey, String> {
    if let Some(ref key_hex) = cli.key {
        eprintln!("WARNING: Using --key flag exposes your private key in shell history and process list.");
        eprintln!("         Use --keystore for production use.");
        let hex_str = key_hex.strip_prefix("0x").unwrap_or(key_hex);
        let bytes = hex::decode(hex_str).map_err(|e| format!("invalid key hex: {e}"))?;
        return SigningKey::from_slice(&bytes).map_err(|e| format!("invalid key: {e}"));
    }

    if let Some(ref path) = cli.keystore {
        let pass = read_passphrase(cli)?;
        return load_keystore(path, &pass).map_err(|e| format!("keystore load: {e}"));
    }

    Err("signing key required: use --keystore <path> or --key <hex>".into())
}

pub(crate) fn prompt_passphrase(prompt: &str) -> String {
    eprint!("{prompt}");
    let mut pass = String::new();
    std::io::stdin()
        .read_line(&mut pass)
        .expect("failed to read passphrase");
    pass.trim().to_string()
}

/// Resolve passphrase from `--passphrase-file` (if set) or interactive prompt.
/// File content is read and trailing whitespace stripped.
pub(crate) fn read_passphrase(cli: &Cli) -> Result<String, String> {
    if let Some(path) = &cli.passphrase_file {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("passphrase file not found: {}: {e}", path.display()))?;
        Ok(contents
            .trim_end_matches(|c: char| c.is_whitespace())
            .to_string())
    } else {
        Ok(prompt_passphrase("Enter keystore passphrase: "))
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Resolve the query address: explicit `--address`, else derive from keystore/key.
/// Errors if neither is available.
pub(crate) fn resolve_address(cli: &Cli, explicit: &Option<String>) -> Result<String, String> {
    if let Some(addr) = explicit {
        return Ok(addr.clone());
    }
    if cli.keystore.is_some() || cli.key.is_some() {
        let key = load_signing_key(cli)?;
        let addr = address_from_key(&key);
        return Ok(format!("0x{}", hex::encode(addr)));
    }
    Err("address required: provide --address or --keystore".into())
}

pub(crate) fn build_and_sign_eip1559_tx(
    key: &SigningKey,
    chain_id: u64,
    nonce: u64,
    to: Address,
    value: U256,
    gas_price: u128,
) -> Vec<u8> {
    let tx = TxEip1559 {
        chain_id,
        nonce,
        gas_limit: 21_000,
        max_fee_per_gas: gas_price,
        max_priority_fee_per_gas: gas_price / 10,
        to: TxKind::Call(to),
        value,
        input: Bytes::new(),
        access_list: Default::default(),
    };

    let mut rlp_buf = Vec::new();
    tx.encode(&mut rlp_buf);
    let mut hash_input = Vec::with_capacity(1 + rlp_buf.len());
    hash_input.push(0x02);
    hash_input.extend_from_slice(&rlp_buf);
    let signing_hash = alloy_primitives::keccak256(&hash_input);

    let (sig, recid) = key
        .sign_prehash_recoverable(signing_hash.as_slice())
        .expect("signing cannot fail");
    let sig_bytes = sig.to_bytes();
    let y_parity = recid.to_byte() != 0;

    let r_u256 = U256::from_be_slice(&sig_bytes[..32]);
    let s_u256 = U256::from_be_slice(&sig_bytes[32..]);
    let alloy_sig = AlloySig::new(r_u256, s_u256, y_parity);
    let signed = alloy_consensus::Signed::new_unchecked(tx, alloy_sig, signing_hash);
    let envelope = alloy_consensus::TxEnvelope::Eip1559(signed);

    let mut encoded = Vec::new();
    envelope.encode_2718(&mut encoded);
    encoded
}

/// Sign `action` and either submit via RPC or, in dry-run mode, print the signed
/// JSON and exit without submitting.
pub(crate) async fn submit_native_action(
    cli: &Cli,
    rpc: &RpcClient,
    action: NativeAction,
) -> Result<(), String> {
    let key = load_signing_key(cli)?;
    let nonce = now_ms();
    let signed = sign_native_action(action, nonce, &key);

    if cli.dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&signed).map_err(|e| format!("serialize: {e}"))?
        );
        return Ok(());
    }

    let json = serde_json::to_string(&signed).map_err(|e| format!("serialize: {e}"))?;
    let result = rpc.submit_native_action(&json).await?;

    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "result": result })).unwrap()
        );
    } else {
        println!("Native action submitted: {result}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn cli_with_passphrase_file(path: std::path::PathBuf) -> Cli {
        Cli {
            command: crate::Command::Validators,
            rpc_url: "http://localhost:8545".to_string(),
            keystore: None,
            key: None,
            chain_id: None,
            json: false,
            dry_run: false,
            passphrase_file: Some(path),
        }
    }

    fn cli_with_keystore(path: std::path::PathBuf, passphrase_file: std::path::PathBuf) -> Cli {
        Cli {
            command: crate::Command::Validators,
            rpc_url: "http://localhost:8545".to_string(),
            keystore: Some(path),
            key: None,
            chain_id: None,
            json: false,
            dry_run: false,
            passphrase_file: Some(passphrase_file),
        }
    }

    #[test]
    fn test_passphrase_file_trim() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"secret\n\n").unwrap();
        let cli = cli_with_passphrase_file(tmp.path().to_path_buf());
        assert_eq!(read_passphrase(&cli).unwrap(), "secret");
    }

    #[test]
    fn test_passphrase_file_trim_trailing_spaces() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"another secret \t\r\n").unwrap();
        let cli = cli_with_passphrase_file(tmp.path().to_path_buf());
        assert_eq!(read_passphrase(&cli).unwrap(), "another secret");
    }

    #[test]
    fn test_passphrase_file_missing() {
        let cli = cli_with_passphrase_file("/nonexistent/path/to/passphrase".into());
        let err = read_passphrase(&cli).unwrap_err();
        assert!(err.contains("passphrase file not found"));
    }

    #[test]
    fn test_resolve_address_explicit() {
        let cli = Cli {
            command: crate::Command::Validators,
            rpc_url: "http://localhost:8545".to_string(),
            keystore: None,
            key: None,
            chain_id: None,
            json: false,
            dry_run: false,
            passphrase_file: None,
        };
        let explicit = Some("0x0000000000000000000000000000000000000123".to_string());
        assert_eq!(
            resolve_address(&cli, &explicit).unwrap(),
            "0x0000000000000000000000000000000000000123"
        );
    }

    #[test]
    fn test_resolve_address_from_keystore() {
        use crate::keystore::generate_keystore;
        let dir = tempfile::TempDir::new().unwrap();
        let ks_path = dir.path().join("wallet.keystore");
        let pf_path = dir.path().join("pass.txt");
        std::fs::write(&pf_path, "mypass\n").unwrap();
        let (_key, addr) = generate_keystore(&ks_path, "mypass").unwrap();

        let cli = cli_with_keystore(ks_path, pf_path);
        let resolved = resolve_address(&cli, &None).unwrap();
        let expected = format!("0x{}", hex::encode(addr));
        assert_eq!(resolved, expected);
    }

    #[test]
    fn test_resolve_address_missing_errors() {
        let cli = Cli {
            command: crate::Command::Validators,
            rpc_url: "http://localhost:8545".to_string(),
            keystore: None,
            key: None,
            chain_id: None,
            json: false,
            dry_run: false,
            passphrase_file: None,
        };
        let err = resolve_address(&cli, &None).unwrap_err();
        assert!(err.contains("address required"));
    }

    #[test]
    fn test_build_eip1559_tx() {
        let key = SigningKey::from_slice(&{
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        })
        .unwrap();
        let to = Address::from_slice(&[0xBB; 20]);
        let raw = build_and_sign_eip1559_tx(&key, 7777, 0, to, U256::from(1000), 1_000_000_000);
        assert_eq!(raw[0], 0x02);
        assert!(raw.len() > 100);
    }

    #[test]
    fn test_native_action_signing() {
        let key = SigningKey::from_slice(&{
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        })
        .unwrap();
        let action = NativeAction::ClaimRewards;
        let signed = sign_native_action(action, 1_700_000_000_000u64, &key);
        let recovered = signed.recover_sender().expect("recovery should succeed");
        assert_eq!(recovered, address_from_key(&key));
    }

    #[test]
    fn test_delegate_action_signing() {
        let key = SigningKey::from_slice(&{
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        })
        .unwrap();
        let validator = Address::from_slice(&[0xCC; 20]);
        let action = NativeAction::Delegate {
            validator,
            amount: U256::from(1_000_000_000_000_000_000u64),
        };
        let signed = sign_native_action(action, 1_700_000_000_000u64, &key);
        let recovered = signed.recover_sender().expect("recovery should succeed");
        assert_eq!(recovered, address_from_key(&key));
    }

    #[test]
    fn test_dry_run_output_is_valid_json() {
        // Pure serialization roundtrip without RPC.
        let key = SigningKey::from_slice(&{
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        })
        .unwrap();
        let signed = sign_native_action(NativeAction::ClaimRewards, 1_700_000_000_000, &key);
        let pretty = serde_json::to_string_pretty(&signed).unwrap();
        let parsed: torus_types::SignedNativeAction = serde_json::from_str(&pretty).unwrap();
        assert_eq!(parsed.nonce, signed.nonce);
        let recovered = parsed.recover_sender().unwrap();
        assert_eq!(recovered, address_from_key(&key));
    }
}
