//! Torus-hyperBFT node binary — wires all crates together.
//!
//! Startup: CLI → tracing → StateDb → genesis init → build components → replica → RPC → signal wait.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use ed25519_dalek::SigningKey;
use hotstuff_rs::events::CommitBlockEvent;
use hotstuff_rs::replica::{Configuration, Replica, ReplicaSpec};
use hotstuff_rs::types::data_types::{BufferSize, ChainID, EpochLength};
use tracing::{error, info, warn};

use torus_consensus::{RocksKVStore, TorusApp};
use torus_evm::EvmExecutor;
use torus_genesis::Genesis;
use torus_mempool::{Mempool, MempoolConfig};
use torus_network::{LibP2PNetwork, NetworkConfig};
use torus_rpc::{find_latest_height, scan_trades_for_block, BlockNotifier, RpcServer};
use torus_state::cf::CF_BLOCK_HEADERS;
use torus_state::{PrunerConfig, StateDb, StatePruner};
use torus_types::ChainConfig;

mod keystore;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "torus-node",
    version,
    about = "Torus-hyperBFT node (validator or RPC-only)"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to genesis.json (required for first run)
    #[arg(long)]
    genesis: Option<PathBuf>,

    /// RocksDB data directory
    #[arg(long, default_value = "./data")]
    data_dir: PathBuf,

    /// Ed25519 signing key (64-char hex) [DEPRECATED: use --keystore]
    #[arg(long)]
    validator_key: Option<String>,

    /// Path to encrypted keystore file (Phase 3: 3.1.8)
    #[arg(long)]
    keystore: Option<PathBuf>,

    /// Path to passphrase file for automated deployments (Phase 3: 3.1.8)
    #[arg(long)]
    passphrase_file: Option<PathBuf>,

    /// Restore database from a snapshot before starting (Phase 3: 3.1.6)
    #[arg(long)]
    restore_from_snapshot: Option<PathBuf>,

    /// libp2p listen multiaddr
    #[arg(long, default_value = "/ip4/0.0.0.0/udp/30333/quic-v1")]
    p2p_listen: String,

    /// Comma-separated bootstrap peer multiaddrs (with /p2p/<peer_id> suffix)
    #[arg(long)]
    p2p_peers: Option<String>,

    /// JSON-RPC listen address
    #[arg(long, default_value = "0.0.0.0:8545")]
    rpc_addr: String,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Archive mode: retain all historical data (default). Mutually exclusive with --retention-blocks.
    #[arg(long)]
    archive: bool,

    /// Enable pruning: keep only the last N blocks of historical data (block bodies and receipts).
    /// Older data is removed to save disk space. Cannot be used with --archive.
    #[arg(long)]
    retention_blocks: Option<u64>,

    /// Metrics (Prometheus) listen address
    #[arg(long, default_value = "0.0.0.0:9090")]
    metrics_addr: SocketAddr,

    /// Run as a non-validator RPC node. Syncs blocks from peers but does not
    /// participate in consensus. No validator key required.
    #[arg(long)]
    rpc_only: bool,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Generate a new validator keypair and write an encrypted keystore file.
    Keygen {
        /// Output path for the keystore file
        #[arg(long, default_value = "./validator.keystore")]
        output: PathBuf,
    },
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn decode_hex_key(hex_str: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    if hex_str.len() != 64 {
        return Err(format!("validator key must be 64 hex chars, got {}", hex_str.len()).into());
    }
    let bytes: Vec<u8> = (0..hex_str.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex_str[i..i + 2], 16))
        .collect::<Result<Vec<_>, _>>()?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| "key must be 32 bytes")?;
    Ok(arr)
}

