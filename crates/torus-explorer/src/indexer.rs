use jsonrpsee::rpc_params;
use serde_json::Value;
use tracing::{info, warn};

use crate::db::*;
use crate::rpc_client::*;

pub struct Indexer {
    pub rpc: NodeRpcClient,
    pub db: ExplorerDb,
    pub batch_size: u32,
}

impl Indexer {
    pub fn new(rpc: NodeRpcClient, db: ExplorerDb, batch_size: u32) -> Self {
        Self { rpc, db, batch_size }
    }

    /// Backfill from last indexed height to current chain tip.
    pub async fn backfill(&self) -> Result<(), RpcError> {
        let chain_height = self.rpc.get_block_number().await?;
        let last_indexed = self
            .db
            .get_last_indexed_height()
            .map_err(|e| -> RpcError { e.to_string().into() })?;
        let start = match last_indexed {
            Some(h) => (h + 1) as u64,
            None => 0,
        };

        if start > chain_height {
            info!("Already up to date at block {chain_height}");
            return Ok(());
        }

        info!("Backfilling blocks {start}..={chain_height}");
        for h in start..=chain_height {
            if let Err(e) = self.index_block(h).await {
                warn!("Failed to index block {h}: {e}");
            }
            if h % 100 == 0 || h == chain_height {
                info!("Indexed up to block {h}/{chain_height}");
            }
        }
        Ok(())
    }

    /// Index a single block. Handles reorgs by checking hash.
    pub async fn index_block(&self, height: u64) -> Result<(), RpcError> {
        let h = height as i64;

        let block = match self.rpc.get_block(height).await? {
            Some(b) => b,
            None => return Ok(()),
        };

        let new_hash = val_str(&block, "hash");

        // Reorg check
        if let Ok(Some(existing)) = self.db.get_block(h) {
            if existing.hash == new_hash {
                return Ok(());
            }
            warn!(
                "Reorg at height {height}: {} -> {}",
                existing.hash, new_hash
            );
            self.db
                .delete_block_data(h)
                .map_err(|e| -> RpcError { e.to_string().into() })?;
        }

        // Parse and store block
        let block_row = parse_block_row(&block, h);
        self.db
            .insert_block(&block_row)
            .map_err(|e| -> RpcError { e.to_string().into() })?;

        // Index EVM transactions + receipts
        if let Some(txs) = block.get("transactions").and_then(|v| v.as_array()) {
            for (i, tx) in txs.iter().enumerate() {
                let tx_hash = val_str(tx, "hash");
                let receipt = self.rpc.get_receipt(&tx_hash).await?;
                let tx_row = parse_tx_row(tx, h, i as i32, receipt.as_ref());
                self.db
                    .insert_transaction(&tx_row)
                    .map_err(|e| -> RpcError { e.to_string().into() })?;

                if let Some(receipt) = &receipt {
                    if let Some(logs) = receipt.get("logs").and_then(|v| v.as_array()) {
                        for log in logs {
                            let log_row = parse_log_row(log, h);
                            self.db
                                .insert_log(&log_row)
                                .map_err(|e| -> RpcError { e.to_string().into() })?;
                        }
                    }
                }
            }
        }

        // Index native actions
        if let Ok(Some(body)) = self.rpc.get_block_body(height).await {
            if let Some(actions) = body.get("nativeActions").and_then(|v| v.as_array()) {
                for (i, action) in actions.iter().enumerate() {
                    let action_row = parse_native_action_row(action, h, i as i32);
                    self.db
                        .insert_native_action(&action_row)
                        .map_err(|e| -> RpcError { e.to_string().into() })?;
                }
            }
        }

        // Snapshot validators every 100 blocks
        if height % 100 == 0 {
            if let Err(e) = self.snapshot_validators(h).await {
                warn!("Failed to snapshot validators at height {height}: {e}");
            }
        }

        self.db
            .set_indexer_state("last_indexed_height", &height.to_string())
            .map_err(|e| -> RpcError { e.to_string().into() })?;

        Ok(())
    }

    async fn snapshot_validators(&self, height: i64) -> Result<(), RpcError> {
        let validators = self.rpc.get_validators().await?;
        for v in &validators {
            let snap = ValidatorSnapshotRow {
                block_height: height,
                address: val_str(v, "address"),
                pubkey: val_str(v, "pubkey"),
                power: val_hex_i64(v, "power"),
                commission_bps: v
                    .get("commissionBps")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32,
                status: val_str(v, "status"),
            };
            self.db
                .insert_validator_snapshot(&snap)
                .map_err(|e| -> RpcError { e.to_string().into() })?;
        }
        Ok(())
    }

