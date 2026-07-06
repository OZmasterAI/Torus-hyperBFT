//! Torus testnet faucet — dispenses TRS to requested addresses.
//!
//! POST /faucet { "address": "0x..." } → sends testnet TRS, returns tx hash
//! GET  /health → faucet balance and status

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_consensus::TxEip1559;
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, Bytes, Signature as AlloySig, TxKind, U256};
use alloy_rlp::Encodable;
use clap::Parser;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

// ============================================================================
// CLI
// ============================================================================

#[derive(Parser)]
#[command(name = "torus-faucet", version, about = "Torus testnet token faucet")]
struct Cli {
    /// JSON-RPC URL of the Torus node
    #[arg(long, default_value = "http://localhost:8545")]
    rpc_url: String,

    /// Faucet private key (hex). Prefer env var FAUCET_PRIVATE_KEY.
    #[arg(long)]
    faucet_key: Option<String>,

    /// Listen address
    #[arg(long, default_value = "0.0.0.0:3002")]
    listen: String,

    /// Drip amount in wei (default: 10 TRS = 10^19)
    #[arg(long, default_value = "10000000000000000000")]
    drip_amount: String,

    /// Cooldown between requests per address, in seconds
    #[arg(long, default_value = "86400")]
    cooldown_seconds: u64,

    /// Chain ID (must match node)
    #[arg(long, default_value = "7778")]
    chain_id: u64,

    /// Low-balance warning threshold in wei (default: 100 TRS)
    #[arg(long, default_value = "100000000000000000000")]
    low_balance_threshold: String,
}

// ============================================================================
// Types
// ============================================================================

#[derive(Deserialize)]
struct FaucetRequest {
    address: String,
}

#[derive(Serialize)]
struct FaucetResponse {
    tx_hash: String,
    amount: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_seconds: Option<u64>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    faucet_address: String,
    balance: String,
    balance_trs: String,
    drip_amount_trs: String,
    cooldown_seconds: u64,
}

struct FaucetState {
    rpc_url: String,
    signing_key: SigningKey,
    faucet_address: Address,
    drip_amount: U256,
    cooldown: Duration,
    chain_id: u64,
    low_balance_threshold: U256,
    cooldowns: Mutex<HashMap<Address, Instant>>,
    nonce: Mutex<Option<u64>>,
    http_client: reqwest::Client,
}

// ============================================================================
// RPC helpers
// ============================================================================

#[derive(Deserialize)]
struct JsonRpcResponse {
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
}

impl FaucetState {
    async fn rpc_call(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        });
        let resp = self
            .http_client
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        let rpc: JsonRpcResponse = resp
            .json()
            .await
            .map_err(|e| format!("RPC response parse failed: {e}"))?;
        if let Some(err) = rpc.error {
            return Err(format!("RPC error: {err}"));
        }
        rpc.result
            .ok_or_else(|| "RPC returned null result".to_string())
    }

    async fn get_balance(&self) -> Result<U256, String> {
        let addr = format!("0x{}", hex::encode(self.faucet_address));
        let result = self
            .rpc_call("eth_getBalance", &serde_json::json!([addr, "latest"]))
            .await?;
        let hex_str = result.as_str().ok_or("balance not a string")?;
        parse_hex_u256(hex_str)
    }

    async fn get_nonce(&self) -> Result<u64, String> {
        let addr = format!("0x{}", hex::encode(self.faucet_address));
        let result = self
            .rpc_call(
                "eth_getTransactionCount",
                &serde_json::json!([addr, "latest"]),
            )
            .await?;
        let hex_str = result.as_str().ok_or("nonce not a string")?;
        parse_hex_u64(hex_str)
    }

    async fn get_gas_price(&self) -> Result<u128, String> {
        let result = self
            .rpc_call("eth_gasPrice", &serde_json::json!([]))
            .await?;
        let hex_str = result.as_str().ok_or("gasPrice not a string")?;
        parse_hex_u128(hex_str)
    }

    async fn send_raw_tx(&self, raw: &[u8]) -> Result<String, String> {
        let hex_data = format!("0x{}", hex::encode(raw));
        let result = self
            .rpc_call("eth_sendRawTransaction", &serde_json::json!([hex_data]))
            .await?;
        result
            .as_str()
            .map(|s| s.to_string())
            .ok_or("tx hash not a string".to_string())
    }
}

