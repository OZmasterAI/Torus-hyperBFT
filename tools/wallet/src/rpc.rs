//! JSON-RPC client for querying the Torus node.

use serde::Deserialize;

#[derive(Deserialize)]
struct JsonRpcResponse {
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
}

pub struct RpcClient {
    url: String,
    client: reqwest::Client,
}

impl RpcClient {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        });
        let resp = self.client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        let rpc: JsonRpcResponse = resp
            .json()
            .await
            .map_err(|e| format!("RPC parse failed: {e}"))?;
        if let Some(err) = rpc.error {
            return Err(format!("RPC error: {err}"));
        }
        rpc.result.ok_or_else(|| "null result".to_string())
    }

    // --- eth namespace ---

    pub async fn chain_id(&self) -> Result<u64, String> {
        let r = self.call("eth_chainId", serde_json::json!([])).await?;
        parse_hex_u64(r.as_str().ok_or("chainId not string")?)
    }

    #[allow(dead_code)]
    pub async fn block_number(&self) -> Result<u64, String> {
        let r = self.call("eth_blockNumber", serde_json::json!([])).await?;
        parse_hex_u64(r.as_str().ok_or("blockNumber not string")?)
    }

    pub async fn get_balance(&self, addr: &str) -> Result<alloy_primitives::U256, String> {
        let r = self.call("eth_getBalance", serde_json::json!([addr, "latest"])).await?;
        parse_hex_u256(r.as_str().ok_or("balance not string")?)
    }

    pub async fn get_transaction_count(&self, addr: &str) -> Result<u64, String> {
        let r = self.call("eth_getTransactionCount", serde_json::json!([addr, "latest"])).await?;
        parse_hex_u64(r.as_str().ok_or("nonce not string")?)
    }

    pub async fn gas_price(&self) -> Result<u128, String> {
        let r = self.call("eth_gasPrice", serde_json::json!([])).await?;
        parse_hex_u128(r.as_str().ok_or("gasPrice not string")?)
    }

    pub async fn send_raw_transaction(&self, raw_hex: &str) -> Result<String, String> {
        let r = self.call("eth_sendRawTransaction", serde_json::json!([raw_hex])).await?;
        r.as_str().map(|s| s.to_string()).ok_or("tx hash not string".to_string())
    }

    pub async fn get_block_by_number(&self, number: &str, full_txs: bool) -> Result<serde_json::Value, String> {
        self.call("eth_getBlockByNumber", serde_json::json!([number, full_txs])).await
    }

    pub async fn get_transaction_by_hash(&self, hash: &str) -> Result<serde_json::Value, String> {
        self.call("eth_getTransactionByHash", serde_json::json!([hash])).await
    }

    // --- torus namespace ---

    pub async fn get_balances(&self, addr: &str) -> Result<serde_json::Value, String> {
        self.call("torus_getBalances", serde_json::json!([addr])).await
    }

    pub async fn get_validators(&self) -> Result<serde_json::Value, String> {
        self.call("torus_getValidators", serde_json::json!([])).await
    }

    pub async fn get_staking_info(&self, addr: &str) -> Result<serde_json::Value, String> {
        self.call("torus_getStakingInfo", serde_json::json!([addr])).await
    }

    pub async fn get_delegations(&self, addr: &str) -> Result<serde_json::Value, String> {
        self.call("torus_getDelegations", serde_json::json!([addr])).await
    }

    pub async fn submit_native_action(&self, signed_action_json: &str) -> Result<String, String> {
        // Server does parse_bytes → hex::decode → serde_json::from_slice.
        // Must send hex-encoded JSON bytes with 0x prefix.
        let hex_payload = format!("0x{}", hex::encode(signed_action_json.as_bytes()));
        let r = self.call("torus_submitNativeAction", serde_json::json!([hex_payload])).await?;
        r.as_str().map(|s| s.to_string()).ok_or("result not string".to_string())
    }

    pub async fn get_order_book(&self, market_id: &str) -> Result<serde_json::Value, String> {
        self.call("torus_getOrderBook", serde_json::json!([market_id])).await
    }

    pub async fn get_position(&self, addr: &str, market_id: &str) -> Result<serde_json::Value, String> {
        self.call("torus_getPosition", serde_json::json!([addr, market_id])).await
    }

    pub async fn get_proposals(&self) -> Result<serde_json::Value, String> {
        self.call("torus_getProposals", serde_json::json!([null])).await
    }

    pub async fn get_proposal(&self, id: u64) -> Result<serde_json::Value, String> {
        self.call("torus_getProposal", serde_json::json!([id])).await
    }

    pub async fn get_open_orders(
        &self,
        trader: &str,
        market_id: Option<u64>,
    ) -> Result<serde_json::Value, String> {
        self.call(
            "torus_getOpenOrders",
            serde_json::json!([trader, market_id]),
        )
        .await
    }

    pub async fn get_epoch(&self) -> Result<serde_json::Value, String> {
        self.call("torus_getEpoch", serde_json::json!([])).await
    }

    pub async fn get_markets(
        &self,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<serde_json::Value, String> {
        self.call("torus_getMarkets", serde_json::json!([offset, limit])).await
    }
}

pub fn parse_hex_u64(s: &str) -> Result<u64, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(s, 16).map_err(|e| format!("invalid hex u64: {e}"))
}

pub fn parse_hex_u128(s: &str) -> Result<u128, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u128::from_str_radix(s, 16).map_err(|e| format!("invalid hex u128: {e}"))
}

pub fn parse_hex_u256(s: &str) -> Result<alloy_primitives::U256, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    alloy_primitives::U256::from_str_radix(s, 16).map_err(|e| format!("invalid hex u256: {e}"))
}

/// Format wei as TRS with up to 6 decimal places.
pub fn format_trs(wei: alloy_primitives::U256) -> String {
    let divisor = alloy_primitives::U256::from(1_000_000_000_000_000_000u64);
    if wei.is_zero() {
        return "0.000000 TRS".to_string();
    }
    let whole = wei / divisor;
    let frac = wei % divisor;
    // 18 decimal places internally, display 6
    let frac_full = format!("{:018}", frac.to::<u64>());
    let frac_6 = &frac_full[..6];
    format!("{whole}.{frac_6} TRS")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_trs() {
        use alloy_primitives::U256;
        assert_eq!(format_trs(U256::from(1_000_000_000_000_000_000u64)), "1.000000 TRS");
        assert_eq!(format_trs(U256::from(12_500_000_000_000_000_000u128)), "12.500000 TRS");
        assert_eq!(format_trs(U256::ZERO), "0.000000 TRS");
        assert_eq!(format_trs(U256::from(500_000_000_000_000u64)), "0.000500 TRS");
    }

    #[test]
    fn test_parse_hex() {
        assert_eq!(parse_hex_u64("0x0").unwrap(), 0);
        assert_eq!(parse_hex_u64("0xff").unwrap(), 255);
        assert_eq!(parse_hex_u128("0x1").unwrap(), 1);
    }

    #[test]
    fn test_submit_hex_roundtrip() {
        let json = r#"{"action":"ClaimRewards","nonce":123}"#;
        let hex_payload = format!("0x{}", hex::encode(json.as_bytes()));
        assert!(hex_payload.starts_with("0x"));
        let decoded = hex::decode(&hex_payload[2..]).unwrap();
        assert_eq!(std::str::from_utf8(&decoded).unwrap(), json);
        let roundtrip: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(roundtrip["action"], "ClaimRewards");
        assert_eq!(roundtrip["nonce"], 123);
    }
}
