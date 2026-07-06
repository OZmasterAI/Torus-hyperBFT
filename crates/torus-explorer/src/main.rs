use clap::Parser;
use tracing::{error, info};

use torus_explorer::config::Config;
use torus_explorer::db::ExplorerDb;
use torus_explorer::indexer::Indexer;
use torus_explorer::rpc_client::NodeRpcClient;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::parse();
    info!("Starting torus-explorer");
    info!("  RPC:    {}", config.rpc_url);
    info!("  WS:     {}", config.ws_url);
    info!("  DB:     {}", config.db_path);
    info!("  Listen: {}", config.listen);

    let db = ExplorerDb::open(&config.db_path).expect("failed to open database");
    let rpc = NodeRpcClient::new(&config.rpc_url).expect("failed to create RPC client");

    // Backfill from genesis to current height
    let indexer = Indexer::new(rpc.clone(), db.clone(), config.backfill_batch_size);
    if let Err(e) = indexer.backfill().await {
        error!("Backfill error: {e}");
    }

    // Start real-time subscription in background
    let ws_url = config.ws_url.clone();
    let indexer_bg = Indexer::new(rpc.clone(), db.clone(), config.backfill_batch_size);
    tokio::spawn(async move {
        loop {
            if let Err(e) = indexer_bg.subscribe_new_heads(&ws_url).await {
                error!("WebSocket subscription error: {e}, reconnecting in 5s...");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    });

    // Start REST API server
    let app = torus_explorer::api::router(db, rpc);
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .expect("failed to bind listener");
    info!("REST API listening on {}", config.listen);
    axum::serve(listener, app).await.expect("server error");
}
