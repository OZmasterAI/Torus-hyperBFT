//! Torus CLI wallet — key management, queries, and transactions.

mod keystore;
mod rpc;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_consensus::TxEip1559;
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, Bytes, Signature as AlloySig, TxKind, U256};
use alloy_rlp::Encodable;
use clap::{Parser, Subcommand};
use k256::ecdsa::SigningKey;
use torus_types::eip712::sign_native_action;
use torus_types::NativeAction;

use crate::keystore::{address_from_key, generate_keystore, load_keystore};
use crate::rpc::{format_trs, RpcClient};

// ============================================================================
// CLI
// ============================================================================

#[derive(Parser)]
#[command(name = "torus-wallet", version, about = "Torus blockchain CLI wallet")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// JSON-RPC URL of the Torus node
    #[arg(long, global = true, default_value = "http://localhost:8545")]
    rpc_url: String,

    /// Path to encrypted keystore file (for transaction commands)
    #[arg(long, global = true)]
    keystore: Option<PathBuf>,

    /// Private key hex (UNSAFE — visible in shell history. Prefer --keystore.)
    #[arg(long, global = true)]
    key: Option<String>,

    /// Chain ID (default: from eth_chainId)
    #[arg(long, global = true)]
    chain_id: Option<u64>,

    /// Output as JSON
    #[arg(long, global = true, default_value = "false")]
    json: bool,
}

#[derive(Subcommand)]
enum Command {
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
        /// Address to query
        address: String,
    },
    /// List active validators
    Validators,
    /// Query staking info and delegations
    Staking {
        /// Address to query
        address: String,
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
    /// List governance proposals
    Proposals,
    /// Vote on a governance proposal
    Vote {
        /// Proposal ID
        #[arg(long)]
        proposal: u64,
        /// Vote option: yes, no, abstain
        #[arg(long)]
        option: String,
    },
}

// ============================================================================
// Key loading
// ============================================================================

fn load_signing_key(cli: &Cli) -> Result<SigningKey, String> {
    if let Some(ref key_hex) = cli.key {
        eprintln!("WARNING: Using --key flag exposes your private key in shell history and process list.");
        eprintln!("         Use --keystore for production use.");
        let hex_str = key_hex.strip_prefix("0x").unwrap_or(key_hex);
        let bytes = hex::decode(hex_str).map_err(|e| format!("invalid key hex: {e}"))?;
        return SigningKey::from_slice(&bytes).map_err(|e| format!("invalid key: {e}"));
    }

    if let Some(ref path) = cli.keystore {
        let pass = prompt_passphrase("Enter keystore passphrase: ");
        return load_keystore(path, &pass).map_err(|e| format!("keystore load: {e}"));
    }

    Err("signing key required: use --keystore <path> or --key <hex>".into())
}

fn prompt_passphrase(prompt: &str) -> String {
    eprint!("{prompt}");
    let mut pass = String::new();
    std::io::stdin().read_line(&mut pass).expect("failed to read passphrase");
    pass.trim().to_string()
}

fn parse_trs_to_wei(trs: &str) -> Result<U256, String> {
    let parts: Vec<&str> = trs.split('.').collect();
    match parts.len() {
        1 => {
            let whole: u128 = parts[0].parse().map_err(|e| format!("invalid amount: {e}"))?;
            Ok(U256::from(whole) * U256::from(1_000_000_000_000_000_000u64))
        }
        2 => {
            let whole: u128 = parts[0].parse().map_err(|e| format!("invalid amount: {e}"))?;
            let frac_str = parts[1];
            if frac_str.len() > 18 {
                return Err("too many decimal places (max 18)".into());
            }
            let padded = format!("{frac_str:0<18}");
            let frac: u128 = padded.parse().map_err(|e| format!("invalid fraction: {e}"))?;
            Ok(U256::from(whole) * U256::from(1_000_000_000_000_000_000u64) + U256::from(frac))
        }
        _ => Err("invalid amount format, expected '1.5' or '1'".into()),
    }
}

