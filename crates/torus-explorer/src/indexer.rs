use std::collections::HashMap;

use jsonrpsee::rpc_params;
use serde_json::Value;
use torus_types::FixedPoint;
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
        Self {
            rpc,
            db,
            batch_size,
        }
    }

    /// Backfill from last indexed height to current chain tip.
    pub async fn backfill(&self) -> Result<(), RpcError> {
        let (start, chain_height) = self.pending_range().await?;

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

    /// The next height to index and the node's head. The head is
    /// `eth_blockNumber`: the node's EXECUTED head (s84), so every block up to
    /// it is final, with its executed/skipped status known.
    async fn pending_range(&self) -> Result<(u64, u64), RpcError> {
        let head = self.rpc.get_block_number().await?;
        let last_indexed = self
            .db
            .get_last_indexed_height()
            .map_err(|e| -> RpcError { e.to_string().into() })?;
        Ok((last_indexed.map_or(0, |h| (h + 1) as u64), head))
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

        // s84: the body lists every EVM tx, executed or skipped, with its
        // status; the eth block lists only the executed ones. Rows use the
        // body position as `tx_index` (first occurrence of a hash).
        let body = self.rpc.get_block_body(height).await.ok().flatten();
        let mut body_position: HashMap<String, i32> = HashMap::new();
        if let Some(txs) = body
            .as_ref()
            .and_then(|b| b.get("evmTransactions")?.as_array())
        {
            for (i, tx) in txs.iter().enumerate() {
                body_position.entry(val_str(tx, "hash")).or_insert(i as i32);
            }
        }

        // Index EVM transactions + receipts
        if let Some(txs) = block.get("transactions").and_then(|v| v.as_array()) {
            for (i, tx) in txs.iter().enumerate() {
                let tx_hash = val_str(tx, "hash");
                let receipt = self.rpc.get_receipt(&tx_hash).await?;
                let tx_index = body_position.get(&tx_hash).copied().unwrap_or(i as i32);
                let tx_row = parse_tx_row(tx, h, tx_index, receipt.as_ref());
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
        if let Some(body) = &body {
            self.record_skipped_evm_txs(h, body)
                .map_err(|e| -> RpcError { e.to_string().into() })?;
            let statuses = native_action_status(body);
            if let Some(actions) = body.get("nativeActions").and_then(|v| v.as_array()) {
                for (i, action) in actions.iter().enumerate() {
                    let mut action_row = parse_native_action_row(action, h, i as i32);
                    action_row.status = statuses.as_ref().and_then(|s| s.get(i).cloned());
                    self.db
                        .insert_native_action(&action_row)
                        .map_err(|e| -> RpcError { e.to_string().into() })?;
                }
            }
        }
        self.fill_unknown_status(h).await;

        // Index trades into OHLCV candles (Phase 7B)
        if let Ok(trades) = self.rpc.get_block_trades(height).await {
            index_trades(&self.db, height, &trades);
        }

        // Snapshot validators every 100 blocks
        if height.is_multiple_of(100) {
            if let Err(e) = self.snapshot_validators(h).await {
                warn!("Failed to snapshot validators at height {height}: {e}");
            }
        }

        self.db
            .set_indexer_state("last_indexed_height", &height.to_string())
            .map_err(|e| -> RpcError { e.to_string().into() })?;

        Ok(())
    }

    /// s84: a block is indexed at commit, usually before the node executed it,
    /// so its native actions' executed/skipped status is still unknown. Fill it
    /// in for the recent blocks (execution trails commit by a few blocks) once
    /// the node reports it. Best-effort: a failure leaves the status unknown.
    async fn fill_unknown_status(&self, height: i64) {
        const STATUS_WINDOW: i64 = 64;
        let Ok(heights) = self.db.heights_with_unknown_status(height - STATUS_WINDOW) else {
            return;
        };
        for h in heights {
            let Ok(Some(body)) = self.rpc.get_block_body(h as u64).await else {
                continue;
            };
            if let Some(statuses) = native_action_status(&body) {
                if let Err(e) = self.db.set_native_action_status(h, &statuses) {
                    warn!("Failed to store native action status at height {h}: {e}");
                }
            }
            if let Err(e) = self.record_skipped_evm_txs(h, &body) {
                warn!("Failed to store skipped EVM txs at height {h}: {e}");
            }
        }
    }

    /// s84: store a block's skipped EVM txs (listed by `torus_getBlockBody`,
    /// not by the eth block) as [`TxStatus::Skipped`], `tx_index` = body
    /// position. Nothing while the status is unknown (`null`). Never replaces
    /// an existing row of the same hash ([`ExplorerDb::insert_transaction`]).
    fn record_skipped_evm_txs(&self, height: i64, body: &Value) -> Result<(), rusqlite::Error> {
        let (Some(txs), Some(status)) = (
            body.get("evmTransactions").and_then(Value::as_array),
            body.get("evmTransactionStatus").and_then(Value::as_array),
        ) else {
            return Ok(());
        };
        for (i, (tx, status)) in txs.iter().zip(status).enumerate() {
            if status.as_str() == Some("skipped") {
                let mut row = parse_tx_row(tx, height, i as i32, None);
                row.status = TxStatus::Skipped;
                self.db.insert_transaction(&row)?;
            }
        }
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
                commission_bps: v.get("commissionBps").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
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
            .subscribe("eth_subscribe", rpc_params!["newHeads"], "eth_unsubscribe")
            .await?;

        info!("Subscribed to newHeads on {ws_url}");

        while let Some(head) = sub.next().await {
            match head {
                // s84: a head is announced at commit, but the node serves a
                // block's eth view only once it executed it. Index up to the
                // node's eth head instead of the announced height, so a block
                // not executed yet is picked up on a later head.
                Ok(_) => match self.pending_range().await {
                    Ok((start, head)) => {
                        for height in start..=head {
                            if let Err(e) = self.index_block(height).await {
                                warn!("Failed to index new block {height}: {e}");
                            }
                        }
                    }
                    Err(e) => warn!("Failed to read the node's head: {e}"),
                },
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
        Some(r) if val_hex_u64(r, "status") == 1 => (val_hex_i64(r, "gasUsed"), TxStatus::Success),
        Some(r) => (val_hex_i64(r, "gasUsed"), TxStatus::Failed),
        None => (0, TxStatus::Failed),
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

/// s84: `torus_getBlockBody` `nativeActionStatus`, `None` while unknown.
/// v2: a `"failed"` action's entry of `nativeActionFailures` is folded into
/// its stored status: `failed (<reason>): <message>`, plus the first failing
/// order and the failed-order count for a PlaceOrderBatch.
/// Row 50: a `"rejected"` entry likewise: `rejected (<HL reason>): <message>`
/// (a batch: `, order N, M not executed`); an entry without `status` (a
/// pre-row-50 node) is a failure. s100: the HL reason is `rejectStatus`
/// (`reason` is the lowercase name); a node before s100 sent it as `reason`.
fn native_action_status(body: &Value) -> Option<Vec<String>> {
    let mut statuses: Vec<String> = body
        .get("nativeActionStatus")?
        .as_array()?
        .iter()
        .map(|s| s.as_str().map(str::to_string))
        .collect::<Option<_>>()?;
    let failures = body.get("nativeActionFailures").and_then(Value::as_array);
    for f in failures.into_iter().flatten() {
        let Some(status) = f
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|i| statuses.get_mut(i as usize))
        else {
            continue;
        };
        let reason = val_str_opt(f, "rejectStatus").unwrap_or_else(|| val_str(f, "reason"));
        let rejected = f.get("status").and_then(Value::as_str) == Some("rejected");
        let (label, count) = if rejected {
            ("rejected", "not executed")
        } else {
            ("failed", "failed")
        };
        let batch = match (val_u64(f, "order"), val_u64(f, "failedOrders")) {
            (order, failed) if order > 0 || failed != 1 => {
                format!(", order {order}, {failed} {count}")
            }
            _ => String::new(),
        };
        *status = format!("{label} ({reason}{batch}): {}", val_str(f, "message"));
    }
    Some(statuses)
}

fn val_u64(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
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
        market_id: extract_i64(
            &action_type,
            inner_ref,
            "market_id",
            &[
                "PlaceOrder",
                "CancelAllOrders",
                "UpdateMarketParams",
                "DelistMarket",
            ],
        ),
        order_id: extract_val_str(
            &action_type,
            inner_ref,
            "order_id",
            &["CancelOrder", "ModifyOrder"],
        ),
        validator: extract_str(
            &action_type,
            inner_ref,
            "validator",
            &["Delegate", "Undelegate"],
        ),
        target: extract_str(&action_type, inner_ref, "target", &["JailVote"]),
        amount: extract_val_str(
            &action_type,
            inner_ref,
            "amount",
            &[
                "Delegate",
                "Undelegate",
                "PermanentStake",
                "Withdraw",
                "TransferToPerp",
                "TransferToSpot",
            ],
        ),
        proposal_id: extract_i64(&action_type, inner_ref, "proposal_id", &["Vote"]),
        payload: serde_json::to_string(action).unwrap_or_default(),
        status: None,
    }
}

/// Fold one block's `torus_getBlockTrades` rows into the OHLCV candles. A
/// trade whose price or quantity does not parse (e.g. an old node still
/// returning fixed-point hex) is skipped with a warning, never written as 0.
fn index_trades(db: &ExplorerDb, height: u64, trades: &[Value]) {
    for t in trades {
        let market_id = val_hex_i64(t, "marketId");
        let timestamp = val_hex_i64(t, "timestamp");
        let (price, qty) = (val_str(t, "price"), val_str(t, "quantity"));
        let parsed = (parse_dec_fp(&price), parse_dec_fp(&qty));
        let (Some(price_raw), Some(qty_raw)) = parsed else {
            warn!(
                "Skipping trade at height {height} (market {market_id}): \
                 unparsable price {price:?} / quantity {qty:?}"
            );
            continue;
        };
        let (price_raw, qty_raw) = (price_raw as i64, qty_raw as i64);
        if let Err(e) = db.upsert_candle(market_id, timestamp, price_raw, qty_raw) {
            warn!("Failed to upsert candle at height {height}: {e}");
        }
    }
}

/// Raw value of a `torus_*` decimal FixedPoint string (s80); None if unparsable.
fn parse_dec_fp(s: &str) -> Option<i128> {
    s.parse::<FixedPoint>().ok().map(|f| f.raw())
}

fn extract_i64(at: &str, inner: Option<&Value>, field: &str, types: &[&str]) -> Option<i64> {
    if !types.contains(&at) {
        return None;
    }
    inner?.get(field).and_then(|v| v.as_i64())
}

fn extract_str(at: &str, inner: Option<&Value>, field: &str, types: &[&str]) -> Option<String> {
    if !types.contains(&at) {
        return None;
    }
    inner?.get(field).and_then(|v| v.as_str()).map(String::from)
}

fn extract_val_str(at: &str, inner: Option<&Value>, field: &str, types: &[&str]) -> Option<String> {
    if !types.contains(&at) {
        return None;
    }
    inner?.get(field).map(|v| v.to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A node stub: `(method, first param, result)`; any other call is `null`.
    async fn mock_node(
        responses: Vec<(&'static str, Value, Value)>,
    ) -> (jsonrpsee::server::ServerHandle, String) {
        let server = jsonrpsee::server::Server::builder()
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let mut module = jsonrpsee::RpcModule::new(());
        let mut methods: Vec<&'static str> = responses.iter().map(|r| r.0).collect();
        methods.dedup();
        for method in methods {
            let table: Vec<(Value, Value)> = responses
                .iter()
                .filter(|r| r.0 == method)
                .map(|r| (r.1.clone(), r.2.clone()))
                .collect();
            module
                .register_method(method, move |params, _, _| {
                    let first = params
                        .parse::<Vec<Value>>()
                        .ok()
                        .and_then(|p| p.first().cloned());
                    table
                        .iter()
                        .find(|(key, _)| Some(key) == first.as_ref())
                        .map_or(Value::Null, |(_, result)| result.clone())
                })
                .unwrap();
        }
        (server.start(module), url)
    }

    /// s84 option (ii): `eth_getBlockByNumber` lists only the executed EVM
    /// txs; the skipped one comes from `torus_getBlockBody` and is stored as
    /// "skipped", not "failed". Every row's `tx_index` is its body position.
    /// A skipped replay of an already-executed tx never overwrites its row.
    #[tokio::test]
    async fn skipped_evm_tx_is_stored_as_skipped() {
        let tx = |hash: &str| {
            json!({"hash": hash, "from": "0xa1", "to": "0xb2", "value": "0x0", "gas": "0x5208",
                   "gasPrice": "0x3b9aca00", "input": "0x", "nonce": "0x0", "type": "0x2"})
        };
        let block = |n: &str, hash: &str, txs: Vec<Value>| {
            json!({"number": n, "hash": hash, "parentHash": "0x00", "timestamp": "0x1",
                   "miner": "0xm", "gasUsed": "0x0", "gasLimit": "0x1", "baseFeePerGas": "0x1",
                   "stateRoot": "0x0", "transactions": txs})
        };
        let body = |txs: Vec<Value>, status: Vec<&str>| {
            json!({"blockNumber": "0x0", "nativeActions": [], "nativeActionCount": 0,
                   "nativeActionStatus": [], "evmTransactions": txs, "evmTransactionStatus": status})
        };
        let receipt = |status: &str| json!({"gasUsed": "0x5208", "status": status, "contractAddress": null, "logs": []});
        let (ok, skip, rev) = ("0x0a", "0x0b", "0x0c");
        let (handle, url) = mock_node(vec![
            (
                "eth_getBlockByNumber",
                json!("0x1"),
                block("0x1", "0xb1", vec![tx(ok), tx(rev)]),
            ),
            (
                "eth_getBlockByNumber",
                json!("0x2"),
                block("0x2", "0xb2", vec![]),
            ),
            ("eth_getTransactionReceipt", json!(ok), receipt("0x1")),
            ("eth_getTransactionReceipt", json!(rev), receipt("0x0")),
            (
                "torus_getBlockBody",
                json!(1),
                body(
                    vec![tx(ok), tx(skip), tx(rev)],
                    vec!["executed", "skipped", "executed"],
                ),
            ),
            // Height 2 re-includes `ok` (a replay): skipped.
            (
                "torus_getBlockBody",
                json!(2),
                body(vec![tx(ok)], vec!["skipped"]),
            ),
        ])
        .await;
        let indexer = Indexer::new(
            NodeRpcClient::new(&url).unwrap(),
            ExplorerDb::open_in_memory().unwrap(),
            10,
        );
        indexer.index_block(1).await.unwrap();
        indexer.index_block(2).await.unwrap();

        let rows: Vec<Value> = indexer
            .db
            .get_block_transactions(1)
            .unwrap()
            .iter()
            .map(|r| {
                let v = serde_json::to_value(r).unwrap();
                json!([v["hash"], v["tx_index"], v["status"]])
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                json!([ok, 0, "success"]),
                json!([skip, 1, "skipped"]),
                json!([rev, 2, "failed"]),
            ]
        );
        assert_eq!(
            indexer.db.get_block(1).unwrap().unwrap().tx_count,
            2,
            "the block's tx count is the eth (executed) count"
        );
        assert!(
            indexer.db.get_block_transactions(2).unwrap().is_empty(),
            "a skipped replay does not move the executed tx's row"
        );
        handle.stop().unwrap();
    }

    /// v2: a failed native action's stored status carries its reason and
    /// message; executed / skipped stay as they are; a pre-v2 node (no
    /// `nativeActionFailures`) reads like before.
    #[test]
    fn native_action_status_shows_failure_reason() {
        let body = json!({
            "nativeActionStatus": ["executed", "failed", "skipped"],
            "nativeActionFailures": [{"index": 1, "reason": "margin",
                "message": "insufficient margin: need 5, have 1 (account)",
                "order": 0, "failedOrders": 1}]
        });
        assert_eq!(
            native_action_status(&body).unwrap(),
            vec![
                "executed".to_string(),
                "failed (margin): insufficient margin: need 5, have 1 (account)".to_string(),
                "skipped".to_string(),
            ]
        );
        let batch = json!({
            "nativeActionStatus": ["failed"],
            "nativeActionFailures": [{"index": 0, "reason": "tick", "message": "off tick",
                "order": 3, "failedOrders": 2}]
        });
        assert_eq!(
            native_action_status(&batch).unwrap(),
            vec!["failed (tick, order 3, 2 failed): off tick".to_string()]
        );
        let v1 = json!({"nativeActionStatus": ["executed", "skipped"]});
        assert_eq!(
            native_action_status(&v1).unwrap(),
            vec!["executed".to_string(), "skipped".to_string()]
        );
        // Row 50: a rejected action shows its status and HL reason. s100:
        // read from `rejectStatus` (`reason` is the lowercase name, kept for
        // a failed entry); a failed entry without one shows its `reason`.
        let rejected = json!({
            "nativeActionStatus": ["rejected", "rejected", "failed"],
            "nativeActionFailures": [
                {"index": 0, "status": "rejected", "reason": "ioc_cancel",
                    "rejectStatus": "iocCancelRejected",
                    "message": "order rejected: IOC", "order": 0, "failedOrders": 1},
                {"index": 1, "status": "rejected", "reason": "bad_alo_px",
                    "rejectStatus": "badAloPxRejected",
                    "message": "order rejected: ALO", "order": 2, "failedOrders": 3},
                {"index": 2, "status": "failed", "reason": "tick",
                    "message": "off tick", "order": 0, "failedOrders": 1}
            ]
        });
        let want = vec![
            "rejected (iocCancelRejected): order rejected: IOC".to_string(),
            "rejected (badAloPxRejected, order 2, 3 not executed): order rejected: ALO".to_string(),
            "failed (tick): off tick".to_string(),
        ];
        assert_eq!(native_action_status(&rejected).unwrap(), want);
        // A row 50 node before s100 sent the HL name as `reason` and no
        // `rejectStatus`: the same rendering.
        let old_rejected = json!({
            "nativeActionStatus": ["rejected", "rejected", "failed"],
            "nativeActionFailures": [
                {"index": 0, "status": "rejected", "reason": "iocCancelRejected",
                    "message": "order rejected: IOC", "order": 0, "failedOrders": 1},
                {"index": 1, "status": "rejected", "reason": "badAloPxRejected",
                    "message": "order rejected: ALO", "order": 2, "failedOrders": 3},
                {"index": 2, "status": "failed", "reason": "tick",
                    "message": "off tick", "order": 0, "failedOrders": 1}
            ]
        });
        assert_eq!(native_action_status(&old_rejected).unwrap(), want);
    }

    #[test]
    fn parse_dec_fp_reads_decimal_trade_fields() {
        assert_eq!(parse_dec_fp("0.00000009"), Some(9));
        assert_eq!(parse_dec_fp("123.45000000"), Some(12_345_000_000));
        assert_eq!(parse_dec_fp("-0.50000000"), Some(-50_000_000));
        assert_eq!(
            parse_dec_fp("0x9"),
            None,
            "old hex encoding is not a decimal"
        );
        assert_eq!(parse_dec_fp(""), None, "a missing field is not zero");
    }

    /// s80 review: a trade whose price or quantity does not parse (an old node
    /// still returning fixed-point hex) is skipped, never written as a 0-price
    /// candle; a decimal trade is applied.
    #[test]
    fn index_trades_skips_unparsable_price_or_quantity() {
        let db = ExplorerDb::open_in_memory().unwrap();
        let trade = |price: &str, qty: &str| json!({"marketId": "0x1", "timestamp": "0x3c", "price": price, "quantity": qty});
        index_trades(
            &db,
            7,
            &[
                trade("0x2dfd4c490", "2.50000000"),
                trade("100.00000000", "0x3b9aca0"),
            ],
        );
        assert!(
            db.get_candles(1, "1m", None, None, 10).unwrap().is_empty(),
            "hex price/quantity must not create a candle"
        );

        index_trades(&db, 8, &[trade("100.00000000", "2.50000000")]);
        index_trades(&db, 9, &[trade("0x2dfd4c490", "1.00000000")]);
        let candles = db.get_candles(1, "1m", None, None, 10).unwrap();
        assert_eq!(candles.len(), 1);
        let c = &candles[0];
        assert_eq!(
            (c.open_time, c.open, c.high, c.low, c.close),
            (
                60,
                10_000_000_000,
                10_000_000_000,
                10_000_000_000,
                10_000_000_000
            )
        );
        assert_eq!(
            (c.volume, c.trade_count),
            (250_000_000, 1),
            "the hex trade changed nothing"
        );
    }

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
        assert_eq!(row.status, TxStatus::Success);
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
            height: 10,
            hash: "0xAAAA".into(),
            parent_hash: "0x0009".into(),
            timestamp: 1000,
            proposer: "0xprop".into(),
            gas_used: 0,
            gas_limit: 30_000_000,
            base_fee: 0,
            tx_count: 1,
            native_action_count: 0,
            epoch: 0,
            validator_set_hash: String::new(),
            state_root: String::new(),
        };
        db.insert_block(&block).unwrap();
        db.insert_transaction(&TxRow {
            hash: "0xtx_old".into(),
            block_height: 10,
            tx_index: 0,
            from_addr: "0xfrom".into(),
            to_addr: None,
            value: "0x0".into(),
            gas_limit: 21000,
            gas_used: 21000,
            gas_price: "0x0".into(),
            input_data: "0x".into(),
            nonce: 0,
            status: TxStatus::Success,
            contract_address: None,
            tx_type: 0,
        })
        .unwrap();
        assert!(db.get_block(10).unwrap().is_some());
        db.delete_block_data(10).unwrap();
        assert!(db.get_block(10).unwrap().is_none());
        assert!(db.get_transaction("0xtx_old").unwrap().is_none());
    }
}
