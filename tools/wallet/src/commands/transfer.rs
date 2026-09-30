//! Transfer commands: send (EVM), transfer-to-perp, transfer-to-spot, withdraw.
//!
//! Units: `send` is an EVM value transfer in wei (18 decimals, `parse_trs_to_wei`).
//! The native lockbox actions (transfer-to-perp / transfer-to-spot / withdraw) carry
//! NATIVE 8-decimal units (`parse_trs_to_native_units`); the node moves
//! `amount × 10^10` wei on the EVM side.

use torus_types::NativeAction;

use crate::keystore::address_from_key;
use crate::parse::{parse_address, parse_trs_to_native_units, parse_trs_to_wei};
use crate::rpc::RpcClient;
use crate::sign::{build_and_sign_eip1559_tx, load_signing_key, submit_native_action};
use crate::Cli;

pub(crate) async fn cmd_send(
    cli: &Cli,
    rpc: &RpcClient,
    to: &str,
    value: &str,
) -> Result<(), String> {
    let key = load_signing_key(cli)?;
    let from_addr = address_from_key(&key);
    let from_hex = format!("0x{}", hex::encode(from_addr));
    let to_addr = parse_address(to)?;
    let value_wei = parse_trs_to_wei(value)?;

    let chain_id = match cli.chain_id {
        Some(id) => id,
        None => rpc.chain_id().await?,
    };
    let nonce = rpc.get_transaction_count(&from_hex).await?;
    let gas_price = rpc.gas_price().await?;

    let raw = build_and_sign_eip1559_tx(&key, chain_id, nonce, to_addr, value_wei, gas_price);
    let raw_hex = format!("0x{}", hex::encode(&raw));
    let tx_hash = rpc.send_raw_transaction(&raw_hex).await?;

    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "tx_hash": tx_hash,
                "from": from_hex,
                "to": to,
                "value_trs": value,
            }))
            .unwrap()
        );
    } else {
        println!("Transaction sent!");
        println!("  Hash: {tx_hash}");
        println!("  From: {from_hex}");
        println!("  To:   {to}");
        println!("  Value: {value} TRS");
    }
    Ok(())
}

pub(crate) async fn cmd_transfer_to_perp(
    cli: &Cli,
    rpc: &RpcClient,
    amount: &str,
) -> Result<(), String> {
    let amount = parse_trs_to_native_units(amount)?;
    submit_native_action(cli, rpc, NativeAction::TransferToPerp { amount }).await
}

pub(crate) async fn cmd_transfer_to_spot(
    cli: &Cli,
    rpc: &RpcClient,
    amount: &str,
) -> Result<(), String> {
    let amount = parse_trs_to_native_units(amount)?;
    submit_native_action(cli, rpc, NativeAction::TransferToSpot { amount }).await
}

pub(crate) async fn cmd_withdraw(
    cli: &Cli,
    rpc: &RpcClient,
    to: &str,
    amount: &str,
) -> Result<(), String> {
    let to_addr = parse_address(to)?;
    let amount = parse_trs_to_native_units(amount)?;
    submit_native_action(
        cli,
        rpc,
        NativeAction::Withdraw {
            amount,
            to: to_addr,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, U256};
    use torus_types::eip712::sign_native_action;
    use torus_types::NativeAction;

    use crate::keystore::address_from_key;
    use crate::test_utils::test_signing_key;

    #[test]
    fn test_transfer_to_perp_roundtrip() {
        let key = test_signing_key();
        let signed = sign_native_action(
            NativeAction::TransferToPerp {
                amount: U256::from(1_000_000_000_000_000_000u64),
            },
            1_700_000_000_000u64,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), address_from_key(&key));
    }

    #[test]
    fn test_transfer_to_spot_roundtrip() {
        let key = test_signing_key();
        let signed = sign_native_action(
            NativeAction::TransferToSpot {
                amount: U256::from(2u64) * U256::from(1_000_000_000_000_000_000u64),
            },
            1_700_000_000_001u64,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), address_from_key(&key));
    }

    #[test]
    fn test_withdraw_roundtrip() {
        let key = test_signing_key();
        let signed = sign_native_action(
            NativeAction::Withdraw {
                amount: U256::from(500_000_000_000_000_000u64),
                to: Address::from_slice(&[0xAB; 20]),
            },
            1_700_000_000_002u64,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), address_from_key(&key));
    }
}
