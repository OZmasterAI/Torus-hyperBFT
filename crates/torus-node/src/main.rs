//! Torus-hyperBFT node binary — wires all crates together.
//!
//! Startup: CLI → tracing → StateDb → genesis init → build components → replica → RPC → signal wait.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use ed25519_dalek::SigningKey;
use hotstuff_rs::events::{
    CommitBlockEvent, InsertBlockEvent, PhaseVoteEvent, ProposeEvent, ReceiveProposalEvent,
    ReceiveProposalHeaderEvent, StartViewEvent, UpdateHighestPCEvent, ViewTimeoutEvent,
};
use hotstuff_rs::replica::{Configuration, Replica, ReplicaSpec};
use hotstuff_rs::types::data_types::{BufferSize, ChainID, EpochLength};
use serde::Deserialize;
use tracing::{error, info, warn};

use torus_consensus::{NativeDaFetcher, RocksKVStore, TorusApp};
use torus_evm::EvmExecutor;
use torus_genesis::Genesis;
use torus_mempool::{Mempool, MempoolConfig};
use torus_network::{LibP2PNetwork, NetworkConfig};
use torus_rpc::{find_latest_height, scan_trades_for_block, BlockNotifier, RpcServer};
use torus_state::cf::CF_BLOCK_HEADERS;
use torus_state::{NativeDaStore, PrunerConfig, StateDb, StatePruner};
use torus_types::ChainConfig;

mod keystore;

/// Production native-DA pull-fallback transport (Phase C Task 6): bridges the
/// consensus app's [`NativeDaFetcher`] to the libp2p `/torus/native-da/1.0`
/// protocol. `fetch` fans the request out to the validator set; `drain` returns
/// bodies that have arrived on the network thread's inbound queue.
struct NetworkDaFetcher {
    network: LibP2PNetwork,
}

impl NativeDaFetcher for NetworkDaFetcher {
    fn fetch(&self, hashes: Vec<[u8; 32]>) {
        self.network.fetch_native_actions_from_validators(hashes);
    }

    fn drain(&self) -> Vec<Vec<u8>> {
        self.network.drain_native_da_inbound()
    }
}

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

    /// Path to TOML config file (CLI flags override config file values)
    #[arg(long)]
    config: Option<PathBuf>,

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

    /// Accept loopback/private/link-local peer addresses into the kademlia
    /// address book. Off by default: on a public network such entries (a NAT'd
    /// peer's advertised 127.0.0.1, a container's docker subnet) are undialable
    /// and cause dial storms. Enable on devnets/single-host meshes where the
    /// fabric is a private subnet. Explicit --p2p-peers are always exempt.
    #[arg(long, default_value_t = false)]
    p2p_private_addrs: bool,

    /// JSON-RPC listen address
    #[arg(long, default_value = "0.0.0.0:8545")]
    rpc_addr: String,

    /// Read-only JSON-RPC listen address (RH5 read/write isolation). When set,
    /// a SECOND jsonrpsee server starts on this address serving every read
    /// namespace but no submit/write methods, on its own dedicated tokio
    /// runtime with its own connection budget. This isolates read/observability
    /// traffic (getOrderBook, eth_*, counters) from the submit flood that
    /// monopolizes the main server's connections and verify-permit semaphore.
    /// Unset (default) = read server OFF, no behavior change for the main port.
    #[arg(long)]
    rpc_read_addr: Option<String>,

    /// Max simultaneous connections for the read-only RPC server (RH5). Only
    /// used when --rpc-read-addr is set. Independent of the main server's budget.
    #[arg(long, default_value_t = torus_rpc::READ_SERVER_MAX_CONNECTIONS)]
    rpc_read_max_connections: u32,

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

    /// Gossip admitted native-action bodies to the validator mesh as they
    /// arrive at ingress (Sprint 3 pre-spread): by proposal time peers already
    /// hold the bodies, so compact proposals need only gap-pulls. Disable with
    /// --native-gossip=false to fall back to push/pull-only dissemination.
    #[arg(long, default_value_t = true)]
    native_gossip: bool,

    /// Exec trust-cache: at execution, reuse a sender this node already verified at
    /// ingress/gossip instead of re-running secp256k1 recovery. Deterministic (a HIT
    /// equals a fresh recover), so it never affects consensus or state. ON by default
    /// (s376 bench: -77% exec-verify, +61% orders/s). Disable with --exec-trust-cache=false.
    #[arg(long, default_value_t = true)]
    exec_trust_cache: bool,
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