    /// Subscribe to new heads via WebSocket for real-time indexing.
    pub async fn subscribe_new_heads(&self, ws_url: &str) -> Result<(), RpcError> {
        use jsonrpsee::core::client::SubscriptionClientT;
        use jsonrpsee::ws_client::WsClientBuilder;

        let ws = WsClientBuilder::default().build(ws_url).await?;
        let mut sub: jsonrpsee::core::client::Subscription<Value> = ws
            .subscribe(
                "eth_subscribe",
                rpc_params!["newHeads"],
                "eth_unsubscribe",
            )
            .await?;

        info!("Subscribed to newHeads on {ws_url}");

        while let Some(head) = sub.next().await {
            match head {
                Ok(head) => {
                    let height = val_hex_u64(&head, "number");
                    if let Err(e) = self.index_block(height).await {
                        warn!("Failed to index new block {height}: {e}");
                    }
                }
                Err(e) => {
                    warn!("Subscription error: {e}");
                    break;
                }
            }
        }
        Ok(())
    }
}

// ============================================================================
// Parsing helpers
// ============================================================================

fn parse_block_row(block: &Value, height: i64) -> BlockRow {
    let tx_count = block
        .get("transactions")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0) as i32;

    BlockRow {
        height,
        hash: val_str(block, "hash"),
        parent_hash: val_str(block, "parentHash"),
        timestamp: val_hex_i64(block, "timestamp"),
        proposer: val_str(block, "miner"),
        gas_used: val_hex_i64(block, "gasUsed"),
        gas_limit: val_hex_i64(block, "gasLimit"),
        base_fee: block
            .get("baseFeePerGas")
            .and_then(|v| v.as_str())
            .map(parse_hex_i64)
            .unwrap_or(0),
        tx_count,
        native_action_count: 0,
        epoch: 0,
        validator_set_hash: String::new(),
        state_root: val_str(block, "stateRoot"),
    }
}

