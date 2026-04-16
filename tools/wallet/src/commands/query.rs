//! Read-only query commands: balance, validators, staking, block, tx, orderbook,
//! position, proposals.

use crate::rpc::{self, format_trs, RpcClient};

pub(crate) async fn cmd_balance(rpc: &RpcClient, address: &str, json_output: bool) -> Result<(), String> {
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

pub(crate) async fn cmd_validators(rpc: &RpcClient, json_output: bool) -> Result<(), String> {
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

pub(crate) async fn cmd_staking(rpc: &RpcClient, address: &str, json_output: bool) -> Result<(), String> {
    let info = rpc.get_staking_info(address).await?;
    let delegations = rpc.get_delegations(address).await.ok();

    if json_output {
        let mut out = serde_json::json!({ "staking_info": info });
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

pub(crate) async fn cmd_block(
    rpc: &RpcClient,
    height: Option<String>,
    json_output: bool,
) -> Result<(), String> {
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
    let tx_count = block
        .get("transactions")
        .and_then(|t| t.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let miner = block.get("miner").and_then(|m| m.as_str()).unwrap_or("?");

    println!("Block {height}");
    println!("  Hash:      {hash}");
    println!("  Timestamp: {timestamp}");
    println!("  Proposer:  {miner}");
    println!("  Txs:       {tx_count}");
    Ok(())
}

pub(crate) async fn cmd_tx(rpc: &RpcClient, hash: &str, json_output: bool) -> Result<(), String> {
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
    let to = tx
        .get("to")
        .and_then(|t| t.as_str())
        .unwrap_or("(contract creation)");
    let value = tx.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
    let block = tx
        .get("blockNumber")
        .and_then(|b| b.as_str())
        .unwrap_or("pending");

    println!("Transaction {hash}");
    println!("  From:    {from}");
    println!("  To:      {to}");
    if let Ok(v) = rpc::parse_hex_u256(value) {
        println!("  Value:   {}", format_trs(v));
    }
    println!("  Block:   {block}");
    Ok(())
}

pub(crate) async fn cmd_orderbook(
    rpc: &RpcClient,
    market_id: &str,
    json_output: bool,
) -> Result<(), String> {
    let ob = rpc.get_order_book(market_id).await?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&ob).unwrap());
    } else {
        println!("Order Book (market {market_id}):");
        println!("{}", serde_json::to_string_pretty(&ob).unwrap());
    }
    Ok(())
}

pub(crate) async fn cmd_position(
    rpc: &RpcClient,
    address: &str,
    market_id: &str,
    json_output: bool,
) -> Result<(), String> {
    let pos = rpc.get_position(address, market_id).await?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&pos).unwrap());
    } else if pos.is_null() {
        println!("No position found");
    } else {
        println!("Position ({address} in market {market_id}):");
        println!("{}", serde_json::to_string_pretty(&pos).unwrap());
    }
    Ok(())
}

pub(crate) async fn cmd_proposals(
    rpc: &RpcClient,
    id: Option<u64>,
    json_output: bool,
) -> Result<(), String> {
    let result = match id {
        Some(n) => rpc.get_proposal(n).await?,
        None => rpc.get_proposals().await?,
    };
    if json_output {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    } else {
        match id {
            Some(n) => println!("Proposal {n}:"),
            None => println!("Governance Proposals:"),
        }
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    }
    Ok(())
}

pub(crate) async fn cmd_orders(
    rpc: &RpcClient,
    address: &str,
    market_id: Option<u64>,
    json_output: bool,
) -> Result<(), String> {
    let orders = rpc.get_open_orders(address, market_id).await?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&orders).unwrap());
    } else {
        println!("Open orders for {address}:");
        println!("{}", serde_json::to_string_pretty(&orders).unwrap());
    }
    Ok(())
}

pub(crate) async fn cmd_positions(
    rpc: &RpcClient,
    address: &str,
    json_output: bool,
) -> Result<(), String> {
    let markets = rpc.get_markets(None, None).await?;
    let mut positions: Vec<serde_json::Value> = Vec::new();
    if let Some(arr) = markets.as_array() {
        for m in arr {
            let mid = m
                .get("market_id")
                .and_then(|v| v.as_u64())
                .or_else(|| m.get("id").and_then(|v| v.as_u64()));
            let mid = match mid {
                Some(id) => id,
                None => continue,
            };
            let pos = rpc.get_position(address, &mid.to_string()).await.ok();
            if let Some(p) = pos {
                if !p.is_null() {
                    positions.push(serde_json::json!({"market_id": mid, "position": p}));
                }
            }
        }
    }
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "address": address,
                "positions": positions,
            }))
            .unwrap()
        );
    } else {
        println!("Positions for {address}:");
        if positions.is_empty() {
            println!("  (none)");
        } else {
            for entry in &positions {
                println!("{}", serde_json::to_string_pretty(entry).unwrap());
            }
        }
    }
    Ok(())
}

pub(crate) async fn cmd_delegations(
    rpc: &RpcClient,
    address: &str,
    json_output: bool,
) -> Result<(), String> {
    let d = rpc.get_delegations(address).await?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&d).unwrap());
    } else {
        println!("Delegations for {address}:");
        println!("{}", serde_json::to_string_pretty(&d).unwrap());
    }
    Ok(())
}

pub(crate) async fn cmd_epoch(rpc: &RpcClient, json_output: bool) -> Result<(), String> {
    let e = rpc.get_epoch().await?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&e).unwrap());
    } else {
        let epoch = e.get("epoch").and_then(|v| v.as_u64()).unwrap_or(0);
        let start = e.get("start_block").and_then(|v| v.as_u64()).unwrap_or(0);
        let end = e.get("end_block").and_then(|v| v.as_u64()).unwrap_or(0);
        let remaining = e
            .get("blocks_remaining")
            .and_then(|v| v.as_u64())
            .unwrap_or(end.saturating_sub(start));
        println!("Epoch {epoch}  blocks {start}-{end}  {remaining} remaining");
    }
    Ok(())
}