fn parse_hex_u256(s: &str) -> Result<U256, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    U256::from_str_radix(s, 16).map_err(|e| format!("invalid hex u256: {e}"))
}

fn parse_hex_u64(s: &str) -> Result<u64, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(s, 16).map_err(|e| format!("invalid hex u64: {e}"))
}

fn parse_hex_u128(s: &str) -> Result<u128, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u128::from_str_radix(s, 16).map_err(|e| format!("invalid hex u128: {e}"))
}

fn format_trs(wei: U256) -> String {
    let divisor = U256::from(1_000_000_000_000_000_000u64);
    let whole = wei / divisor;
    let frac = wei % divisor;
    let frac_str = format!("{:018}", frac.to::<u64>());
    let trimmed = frac_str.trim_end_matches('0');
    if trimmed.is_empty() {
        format!("{whole} TRS")
    } else {
        format!("{whole}.{trimmed} TRS")
    }
}

fn address_from_signing_key(key: &SigningKey) -> Address {
    use k256::ecdsa::VerifyingKey;
    let vk = VerifyingKey::from(key);
    let uncompressed = vk.to_encoded_point(false);
    let hash = alloy_primitives::keccak256(&uncompressed.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

fn is_valid_address(s: &str) -> bool {
    let s = s.strip_prefix("0x").unwrap_or(s);
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn parse_address(s: &str) -> Result<Address, String> {
    if !is_valid_address(s) {
        return Err("invalid Ethereum address".to_string());
    }
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).map_err(|e| format!("hex decode: {e}"))?;
    Ok(Address::from_slice(&bytes))
}

// ============================================================================
// Transaction building & signing
// ============================================================================

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
        max_priority_fee_per_gas: gas_price / 10, // 10% tip
        to: TxKind::Call(to),
        value,
        input: Bytes::new(),
        access_list: Default::default(),
    };

    // Compute EIP-1559 signing hash: keccak256(0x02 || RLP([chainId, nonce, ...]))
    let mut rlp_buf = Vec::new();
    tx.encode(&mut rlp_buf);
    let mut hash_input = Vec::with_capacity(1 + rlp_buf.len());
    hash_input.push(0x02); // EIP-1559 type prefix
    hash_input.extend_from_slice(&rlp_buf);
    let signing_hash = alloy_primitives::keccak256(&hash_input);

    // Sign
    let (sig, recid) = key
        .sign_prehash_recoverable(signing_hash.as_slice())
        .expect("signing cannot fail with valid key");
    let sig_bytes = sig.to_bytes();
    let y_parity = recid.to_byte() != 0;

    // Build the signed envelope: 0x02 || RLP([..., y_parity, r, s])
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
// HTTP server (minimal, raw TCP like torus-telemetry)
// ============================================================================

async fn handle_request(state: &Arc<FaucetState>, request: &str) -> (u16, String, String) {
    // Parse method and path from first line
    let first_line = request.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return (
            400,
            "application/json".into(),
            r#"{"error":"bad request"}"#.into(),
        );
    }
    let (method, path) = (parts[0], parts[1]);

    // CORS preflight
    if method == "OPTIONS" {
        return (204, "text/plain".into(), String::new());
    }

    match (method, path) {
        ("GET", "/health") => handle_health(state).await,
        ("POST", "/faucet") => {
            // Extract JSON body (everything after the blank line)
            let body = request
                .split("\r\n\r\n")
                .nth(1)
                .or_else(|| request.split("\n\n").nth(1))
                .unwrap_or("");
            handle_faucet(state, body).await
        }
        _ => (
            404,
            "application/json".into(),
            r#"{"error":"not found"}"#.into(),
        ),
    }
}

