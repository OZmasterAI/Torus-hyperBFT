//! Staking commands: delegate, undelegate, claim-rewards.

use torus_types::NativeAction;

use crate::parse::{parse_address, parse_trs_to_wei};
use crate::rpc::RpcClient;
use crate::sign::submit_native_action;
use crate::Cli;

pub(crate) async fn cmd_delegate(
    cli: &Cli,
    rpc: &RpcClient,
    validator: &str,
    amount: &str,
) -> Result<(), String> {
    let validator_addr = parse_address(validator)?;
    let amount_wei = parse_trs_to_wei(amount)?;
    let action = NativeAction::Delegate {
        validator: validator_addr,
        amount: amount_wei,
    };
    submit_native_action(cli, rpc, action).await
}

pub(crate) async fn cmd_undelegate(
    cli: &Cli,
    rpc: &RpcClient,
    validator: &str,
    amount: &str,
) -> Result<(), String> {
    let validator_addr = parse_address(validator)?;
    let amount_wei = parse_trs_to_wei(amount)?;
    let action = NativeAction::Undelegate {
        validator: validator_addr,
        amount: amount_wei,
    };
    submit_native_action(cli, rpc, action).await
}

pub(crate) async fn cmd_claim_rewards(cli: &Cli, rpc: &RpcClient) -> Result<(), String> {
    submit_native_action(cli, rpc, NativeAction::ClaimRewards).await
}

pub(crate) async fn cmd_permanent_stake(
    cli: &Cli,
    rpc: &RpcClient,
    amount: &str,
) -> Result<(), String> {
    let amount_wei = parse_trs_to_wei(amount)?;
    submit_native_action(
        cli,
        rpc,
        NativeAction::PermanentStake { amount: amount_wei },
    )
    .await
}

pub(crate) async fn cmd_top_up_self_stake(
    cli: &Cli,
    rpc: &RpcClient,
    amount: &str,
) -> Result<(), String> {
    let amount_wei = parse_trs_to_wei(amount)?;
    submit_native_action(
        cli,
        rpc,
        NativeAction::TopUpSelfStake { amount: amount_wei },
    )
    .await
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;
    use torus_types::eip712::sign_native_action;
    use torus_types::NativeAction;

    use crate::keystore::address_from_key;
    use crate::test_utils::test_signing_key;

    #[test]
    fn test_permanent_stake_roundtrip() {
        let key = test_signing_key();
        let signed = sign_native_action(
            NativeAction::PermanentStake {
                amount: U256::from(5u64) * U256::from(1_000_000_000_000_000_000u64),
            },
            1_700_000_000_000u64,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), address_from_key(&key));
    }

    #[test]
    fn test_top_up_self_stake_roundtrip() {
        let key = test_signing_key();
        let signed = sign_native_action(
            NativeAction::TopUpSelfStake {
                amount: U256::from(3u64) * U256::from(1_000_000_000_000_000_000u64),
            },
            1_700_000_000_001u64,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), address_from_key(&key));
    }
}
