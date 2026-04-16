//! Governance commands: vote, submit-proposal.

use torus_types::{
    MarketListing, MarketParams, NativeAction, Proposal, ProposalAction, VoteOption,
};

use crate::parse::{parse_address, parse_decimal_to_fixed_point};
use crate::rpc::RpcClient;
use crate::sign::submit_native_action;
use crate::Cli;

pub(crate) async fn cmd_vote(
    cli: &Cli,
    rpc: &RpcClient,
    proposal_id: u64,
    option: &str,
) -> Result<(), String> {
    let vote_option = match option.to_lowercase().as_str() {
        "yes" => VoteOption::Yes,
        "no" => VoteOption::No,
        "abstain" => VoteOption::Abstain,
        _ => return Err("invalid vote option, use: yes, no, abstain".into()),
    };
    submit_native_action(
        cli,
        rpc,
        NativeAction::Vote {
            proposal_id,
            option: vote_option,
        },
    )
    .await
}

/// Build and submit a governance proposal.
///
/// `proposal_type` selects one of five `ProposalAction` variants; `params_json`
/// supplies the variant-specific fields (see tech-req §3.5 for schemas).
///
/// Note: `MarketListing` has NO `max_funding_rate_bps`; `MarketParams` DOES.
pub(crate) async fn cmd_submit_proposal(
    cli: &Cli,
    rpc: &RpcClient,
    title: &str,
    description: &str,
    proposal_type: &str,
    params_json: &str,
) -> Result<(), String> {
    let action = parse_proposal_action(proposal_type, params_json)?;
    let proposal = Proposal {
        title: title.to_string(),
        description: description.to_string(),
        action,
    };
    submit_native_action(cli, rpc, NativeAction::SubmitProposal(proposal)).await
}