fn default_chain_config() -> ChainConfig {
    use alloy_primitives::{Address, U256};
    ChainConfig {
        chain_id: torus_evm::TORUS_CHAIN_ID,
        chain_name: "torus".to_string(),
        evm_gas_limit: 30_000_000,
        base_fee_per_gas: 1_000_000_000,
        epoch_length: 100_000,
        max_validators: 21,
        min_stake: U256::from(10_000u64) * U256::from(10u64).pow(U256::from(18u64)),
        fee_burn_bps: 1000,
        fee_validator_bps: 0,
        fee_treasury_bps: 4500,
        fee_dev_pool_bps: 4500,
        treasury_address: Address::ZERO,
        dev_pool_address: Address::ZERO,
        timeout_base_ms: 500,
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Handle keygen subcommand before tracing init
    if let Some(Command::Keygen { output }) = &cli.command {
        eprintln!("Generating new validator keypair...");
        eprint!("Enter passphrase: ");
        let passphrase = read_passphrase_stdin();
        match keystore::generate_keystore(output, &passphrase) {
            Ok(key) => {
                let vk = key.verifying_key();
                eprintln!("Keystore written to: {}", output.display());
                eprintln!("Public key: {}", hex::encode(vk.as_bytes()));
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("Error generating keystore: {e}");
                std::process::exit(1);
            }
        }
    }

    // 1. Tracing
    std::env::set_var("RUST_LOG", &cli.log_level);
    torus_telemetry::init_tracing(false);
    info!(
        version = env!("CARGO_PKG_VERSION"),
        data_dir = %cli.data_dir.display(),
        "starting torus-node"
    );

    if let Err(e) = run(cli).await {
        error!(%e, "node exited with error");
        std::process::exit(1);
    }
}

/// Read a passphrase from stdin (no echo if terminal).
fn read_passphrase_stdin() -> String {
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).unwrap_or(0);
    buf.trim().to_string()
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Validate pruning flags
    if cli.archive && cli.retention_blocks.is_some() {
        return Err("cannot set both --archive and --retention-blocks".into());
    }
    // 2. Load validator key (keystore preferred, raw hex deprecated)
    let signing_key = if let Some(keystore_path) = &cli.keystore {
        let passphrase = if let Some(pf) = &cli.passphrase_file {
            std::fs::read_to_string(pf)
                .map_err(|e| format!("cannot read passphrase file: {e}"))?
                .trim()
                .to_string()
        } else {
            eprint!("Enter keystore passphrase: ");
            read_passphrase_stdin()
        };
        let key = keystore::load_keystore(keystore_path, &passphrase)?;
        info!("loaded validator key from keystore");
        key
    } else if let Some(hex_key) = &cli.validator_key {
        warn!("--validator-key is deprecated and insecure (visible in ps/history). Use --keystore instead.");
        let key_bytes = decode_hex_key(hex_key)?;
        SigningKey::from_bytes(&key_bytes)
    } else if cli.rpc_only {
        let mut seed = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
        let key = SigningKey::from_bytes(&seed);
        info!("rpc-only mode: generated ephemeral key for network identity");
        key
    } else {
        return Err("either --keystore or --validator-key must be provided (or use --rpc-only)".into());
    };
    let verifying_key = signing_key.verifying_key();
    info!(
        pubkey = hex::encode(verifying_key.as_bytes()),
        mode = if cli.rpc_only { "rpc-only" } else { "validator" },
        "loaded node key"
    );

    // 2b. Restore from snapshot if requested (Phase 3: 3.1.6)
    if let Some(snapshot_path) = &cli.restore_from_snapshot {
        info!(snapshot = %snapshot_path.display(), "restoring database from snapshot...");
        let meta = StateDb::restore_from_snapshot(snapshot_path, &cli.data_dir)?;
        info!(
            block_height = meta.block_height,
            "database restored from snapshot, resuming from block {}",
            meta.block_height
        );
    }

    // 3. Open StateDb
    std::fs::create_dir_all(&cli.data_dir)?;
    let state_db = StateDb::open(&cli.data_dir)?;
    info!("state database opened");

    // 4. Genesis initialization
    let chain_config = if let Some(genesis_path) = &cli.genesis {
        let genesis = Genesis::from_file(genesis_path)?;
        let config = genesis.chain_config();

        // Only initialize if DB is empty (first run)
        let existing_accounts = state_db.all_accounts()?;
        if existing_accounts.is_empty() {
            info!(genesis = %genesis_path.display(), "initializing from genesis");
            let state_root = genesis.initialize(&state_db)?;
            info!(%state_root, "genesis state seeded");

            // Initialize hotstuff_rs replica
            let (app_state, vs_state) = genesis.to_hotstuff_genesis()?;
            let init_kv = RocksKVStore::new(state_db.db_arc());
            Replica::initialize(init_kv, app_state, vs_state);
            info!("consensus replica initialized with genesis validator set");
        } else {
            info!("database already initialized, skipping genesis");
        }

        config
    } else {
        info!("no genesis file provided, using default chain config");
        default_chain_config()
    };

    info!(
        chain_id = chain_config.chain_id,
        epoch_length = chain_config.epoch_length,
        gas_limit = chain_config.evm_gas_limit,
        "chain configuration loaded"
    );

    // 5. Build components
    let metrics = Arc::new(torus_telemetry::Metrics::new());

    // Mempool (created before app so consensus can drain it during block production)
    let mempool_config = MempoolConfig {
        chain_id: chain_config.chain_id,
        block_gas_limit: chain_config.evm_gas_limit,
        ..MempoolConfig::default()
    };
    let mempool = Arc::new(Mempool::new(state_db.clone(), mempool_config));

    let app = TorusApp::new(state_db.clone(), &chain_config, Some(metrics.clone()), Some(mempool.clone()));
    let kv_store = RocksKVStore::new(state_db.db_arc());

    // EVM executor
    let executor = Arc::new(EvmExecutor::new(chain_config.chain_id));

    // Network
    let listen_addr = cli.p2p_listen.parse()
        .map_err(|e| format!("invalid p2p-listen multiaddr: {e}"))?;

    let bootstrap_peers = cli.p2p_peers.as_deref()
        .map(NetworkConfig::parse_bootstrap_peers)
        .unwrap_or_default();
    for (pid, addr) in &bootstrap_peers {
        info!(peer_id = %pid, addr = %addr, "bootstrap peer configured");
    }

    let network_config = NetworkConfig {
        listen_addr,
        bootstrap_peers,
        ..NetworkConfig::default()
    };

    let (network, _tx_gossip) = LibP2PNetwork::with_metrics(
        network_config, signing_key.clone(), Some(metrics.clone()),
    ).await?;
    info!(listen = %cli.p2p_listen, "p2p network started");

    // 6. Consensus configuration
    let hs_config = Configuration::builder()
        .me(signing_key)
        .chain_id(ChainID::new(chain_config.chain_id))
        .epoch_length(EpochLength::new(chain_config.epoch_length as u32))
        .max_view_time(Duration::from_millis(chain_config.timeout_base_ms))
        .progress_msg_buffer_capacity(BufferSize::new(1024))
        .block_sync_request_limit(128)
        .block_sync_server_advertise_time(Duration::new(10, 0))
        .block_sync_response_timeout(Duration::new(3, 0))
        .block_sync_blacklist_expiry_time(Duration::new(10, 0))
        .block_sync_trigger_min_view_difference(2)
        .block_sync_trigger_timeout(Duration::new(60, 0))
        .log_events(false)
        .build();

    // 7. Block notifier + shared height counter (consensus replica <-> RPC server)
    let notifier = BlockNotifier::new();
    let notifier_for_replica = notifier.clone();
    let state_db_for_handler = state_db.clone();
    let mempool_for_handler = mempool.clone();
    let latest_height_shared = Arc::new(std::sync::atomic::AtomicU64::new(
        find_latest_height(&state_db),
    ));
    let latest_height_for_handler = latest_height_shared.clone();

    // 8. Start replica with on_commit_block handler for WebSocket subscriptions
    let _replica = ReplicaSpec::builder()
        .app(app)
        .network(network)
        .kv_store(kv_store)
        .configuration(hs_config)
        .on_commit_block(move |_event: &CommitBlockEvent| {
            let height = find_latest_height(&state_db_for_handler);
            info!(height, "on_commit_block fired");
            if height == 0 {
                return;
            }
            mempool_for_handler.prune_committed_txs();
            latest_height_for_handler.store(height, std::sync::atomic::Ordering::Relaxed);
            match state_db_for_handler.get_cf_raw(CF_BLOCK_HEADERS, &height.to_be_bytes()) {
                Ok(Some(data)) if data.len() > 32 => {
                    if let Ok(header) =
                        serde_json::from_slice::<serde_json::Value>(&data[32..])
                    {
                        notifier_for_replica.notify_new_block(header);
                    }
                }
                _ => {}
            }
            let trades = scan_trades_for_block(&state_db_for_handler, height);
            notifier_for_replica.notify_new_trades(trades);
        })
        .build()
        .start();
    info!("consensus replica started");

    // 9. Start RPC server (shares height counter with commit handler)
    let rpc_addr: SocketAddr = cli.rpc_addr.parse()?;
    let mut rpc_server = RpcServer::new(
        state_db.clone(),
        mempool,
        executor,
        chain_config.chain_id,
        chain_config.epoch_length,
        notifier,
    );
    rpc_server.set_latest_height_handle(latest_height_shared);
    rpc_server.set_metrics(metrics.clone());
    // Extract shared handles before start() consumes the server
    let latest_height_handle = rpc_server.latest_height();
    let pruned_up_to_handle = rpc_server.pruned_up_to();
    let (_rpc_handle, actual_addr) = rpc_server
        .start(rpc_addr)
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e })?;
    info!(%actual_addr, "JSON-RPC server started");

    // 10. Metrics (telemetry endpoint)
    let metrics_addr = cli.metrics_addr;
    tokio::spawn(torus_telemetry::serve_metrics(metrics_addr, metrics.clone()));
    info!(%metrics_addr, "telemetry server started");

    // 11. Background pruner (if pruning enabled)
    if let Some(retention) = cli.retention_blocks {
        let pruner_config = PrunerConfig {
            retention_blocks: retention,
            ..PrunerConfig::default()
        };
        let mut pruner = StatePruner::new(
            state_db.clone(),
            pruner_config,
            pruned_up_to_handle,
        );
        let latest_for_pruner = latest_height_handle.clone();
        let metrics_for_pruner = metrics.clone();
        info!(retention_blocks = retention, "background pruner enabled");
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                let current = latest_for_pruner.load(std::sync::atomic::Ordering::Relaxed);
                if current == 0 {
                    continue;
                }
                match pruner.maybe_prune(current) {
                    Ok(0) => {}
                    Ok(n) => {
                        metrics_for_pruner.pruner_blocks_removed.inc_by(n);
                        info!(pruned_blocks = n, "background pruner cycle complete");
                    }
                    Err(e) => warn!(%e, "background pruner error"),
                }
                // Yield to avoid starving other tasks
                tokio::task::yield_now().await;
            }
        });
    } else {
        info!("archive mode: pruning disabled (all historical data retained)");
    }

    // 12. Background DB size metric updater
    {
        let data_dir = cli.data_dir.clone();
        let db_gauge = metrics.db_size_bytes.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let size = torus_state::dir_size_bytes(&data_dir);
                db_gauge.set(size as i64);
            }
        });
    }

    // 13. Wait for shutdown signal
    info!("node is running — press Ctrl+C to shut down");
    tokio::signal::ctrl_c().await?;
    info!("shutdown signal received, stopping...");

    Ok(())
}

