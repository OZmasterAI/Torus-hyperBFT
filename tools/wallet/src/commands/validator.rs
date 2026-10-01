//! Validator operations: register-validator, update-commission, jail-vote, unjail, rotate-key,
//! set-oracle-signer.

use alloy_primitives::Address;
use torus_types::NativeAction;

use crate::parse::{parse_address, parse_pubkey};
use crate::rpc::RpcClient;
use crate::sign::submit_native_action;
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

/// `--signer <addr>` sets or rotates the hot oracle signer, `--clear` removes
/// it (`Address::ZERO` on chain). Exactly one of the two.
pub(crate) fn build_set_oracle_signer(signer: Option<&str>, clear: bool) -> Result<NativeAction, String> {
    let signer = match (signer, clear) {
        (Some(s), false) => {
            let a = parse_address(s)?;
            if a == Address::ZERO {
                return Err("the zero address clears the signer: use --clear".into());
            }
            a
        }
        (None, true) => Address::ZERO,
        _ => return Err("give exactly one of --signer or --clear".into()),
    };
    Ok(NativeAction::SetOracleSigner { signer })
}

pub(crate) async fn cmd_set_oracle_signer(
    cli: &Cli,
    rpc: &RpcClient,
    signer: Option<&str>,
    clear: bool,
) -> Result<(), String> {
    let action = build_set_oracle_signer(signer, clear)?;
    submit_native_action(cli, rpc, action).await
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

    /// s517 oracle feeder W1: `set-oracle-signer --signer <addr> | --clear`.
    #[test]
    fn set_oracle_signer_action() {
        use super::build_set_oracle_signer;
        let s = "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
        let want = Address::repeat_byte(0x5a);
        let a = build_set_oracle_signer(Some(s), false).unwrap();
        assert!(matches!(a, NativeAction::SetOracleSigner { signer } if signer == want));
        let c = build_set_oracle_signer(None, true).unwrap();
        assert!(matches!(c, NativeAction::SetOracleSigner { signer } if signer == Address::ZERO));
        for (sig, clear) in [(None, false), (Some(s), true)] {
            let e = build_set_oracle_signer(sig, clear).unwrap_err();
            assert!(e.contains("exactly one of --signer or --clear"), "{e}");
        }
        assert!(build_set_oracle_signer(Some("0x12"), false).is_err());
        assert!(build_set_oracle_signer(Some("0x0000000000000000000000000000000000000000"), false)
            .unwrap_err()
            .contains("--clear"));
        let json = serde_json::to_string(&a).unwrap();
        let back: NativeAction = serde_json::from_str(&json).unwrap();
        assert_eq!(back.canonical_bytes(), a.canonical_bytes());
        assert!(torus_types::eip712::requires_eip712(&a));
        roundtrip(a);
    }
}