async fn handle_health(state: &Arc<FaucetState>) -> (u16, String, String) {
    match state.get_balance().await {
        Ok(balance) => {
            let resp = HealthResponse {
                status: "ok".to_string(),
                faucet_address: format!("0x{}", hex::encode(state.faucet_address)),
                balance: format!("0x{:x}", balance),
                balance_trs: format_trs(balance),
                drip_amount_trs: format_trs(state.drip_amount),
                cooldown_seconds: state.cooldown.as_secs(),
            };
            (
                200,
                "application/json".into(),
                serde_json::to_string(&resp).unwrap(),
            )
        }
        Err(e) => {
            let resp = ErrorResponse {
                error: format!("cannot query balance: {e}"),
                retry_after_seconds: None,
            };
            (
                503,
                "application/json".into(),
                serde_json::to_string(&resp).unwrap(),
            )
        }
    }
}

async fn handle_faucet(state: &Arc<FaucetState>, body: &str) -> (u16, String, String) {
    // Parse request
    let req: FaucetRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(_) => {
            let resp = ErrorResponse {
                error: "invalid JSON body, expected {\"address\":\"0x...\"}".into(),
                retry_after_seconds: None,
            };
            return (
                400,
                "application/json".into(),
                serde_json::to_string(&resp).unwrap(),
            );
        }
    };

    // Validate address
    let to_addr = match parse_address(&req.address) {
        Ok(a) => a,
        Err(_) => {
            let resp = ErrorResponse {
                error: "invalid Ethereum address".into(),
                retry_after_seconds: None,
            };
            return (
                400,
                "application/json".into(),
                serde_json::to_string(&resp).unwrap(),
            );
        }
    };

    // Check cooldown
    {
        let cooldowns = state.cooldowns.lock().await;
        if let Some(last) = cooldowns.get(&to_addr) {
            let elapsed = last.elapsed();
            if elapsed < state.cooldown {
                let remaining = (state.cooldown - elapsed).as_secs();
                let resp = ErrorResponse {
                    error: format!("address is rate-limited, try again in {remaining}s"),
                    retry_after_seconds: Some(remaining),
                };
                return (
                    429,
                    "application/json".into(),
                    serde_json::to_string(&resp).unwrap(),
                );
            }
        }
    }

    // Check balance
    let balance = match state.get_balance().await {
        Ok(b) => b,
        Err(e) => {
            let resp = ErrorResponse {
                error: format!("cannot query faucet balance: {e}"),
                retry_after_seconds: None,
            };
            return (
                503,
                "application/json".into(),
                serde_json::to_string(&resp).unwrap(),
            );
        }
    };

    if balance < state.drip_amount {
        let resp = ErrorResponse {
            error: "faucet is empty, please try again later".into(),
            retry_after_seconds: None,
        };
        return (
            503,
            "application/json".into(),
            serde_json::to_string(&resp).unwrap(),
        );
    }

    if balance < state.low_balance_threshold {
        warn!(
            balance = %format_trs(balance),
            threshold = %format_trs(state.low_balance_threshold),
            "faucet balance is low"
        );
    }

    // Get nonce (use cached + increment to avoid collisions on rapid requests)
    let nonce = {
        let mut nonce_lock = state.nonce.lock().await;
        let n = match *nonce_lock {
            Some(cached) => cached,
            None => match state.get_nonce().await {
                Ok(n) => n,
                Err(e) => {
                    let resp = ErrorResponse {
                        error: format!("cannot query nonce: {e}"),
                        retry_after_seconds: None,
                    };
                    return (
                        500,
                        "application/json".into(),
                        serde_json::to_string(&resp).unwrap(),
                    );
                }
            },
        };
        *nonce_lock = Some(n + 1);
        n
    };

    // Get gas price
    let gas_price = match state.get_gas_price().await {
        Ok(p) => p,
        Err(e) => {
            let resp = ErrorResponse {
                error: format!("cannot query gas price: {e}"),
                retry_after_seconds: None,
            };
            return (
                500,
                "application/json".into(),
                serde_json::to_string(&resp).unwrap(),
            );
        }
    };

    // Build, sign, and send tx
    let raw_tx = build_and_sign_eip1559_tx(
        &state.signing_key,
        state.chain_id,
        nonce,
        to_addr,
        state.drip_amount,
        gas_price,
    );

    match state.send_raw_tx(&raw_tx).await {
        Ok(tx_hash) => {
            // Record cooldown
            state.cooldowns.lock().await.insert(to_addr, Instant::now());
            info!(
                to = %req.address,
                tx_hash = %tx_hash,
                amount = %format_trs(state.drip_amount),
                "faucet drip sent"
            );
            let resp = FaucetResponse {
                tx_hash,
                amount: format_trs(state.drip_amount),
            };
            (
                200,
                "application/json".into(),
                serde_json::to_string(&resp).unwrap(),
            )
        }
        Err(e) => {
            // Reset nonce cache on send failure
            *state.nonce.lock().await = None;
            error!(error = %e, "failed to send faucet tx");
            let resp = ErrorResponse {
                error: "transaction failed, please try again".into(),
                retry_after_seconds: None,
            };
            (
                500,
                "application/json".into(),
                serde_json::to_string(&resp).unwrap(),
            )
        }
    }
}