fn parse_address(s: &str) -> Result<Address, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 40 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("invalid Ethereum address".into());
    }
    let bytes = hex::decode(s).map_err(|e| format!("hex: {e}"))?;
    Ok(Address::from_slice(&bytes))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn build_and_sign_eip1559_tx(
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

// ============================================================================
// Command handlers
// ============================================================================

async fn cmd_keygen(output: PathBuf) -> Result<(), String> {
    let pass = prompt_passphrase("Enter passphrase for new keystore: ");
    let pass2 = prompt_passphrase("Confirm passphrase: ");
    if pass != pass2 {
        return Err("passphrases do not match".into());
    }

    let (key, addr) = generate_keystore(&output, &pass)
        .map_err(|e| format!("keygen failed: {e}"))?;

    let private_key_hex = hex::encode(key.to_bytes());
    println!("Address:     0x{}", hex::encode(addr));
    println!("Private key: 0x{private_key_hex}");
    println!();
    println!("Keystore saved to: {}", output.display());
    println!("IMPORTANT: The private key above will NEVER be shown again.");
    println!("           Store it securely or keep the keystore file safe.");

    Ok(())
}

async fn cmd_import(key_hex: String, output: PathBuf) -> Result<(), String> {
    let hex_str = key_hex.strip_prefix("0x").unwrap_or(&key_hex);
    let bytes = hex::decode(hex_str).map_err(|e| format!("invalid hex: {e}"))?;
    let key = SigningKey::from_slice(&bytes).map_err(|e| format!("invalid key: {e}"))?;

    let pass = prompt_passphrase("Enter passphrase for keystore: ");
    let pass2 = prompt_passphrase("Confirm passphrase: ");
    if pass != pass2 {
        return Err("passphrases do not match".into());
    }

    keystore::write_keystore(&output, &key, &pass)
        .map_err(|e| format!("write failed: {e}"))?;

    let addr = address_from_key(&key);
    println!("Imported address: 0x{}", hex::encode(addr));
    println!("Keystore saved to: {}", output.display());
    Ok(())
}

async fn cmd_address(keystore_path: PathBuf) -> Result<(), String> {
    let pass = prompt_passphrase("Enter keystore passphrase: ");
    let key = load_keystore(&keystore_path, &pass).map_err(|e| format!("{e}"))?;
    let addr = address_from_key(&key);
    println!("0x{}", hex::encode(addr));
    Ok(())
}

async fn cmd_balance(rpc: &RpcClient, address: &str, json_output: bool) -> Result<(), String> {
    let evm_balance = rpc.get_balance(address).await?;
    let native_balances = rpc.get_balances(address).await.ok();

    if json_output {
        let mut out = serde_json::json!({
            "address": address,
            "evm_balance_wei": format!("0x{:x}", evm_balance),
            "evm_balance_trs": format_trs(evm_balance),
        });
        if let Some(nb) = native_balances {
            out["native_balances"] = nb;
        }
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        println!("Address: {address}");
        println!("EVM Balance: {}", format_trs(evm_balance));
        if let Some(nb) = native_balances {
            if let Some(spot) = nb.get("spot") {
                if let Some(s) = spot.as_str() {
                    if let Ok(v) = rpc::parse_hex_u256(s) {
                        println!("Spot Balance: {}", format_trs(v));
                    }
                }
            }
            if let Some(perp) = nb.get("perp") {
                if let Some(s) = perp.as_str() {
                    if let Ok(v) = rpc::parse_hex_u256(s) {
                        println!("Perp Balance: {}", format_trs(v));
                    }
                }
            }
        }
    }
    Ok(())
}

async fn cmd_validators(rpc: &RpcClient, json_output: bool) -> Result<(), String> {
    let vals = rpc.get_validators().await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&vals).unwrap());
        return Ok(());
    }

    if let Some(arr) = vals.as_array() {
        println!("{:<4} {:<44} {:>15} {:>10}", "#", "Address", "Stake", "Commission");
        println!("{}", "-".repeat(80));
        for (i, v) in arr.iter().enumerate() {
            let addr = v.get("address").and_then(|a| a.as_str()).unwrap_or("?");
            let power = v.get("power").and_then(|p| p.as_u64()).unwrap_or(0);
            let commission = v.get("commission_bps").and_then(|c| c.as_u64()).unwrap_or(0);
            println!(
                "{:<4} {:<44} {:>15} {:>8}.{:02}%",
                i + 1,
                addr,
                power,
                commission / 100,
                commission % 100,
            );
        }
    }
    Ok(())
}

async fn cmd_staking(rpc: &RpcClient, address: &str, json_output: bool) -> Result<(), String> {
    let info = rpc.get_staking_info(address).await?;
    let delegations = rpc.get_delegations(address).await.ok();

    if json_output {
        let mut out = serde_json::json!({"staking_info": info});
        if let Some(d) = delegations {
            out["delegations"] = d;
        }
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return Ok(());
    }

    println!("Staking Info for {address}:");
    println!("{}", serde_json::to_string_pretty(&info).unwrap());
    if let Some(d) = delegations {
        println!("\nDelegations:");
        println!("{}", serde_json::to_string_pretty(&d).unwrap());
    }
    Ok(())
}

