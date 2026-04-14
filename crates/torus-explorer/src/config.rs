use clap::Parser;

#[derive(Parser, Clone, Debug)]
#[command(name = "torus-explorer", about = "Block explorer backend for Torus-hyperBFT")]
pub struct Config {
    /// JSON-RPC HTTP endpoint of the Torus node
    #[arg(long, default_value = "http://localhost:8545")]
    pub rpc_url: String,

    /// WebSocket endpoint for real-time subscriptions
    #[arg(long, default_value = "ws://localhost:8545")]
    pub ws_url: String,

    /// Path to the SQLite database file
    #[arg(long, default_value = "./explorer.db")]
    pub db_path: String,

    /// Listen address for the REST API
    #[arg(long, default_value = "0.0.0.0:3001")]
    pub listen: String,

    /// Number of blocks to fetch per backfill batch
    #[arg(long, default_value_t = 100)]
    pub backfill_batch_size: u32,
}