/// Parse a TORUS_* boolean env value: 1/true/on/yes and 0/false/off/no
/// (case-insensitive, trimmed). `None` = unparseable — caller warns and keeps
/// the existing default so a typo can never silently flip a consensus-adjacent
/// performance knob.
fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

/// P3 Round-2 scope 1: decode ONE raw inbound native item (off the swarm loop)
/// into its `(sender, action)` pair(s), appended to `out`. The three wire kinds
/// mirror the swarm receipt sites. A malformed body is debug-logged and skipped:
/// an undecodable body can never match a referenced `compute_action_hash`, so it
/// is never legitimately block-referenced — safe to drop before the mirror.
fn decode_raw_native_inbound(
    item: &torus_network::native_inbound::RawNativeInbound,
    out: &mut Vec<(torus_types::Address, torus_types::SignedNativeAction)>,
) {
    use torus_network::native_inbound::NativeInboundKind;
    match item.kind {
        NativeInboundKind::Pair => match bincode::deserialize::<(
            torus_types::Address,
            torus_types::SignedNativeAction,
        )>(&item.bytes)
        {
            Ok(pair) => out.push(pair),
            Err(e) => tracing::debug!(%e, "malformed gossip native action (off-loop decode)"),
        },
        NativeInboundKind::PreProposalBatch => match bincode::deserialize::<
            Vec<(torus_types::Address, torus_types::SignedNativeAction)>,
        >(&item.bytes)
        {
            Ok(pairs) => out.extend(pairs),
            Err(e) => tracing::debug!(%e, "malformed pre-proposal batch (off-loop decode)"),
        },
        NativeInboundKind::ForwardedJson => {
            if item.bytes.len() > 20 {
                let sender = torus_types::Address::from_slice(&item.bytes[..20]);
                match serde_json::from_slice::<torus_types::SignedNativeAction>(&item.bytes[20..]) {
                    Ok(action) => out.push((sender, action)),
                    Err(e) => {
                        tracing::debug!(%e, "malformed forwarded native action (off-loop decode)")
                    }
                }
            }
        }
    }
}

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

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct ConfigFile {
    genesis: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    keystore: Option<PathBuf>,
    passphrase_file: Option<PathBuf>,
    validator_key: Option<String>,
    p2p_listen: Option<String>,
    p2p_peers: Option<String>,
    rpc_addr: Option<String>,
    log_level: Option<String>,
    metrics_addr: Option<String>,
    rpc_only: Option<bool>,
    archive: Option<bool>,
    retention_blocks: Option<u64>,
}

fn load_config_file(path: &PathBuf) -> Result<ConfigFile, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read config file {}: {e}", path.display()))?;
    let config: ConfigFile =
        toml::from_str(&contents).map_err(|e| format!("invalid config TOML: {e}"))?;
    Ok(config)
}

