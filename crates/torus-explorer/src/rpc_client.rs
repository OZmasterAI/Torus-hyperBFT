use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use jsonrpsee::rpc_params;
use serde_json::Value;

pub type RpcError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
pub struct NodeRpcClient {
    http: HttpClient,
}

impl NodeRpcClient {
    pub fn new(rpc_url: &str) -> Result<Self, RpcError> {
        let http = HttpClientBuilder::default().build(rpc_url)?;
        Ok(Self { http })
    }

    pub async fn get_block_number(&self) -> Result<u64, RpcError> {
        let result: String = self.http.request("eth_blockNumber", rpc_params![]).await?;
        Ok(parse_hex_u64(&result))
    }

    pub async fn get_block(&self, height: u64) -> Result<Option<Value>, RpcError> {
        let hex = format!("0x{:x}", height);
        let result: Option<Value> = self
            .http
            .request("eth_getBlockByNumber", rpc_params![hex, true])
            .await?;
        Ok(result)
    }

    pub async fn get_receipt(&self, tx_hash: &str) -> Result<Option<Value>, RpcError> {
        let result: Option<Value> = self
            .http
            .request("eth_getTransactionReceipt", rpc_params![tx_hash])
            .await?;
        Ok(result)
    }

    pub async fn get_block_body(&self, height: u64) -> Result<Option<Value>, RpcError> {
        let result: Option<Value> = self
            .http
            .request("torus_getBlockBody", rpc_params![height])
            .await?;
        Ok(result)
    }

    pub async fn get_validators(&self) -> Result<Vec<Value>, RpcError> {
        let result: Vec<Value> = self
            .http
            .request("torus_getValidators", rpc_params![])
            .await?;
        Ok(result)
    }

    pub async fn get_balance(&self, addr: &str) -> Result<String, RpcError> {
        let result: String = self
            .http
            .request("eth_getBalance", rpc_params![addr, "latest"])
            .await?;
        Ok(result)
    }

    pub async fn get_staking_info(&self, addr: &str) -> Result<Value, RpcError> {
        let result: Value = self
            .http
            .request("torus_getStakingInfo", rpc_params![addr])
            .await?;
        Ok(result)
    }

    /// Fetch all trades that occurred in a single block (Phase 7B).
    pub async fn get_block_trades(&self, block_number: u64) -> Result<Vec<Value>, RpcError> {
        let result: Vec<Value> = self
            .http
            .request("torus_getBlockTrades", rpc_params![block_number])
            .await?;
        Ok(result)
    }
}

// ============================================================================
// Hex parsing helpers
// ============================================================================

pub fn parse_hex_u64(s: &str) -> u64 {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(s, 16).unwrap_or(0)
}

pub fn parse_hex_i64(s: &str) -> i64 {
    parse_hex_u64(s) as i64
}

pub fn val_str(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub fn val_str_opt(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub fn val_hex_i64(v: &Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|v| v.as_str())
        .map(parse_hex_i64)
        .unwrap_or(0)
}

pub fn val_hex_u64(v: &Value, key: &str) -> u64 {
    v.get(key)
        .and_then(|v| v.as_str())
        .map(parse_hex_u64)
        .unwrap_or(0)
}