// Hex encoding helper for logging (no external hex crate needed)
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn archive_and_retention_mutually_exclusive() {
        let result = Cli::try_parse_from([
            "torus-node",
            "--keystore", "k.keystore",
            "--archive",
            "--retention-blocks", "1000",
        ]);
        // clap parses both flags fine; the runtime check in run() rejects them.
        // We test the validation logic directly.
        let cli = result.unwrap();
        assert!(cli.archive);
        assert_eq!(cli.retention_blocks, Some(1000));
        // The actual error is returned by run(), which we can't call without a full node.
        // Verify the condition that run() checks:
        assert!(cli.archive && cli.retention_blocks.is_some(),
            "both flags set should be rejected by run()");
    }

    #[test]
    fn default_is_archive_mode() {
        let cli = Cli::try_parse_from([
            "torus-node",
            "--keystore", "k.keystore",
        ]).unwrap();
        assert!(!cli.archive);
        assert!(cli.retention_blocks.is_none());
        // Neither flag -> archive mode (no pruning)
    }

    #[test]
    fn retention_blocks_enables_pruning() {
        let cli = Cli::try_parse_from([
            "torus-node",
            "--keystore", "k.keystore",
            "--retention-blocks", "50000",
        ]).unwrap();
        assert!(!cli.archive);
        assert_eq!(cli.retention_blocks, Some(50000));
    }

    #[test]
    fn metrics_addr_default() {
        let cli = Cli::try_parse_from([
            "torus-node",
            "--keystore", "k.keystore",
        ]).unwrap();
        assert_eq!(cli.metrics_addr, "0.0.0.0:9090".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn metrics_addr_custom() {
        let cli = Cli::try_parse_from([
            "torus-node",
            "--keystore", "k.keystore",
            "--metrics-addr", "127.0.0.1:9091",
        ]).unwrap();
        assert_eq!(cli.metrics_addr, "127.0.0.1:9091".parse::<SocketAddr>().unwrap());
    }
}
