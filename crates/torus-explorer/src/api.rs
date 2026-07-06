use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;

use crate::db::ExplorerDb;
use crate::rpc_client::NodeRpcClient;

#[derive(Clone)]
pub struct AppState {
    pub db: ExplorerDb,
    pub rpc: Arc<NodeRpcClient>,
}

pub fn router(db: ExplorerDb, rpc: NodeRpcClient) -> Router {
    let state = AppState {
        db,
        rpc: Arc::new(rpc),
    };
    Router::new()
        .route("/api/blocks", get(list_blocks))
        .route("/api/blocks/{height}", get(get_block))
        .route("/api/txs/{hash}", get(get_tx))
        .route("/api/addresses/{addr}", get(get_address))
        .route("/api/addresses/{addr}/txs", get(get_address_txs))
        .route("/api/addresses/{addr}/actions", get(get_address_actions))
        .route("/api/validators", get(list_validators))
        .route("/api/validators/{addr}", get(get_validator))
        .route("/api/search", get(search))
        .route("/api/stats", get(get_stats))
        .route("/api/candles/{market_id}", get(get_candles))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

#[derive(Deserialize)]
pub struct Pagination {
    pub page: Option<u32>,
    pub limit: Option<u32>,
}

impl Pagination {
    fn page(&self) -> u32 {
        self.page.unwrap_or(1).max(1)
    }
    fn limit(&self) -> u32 {
        self.limit.unwrap_or(20).clamp(1, 100)
    }
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
}

struct ApiError(String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": self.0})),
        )
            .into_response()
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError(e.to_string())
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

async fn list_blocks(State(s): State<AppState>, Query(p): Query<Pagination>) -> ApiResult {
    let (blocks, total) = s.db.get_blocks(p.page(), p.limit())?;
    Ok(Json(
        json!({"data": blocks, "total": total, "page": p.page(), "limit": p.limit()}),
    ))
}

async fn get_block(State(s): State<AppState>, Path(height): Path<i64>) -> ApiResult {
    match s.db.get_block(height)? {
        Some(b) => {
            let txs = s.db.get_block_transactions(height)?;
            let actions = s.db.get_block_native_actions(height)?;
            Ok(Json(
                json!({"block": b, "transactions": txs, "native_actions": actions}),
            ))
        }
        None => Err(ApiError("block not found".into())),
    }
}

async fn get_tx(State(s): State<AppState>, Path(hash): Path<String>) -> ApiResult {
    match s.db.get_transaction(&hash)? {
        Some(t) => {
            let logs = s.db.get_transaction_logs(&hash)?;
            Ok(Json(json!({"transaction": t, "logs": logs})))
        }
        None => Err(ApiError("transaction not found".into())),
    }
}

async fn get_address(State(s): State<AppState>, Path(addr): Path<String>) -> ApiResult {
    let addr_lower = addr.to_lowercase();
    let tx_count = s.db.get_address_tx_count(&addr_lower)?;
    let action_count = s.db.get_address_action_count(&addr_lower)?;
    let balance = s.rpc.get_balance(&addr_lower).await.unwrap_or_default();
    let staking = s.rpc.get_staking_info(&addr_lower).await.ok();
    Ok(Json(json!({
        "address": addr_lower, "evm_balance": balance,
        "tx_count": tx_count, "native_action_count": action_count, "staking": staking
    })))
}

async fn get_address_txs(
    State(s): State<AppState>,
    Path(addr): Path<String>,
    Query(p): Query<Pagination>,
) -> ApiResult {
    let (txs, total) =
        s.db.get_address_transactions(&addr.to_lowercase(), p.page(), p.limit())?;
    Ok(Json(
        json!({"data": txs, "total": total, "page": p.page(), "limit": p.limit()}),
    ))
}

async fn get_address_actions(
    State(s): State<AppState>,
    Path(addr): Path<String>,
    Query(p): Query<Pagination>,
) -> ApiResult {
    let (actions, total) =
        s.db.get_address_actions(&addr.to_lowercase(), p.page(), p.limit())?;
    Ok(Json(
        json!({"data": actions, "total": total, "page": p.page(), "limit": p.limit()}),
    ))
}

async fn list_validators(State(s): State<AppState>) -> ApiResult {
    let validators = s.db.get_latest_validators()?;
    Ok(Json(json!({"data": validators, "total": validators.len()})))
}

async fn get_validator(State(s): State<AppState>, Path(addr): Path<String>) -> ApiResult {
    let addr_lower = addr.to_lowercase();
    match s.db.get_validator_detail(&addr_lower)? {
        Some(v) => {
            let blocks_proposed = s.db.get_validator_blocks_proposed(&addr_lower)?;
            Ok(Json(
                json!({"validator": v, "blocks_proposed": blocks_proposed}),
            ))
        }
        None => Err(ApiError("validator not found".into())),
    }
}

async fn search(State(s): State<AppState>, Query(q): Query<SearchQuery>) -> ApiResult {
    let query = q.q.trim().to_lowercase();

    if let Ok(height) = query.parse::<i64>() {
        if let Some(block) = s.db.get_block(height)? {
            return Ok(Json(json!({"type": "block", "result": block})));
        }
    }
    if query.starts_with("0x") && query.len() == 66 {
        if let Some(tx) = s.db.get_transaction(&query)? {
            return Ok(Json(json!({"type": "transaction", "result": tx})));
        }
        if let Some(block) = s.db.get_block_by_hash(&query)? {
            return Ok(Json(json!({"type": "block", "result": block})));
        }
    }
    if query.starts_with("0x") && query.len() == 42 {
        let tx_count = s.db.get_address_tx_count(&query)?;
        let action_count = s.db.get_address_action_count(&query)?;
        return Ok(Json(json!({
            "type": "address",
            "result": {"address": query, "tx_count": tx_count, "native_action_count": action_count}
        })));
    }
    if query.starts_with("0x") {
        if let Ok(height) = i64::from_str_radix(query.trim_start_matches("0x"), 16) {
            if let Some(block) = s.db.get_block(height)? {
                return Ok(Json(json!({"type": "block", "result": block})));
            }
        }
    }
    Err(ApiError("no results found".into()))
}

async fn get_stats(State(s): State<AppState>) -> ApiResult {
    let stats = s.db.get_stats()?;
    Ok(Json(json!(stats)))
}

#[derive(Deserialize)]
pub struct CandleQuery {
    pub interval: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: Option<u32>,
}

/// GET /api/candles/{market_id}?interval=5m&from=1700000000&to=1700003600&limit=500
/// Returns OHLCV candles with floats (raw / 10^8).
async fn get_candles(
    State(s): State<AppState>,
    Path(market_id): Path<i64>,
    Query(q): Query<CandleQuery>,
) -> ApiResult {
    let interval = q.interval.as_deref().unwrap_or("5m");
    let valid_intervals = ["1m", "5m", "15m", "1h"];
    if !valid_intervals.contains(&interval) {
        return Err(ApiError(format!(
            "invalid interval '{}', must be one of: {}",
            interval,
            valid_intervals.join(", ")
        )));
    }
    let limit = q.limit.unwrap_or(500).min(2000);
    let candles = s.db.get_candles(market_id, interval, q.from, q.to, limit)?;

    const SCALE: f64 = 100_000_000.0; // 10^8
    let data: Vec<Value> = candles
        .iter()
        .map(|c| {
            json!({
                "time": c.open_time,
                "open": c.open as f64 / SCALE,
                "high": c.high as f64 / SCALE,
                "low": c.low as f64 / SCALE,
                "close": c.close as f64 / SCALE,
                "volume": (c.volume as f64 / SCALE).abs(),
                "tradeCount": c.trade_count,
            })
        })
        .collect();

    Ok(Json(
        json!({"data": data, "marketId": market_id, "interval": interval}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::*;
    use axum::body::Body;
    use axum::http::Request;

    fn test_block(height: i64) -> BlockRow {
        BlockRow {
            height,
            hash: format!("0x{:064x}", height),
            parent_hash: format!("0x{:064x}", height.saturating_sub(1)),
            timestamp: 1_700_000_000 + height,
            proposer: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            gas_used: 21000,
            gas_limit: 30_000_000,
            base_fee: 1_000_000_000,
            tx_count: 0,
            native_action_count: 0,
            epoch: 0,
            validator_set_hash: String::new(),
            state_root: String::new(),
        }
    }

    fn setup_db() -> ExplorerDb {
        let db = ExplorerDb::open_in_memory().unwrap();
        for h in 0..5 {
            db.insert_block(&test_block(h)).unwrap();
        }
        db.insert_transaction(&TxRow {
            hash: format!("0x{:064x}", 999),
            block_height: 3,
            tx_index: 0,
            from_addr: "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            to_addr: Some("0xcccccccccccccccccccccccccccccccccccccccc".into()),
            value: "0x100".into(),
            gas_limit: 21000,
            gas_used: 21000,
            gas_price: "0x3b9aca00".into(),
            input_data: "0x".into(),
            nonce: 0,
            status: true,
            contract_address: None,
            tx_type: 2,
        })
        .unwrap();
        db
    }

    fn test_router(db: ExplorerDb) -> Router {
        let rpc = NodeRpcClient::new("http://localhost:1").unwrap();
        router(db, rpc)
    }

    async fn get_json(app: &Router, path: &str) -> Value {
        use tower::ServiceExt;
        let req = Request::builder().uri(path).body(Body::empty()).unwrap();
        let resp: axum::response::Response = app.clone().oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn api_list_blocks() {
        let app = test_router(setup_db());
        let json = get_json(&app, "/api/blocks?page=1&limit=3").await;
        assert_eq!(json["total"], 5);
        assert_eq!(json["data"].as_array().unwrap().len(), 3);
        assert_eq!(json["data"][0]["height"], 4);
    }

    #[tokio::test]
    async fn api_get_block() {
        let app = test_router(setup_db());
        let json = get_json(&app, "/api/blocks/3").await;
        assert_eq!(json["block"]["height"], 3);
        assert_eq!(json["transactions"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn api_get_tx() {
        let app = test_router(setup_db());
        let hash = format!("0x{:064x}", 999);
        let json = get_json(&app, &format!("/api/txs/{hash}")).await;
        assert_eq!(json["transaction"]["hash"], hash);
    }

    #[tokio::test]
    async fn api_search_block_height() {
        let app = test_router(setup_db());
        let json = get_json(&app, "/api/search?q=2").await;
        assert_eq!(json["type"], "block");
        assert_eq!(json["result"]["height"], 2);
    }

    #[tokio::test]
    async fn api_search_tx_hash() {
        let app = test_router(setup_db());
        let hash = format!("0x{:064x}", 999);
        let json = get_json(&app, &format!("/api/search?q={hash}")).await;
        assert_eq!(json["type"], "transaction");
    }

    #[tokio::test]
    async fn api_stats() {
        let app = test_router(setup_db());
        let json = get_json(&app, "/api/stats").await;
        assert_eq!(json["total_blocks"], 5);
        assert_eq!(json["total_txs"], 1);
    }

    #[tokio::test]
    async fn api_address_txs() {
        let app = test_router(setup_db());
        let json = get_json(
            &app,
            "/api/addresses/0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/txs?page=1&limit=10",
        )
        .await;
        assert_eq!(json["total"], 1);
    }

    #[tokio::test]
    async fn api_validators_empty() {
        let app = test_router(setup_db());
        let json = get_json(&app, "/api/validators").await;
        assert_eq!(json["data"].as_array().unwrap().len(), 0);
    }
}