async fn serve(state: Arc<FaucetState>, addr: SocketAddr) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "faucet server listening");

    loop {
        let (mut stream, peer) = listener.accept().await?;
        let state = state.clone();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 16384];
            let n = match stream.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let request = String::from_utf8_lossy(&buf[..n]);

            let (status, content_type, body) = handle_request(&state, &request).await;

            let status_text = match status {
                200 => "OK",
                204 => "No Content",
                400 => "Bad Request",
                404 => "Not Found",
                429 => "Too Many Requests",
                500 => "Internal Server Error",
                503 => "Service Unavailable",
                _ => "Unknown",
            };

            let cors_headers = "Access-Control-Allow-Origin: *\r\n\
                               Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                               Access-Control-Allow-Headers: Content-Type\r\n";

            let response = format!(
                "HTTP/1.1 {status} {status_text}\r\n\
                 Content-Type: {content_type}\r\n\
                 Content-Length: {}\r\n\
                 {cors_headers}\
                 \r\n\
                 {body}",
                body.len(),
            );

            let _ = stream.write_all(response.as_bytes()).await;
            tracing::debug!(%peer, %status, "request handled");
        });
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // Load private key from flag or env var
    let key_hex = cli
        .faucet_key
        .or_else(|| std::env::var("FAUCET_PRIVATE_KEY").ok())
        .expect("faucet private key required: set FAUCET_PRIVATE_KEY or use --faucet-key");

    let key_hex = key_hex.strip_prefix("0x").unwrap_or(&key_hex);
    let key_bytes = hex::decode(key_hex).expect("invalid hex in faucet key");
    assert!(key_bytes.len() == 32, "faucet key must be 32 bytes");

    let signing_key = SigningKey::from_slice(&key_bytes).expect("invalid secp256k1 key");
    let faucet_address = address_from_signing_key(&signing_key);

    let drip_amount = U256::from_str_radix(&cli.drip_amount, 10).expect("invalid drip amount");
    let low_balance_threshold = U256::from_str_radix(&cli.low_balance_threshold, 10)
        .expect("invalid low balance threshold");

    info!(
        address = %format!("0x{}", hex::encode(faucet_address)),
        drip = %format_trs(drip_amount),
        cooldown = %cli.cooldown_seconds,
        rpc = %cli.rpc_url,
        "starting faucet"
    );

    let state = Arc::new(FaucetState {
        rpc_url: cli.rpc_url,
        signing_key,
        faucet_address,
        drip_amount,
        cooldown: Duration::from_secs(cli.cooldown_seconds),
        chain_id: cli.chain_id,
        low_balance_threshold,
        cooldowns: Mutex::new(HashMap::new()),
        nonce: Mutex::new(None),
        http_client: reqwest::Client::new(),
    });

    let addr: SocketAddr = cli.listen.parse().expect("invalid listen address");
    serve(state, addr).await.expect("server failed");
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_address() {
        assert!(is_valid_address(
            "0x0000000000000000000000000000000000000001"
        ));
        assert!(is_valid_address(
            "0xAbCdEf0123456789AbCdEf0123456789AbCdEf01"
        ));
        assert!(!is_valid_address("0x123")); // too short
        assert!(!is_valid_address("not_an_address"));
        assert!(!is_valid_address(
            "0xGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG"
        ));
    }

    #[test]
    fn test_parse_address() {
        let addr = parse_address("0x0000000000000000000000000000000000000001").unwrap();
        assert_eq!(
            addr,
            Address::from_slice(&{
                let mut b = [0u8; 20];
                b[19] = 1;
                b
            })
        );
        assert!(parse_address("invalid").is_err());
    }

    #[test]
    fn test_format_trs() {
        let one_trs = U256::from(1_000_000_000_000_000_000u64);
        assert_eq!(format_trs(one_trs), "1 TRS");

        let half_trs = U256::from(500_000_000_000_000_000u64);
        assert_eq!(format_trs(half_trs), "0.5 TRS");

        let ten_trs = U256::from(10_000_000_000_000_000_000u128);
        assert_eq!(format_trs(ten_trs), "10 TRS");
    }

    #[test]
    fn test_parse_hex_u256() {
        assert_eq!(parse_hex_u256("0x0").unwrap(), U256::ZERO);
        assert_eq!(parse_hex_u256("0xa").unwrap(), U256::from(10));
        assert_eq!(parse_hex_u256("0xff").unwrap(), U256::from(255));
    }

    #[test]
    fn test_parse_hex_u64() {
        assert_eq!(parse_hex_u64("0x0").unwrap(), 0u64);
        assert_eq!(parse_hex_u64("0x1").unwrap(), 1u64);
        assert_eq!(parse_hex_u64("0xff").unwrap(), 255u64);
    }

    #[test]
    fn test_address_from_key() {
        // Known test vector: private key = 1
        let key = SigningKey::from_slice(&{
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        })
        .unwrap();
        let addr = address_from_signing_key(&key);
        // The address for private key 1 is well-known
        assert_eq!(addr.len(), 20);
        assert_ne!(addr, Address::ZERO);
    }

    #[test]
    fn test_build_and_sign_tx() {
        let key = SigningKey::from_slice(&{
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        })
        .unwrap();

        let to = Address::from_slice(&[0xAA; 20]);
        let value = U256::from(1_000_000_000_000_000_000u64); // 1 TRS

        let raw = build_and_sign_eip1559_tx(&key, 7778, 0, to, value, 1_000_000_000);
        // EIP-1559 tx starts with 0x02 type prefix
        assert_eq!(raw[0], 0x02);
        assert!(raw.len() > 100); // signed tx should be substantial
    }

    #[test]
    fn test_cooldown_tracking() {
        // Verify HashMap-based cooldown logic
        let mut map: HashMap<Address, Instant> = HashMap::new();
        let addr = Address::from_slice(&[1; 20]);

        assert!(map.get(&addr).is_none());
        map.insert(addr, Instant::now());
        assert!(map.get(&addr).is_some());
        assert!(map.get(&addr).unwrap().elapsed() < Duration::from_secs(1));
    }
}