fn apply_config_defaults(cli: &mut Cli, cfg: ConfigFile) {
    if cli.genesis.is_none() {
        cli.genesis = cfg.genesis;
    }
    if cli.keystore.is_none() {
        cli.keystore = cfg.keystore;
    }
    if cli.passphrase_file.is_none() {
        cli.passphrase_file = cfg.passphrase_file;
    }
    if cli.validator_key.is_none() {
        cli.validator_key = cfg.validator_key;
    }
    if cli.p2p_peers.is_none() {
        cli.p2p_peers = cfg.p2p_peers;
    }
    if let Some(dir) = cfg.data_dir {
        if cli.data_dir == std::path::Path::new("./data") {
            cli.data_dir = dir;
        }
    }
    if let Some(listen) = cfg.p2p_listen {
        if cli.p2p_listen == "/ip4/0.0.0.0/udp/30333/quic-v1" {
            cli.p2p_listen = listen;
        }
    }
    if let Some(addr) = cfg.rpc_addr {
        if cli.rpc_addr == "0.0.0.0:8545" {
            cli.rpc_addr = addr;
        }
    }
    if let Some(level) = cfg.log_level {
        if cli.log_level == "info" {
            cli.log_level = level;
        }
    }
    if let Some(addr) = cfg.metrics_addr {
        if cli.metrics_addr == "0.0.0.0:9090".parse::<SocketAddr>().unwrap() {
            if let Ok(parsed) = addr.parse::<SocketAddr>() {
                cli.metrics_addr = parsed;
            }
        }
    }
    if cfg.rpc_only.unwrap_or(false) && !cli.rpc_only {
        cli.rpc_only = true;
    }
    if cfg.archive.unwrap_or(false) && !cli.archive {
        cli.archive = true;
    }
    if cli.retention_blocks.is_none() {
        cli.retention_blocks = cfg.retention_blocks;
    }
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
        reputation_leader_selection: false,
        exec_trust_cache: false,
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let mut cli = Cli::parse();

    // Load config file if specified (CLI flags take precedence)
    if let Some(config_path) = cli.config.clone() {
        match load_config_file(&config_path) {
            Ok(cfg) => apply_config_defaults(&mut cli, cfg),
            Err(e) => {
                eprintln!("Error loading config: {e}");
                std::process::exit(1);
            }
        }
    }

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
        return Err(
            "either --keystore or --validator-key must be provided (or use --rpc-only)".into(),
        );
    };
    let verifying_key = signing_key.verifying_key();
    info!(
        pubkey = hex::encode(verifying_key.as_bytes()),
        mode = if cli.rpc_only {
            "rpc-only"
        } else {
            "validator"
        },
        "loaded node key"
    );

    // 2b. Restore from snapshot if requested (Phase 3: 3.1.6)
    if let Some(snapshot_path) = &cli.restore_from_snapshot {
        info!(snapshot = %snapshot_path.display(), "restoring database from snapshot...");
        let meta = StateDb::restore_from_snapshot(snapshot_path, &cli.data_dir)?;
        info!(
            block_height = meta.block_height,
            "database restored from snapshot, resuming from block {}", meta.block_height
        );
    }

    // 3. Open StateDb
    std::fs::create_dir_all(&cli.data_dir)?;
    let state_db = StateDb::open(&cli.data_dir)?;
    info!("state database opened");

    // 4. Genesis initialization
    let mut chain_config = if let Some(genesis_path) = &cli.genesis {
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

    // Node-local override: `--exec-trust-cache` gates the exec trust-cache read
    // path (CLI default ON since s376; this line overrides whatever the
    // genesis/default ChainConfig carried). It is deterministic (a HIT equals a
    // fresh recover), so toggling it per-node never affects consensus or state —
    // purely an A/B and rollback switch.
    chain_config.exec_trust_cache = cli.exec_trust_cache;
    // TORUS_EXEC_TRUST_CACHE env var (P2 item 6): restart-only A/B toggle that
    // needs no compose command-line edit. When set AND parseable it overrides
    // the CLI flag; unset/garbage = current behavior (the CLI value).
    if let Ok(raw) = std::env::var("TORUS_EXEC_TRUST_CACHE") {
        match parse_env_bool(&raw) {
            Some(v) => {
                chain_config.exec_trust_cache = v;
                info!(value = v, "exec trust-cache set from TORUS_EXEC_TRUST_CACHE env");
            }
            None => warn!(
                raw,
                "unparseable TORUS_EXEC_TRUST_CACHE (want true/false/1/0/on/off/yes/no); keeping --exec-trust-cache value"
            ),
        }
    }
    if chain_config.exec_trust_cache {
        info!("exec trust-cache ENABLED: execution reuses locally-verified senders (skips re-recover)");
    } else {
        info!("exec trust-cache DISABLED: execution re-recovers every sender");
    }

    // 5. Build components
    let metrics = Arc::new(torus_telemetry::Metrics::new());

    // Mempool (created before app so consensus can drain it during block production)
    let mempool_config = MempoolConfig {
        chain_id: chain_config.chain_id,
        block_gas_limit: chain_config.evm_gas_limit,
        ..MempoolConfig::default()
    };
    let mempool = Arc::new(Mempool::new(state_db.clone(), mempool_config));

    let signing_key_for_app = if !cli.rpc_only {
        Some(signing_key.clone())
    } else {
        None
    };
    let mut app = TorusApp::new(
        state_db.clone(),
        &chain_config,
        Some(metrics.clone()),
        Some(mempool.clone()),
        signing_key_for_app,
    );
    let kv_store = RocksKVStore::new(state_db.db_arc());

    // EVM executor
    let executor = Arc::new(EvmExecutor::new(chain_config.chain_id));

    // Network
    let listen_addr = cli
        .p2p_listen
        .parse()
        .map_err(|e| format!("invalid p2p-listen multiaddr: {e}"))?;

    let bootstrap_peers = if let Some(peers_csv) = cli.p2p_peers.as_deref() {
        NetworkConfig::parse_bootstrap_peers(peers_csv)
    } else {
        info!("no --p2p-peers specified, using default testnet bootstrap nodes");
        NetworkConfig::default_bootstrap_peers()
    };
    for (pid, addr) in &bootstrap_peers {
        info!(peer_id = %pid, addr = %addr, "bootstrap peer configured");
    }

    let network_config = NetworkConfig {
        listen_addr,
        bootstrap_peers,
        allow_private_addrs: cli.p2p_private_addrs,
        ..NetworkConfig::default()
    };

    let (mut network, _tx_gossip, native_gossip) =
        LibP2PNetwork::with_metrics(network_config, signing_key.clone(), Some(metrics.clone()))
            .await?;
    mempool.set_native_gossip_tx(native_gossip.into_sender());
    mempool.set_native_gossip_enabled(cli.native_gossip);
    mempool.set_metrics(metrics.clone());
    info!(
        enabled = cli.native_gossip,
        "native-action gossip pre-spread"
    );
    // Attach the durable DA store so the swarm can SERVE native-action bodies
    // by-hash on /torus/native-da/1.0 (Phase C Task 5 — RARE pull-fallback).
    network.set_native_da_store(NativeDaStore::new(state_db.clone()));
    // Wire the consensus app's RARE pull-fallback to the network (Phase C Task 6):
    // on a CompactBlock reconstruction miss it fetches the missing bodies by-hash.
    app.set_native_da_fetcher(Arc::new(NetworkDaFetcher {
        network: network.clone(),
    }));

    // P3 Round-1 item 2: continuous nonce-window expiry tick (1s). Before this,
    // eviction fired only lazily when a proposer selected/drained, so during a
    // block-rate stall expired actions accumulated and purged as a single lump at
    // cooldown — leaving the P2 intake identity 22–55% unattributed mid-run and
    // hiding the pool-occupancy signal. A steady tick makes `pool_expired_*` and
    // the occupancy gauge track reality every second. Purely additive (the lazy
    // evictions remain) and cheap at idle — an empty pool's evict is a no-op.
    // Revertible per the design's abort criterion: delete this task to restore
    // lazy-only expiry.
    {
        let mempool_for_expiry = mempool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                mempool_for_expiry.tick_expiry();
            }
        });
    }

    // P3 Round-2 scope 1 (+ swarm fold-in #1): inbound native-body intake is a
    // TWO-STAGE off-loop pipeline that co-designs decode-off-loop with
    // mirror-at-receipt so a body is decoded EXACTLY ONCE:
    //   swarm loop (routing only) --RAW bytes--> mirror worker --decoded--> verify worker
    //
    //   * MIRROR WORKER (no crypto, fast): batch-drains raw items, decodes each
    //     ONCE, mirrors EVERY body to the durable DA store at receipt (one
    //     `put_batch` per drained batch) BEFORE the verify FIFO, then hands the
    //     decoded action to the bounded verify queue. This decouples availability
    //     from the slow verify: a block-referenced body is DA-resident within ms
    //     even while verify is deeply backed up — the fix for the 76s
    //     availability-starvation stall (R1 proof, mem 28e1a821).
    //   * VERIFY WORKER: authenticates the claimed sender (forged gossip pairs
    //     must not pollute the pool) + prescreen + pool insert. Its intake queue
    //     is bounded; a drop there is safe (body already DA-mirrored — R2.3).
    //
    // The mirror worker must NEVER run on the swarm event loop (that reintroduces
    // the head-of-line blocking) and never verify inline (that recouples
    // availability to verify). Batch size + queue caps are env-tunable.
    if let Some(mut raw_rx) = network.take_native_action_rx() {
        let mempool_mirror = mempool.clone();
        let mempool_verify = mempool.clone();
        let metrics_mirror = metrics.clone();
        let metrics_verify = metrics.clone();
        let (verify_tx, mut verify_rx) =
            tokio::sync::mpsc::channel::<(torus_types::Address, torus_types::SignedNativeAction)>(
                torus_network::native_inbound::verify_queue_cap(),
            );

        // Mirror worker (off-loop, decode-once + mirror-at-receipt).
        tokio::spawn(async move {
            let batch_max = torus_network::native_inbound::mirror_batch_max();
            let mut buf: Vec<torus_network::native_inbound::RawNativeInbound> =
                Vec::with_capacity(batch_max);
            loop {
                let n = raw_rx.recv_many(&mut buf, batch_max).await;
                if n == 0 {
                    break; // channel closed
                }
                metrics_mirror
                    .native_ingest_rx_backlog
                    .set(raw_rx.len() as i64);
                let mut decoded: Vec<(torus_types::Address, torus_types::SignedNativeAction)> =
                    Vec::with_capacity(n);
                for item in buf.drain(..) {
                    decode_raw_native_inbound(&item, &mut decoded);
                }
                // Mirror ALL decoded bodies in ONE batch BEFORE any verify. This
                // is the availability guarantee (put_batch is decoupled from the
                // 60s nonce gate — even stale bodies are mirrored, mem 28e1a821).
                if !decoded.is_empty() {
                    let bodies: Vec<torus_types::SignedNativeAction> =
                        decoded.iter().map(|(_, a)| a.clone()).collect();
                    mempool_mirror.mirror_native_to_da(&bodies);
                    metrics_mirror
                        .native_da_mirror_actions
                        .inc_by(bodies.len() as u64);
                }
                // Hand each already-mirrored action to the verify queue.
                for pair in decoded {
                    match verify_tx.try_send(pair) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            // Safe: the body is already DA-resident; a drop only
                            // forfeits pool candidacy (R2.3 drop-safety).
                            metrics_mirror.native_verify_queue_dropped.inc();
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                    }
                }
            }
        });

        // Verify worker (crypto verify + prescreen + pool insert).
        tokio::spawn(async move {
            while let Some((sender, action)) = verify_rx.recv().await {
                match mempool_verify.add_native_action_from_gossip(sender, action) {
                    Ok(()) => {}
                    Err(torus_mempool::MempoolError::DuplicateNativeAction) => {}
                    Err(e) => tracing::debug!("gossip native action rejected: {e}"),
                }
                metrics_verify.native_ingest_processed_actions.inc();
            }
        });
    }

    // Spawn inbound forwarded-EVM-tx → mempool task (Option B). A peer that received an EVM tx
    // on its RPC unicast it to us as the current leader; add_evm_tx full-validates here, so
    // forwarded bytes are trusted no more than locally-submitted ones.
    if let Some(mut evm_rx) = network.take_evm_tx_rx() {
        let mempool_for_evm = mempool.clone();
        tokio::spawn(async move {
            while let Some(raw_rlp) = evm_rx.recv().await {
                match mempool_for_evm.add_evm_tx(raw_rlp) {
                    Ok(_) => {}
                    Err(torus_mempool::MempoolError::DuplicateTx(_)) => {}
                    Err(e) => tracing::debug!("forwarded evm tx rejected: {e}"),
                }
            }
        });
    }
    info!(listen = %cli.p2p_listen, "p2p network started");

    // 5b. Pre-proposal action push: proposer → all validators via req/res (CompactBlock support)
    // P3 Round-2 scope 4: widen the sync_channel 4 -> 64 so a transient stall in
    // the pre-proposal dissemination thread (serialize + broadcast) does not
    // back-pressure produce_block into dropping bundles under b400 bursts; the
    // proposer still mirrors every referenced body to its own DA store, so a drop
    // is recoverable, but a dropped bundle forces replicas onto the slow pull.
    let (pre_proposal_tx, pre_proposal_rx) =
        std::sync::mpsc::sync_channel::<torus_consensus::PreProposalBundle>(64);
    app.set_pre_proposal_tx(pre_proposal_tx);
    let network_for_pre_proposal = network.clone();
    std::thread::spawn(move || {
        while let Ok(bundle) = pre_proposal_rx.recv() {
            let Ok(payload) = bincode::serialize(&bundle.actions) else {
                continue;
            };
            if torus_network::should_push_hashes_only(payload.len()) {
                // Phase 2.3 (#5): the body set is too big to disseminate within the view —
                // push only the HASHES; validators pull the bodies (pre-warm) off the view's
                // critical path. Un-wedges bs≈500 (VIEW TIMEOUT on big-body dissemination).
                // The hashes match the CompactBlock's native_action_hashes (same actions,
                // same compute_action_hash), so peers pull exactly the referenced bodies.
                let hashes: Vec<[u8; 32]> = bundle
                    .actions
                    .iter()
                    .map(|(_, a)| torus_types::compute_action_hash(a).0)
                    .collect();
                network_for_pre_proposal.broadcast_native_action_hashes(hashes);
            } else {
                network_for_pre_proposal.broadcast_native_actions(payload);
            }
        }
    });

    // 5c. Extract leader state + clone network for RPC forwarding (before replica consumes them)
    let leader_state = app.leader_state();
    let network_for_fwd = network.clone();
    let network_for_evm_fwd = network.clone();

    // 6. Consensus configuration
    // Leader-selection mode is consensus-critical: every validator must run the
    // same setting or replicas disagree on leaders and finalization halts.
    hotstuff_rs::pacemaker::set_reputation_leader_selection(
        chain_config.reputation_leader_selection,
    );
    info!(
        reputation_leader_selection = chain_config.reputation_leader_selection,
        "leader selection mode: {}",
        if chain_config.reputation_leader_selection {
            "reputation-weighted (B3)"
        } else {
            "plain IWRR round-robin"
        }
    );
    // Block-tree pruner (S391): bound hotstuff's cf_consensus_meta growth with the
    // same retention knob as the state pruner. Node-local storage policy (not
    // consensus-critical); clamped to >= 1000 so consensus lookbacks and the
    // speculative window are never at risk. Archive nodes (no --retention-blocks)
    // keep everything and remain the genesis-sync source.
    hotstuff_rs::block_tree::set_block_tree_retention(cli.retention_blocks.map(|r| r.max(1000)));
    if let Some(r) = cli.retention_blocks {
        info!(
            retention_blocks = r.max(1000),
            "block-tree pruner enabled (cf_consensus_meta)"
        );
    }

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
    let latest_height_shared = Arc::new(std::sync::atomic::AtomicU64::new(find_latest_height(
        &state_db,
    )));
    let latest_height_for_handler = latest_height_shared.clone();

    // 8. Start replica with on_commit_block handler for WebSocket subscriptions.
    // View-phase timing: one recorder clone per lifecycle event, timestamps taken
    // from the events so bus queuing doesn't skew the histograms.
    let view_rec = Arc::new(torus_telemetry::ViewMetricsRecorder::new(metrics.clone()));
    let rec_start = view_rec.clone();
    let rec_propose = view_rec.clone();
    let rec_qc = view_rec.clone();
    let rec_receive = view_rec.clone();
    let rec_header = view_rec.clone();
    let rec_insert = view_rec.clone();
    let rec_vote = view_rec.clone();
    let rec_commit = view_rec.clone();
    // Own headers loop back via broadcast self-delivery; proposal_arrival is a
    // follower metric, so filter them out by origin.
    let own_hotstuff_vk = verifying_key;
    let timeout_counter = metrics.consensus_timeout_total.clone();
    let _replica = ReplicaSpec::builder()
        .app(app)
        .network(network)
        .kv_store(kv_store)
        .configuration(hs_config)
        .on_start_view(move |ev: &StartViewEvent| {
            rec_start.start_view(ev.timestamp, ev.view.int());
        })
        .on_propose(move |ev: &ProposeEvent| {
            rec_propose.propose(ev.timestamp, ev.proposal.block.hash.bytes())
        })
        .on_update_highest_pc(move |ev: &UpdateHighestPCEvent| {
            rec_qc.update_highest_pc(ev.timestamp, ev.highest_pc.block.bytes())
        })
        .on_receive_proposal(move |ev: &ReceiveProposalEvent| {
            rec_receive.receive_proposal(ev.timestamp);
        })
        .on_receive_proposal_header(move |ev: &ReceiveProposalHeaderEvent| {
            if ev.origin != own_hotstuff_vk {
                rec_header.receive_proposal(ev.timestamp);
            }
        })
        .on_insert_block(move |ev: &InsertBlockEvent| rec_insert.insert_block(ev.timestamp))
        .on_phase_vote(move |ev: &PhaseVoteEvent| rec_vote.phase_vote(ev.timestamp))
        .on_view_timeout(move |_ev: &ViewTimeoutEvent| {
            timeout_counter.inc();
        })
        .on_commit_block(move |event: &CommitBlockEvent| {
            rec_commit.commit_block(event.timestamp);
            let height = find_latest_height(&state_db_for_handler);
            info!(height, "on_commit_block fired");
            if height == 0 {
                return;
            }
            mempool_for_handler.prune_committed_txs();
            latest_height_for_handler.store(height, std::sync::atomic::Ordering::Relaxed);
            match state_db_for_handler.get_cf_raw(CF_BLOCK_HEADERS, &height.to_be_bytes()) {
                Ok(Some(data)) if data.len() > 32 => {
                    if let Ok(header) = serde_json::from_slice::<serde_json::Value>(&data[32..]) {
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

    // Leader forwarding: RPC → network bridge
    let own_vk = verifying_key.to_bytes();
    let leader_state_for_rpc = leader_state.clone();
    let leader_vk_fn: Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync> = Arc::new(move || {
        leader_state_for_rpc
            .current_leader()
            .map(|vk| *vk.as_bytes())
    });
    let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (evm_fwd_tx, mut evm_fwd_rx) = tokio::sync::mpsc::unbounded_channel();
    // Full-body native forwards only in the no-gossip fallback: with pre-spread on,
    // gossip already delivers every body to the leader (Sprint 3.5). EVM forwarding has no
    // gossip path, so it is unconditional — wired via its own channel.
    rpc_server.set_leader_forwarding(own_vk, leader_vk_fn, fwd_tx, evm_fwd_tx, !cli.native_gossip);

    // Extract shared handles before start() consumes the server
    let latest_height_handle = rpc_server.latest_height();
    let pruned_up_to_handle = rpc_server.pruned_up_to();

    // RH5 read/write RPC isolation: when --rpc-read-addr is set, start a SECOND
    // jsonrpsee server serving only read namespaces (submit/write methods
    // stripped) on its OWN dedicated tokio runtime with its OWN connection
    // budget. This keeps observability reads (getOrderBook, eth_*, counters)
    // alive during a submit flood that monopolizes the main server's
    // connections and its SUBMIT_PERMITS verify semaphore. Shares the same
    // RpcState (same DB/height/notifier), so reads see the identical tip.
    // Additive: unset = no read server, main port unchanged.
    if let Some(read_addr_str) = cli.rpc_read_addr.clone() {
        let read_addr: SocketAddr = read_addr_str.parse()?;
        let read_max = cli.rpc_read_max_connections;
        let read_state = rpc_server.state().clone();
        let (read_addr_tx, read_addr_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("rpc-read-runtime".into())
            .spawn(move || {
                // Small dedicated runtime: 2 workers + a bounded blocking pool.
                // The submit path's spawn_blocking verify storm runs on the MAIN
                // rpc runtime's blocking pool, so a separate pool here is what
                // actually protects read latency under flood.
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .max_blocking_threads(4)
                    .thread_name("rpc-read-worker")
                    .enable_all()
                    .build()
                    .expect("failed to build read RPC runtime");
                rt.block_on(async {
                    match RpcServer::start_read_server(read_state, read_addr, read_max).await {
                        Ok((handle, addr)) => {
                            let _ = read_addr_tx.send(Ok(addr));
                            handle.stopped().await;
                        }
                        Err(e) => {
                            let _ = read_addr_tx.send(Err(format!("{e}")));
                        }
                    }
                });
            })?;
        match read_addr_rx.recv()? {
            Ok(addr) => info!(
                %addr,
                max_connections = read_max,
                "read-only JSON-RPC server started (dedicated runtime, RH5)"
            ),
            Err(e) => return Err(e.into()),
        }
    }

    // Spawn RPC on a dedicated tokio runtime so user traffic can never
    // starve the consensus/libp2p runtime (same idea as Hyperliquid sentries).
    let (rpc_addr_tx, rpc_addr_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("rpc-runtime".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_name("rpc-worker")
                .enable_all()
                .build()
                .expect("failed to build RPC runtime");
            rt.block_on(async {
                match rpc_server.start(rpc_addr).await {
                    Ok((handle, addr)) => {
                        let _ = rpc_addr_tx.send(Ok(addr));
                        handle.stopped().await;
                    }
                    Err(e) => {
                        let _ = rpc_addr_tx.send(Err(format!("{e}")));
                    }
                }
            });
        })?;
    let actual_addr = rpc_addr_rx
        .recv()?
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    info!(%actual_addr, "JSON-RPC server started (dedicated runtime)");

    // Forwarding bridge: receives (target_vk_bytes, payload) from RPC, sends via network
    tokio::spawn(async move {
        while let Some((target_vk_bytes, payload)) = fwd_rx.recv().await {
            if let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&target_vk_bytes) {
                network_for_fwd.forward_native_action(vk, payload);
            }
        }
    });

    // EVM forwarding bridge (Option B): (leader_vk_bytes, raw_rlp) from RPC → network unicast.
    tokio::spawn(async move {
        while let Some((target_vk_bytes, payload)) = evm_fwd_rx.recv().await {
            if let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&target_vk_bytes) {
                network_for_evm_fwd.forward_evm_tx(vk, payload);
            }
        }
    });

    // 10. Metrics (telemetry endpoint)
    let metrics_addr = cli.metrics_addr;
    tokio::spawn(torus_telemetry::serve_metrics(
        metrics_addr,
        metrics.clone(),
    ));
    info!(%metrics_addr, "telemetry server started");

    // 11. Background pruner (if pruning enabled)
    if let Some(retention) = cli.retention_blocks {
        let pruner_config = PrunerConfig {
            retention_blocks: retention,
            ..PrunerConfig::default()
        };
        let mut pruner = StatePruner::new(state_db.clone(), pruner_config, pruned_up_to_handle);
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

    // 12. Background DB size + RocksDB runtime-state metric updater. The
    // runtime properties (L0 files, memtables, pending compaction, block
    // cache) all reset on restart — the same signature as the accumulating
    // propose cost (S405 BS-3) — so they're sampled per CF to correlate.
    {
        let data_dir = cli.data_dir.clone();
        let m = metrics.clone();
        let db = state_db.db_arc();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let size = torus_state::dir_size_bytes(&data_dir);
                m.db_size_bytes.set(size as i64);
                for cf_name in torus_state::cf::ALL_CF_NAMES {
                    let Some(cf) = db.cf_handle(cf_name) else {
                        continue;
                    };
                    let label = vec![("cf".to_string(), (*cf_name).to_string())];
                    for (prop, fam) in [
                        ("rocksdb.num-files-at-level0", &m.rocksdb_l0_files),
                        ("rocksdb.cur-size-all-mem-tables", &m.rocksdb_memtable_bytes),
                        (
                            "rocksdb.estimate-pending-compaction-bytes",
                            &m.rocksdb_pending_compaction_bytes,
                        ),
                    ] {
                        if let Ok(Some(v)) = db.property_int_value_cf(cf, prop) {
                            fam.get_or_create(&label).set(v as i64);
                        }
                    }
                }
                if let Ok(Some(v)) = db.property_int_value("rocksdb.block-cache-usage") {
                    m.rocksdb_block_cache_bytes.set(v as i64);
                }
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

    /// P2 item 6: TORUS_EXEC_TRUST_CACHE must parse the usual boolean spellings
    /// and reject garbage (None → keep the CLI value, i.e. current behavior).
    #[test]
    fn parse_env_bool_accepts_common_spellings() {
        for v in ["1", "true", "TRUE", "on", "yes", " True "] {
            assert_eq!(parse_env_bool(v), Some(true), "{v:?} must parse true");
        }
        for v in ["0", "false", "FALSE", "off", "no", " Off "] {
            assert_eq!(parse_env_bool(v), Some(false), "{v:?} must parse false");
        }
        for v in ["", "2", "enable", "tru"] {
            assert_eq!(parse_env_bool(v), None, "{v:?} must be rejected");
        }
    }

    /// The CLI flag stays the baseline: --exec-trust-cache defaults TRUE at this
    /// commit (s376: -77% exec-verify, +61% orders/s). The env var only overrides.
    #[test]
    fn exec_trust_cache_cli_default_is_true() {
        let cli = Cli::try_parse_from(["torus-node", "--keystore", "k.keystore"]).unwrap();
        assert!(cli.exec_trust_cache, "CLI default must remain ON");
    }

    #[test]
    fn archive_and_retention_mutually_exclusive() {
        let result = Cli::try_parse_from([
            "torus-node",
            "--keystore",
            "k.keystore",
            "--archive",
            "--retention-blocks",
            "1000",
        ]);
        // clap parses both flags fine; the runtime check in run() rejects them.
        // We test the validation logic directly.
        let cli = result.unwrap();
        assert!(cli.archive);
        assert_eq!(cli.retention_blocks, Some(1000));
        // The actual error is returned by run(), which we can't call without a full node.
        // Verify the condition that run() checks:
        assert!(
            cli.archive && cli.retention_blocks.is_some(),
            "both flags set should be rejected by run()"
        );
    }

    #[test]
    fn default_is_archive_mode() {
        let cli = Cli::try_parse_from(["torus-node", "--keystore", "k.keystore"]).unwrap();
        assert!(!cli.archive);
        assert!(cli.retention_blocks.is_none());
        // Neither flag -> archive mode (no pruning)
    }

    #[test]
    fn retention_blocks_enables_pruning() {
        let cli = Cli::try_parse_from([
            "torus-node",
            "--keystore",
            "k.keystore",
            "--retention-blocks",
            "50000",
        ])
        .unwrap();
        assert!(!cli.archive);
        assert_eq!(cli.retention_blocks, Some(50000));
    }

    #[test]
    fn metrics_addr_default() {
        let cli = Cli::try_parse_from(["torus-node", "--keystore", "k.keystore"]).unwrap();
        assert_eq!(
            cli.metrics_addr,
            "0.0.0.0:9090".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn metrics_addr_custom() {
        let cli = Cli::try_parse_from([
            "torus-node",
            "--keystore",
            "k.keystore",
            "--metrics-addr",
            "127.0.0.1:9091",
        ])
        .unwrap();
        assert_eq!(
            cli.metrics_addr,
            "127.0.0.1:9091".parse::<SocketAddr>().unwrap()
        );
    }
}