async fn cmd_block(rpc: &RpcClient, height: Option<String>, json_output: bool) -> Result<(), String> {
    let block_num = match height {
        Some(h) => format!("0x{:x}", h.parse::<u64>().map_err(|e| format!("invalid height: {e}"))?),
        None => "latest".to_string(),
    };
    let block = rpc.get_block_by_number(&block_num, false).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&block).unwrap());
        return Ok(());
    }

    if block.is_null() {
        println!("Block not found");
        return Ok(());
    }
    let height = block.get("number").and_then(|n| n.as_str()).unwrap_or("?");
    let hash = block.get("hash").and_then(|h| h.as_str()).unwrap_or("?");
    let timestamp = block.get("timestamp").and_then(|t| t.as_str()).unwrap_or("?");
    let tx_count = block.get("transactions").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0);
    let miner = block.get("miner").and_then(|m| m.as_str()).unwrap_or("?");

    println!("Block {height}");
    println!("  Hash:      {hash}");
    println!("  Timestamp: {timestamp}");
    println!("  Proposer:  {miner}");
    println!("  Txs:       {tx_count}");
    Ok(())
}

async fn cmd_tx(rpc: &RpcClient, hash: &str, json_output: bool) -> Result<(), String> {
    let tx = rpc.get_transaction_by_hash(hash).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&tx).unwrap());
        return Ok(());
    }

    if tx.is_null() {
        println!("Transaction not found");
        return Ok(());
    }
    let from = tx.get("from").and_then(|f| f.as_str()).unwrap_or("?");
    let to = tx.get("to").and_then(|t| t.as_str()).unwrap_or("(contract creation)");
    let value = tx.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
    let block = tx.get("blockNumber").and_then(|b| b.as_str()).unwrap_or("pending");

    println!("Transaction {hash}");
    println!("  From:    {from}");
    println!("  To:      {to}");
    if let Ok(v) = rpc::parse_hex_u256(value) {
        println!("  Value:   {}", format_trs(v));
    }
    println!("  Block:   {block}");
    Ok(())
}

async fn cmd_send(cli: &Cli, rpc: &RpcClient, to: &str, value: &str) -> Result<(), String> {
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
        println!("{}", serde_json::json!({
            "tx_hash": tx_hash,
            "from": from_hex,
            "to": to,
            "value_trs": value,
        }));
    } else {
        println!("Transaction sent!");
        println!("  Hash: {tx_hash}");
        println!("  From: {from_hex}");
        println!("  To:   {to}");
        println!("  Value: {value} TRS");
    }
    Ok(())
}

async fn submit_native_action(cli: &Cli, rpc: &RpcClient, action: NativeAction) -> Result<(), String> {
    let key = load_signing_key(cli)?;
    let nonce = now_ms();
    let signed = sign_native_action(action, nonce, &key);
    let json = serde_json::to_string(&signed).map_err(|e| format!("serialize: {e}"))?;
    let result = rpc.submit_native_action(&json).await?;

    if cli.json {
        println!("{}", serde_json::json!({"result": result}));
    } else {
        println!("Native action submitted: {result}");
    }
    Ok(())
}

async fn cmd_delegate(cli: &Cli, rpc: &RpcClient, validator: &str, amount: &str) -> Result<(), String> {
    let validator_addr = parse_address(validator)?;
    let amount_wei = parse_trs_to_wei(amount)?;
    let action = NativeAction::Delegate {
        validator: validator_addr,
        amount: amount_wei,
    };
    submit_native_action(cli, rpc, action).await
}

async fn cmd_undelegate(cli: &Cli, rpc: &RpcClient, validator: &str, amount: &str) -> Result<(), String> {
    let validator_addr = parse_address(validator)?;
    let amount_wei = parse_trs_to_wei(amount)?;
    let action = NativeAction::Undelegate {
        validator: validator_addr,
        amount: amount_wei,
    };
    submit_native_action(cli, rpc, action).await
}

async fn cmd_claim_rewards(cli: &Cli, rpc: &RpcClient) -> Result<(), String> {
    submit_native_action(cli, rpc, NativeAction::ClaimRewards).await
}

async fn cmd_orderbook(rpc: &RpcClient, market_id: &str, json_output: bool) -> Result<(), String> {
    let ob = rpc.get_order_book(market_id).await?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&ob).unwrap());
    } else {
        println!("Order Book (market {market_id}):");
        println!("{}", serde_json::to_string_pretty(&ob).unwrap());
    }
    Ok(())
}

async fn cmd_position(rpc: &RpcClient, address: &str, market_id: &str, json_output: bool) -> Result<(), String> {
    let pos = rpc.get_position(address, market_id).await?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&pos).unwrap());
    } else {
        if pos.is_null() {
            println!("No position found");
        } else {
            println!("Position ({address} in market {market_id}):");
            println!("{}", serde_json::to_string_pretty(&pos).unwrap());
        }
    }
    Ok(())
}