fn parse_tx_row(tx: &Value, block_height: i64, tx_index: i32, receipt: Option<&Value>) -> TxRow {
    let (gas_used, status) = match receipt {
        Some(r) => (val_hex_i64(r, "gasUsed"), val_hex_u64(r, "status") == 1),
        None => (0, false),
    };
    let contract_address = receipt
        .and_then(|r| r.get("contractAddress"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "null")
        .map(|s| s.to_string());

    TxRow {
        hash: val_str(tx, "hash"),
        block_height,
        tx_index,
        from_addr: val_str(tx, "from"),
        to_addr: val_str_opt(tx, "to"),
        value: val_str(tx, "value"),
        gas_limit: val_hex_i64(tx, "gas"),
        gas_used,
        gas_price: val_str(tx, "gasPrice"),
        input_data: val_str(tx, "input"),
        nonce: val_hex_i64(tx, "nonce"),
        status,
        contract_address,
        tx_type: tx
            .get("type")
            .and_then(|v| v.as_str())
            .map(parse_hex_i64)
            .unwrap_or(0) as i32,
    }
}

fn parse_log_row(log: &Value, block_height: i64) -> LogRow {
    let topics = log
        .get("topics")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let topic =
        |i: usize| -> Option<String> { topics.get(i).and_then(|v| v.as_str()).map(String::from) };

    LogRow {
        block_height,
        tx_hash: val_str(log, "transactionHash"),
        log_index: val_hex_i64(log, "logIndex") as i32,
        address: val_str(log, "address"),
        topic0: topic(0),
        topic1: topic(1),
        topic2: topic(2),
        topic3: topic(3),
        data: val_str(log, "data"),
    }
}

pub fn parse_native_action_row(
    action: &Value,
    block_height: i64,
    action_index: i32,
) -> NativeActionRow {
    let (action_type, inner) = if let Some(obj) = action.as_object() {
        if let Some((key, val)) = obj.iter().next() {
            (key.clone(), Some(val.clone()))
        } else {
            ("Unknown".to_string(), None)
        }
    } else if let Some(s) = action.as_str() {
        (s.to_string(), None)
    } else {
        ("Unknown".to_string(), None)
    };

    let inner_ref = inner.as_ref();

    NativeActionRow {
        block_height,
        action_index,
        action_type: action_type.clone(),
        market_id: extract_i64(&action_type, inner_ref, "market_id", &["PlaceOrder", "CancelAllOrders", "UpdateMarketParams", "DelistMarket"]),
        order_id: extract_val_str(&action_type, inner_ref, "order_id", &["CancelOrder", "ModifyOrder"]),
        validator: extract_str(&action_type, inner_ref, "validator", &["Delegate", "Undelegate"]),
        target: extract_str(&action_type, inner_ref, "target", &["JailVote"]),
        amount: extract_val_str(&action_type, inner_ref, "amount", &["Delegate", "Undelegate", "PermanentStake", "Withdraw", "TransferToPerp", "TransferToSpot"]),
        proposal_id: extract_i64(&action_type, inner_ref, "proposal_id", &["Vote"]),
        payload: serde_json::to_string(action).unwrap_or_default(),
    }
}

fn extract_i64(at: &str, inner: Option<&Value>, field: &str, types: &[&str]) -> Option<i64> {
    if !types.contains(&at) { return None; }
    inner?.get(field).and_then(|v| v.as_i64())
}

fn extract_str(at: &str, inner: Option<&Value>, field: &str, types: &[&str]) -> Option<String> {
    if !types.contains(&at) { return None; }
    inner?.get(field).and_then(|v| v.as_str()).map(String::from)
}

fn extract_val_str(at: &str, inner: Option<&Value>, field: &str, types: &[&str]) -> Option<String> {
    if !types.contains(&at) { return None; }
    inner?.get(field).map(|v| v.to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_block_from_rpc_json() {
        let block = json!({
            "number": "0xa", "hash": "0xabcdef", "parentHash": "0x000000",
            "timestamp": "0x65a3b800", "miner": "0xaaaa",
            "gasUsed": "0x5208", "gasLimit": "0x1c9c380",
            "baseFeePerGas": "0x3b9aca00", "stateRoot": "0x1111",
            "transactions": []
        });
        let row = parse_block_row(&block, 10);
        assert_eq!(row.height, 10);
        assert_eq!(row.hash, "0xabcdef");
        assert_eq!(row.gas_used, 21000);
        assert_eq!(row.base_fee, 1_000_000_000);
    }

    #[test]
    fn parse_tx_from_rpc_json() {
        let tx = json!({
            "hash": "0xtxhash", "from": "0xsender", "to": "0xreceiver",
            "value": "0x100", "gas": "0x5208", "gasPrice": "0x3b9aca00",
            "input": "0x", "nonce": "0x5", "type": "0x2"
        });
        let receipt = json!({
            "gasUsed": "0x5208", "status": "0x1", "contractAddress": null, "logs": []
        });
        let row = parse_tx_row(&tx, 10, 0, Some(&receipt));
        assert!(row.status);
        assert_eq!(row.gas_used, 21000);
        assert_eq!(row.nonce, 5);
    }

    #[test]
    fn parse_native_action_variants() {
        let delegate = json!({"Delegate": {"validator": "0xval", "amount": "0x1000"}});
        let r = parse_native_action_row(&delegate, 5, 0);
        assert_eq!(r.action_type, "Delegate");
        assert_eq!(r.validator, Some("0xval".to_string()));

        let order = json!({"PlaceOrder": {"market_id": 1, "is_buy": true}});
        let r = parse_native_action_row(&order, 5, 1);
        assert_eq!(r.market_id, Some(1));

        let claim = json!("ClaimRewards");
        let r = parse_native_action_row(&claim, 5, 2);
        assert_eq!(r.action_type, "ClaimRewards");
    }

    #[test]
    fn indexer_reorg_flow() {
        let db = ExplorerDb::open_in_memory().unwrap();
        let block = BlockRow {
            height: 10, hash: "0xAAAA".into(), parent_hash: "0x0009".into(),
            timestamp: 1000, proposer: "0xprop".into(), gas_used: 0,
            gas_limit: 30_000_000, base_fee: 0, tx_count: 1,
            native_action_count: 0, epoch: 0,
            validator_set_hash: String::new(), state_root: String::new(),
        };
        db.insert_block(&block).unwrap();
        db.insert_transaction(&TxRow {
            hash: "0xtx_old".into(), block_height: 10, tx_index: 0,
            from_addr: "0xfrom".into(), to_addr: None, value: "0x0".into(),
            gas_limit: 21000, gas_used: 21000, gas_price: "0x0".into(),
            input_data: "0x".into(), nonce: 0, status: true,
            contract_address: None, tx_type: 0,
        }).unwrap();
        assert!(db.get_block(10).unwrap().is_some());
        db.delete_block_data(10).unwrap();
        assert!(db.get_block(10).unwrap().is_none());
        assert!(db.get_transaction("0xtx_old").unwrap().is_none());
    }
}