fn parse_proposal_action(proposal_type: &str, params_json: &str) -> Result<ProposalAction, String> {
    let v: serde_json::Value =
        serde_json::from_str(params_json).map_err(|e| format!("invalid --params JSON: {e}"))?;

    fn req_str<'a>(v: &'a serde_json::Value, field: &str) -> Result<&'a str, String> {
        v.get(field)
            .and_then(|x| x.as_str())
            .ok_or_else(|| format!("params.{field} required (string)"))
    }
    fn req_u64(v: &serde_json::Value, field: &str) -> Result<u64, String> {
        v.get(field)
            .and_then(|x| x.as_u64())
            .ok_or_else(|| format!("params.{field} required (integer)"))
    }

    match proposal_type {
        "param-change" => Ok(ProposalAction::ParameterChange {
            key: req_str(&v, "key")?.to_string(),
            value: req_str(&v, "value")?.to_string(),
        }),
        "list-market" => Ok(ProposalAction::ListMarket(MarketListing {
            base_asset: req_str(&v, "base_asset")?.to_string(),
            quote_asset: req_str(&v, "quote_asset")?.to_string(),
            tick_size: parse_decimal_to_fixed_point(req_str(&v, "tick_size")?)?,
            lot_size: parse_decimal_to_fixed_point(req_str(&v, "lot_size")?)?,
            max_leverage: req_u64(&v, "max_leverage")? as u32,
            maintenance_margin_bps: req_u64(&v, "maintenance_margin_bps")? as u32,
        })),
        "delist-market" => Ok(ProposalAction::DelistMarket {
            market_id: req_u64(&v, "market_id")?,
        }),
        "update-market-params" => Ok(ProposalAction::UpdateMarketParams {
            market_id: req_u64(&v, "market_id")?,
            params: MarketParams {
                tick_size: parse_decimal_to_fixed_point(req_str(&v, "tick_size")?)?,
                lot_size: parse_decimal_to_fixed_point(req_str(&v, "lot_size")?)?,
                max_leverage: req_u64(&v, "max_leverage")? as u32,
                maintenance_margin_bps: req_u64(&v, "maintenance_margin_bps")? as u32,
                max_funding_rate_bps: req_u64(&v, "max_funding_rate_bps")? as u32,
            },
        }),
        "validator-registration" => Ok(ProposalAction::ValidatorRegistration {
            candidate: parse_address(req_str(&v, "candidate")?)?,
        }),
        other => Err(format!(
            "unknown --proposal-type '{other}' (param-change|list-market|delist-market|update-market-params|validator-registration)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;
    use torus_types::eip712::sign_native_action;

    use crate::keystore::address_from_key;

    fn test_key() -> SigningKey {
        SigningKey::from_slice(&{
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        })
        .unwrap()
    }

    fn roundtrip(action: ProposalAction) {
        let key = test_key();
        let proposal = Proposal {
            title: "T".into(),
            description: "D".into(),
            action,
        };
        let signed = sign_native_action(
            NativeAction::SubmitProposal(proposal),
            1_700_000_000_000u64,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), address_from_key(&key));
    }

    #[test]
    fn test_proposal_param_change() {
        let a = parse_proposal_action(
            "param-change",
            r#"{"key":"epoch_length","value":"1000"}"#,
        )
        .unwrap();
        match &a {
            ProposalAction::ParameterChange { key, value } => {
                assert_eq!(key, "epoch_length");
                assert_eq!(value, "1000");
            }
            _ => panic!("wrong variant"),
        }
        roundtrip(a);
    }

    #[test]
    fn test_proposal_list_market() {
        let a = parse_proposal_action(
            "list-market",
            r#"{"base_asset":"BTC","quote_asset":"USD","tick_size":"0.01","lot_size":"0.001","max_leverage":50,"maintenance_margin_bps":300}"#,
        )
        .unwrap();
        match &a {
            ProposalAction::ListMarket(l) => {
                assert_eq!(l.base_asset, "BTC");
                assert_eq!(l.quote_asset, "USD");
                assert_eq!(l.tick_size.raw(), 1_000_000);
                assert_eq!(l.lot_size.raw(), 100_000);
                assert_eq!(l.max_leverage, 50);
                assert_eq!(l.maintenance_margin_bps, 300);
            }
            _ => panic!("wrong variant"),
        }
        roundtrip(a);
    }

    #[test]
    fn test_proposal_delist_market() {
        let a = parse_proposal_action("delist-market", r#"{"market_id":5}"#).unwrap();
        match &a {
            ProposalAction::DelistMarket { market_id } => assert_eq!(*market_id, 5),
            _ => panic!("wrong variant"),
        }
        roundtrip(a);
    }

    #[test]
    fn test_proposal_update_market_params() {
        let a = parse_proposal_action(
            "update-market-params",
            r#"{"market_id":1,"tick_size":"0.01","lot_size":"0.001","max_leverage":20,"maintenance_margin_bps":500,"max_funding_rate_bps":100}"#,
        )
        .unwrap();
        match &a {
            ProposalAction::UpdateMarketParams { market_id, params } => {
                assert_eq!(*market_id, 1);
                assert_eq!(params.max_funding_rate_bps, 100);
            }
            _ => panic!("wrong variant"),
        }
        roundtrip(a);
    }

    #[test]
    fn test_proposal_validator_registration() {
        let a = parse_proposal_action(
            "validator-registration",
            r#"{"candidate":"0x0000000000000000000000000000000000000abc"}"#,
        )
        .unwrap();
        match &a {
            ProposalAction::ValidatorRegistration { candidate } => {
                assert_eq!(candidate.as_slice()[19], 0xbc);
            }
            _ => panic!("wrong variant"),
        }
        roundtrip(a);
    }

    #[test]
    fn test_proposal_unknown_type_errors() {
        let err = parse_proposal_action("bogus", "{}").unwrap_err();
        assert!(err.contains("unknown --proposal-type"));
    }

    #[test]
    fn test_proposal_invalid_json_errors() {
        let err = parse_proposal_action("param-change", "{not json").unwrap_err();
        assert!(err.contains("invalid --params JSON"));
    }

    #[test]
    fn test_proposal_missing_field_errors() {
        let err = parse_proposal_action("param-change", r#"{"key":"x"}"#).unwrap_err();
        assert!(err.contains("params.value required"));
    }
}