async fn cmd_proposals(rpc: &RpcClient, json_output: bool) -> Result<(), String> {
    let props = rpc.get_proposals().await?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&props).unwrap());
    } else {
        println!("Governance Proposals:");
        println!("{}", serde_json::to_string_pretty(&props).unwrap());
    }
    Ok(())
}

async fn cmd_vote(cli: &Cli, rpc: &RpcClient, proposal_id: u64, option: &str) -> Result<(), String> {
    let vote_option = match option.to_lowercase().as_str() {
        "yes" => torus_types::VoteOption::Yes,
        "no" => torus_types::VoteOption::No,
        "abstain" => torus_types::VoteOption::Abstain,
        _ => return Err("invalid vote option, use: yes, no, abstain".into()),
    };
    let action = NativeAction::Vote {
        proposal_id,
        option: vote_option,
    };
    submit_native_action(cli, rpc, action).await
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let rpc = RpcClient::new(&cli.rpc_url);

    let result = match &cli.command {
        Command::Keygen { output } => cmd_keygen(output.clone()).await,
        Command::Import { key, output } => cmd_import(key.clone(), output.clone()).await,
        Command::Address { keystore } => cmd_address(keystore.clone()).await,
        Command::Balance { address } => cmd_balance(&rpc, address, cli.json).await,
        Command::Validators => cmd_validators(&rpc, cli.json).await,
        Command::Staking { address } => cmd_staking(&rpc, address, cli.json).await,
        Command::Block { height } => cmd_block(&rpc, height.clone(), cli.json).await,
        Command::Tx { hash } => cmd_tx(&rpc, hash, cli.json).await,
        Command::Send { to, value } => cmd_send(&cli, &rpc, to, value).await,
        Command::Delegate { validator, amount } => cmd_delegate(&cli, &rpc, validator, amount).await,
        Command::Undelegate { validator, amount } => cmd_undelegate(&cli, &rpc, validator, amount).await,
        Command::ClaimRewards => cmd_claim_rewards(&cli, &rpc).await,
        Command::Orderbook { market_id } => cmd_orderbook(&rpc, market_id, cli.json).await,
        Command::Position { address, market_id } => cmd_position(&rpc, address, market_id, cli.json).await,
        Command::Proposals => cmd_proposals(&rpc, cli.json).await,
        Command::Vote { proposal, option } => cmd_vote(&cli, &rpc, *proposal, option).await,
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_trs_to_wei() {
        assert_eq!(parse_trs_to_wei("1").unwrap(), U256::from(1_000_000_000_000_000_000u64));
        assert_eq!(parse_trs_to_wei("0.5").unwrap(), U256::from(500_000_000_000_000_000u64));
        assert_eq!(parse_trs_to_wei("10").unwrap(), U256::from(10_000_000_000_000_000_000u128));
        assert_eq!(parse_trs_to_wei("1.5").unwrap(), U256::from(1_500_000_000_000_000_000u64));
        assert_eq!(parse_trs_to_wei("0.000001").unwrap(), U256::from(1_000_000_000_000u64));
    }

    #[test]
    fn test_parse_address() {
        let addr = parse_address("0x0000000000000000000000000000000000000001").unwrap();
        assert_eq!(addr, Address::from_slice(&{
            let mut b = [0u8; 20];
            b[19] = 1;
            b
        }));
        assert!(parse_address("invalid").is_err());
        assert!(parse_address("0x123").is_err());
    }

    #[test]
    fn test_build_eip1559_tx() {
        let key = SigningKey::from_slice(&{
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        }).unwrap();
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
        }).unwrap();

        let action = NativeAction::ClaimRewards;
        let nonce = 1_700_000_000_000u64;
        let signed = sign_native_action(action, nonce, &key);

        // Verify the signature recovers the correct address
        let recovered = signed.recover_sender().expect("recovery should succeed");
        let expected = address_from_key(&key);
        assert_eq!(recovered, expected);
    }

    #[test]
    fn test_delegate_action_signing() {
        let key = SigningKey::from_slice(&{
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        }).unwrap();

        let validator = Address::from_slice(&[0xCC; 20]);
        let action = NativeAction::Delegate {
            validator,
            amount: U256::from(1_000_000_000_000_000_000u64),
        };
        let nonce = 1_700_000_000_000u64;
        let signed = sign_native_action(action, nonce, &key);

        let recovered = signed.recover_sender().expect("recovery should succeed");
        assert_eq!(recovered, address_from_key(&key));
    }

    #[test]
    fn test_key_flag_warning() {
        // Just verify the --key parsing logic works
        let hex = "0000000000000000000000000000000000000000000000000000000000000001";
        let bytes = hex::decode(hex).unwrap();
        let key = SigningKey::from_slice(&bytes).unwrap();
        assert_ne!(address_from_key(&key), Address::ZERO);
    }
}
