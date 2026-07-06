//! Torus CLI wallet — key management, queries, and transactions.

mod commands;
mod keystore;
mod parse;
mod rpc;
mod sign;
#[cfg(test)]
mod test_utils;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::rpc::RpcClient;

// ============================================================================
// CLI
// ============================================================================

#[derive(Parser)]
#[command(name = "torus-wallet", version, about = "Torus blockchain CLI wallet")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,

    /// JSON-RPC URL of the Torus node
    #[arg(long, global = true, default_value = "http://localhost:8545")]
    pub(crate) rpc_url: String,

    /// Path to encrypted keystore file (for transaction commands)
    #[arg(long, global = true)]
    pub(crate) keystore: Option<PathBuf>,

    /// Private key hex (UNSAFE — visible in shell history. Prefer --keystore.)
    #[arg(long, global = true)]
    pub(crate) key: Option<String>,

    /// Chain ID (default: from eth_chainId)
    #[arg(long, global = true)]
    pub(crate) chain_id: Option<u64>,

    /// Output as JSON
    #[arg(long, global = true, default_value = "false")]
    pub(crate) json: bool,

    /// Build and sign but do not submit — print the SignedNativeAction JSON
    #[arg(long, global = true, default_value = "false")]
    pub(crate) dry_run: bool,

    /// Read passphrase from file (trims trailing whitespace) instead of prompting
    #[arg(long, global = true)]
    pub(crate) passphrase_file: Option<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Generate a new secp256k1 keypair
    Keygen {
        /// Output keystore file path
        #[arg(long, default_value = "./wallet.keystore")]
        output: PathBuf,
    },
    /// Import a private key into a keystore file
    Import {
        /// Private key hex
        #[arg(long)]
        key: String,
        /// Output keystore file path
        #[arg(long, default_value = "./wallet.keystore")]
        output: PathBuf,
    },
    /// Show address from a keystore file
    Address {
        /// Keystore file path
        #[arg(long)]
        keystore: PathBuf,
    },
    /// Query account balance
    Balance {
        /// Address to query (defaults to keystore address)
        address: Option<String>,
    },
    /// List active validators
    Validators,
    /// Query staking info and delegations
    Staking {
        /// Address to query (defaults to keystore address)
        address: Option<String>,
    },
    /// Get block info
    Block {
        /// Block height (latest if omitted)
        height: Option<String>,
    },
    /// Get transaction details
    Tx {
        /// Transaction hash
        hash: String,
    },
    /// Send TRS to an address
    Send {
        /// Recipient address
        #[arg(long)]
        to: String,
        /// Amount in TRS (e.g., "1.5")
        #[arg(long)]
        value: String,
    },
    /// Delegate stake to a validator
    Delegate {
        /// Validator address
        #[arg(long)]
        validator: String,
        /// Amount in TRS
        #[arg(long)]
        amount: String,
    },
    /// Undelegate stake from a validator
    Undelegate {
        /// Validator address
        #[arg(long)]
        validator: String,
        /// Amount in TRS
        #[arg(long)]
        amount: String,
    },
    /// Claim staking rewards
    ClaimRewards,
    /// View order book for a market
    Orderbook {
        /// Market ID
        market_id: String,
    },
    /// View position for an address in a market
    Position {
        /// Trader address
        address: String,
        /// Market ID
        market_id: String,
    },
    /// List governance proposals (or show a specific one with --id)
    Proposals {
        #[arg(long)]
        id: Option<u64>,
    },
    /// List open orders for an address
    Orders {
        #[arg(long)]
        address: Option<String>,
        #[arg(long)]
        market: Option<u64>,
    },
    /// List open positions across all markets
    Positions {
        #[arg(long)]
        address: Option<String>,
    },
    /// List delegations for an address
    Delegations {
        #[arg(long)]
        address: Option<String>,
    },
    /// Show current epoch info
    Epoch,
    /// Vote on a governance proposal
    Vote {
        /// Proposal ID
        #[arg(long)]
        proposal: u64,
        /// Vote option: yes, no, abstain
        #[arg(long)]
        option: String,
    },
    /// Place a new order
    PlaceOrder {
        /// Market ID
        #[arg(long)]
        market: u64,
        /// Side: buy or sell
        #[arg(long)]
        side: String,
        /// Limit price (decimal, up to 8 places)
        #[arg(long)]
        price: String,
        /// Quantity (decimal, up to 8 places)
        #[arg(long)]
        quantity: String,
        /// Order type: limit, market, stop-market, stop-limit
        #[arg(long, default_value = "limit")]
        order_type: String,
        /// Time-in-force: gtc, ioc, fok, post-only
        #[arg(long, default_value = "gtc")]
        tif: String,
        /// Trigger price (required for stop-market/stop-limit)
        #[arg(long)]
        trigger_price: Option<String>,
        /// Reduce-only
        #[arg(long, default_value = "false")]
        reduce_only: bool,
        /// Client-supplied order id
        #[arg(long)]
        client_id: Option<u64>,
    },
    /// Cancel an order by id
    CancelOrder {
        /// Order ID (u128)
        #[arg(long)]
        order_id: u128,
    },
    /// Cancel all orders (in one market or across all markets)
    CancelAll {
        /// Market ID (omit with --all-markets to cancel across all markets)
        #[arg(long, required_unless_present = "all_markets")]
        market: Option<u64>,
        /// Cancel across all markets
        #[arg(long)]
        all_markets: bool,
    },
    /// Modify an existing order (price and/or quantity)
    ModifyOrder {
        /// Order ID (u128)
        #[arg(long)]
        order_id: u128,
        /// New price (decimal)
        #[arg(long)]
        price: Option<String>,
        /// New quantity (decimal)
        #[arg(long)]
        quantity: Option<String>,
    },
    /// Transfer TRS from spot balance to perp (margin) balance
    TransferToPerp {
        /// Amount in TRS
        #[arg(long)]
        amount: String,
    },
    /// Transfer TRS from perp to spot balance
    TransferToSpot {
        /// Amount in TRS
        #[arg(long)]
        amount: String,
    },
    /// Withdraw native TRS to an EVM address
    Withdraw {
        /// Recipient address
        #[arg(long)]
        to: String,
        /// Amount in TRS
        #[arg(long)]
        amount: String,
    },
    /// Permanent (irreversible) stake lock
    PermanentStake {
        /// Amount in TRS
        #[arg(long)]
        amount: String,
    },
    /// Top up validator self-stake (validator-only)
    TopUpSelfStake {
        /// Amount in TRS
        #[arg(long)]
        amount: String,
    },
    /// Submit a governance proposal
    SubmitProposal {
        /// Proposal title
        #[arg(long)]
        title: String,
        /// Proposal description
        #[arg(long)]
        description: String,
        /// Proposal type: param-change, list-market, delist-market, update-market-params, validator-registration
        #[arg(long)]
        proposal_type: String,
        /// JSON string with variant-specific fields
        #[arg(long)]
        params: String,
    },
    /// Register as a validator (pubkey = 64 hex chars)
    RegisterValidator {
        /// Ed25519 validator pubkey (64 hex chars)
        #[arg(long)]
        pubkey: String,
        /// Commission in basis points
        #[arg(long)]
        commission_bps: u16,
    },
    /// Update validator commission
    UpdateCommission {
        /// Commission in basis points
        #[arg(long)]
        commission_bps: u16,
    },
    /// Cast a jail vote against a validator
    JailVote {
        /// Target validator address
        #[arg(long)]
        validator: String,
    },
    /// Unjail self (validator-only)
    Unjail,
    /// Rotate validator consensus key
    RotateKey {
        /// New pubkey (64 hex chars)
        #[arg(long)]
        new_pubkey: String,
    },
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let rpc = RpcClient::new(&cli.rpc_url);

    let result = match &cli.command {
        Command::Keygen { output } => commands::keys::cmd_keygen(output.clone()).await,
        Command::Import { key, output } => {
            commands::keys::cmd_import(key.clone(), output.clone()).await
        }
        Command::Address { keystore } => commands::keys::cmd_address(keystore.clone()).await,
        Command::Balance { address } => {
            let addr = match address {
                Some(a) => a.clone(),
                None => match sign::resolve_address(&cli, &None) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    }
                },
            };
            commands::query::cmd_balance(&rpc, &addr, cli.json).await
        }
        Command::Validators => commands::query::cmd_validators(&rpc, cli.json).await,
        Command::Staking { address } => {
            let addr = match address {
                Some(a) => a.clone(),
                None => match sign::resolve_address(&cli, &None) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    }
                },
            };
            commands::query::cmd_staking(&rpc, &addr, cli.json).await
        }
        Command::Block { height } => {
            commands::query::cmd_block(&rpc, height.clone(), cli.json).await
        }
        Command::Tx { hash } => commands::query::cmd_tx(&rpc, hash, cli.json).await,
        Command::Send { to, value } => commands::transfer::cmd_send(&cli, &rpc, to, value).await,
        Command::Delegate { validator, amount } => {
            commands::staking::cmd_delegate(&cli, &rpc, validator, amount).await
        }
        Command::Undelegate { validator, amount } => {
            commands::staking::cmd_undelegate(&cli, &rpc, validator, amount).await
        }
        Command::ClaimRewards => commands::staking::cmd_claim_rewards(&cli, &rpc).await,
        Command::Orderbook { market_id } => {
            commands::query::cmd_orderbook(&rpc, market_id, cli.json).await
        }
        Command::Position { address, market_id } => {
            commands::query::cmd_position(&rpc, address, market_id, cli.json).await
        }
        Command::Proposals { id } => commands::query::cmd_proposals(&rpc, *id, cli.json).await,
        Command::Orders { address, market } => {
            let addr = match sign::resolve_address(&cli, address) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            };
            commands::query::cmd_orders(&rpc, &addr, *market, cli.json).await
        }
        Command::Positions { address } => {
            let addr = match sign::resolve_address(&cli, address) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            };
            commands::query::cmd_positions(&rpc, &addr, cli.json).await
        }
        Command::Delegations { address } => {
            let addr = match sign::resolve_address(&cli, address) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            };
            commands::query::cmd_delegations(&rpc, &addr, cli.json).await
        }
        Command::Epoch => commands::query::cmd_epoch(&rpc, cli.json).await,
        Command::Vote { proposal, option } => {
            commands::governance::cmd_vote(&cli, &rpc, *proposal, option).await
        }
        Command::PlaceOrder {
            market,
            side,
            price,
            quantity,
            order_type,
            tif,
            trigger_price,
            reduce_only,
            client_id,
        } => {
            commands::trading::cmd_place_order(
                &cli,
                &rpc,
                *market,
                side,
                price,
                quantity,
                order_type,
                tif,
                trigger_price.as_deref(),
                *reduce_only,
                *client_id,
            )
            .await
        }
        Command::CancelOrder { order_id } => {
            commands::trading::cmd_cancel_order(&cli, &rpc, *order_id).await
        }
        Command::CancelAll { market, .. } => {
            commands::trading::cmd_cancel_all(&cli, &rpc, *market).await
        }
        Command::ModifyOrder {
            order_id,
            price,
            quantity,
        } => {
            commands::trading::cmd_modify_order(
                &cli,
                &rpc,
                *order_id,
                price.as_deref(),
                quantity.as_deref(),
            )
            .await
        }
        Command::TransferToPerp { amount } => {
            commands::transfer::cmd_transfer_to_perp(&cli, &rpc, amount).await
        }
        Command::TransferToSpot { amount } => {
            commands::transfer::cmd_transfer_to_spot(&cli, &rpc, amount).await
        }
        Command::Withdraw { to, amount } => {
            commands::transfer::cmd_withdraw(&cli, &rpc, to, amount).await
        }
        Command::PermanentStake { amount } => {
            commands::staking::cmd_permanent_stake(&cli, &rpc, amount).await
        }
        Command::TopUpSelfStake { amount } => {
            commands::staking::cmd_top_up_self_stake(&cli, &rpc, amount).await
        }
        Command::SubmitProposal {
            title,
            description,
            proposal_type,
            params,
        } => {
            commands::governance::cmd_submit_proposal(
                &cli,
                &rpc,
                title,
                description,
                proposal_type,
                params,
            )
            .await
        }
        Command::RegisterValidator {
            pubkey,
            commission_bps,
        } => commands::validator::cmd_register_validator(&cli, &rpc, pubkey, *commission_bps).await,
        Command::UpdateCommission { commission_bps } => {
            commands::validator::cmd_update_commission(&cli, &rpc, *commission_bps).await
        }
        Command::JailVote { validator } => {
            commands::validator::cmd_jail_vote(&cli, &rpc, validator).await
        }
        Command::Unjail => commands::validator::cmd_unjail(&cli, &rpc).await,
        Command::RotateKey { new_pubkey } => {
            commands::validator::cmd_rotate_key(&cli, &rpc, new_pubkey).await
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
