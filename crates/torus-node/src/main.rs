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
    CollectPCEvent, CommitBlockEvent, InsertBlockEvent, PhaseVoteEvent, ProposeEvent,
    ReceivePhaseVoteEvent, ReceiveProposalEvent, ReceiveProposalHeaderEvent, StartViewEvent,
    UpdateHighestPCEvent, ViewTimeoutEvent,
};
use hotstuff_rs::replica::{Configuration, ReplicaSpec};
use hotstuff_rs::types::data_types::{BufferSize, ChainID, EpochLength};
use serde::Deserialize;
use tracing::{error, info, warn};

use torus_consensus::{NativeDaFetcher, TorusApp};
use torus_evm::EvmExecutor;
use torus_genesis::Genesis;
use torus_mempool::{Mempool, MempoolConfig};
use torus_network::{LibP2PNetwork, NetworkConfig};
use torus_rpc::{find_latest_height, BlockNotifier, RpcServer};
use torus_state::cf::{CF_BLOCK_HEADERS, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
use torus_state::{PrunerConfig, StateDb, StatePruner};
use torus_types::ChainConfig;

mod allocator;
mod forward_batcher;
mod keystore;
mod node_dbs;

// Process allocator (default feature `jemalloc`, Linux): jemalloc for the Rust
// heap and, unprefixed, for C/C++ malloc (RocksDB). Only this binary; the
// helper bins (torus-unwedge, torus-wedge-inject) set no global allocator.
#[cfg(all(feature = "jemalloc", target_os = "linux"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

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

/// Production erasure-shard recovery transport (T8-integration inc 3): bridges the
/// consensus app's [`torus_consensus::app::ShardFetcher`] to the libp2p
/// `/torus/native-da-shards/1.0` protocol. `fetch_shards` fans one shard-index
/// request out to each distinct validator (k shards from k distinct SOURCES);
/// `drain_shards` converts the network layer's `(source, StoredShard)` tuples —
/// the network crate cannot depend on torus-consensus — into `GatheredShard`s
/// for reconstruction.
struct NetworkShardFetcher {
    network: LibP2PNetwork,
}

impl torus_consensus::app::ShardFetcher for NetworkShardFetcher {
    fn fetch_shards(&self, body_hash: [u8; 32], want: u16) {
        self.network.fetch_shards_from_validators(body_hash, want);
    }

    fn drain_shards(
        &self,
    ) -> Vec<torus_consensus::shard_recovery::GatheredShard<torus_consensus::app::ShardSource>> {
        self.network
            .drain_native_da_shards_inbound()
            .into_iter()
            .map(|(source, shard)| torus_consensus::shard_recovery::GatheredShard { source, shard })
            .collect()
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

    /// Trade-history retention (D13): keep only the trade rows (getTradeHistory,
    /// getTradeHistoryRange, getBlockTrades, getUserTrades) of the last N blocks.
    /// Unset = keep all trade history. Independent of --retention-blocks; never
    /// prunes at or above the durable applied height. Cannot be used with --archive.
    #[arg(long)]
    trade_retention_blocks: Option<u64>,

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
    /// --native-gossip=false to fall back to push/pull-only dissemination
    /// (activates RPC direct-to-leader body forwarding).
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    native_gossip: bool,

    /// Exec trust-cache: at execution, reuse a sender this node already verified at
    /// ingress/gossip instead of re-running secp256k1 recovery. Deterministic (a HIT
    /// equals a fresh recover), so it never affects consensus or state. ON by default
    /// (s376 bench: -77% exec-verify, +61% orders/s). Disable with --exec-trust-cache=false.
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    exec_trust_cache: bool,

    /// Running state hash: path to a file holding this validator's ACCOUNT key
    /// (secp256k1, 64 hex chars, optional 0x — the address it is registered
    /// under, NOT the ed25519 consensus key). When set, the node signs and
    /// submits `AttestStateHash` for every durable checkpoint (every 100
    /// blocks). A file, not a value, so the key never shows in `ps`. Unset:
    /// the node never attests.
    #[arg(long)]
    state_hash_attest_key: Option<PathBuf>,
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

/// Load `--state-hash-attest-key` (see the flag). The file must be
/// owner-only (no group/other permission bits, i.e. 0600 or stricter); the
/// file text and the decoded bytes are zeroized after the key is built (the
/// `SigningKey` zeroizes itself on drop).
fn load_state_hash_attest_key(
    path: &std::path::Path,
) -> Result<k256::ecdsa::SigningKey, Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map_err(|e| format!("cannot read --state-hash-attest-key {}: {e}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "--state-hash-attest-key {} is accessible by group/other (mode {:o}); \
             restrict it to the owner: chmod 0600 {}",
            path.display(),
            mode & 0o777,
            path.display()
        )
        .into());
    }
    let text = zeroize::Zeroizing::new(
        std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read --state-hash-attest-key {}: {e}", path.display()))?,
    );
    let bytes = zeroize::Zeroizing::new(decode_hex_key(text.trim())?);
    Ok(k256::ecdsa::SigningKey::from_slice(bytes.as_slice())
        .map_err(|e| format!("invalid --state-hash-attest-key: {e}"))?)
}

fn decode_hex_key(hex_str: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    if !hex_str.is_ascii() {
        return Err("validator key must be 64 hex chars (non-ASCII input)".into());
    }
    if hex_str.len() != 64 {
        return Err(format!("validator key must be 64 hex chars, got {}", hex_str.len()).into());
    }
    let mut arr = [0u8; 32];
    for (i, byte) in arr.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex_str[2 * i..2 * i + 2], 16)?;
    }
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
    trade_retention_blocks: Option<u64>,
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
    if cli.trade_retention_blocks.is_none() {
        cli.trade_retention_blocks = cfg.trade_retention_blocks;
    }
}

/// The background pruner's settings from the retention flags, or `None` when
/// neither `--retention-blocks` nor `--trade-retention-blocks` is set (archive:
/// nothing pruned). Each flag prunes only its own data: bodies / receipts
/// without `--retention-blocks` get `u64::MAX` (cutoff 0, kept), trade history
/// without `--trade-retention-blocks` gets `None` (kept).
fn pruner_config(cli: &Cli) -> Result<Option<PrunerConfig>, String> {
    if cli.archive && cli.retention_blocks.is_some() {
        return Err("cannot set both --archive and --retention-blocks".into());
    }
    if cli.archive && cli.trade_retention_blocks.is_some() {
        return Err("cannot set both --archive and --trade-retention-blocks".into());
    }
    if cli.retention_blocks.is_none() && cli.trade_retention_blocks.is_none() {
        return Ok(None);
    }
    Ok(Some(PrunerConfig {
        retention_blocks: cli.retention_blocks.unwrap_or(u64::MAX),
        trade_retention_blocks: cli.trade_retention_blocks,
        ..PrunerConfig::default()
    }))
}

fn default_chain_config() -> ChainConfig {
    use alloy_primitives::{Address, U256};
    ChainConfig {
        chain_id: torus_evm::TORUS_CHAIN_ID,
        chain_name: "torus".to_string(),
        evm_gas_limit: torus_types::DEFAULT_EVM_GAS_LIMIT,
        evm_block_interval: torus_types::DEFAULT_EVM_BLOCK_INTERVAL,
        evm_big_block_interval: torus_types::DEFAULT_EVM_BIG_BLOCK_INTERVAL,
        evm_big_gas_limit: torus_types::DEFAULT_EVM_BIG_GAS_LIMIT,
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
        timeout_base_ms: 1200,
        backoff_factor: 2,
        backoff_cap: 8,
        commit_lag_backoff_cap: 0,
        reputation_leader_selection: false,
        exec_trust_cache: false,
        state_hash_activation_height: None,
    }
}

/// S470: effective commit-lag backoff cap — the genesis value, overridable for
/// DEVNETS via `TORUS_COMMIT_LAG_BACKOFF_CAP` (a well-formed u32 wins; unset,
/// empty, or garbage falls back to genesis). The genesis default is 0 = off =
/// deadline schedule byte-identical to pre-S470.
///
/// FLEET-UNIFORM WARNING: this knob is consensus-liveness critical. If the env
/// override is used, it must be set to the SAME value on every validator of
/// the chain — divergent values mean divergent view-deadline schedules and
/// degraded liveness (premature Bracha timeouts), though never a safety
/// violation. Production chains should set it in genesis only.
fn resolve_commit_lag_cap(genesis_value: u32, env_value: Option<&str>) -> u32 {
    env_value
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(genesis_value)
}

#[cfg(test)]
mod commit_lag_cap_tests {
    use super::resolve_commit_lag_cap;

    /// S470 knob defaults: unset / empty / garbage env and an absent genesis
    /// field (=> genesis_value 0) all resolve to 0 — the exact-today schedule.
    #[test]
    fn resolve_commit_lag_cap_defaults_off() {
        assert_eq!(resolve_commit_lag_cap(0, None), 0);
        assert_eq!(resolve_commit_lag_cap(0, Some("")), 0);
        assert_eq!(resolve_commit_lag_cap(0, Some("garbage")), 0);
        assert_eq!(resolve_commit_lag_cap(0, Some("-1")), 0);
        assert_eq!(resolve_commit_lag_cap(0, Some("0")), 0);
    }

    /// Env override wins over genesis when well-formed; garbage falls back.
    #[test]
    fn resolve_commit_lag_cap_env_override() {
        assert_eq!(resolve_commit_lag_cap(0, Some("4")), 4);
        assert_eq!(resolve_commit_lag_cap(8, Some("0")), 0);
        assert_eq!(resolve_commit_lag_cap(8, None), 8);
        assert_eq!(resolve_commit_lag_cap(8, Some("not-a-number")), 8);
    }
}

/// macOS `OPEN_MAX` (<sys/syslimits.h>; not exported by libc): setrlimit
/// rejects a soft NOFILE above it even when the hard limit is RLIM_INFINITY.
#[cfg(target_os = "macos")]
const MACOS_OPEN_MAX: libc::rlim_t = 10240;

/// Raise the soft RLIMIT_NOFILE to the hard limit (Go does this at startup).
/// RocksDB keeps every SST open (max_open_files = -1) and the book CF writes
/// 4 MiB SSTs (~256 files/GB), while a systemd service starts at soft 1024.
/// Never lowers. Returns (old soft, new soft, hard).
#[cfg(unix)]
fn raise_nofile_limit() -> std::io::Result<(libc::rlim_t, libc::rlim_t, libc::rlim_t)> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a valid, writable rlimit for the duration of the call.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let old = lim.rlim_cur;
    #[cfg(target_os = "macos")]
    let target = lim.rlim_max.min(MACOS_OPEN_MAX);
    #[cfg(not(target_os = "macos"))]
    let target = lim.rlim_max;
    if old >= target {
        return Ok((old, old, lim.rlim_max));
    }
    lim.rlim_cur = target;
    // SAFETY: `lim` is a valid rlimit read by the call.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((old, target, lim.rlim_max))
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
    // R01b: a panic on a consensus / execution thread fail-stops the node.
    install_fail_stop_panic_hook(|code| std::process::exit(code));
    info!(
        version = env!("CARGO_PKG_VERSION"),
        data_dir = %cli.data_dir.display(),
        "starting torus-node"
    );
    info!(
        allocator = allocator::NAME,
        jemalloc_version = allocator::version().unwrap_or("-"),
        "process allocator"
    );

    // Before the StateDb opens (every SST stays open) and any socket listens.
    #[cfg(unix)]
    match raise_nofile_limit() {
        Ok((old, new, hard)) if new > old => info!(old, new, hard, "raised open-file soft limit"),
        Ok(_) => {}
        Err(e) => warn!(%e, "could not raise open-file soft limit"),
    }

    if let Err(e) = run(cli).await {
        error!(%e, "node exited with error");
        std::process::exit(1);
    }
    // Exit now rather than drop the runtime: the drop waits for every
    // blocking task (e.g. a pruner pass), which could hold up a finished
    // graceful shutdown.
    std::process::exit(0);
}

/// Read a passphrase from stdin (no echo if terminal).
fn read_passphrase_stdin() -> String {
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).unwrap_or(0);
    buf.trim().to_string()
}

/// R19: load the genesis file; a malformed or ambiguous genesis refuses to
/// start, naming the file.
fn load_genesis(path: &std::path::Path) -> Result<Genesis, String> {
    Genesis::from_file(path)
        .map_err(|e| format!("refusing to start: genesis {}: {e}", path.display()))
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Validate pruning flags
    let pruner_settings = pruner_config(&cli)?;
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

    // 3. Open the state DB and (section 71.8 option A) the consensus DB at
    // <data_dir>/consensus-db: all HotStuff store traffic and the DA store,
    // with their own WAL and write queue. Refuses an old-layout data dir.
    let dbs = node_dbs::open_node_dbs(&cli.data_dir)?;
    let state_db = dbs.state.clone();
    info!(
        consensus_db = %torus_state::consensus_db_path(&cli.data_dir).display(),
        "state database and consensus database (HotStuff + DA store) opened"
    );

    // 4. Genesis initialization
    let mut chain_config = if let Some(genesis_path) = &cli.genesis {
        let genesis = load_genesis(genesis_path)?;
        let config = genesis.chain_config();

        // Only initialize on first run (see `node_dbs::genesis_applied`).
        info!(genesis = %genesis_path.display(), "genesis file");
        node_dbs::apply_genesis_if_fresh(&dbs, &genesis)?;

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
    // Every native action this node signs or verifies uses this network's
    // EIP-712 domain (genesis `chain_id`, devnet 7778). Set before any
    // component exists; fails if anything already used another id.
    torus_types::eip712::set_network_chain_id(chain_config.chain_id)
        .map_err(|e| format!("EIP-712 network chain id: {e}"))?;

    // Node-local override: `--exec-trust-cache` enables the exec trust-cache read
    // path (default off). It is deterministic (a HIT equals a fresh recover), so
    // toggling it per-node never affects consensus or state — purely an A/B and
    // rollback switch.
    chain_config.exec_trust_cache = cli.exec_trust_cache;
    if chain_config.exec_trust_cache {
        info!("exec trust-cache ENABLED: execution reuses locally-verified senders (skips re-recover)");
    }

    // 5. Build components
    let metrics = Arc::new(torus_telemetry::Metrics::new());

    // Mempool (created before app so consensus can drain it during block production)
    let mempool_config = MempoolConfig {
        chain_id: chain_config.chain_id,
        block_gas_limit: chain_config.evm_gas_limit,
        // Anti-spam item A: ON in the node (default 1 TRS), 0 = off.
        ingress_min_collateral_trs: torus_mempool::funded::ingress_min_collateral(),
        ..MempoolConfig::default()
    };
    info!(
        ingress_min_collateral_trs = mempool_config.ingress_min_collateral_trs,
        "native ingress anti-spam: funded-account check (TORUS_INGRESS_MIN_COLLATERAL, whole TRS, 0 = off; node-local)"
    );
    info!(
        cancel_block_share_pct = mempool_config.native_cancel_block_share_pct,
        "native pool: cancels take at most this share of a selected block ahead of orders (TORUS_CANCEL_BLOCK_SHARE_PCT, 100 = old cancel priority; proposer-local)"
    );
    let mempool = Arc::new(Mempool::with_da_store(
        state_db.clone(),
        dbs.da_store(),
        mempool_config,
    ));
    // r4: log the effective native block-selection bundle + direct-push floor
    // once at startup so a bench/ops snapshot (`nodeenv-*.txt`, journal) shows
    // the caps this proposer actually runs. All of these are proposer-local
    // selection policy / node-local sizing / transport send policy —
    // validate_block rejects on none of them, so mixed values across the
    // fleet cannot fork; the log exists so a mixed fleet is VISIBLE, not to
    // gate anything.
    {
        use torus_mempool::rate_limit as rl;
        info!(
            native_total_block_cap = rl::native_total_block_cap(),
            native_orders_per_block_cap = rl::native_orders_per_block_cap(),
            native_block_bytes_cap = rl::native_block_bytes_cap(),
            verified_sender_cache_cap = rl::verified_sender_cache_cap(),
            direct_push_body_bytes = torus_network::caps::direct_push_body_bytes(),
            parallel_bucket_hash_threads = torus_state::native_trie::parallel_bucket_hash_threads(),
            "native block-selection caps (proposer-local; compiled default = r3 cap-200 bundle) + root bucket-hash workers (node-local, value-neutral; default 8)"
        );
    }

    let signing_key_for_app = if !cli.rpc_only {
        Some(signing_key.clone())
    } else {
        None
    };
    // s109 tw baseline: trade-history rows off on validators, on for
    // --rpc-only (RPC / explorer serve them); TORUS_TRADE_HISTORY overrides.
    let trade_history = torus_consensus::app::set_trade_history_default(cli.rpc_only);
    info!(
        trade_history,
        "trade history (TORUS_TRADE_HISTORY; default off on validators, on for --rpc-only)"
    );
    // Item 6 Phase 3 step 6: coalesced checkpoints every 15 s on validators,
    // off (per-block flush) on --rpc-only; TORUS_CHECKPOINT_INTERVAL_MS
    // overrides (0 = per-block). A deferred book mode turns the default off.
    let checkpoint_interval =
        torus_consensus::exec_pipeline::set_checkpoint_interval_default(cli.rpc_only);
    info!(
        ?checkpoint_interval,
        "checkpoint interval (TORUS_CHECKPOINT_INTERVAL_MS; default 15 s on validators, 0 on --rpc-only)"
    );
    // R02 branch 8 (D1): a startup storage fault (e.g. the validator rows
    // cannot be read to resolve the proposer address) stops the node here.
    let mut app = TorusApp::try_new(
        state_db.clone(),
        &chain_config,
        Some(metrics.clone()),
        Some(mempool.clone()),
        signing_key_for_app,
    )?;
    let kv_store = dbs.kv_store().with_metrics(metrics.clone());

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
    network.set_native_da_store(dbs.da_store());
    // Wire the erasure-shard recovery pull to the network (T8-integration inc 3):
    // an ADDITIVE pre-step to the whole-body pull — on a reconstruction miss it
    // gathers erasure shards spread across DISTINCT peers before the single-source
    // whole-body fetch, killing the s338 serve hotspot. Set BEFORE
    // set_native_da_fetcher: the DA recovery worker that call spawns captures the
    // shard fetcher, so it must already be installed.
    app.set_shard_fetcher(Arc::new(NetworkShardFetcher {
        network: network.clone(),
    }));
    // Wire the consensus app's RARE pull-fallback to the network (Phase C Task 6):
    // on a CompactBlock reconstruction miss it fetches the missing bodies by-hash.
    app.set_native_da_fetcher(Arc::new(NetworkDaFetcher {
        network: network.clone(),
    }));
    // Item 3 (TORUS_ASYNC_VALIDATE): with the flag on, tee inbound proposal/body
    // datum bytes ON THE NETWORK THREAD into the speculative validate worker, so
    // the hotstuff-algo thread may find a ready heavy-stage verdict instead of
    // doing the DA reconstruct + sig verify inline. Installed ONCE, AFTER
    // set_native_da_fetcher, which re-spawns the worker (earlier handles would go
    // stale). Flag off (the default): no handle exists and nothing is installed;
    // the network path still pays a OnceLock presence check per inbound message.
    // Experimental flag-ON gates remain pending: chain-id/view/QC admission before
    // teeing, pre-verification DA write amplification, and duplicate metrics.
    if let Some(validate_tee) = app.async_validate_handle() {
        network.set_validate_tee(Arc::new(move |datum: Vec<u8>| {
            let _ = validate_tee.submit(datum);
        }));
        info!("TORUS_ASYNC_VALIDATE: network-thread proposal tee installed");
    }

    // Spawn inbound native action gossip → mempool task
    if let Some(mut native_rx) = network.take_native_action_rx() {
        let mempool_for_gossip = mempool.clone();
        // T2.1: gossip-ingest verify runs on a DEDICATED rayon pool — the one
        // serial inbound task capped verified ingest at a single ecrecover at
        // a time, and the GLOBAL pool is off-limits (a shared pool starved
        // consensus verify historically, s352). Half the cores (min 2) keeps
        // ingest parallel without starving the consensus threads.
        let gossip_verify_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(
                (std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(8)
                    / 2)
                .max(2),
            )
            .thread_name(|i| format!("torus-gossip-verify-{i}"))
            .build()
            .expect("build gossip verify pool");
        // Bounded handoff: past this many queued verifies, ingest inline on
        // the inbound task (the old serial behavior) so a gossip flood exerts
        // backpressure instead of growing the pool queue without bound.
        const GOSSIP_VERIFY_MAX_INFLIGHT: usize = 1024;
        let in_flight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        tokio::spawn(async move {
            while let Some((sender, action)) = native_rx.recv().await {
                let mempool = mempool_for_gossip.clone();
                // Verified ingest: authenticates the claimed sender (forged
                // gossip pairs must not pollute the pool); DA-mirrors first.
                // Dedups against the verified-sender trust cache before the
                // recover (T2.1) — re-gossiped known bodies skip the ecrecover.
                let ingest = move || match mempool.add_native_action_from_gossip(sender, action) {
                    Ok(()) => {}
                    Err(torus_mempool::MempoolError::DuplicateNativeAction) => {}
                    Err(e) => tracing::debug!("gossip native action rejected: {e}"),
                };
                if in_flight.load(std::sync::atomic::Ordering::Relaxed)
                    >= GOSSIP_VERIFY_MAX_INFLIGHT
                {
                    ingest();
                    continue;
                }
                in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let in_flight_done = in_flight.clone();
                gossip_verify_pool.spawn(move || {
                    ingest();
                    in_flight_done.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                });
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
    let (pre_proposal_tx, pre_proposal_rx) =
        std::sync::mpsc::sync_channel::<torus_consensus::PreProposalBundle>(4);
    app.set_pre_proposal_tx(pre_proposal_tx);
    let network_for_pre_proposal = network.clone();
    std::thread::spawn(move || {
        while let Ok(bundle) = pre_proposal_rx.recv() {
            network_for_pre_proposal.broadcast_pre_proposal_actions(&bundle.actions);
        }
    });

    // 5c. Extract leader state + clone network for RPC forwarding (before replica consumes them)
    let leader_state = app.leader_state();
    let network_for_fwd = network.clone();
    let network_for_evm_fwd = network.clone();

    // 5c-fail-stop (P1(b)): watch the execution pipeline's fail-stop latch and
    // turn it into real process termination. `TorusApp` SETs this shared flag
    // (never clears it) when block execution dies — a panicked market worker,
    // a fatal exec error, or a closed exec channel — after which the node must
    // NOT keep producing, voting, or finalizing over frozen state. The latch was
    // exposed via `exec_failed_handle()` but had no consumer repo-wide, so
    // fail-stop was in-process only: consensus stopped advancing but the process
    // never actually halted (no alert, no restart trigger). This lightweight
    // poller (grabbed BEFORE the replica consumes `app` on `.app(app)`) logs a
    // fatal line and exits non-zero so a supervisor can alert/restart the node.
    let exec_failed = app.exec_failed_handle();
    tokio::spawn(async move {
        loop {
            if exec_failed.load(std::sync::atomic::Ordering::SeqCst) {
                error!(
                    "FATAL: execution pipeline has died (fail-stop latch set) — \
                     terminating node to avoid finalizing over frozen state"
                );
                // Let tracing flush the fatal line before the process goes away.
                tokio::time::sleep(Duration::from_millis(200)).await;
                std::process::exit(70); // sysexits.h EX_SOFTWARE
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    });

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
    // RH4 durable DA body GC: committed bodies (and their shards) stay as long
    // as block sync can still hand their block to a lagging peer: a body goes
    // once its window (block-tree window + 128, or TORUS_DA_BODY_RETENTION;
    // 0 = off) has passed AND the block tree's durable prune pointer is 128
    // past it. Archive (no --retention-blocks): no GC. The pointer is read
    // from the consensus DB, where the DA store writes too (one WAL). See
    // torus_consensus::da_gc.
    let tree_retention = hotstuff_rs::block_tree::block_tree_retention();
    let da_retention_env = std::env::var("TORUS_DA_BODY_RETENTION").ok();
    let da_retention = torus_consensus::da_gc::resolve_da_body_retention(
        da_retention_env.as_deref(),
        tree_retention,
    );
    app.set_da_body_gc(
        da_retention.map(|retention| torus_consensus::da_gc::DaBodyGc {
            retention,
            block_tree_db: dbs.consensus.clone(),
        }),
    );
    match (da_retention, tree_retention) {
        (None, None) => info!(
            env = ?da_retention_env,
            "DA body GC off: archive node (no --retention-blocks), every committed body is kept"
        ),
        (None, Some(_)) => {
            info!("DA body GC off (TORUS_DA_BODY_RETENTION=0): every committed body is kept")
        }
        (Some(window), Some(tree)) if window <= tree => info!(
            window,
            block_tree_retention = tree,
            "DA body GC on: TORUS_DA_BODY_RETENTION is below the block-tree window, so bodies \
             still stay until the block tree has pruned their block (+128)"
        ),
        (Some(window), tree) => info!(
            window,
            block_tree_retention = ?tree,
            "DA body GC on: committed bodies deleted {window} dispatched blocks after commit, \
             once the block tree pruned their block (+128)"
        ),
    }

    // S470: commit-lag backoff cap — genesis-sourced, env-overridable for
    // devnets (TORUS_COMMIT_LAG_BACKOFF_CAP). Fleet-uniform: see
    // `resolve_commit_lag_cap`. Logged loudly because a divergent fleet
    // degrades liveness.
    let commit_lag_cap = resolve_commit_lag_cap(
        chain_config.commit_lag_backoff_cap,
        std::env::var("TORUS_COMMIT_LAG_BACKOFF_CAP").ok().as_deref(),
    );
    info!(
        commit_lag_cap,
        genesis_value = chain_config.commit_lag_backoff_cap,
        "S470 commit-lag view backoff: {} (0 = off / pre-S470 schedule; \
         MUST be identical on every validator of the chain)",
        if commit_lag_cap == 0 { "DISABLED" } else { "ENABLED" },
    );

    let hs_config = Configuration::builder()
        .me(signing_key)
        .chain_id(ChainID::new(chain_config.chain_id))
        .epoch_length(EpochLength::new(chain_config.epoch_length as u32))
        .max_view_time(Duration::from_millis(chain_config.timeout_base_ms))
        .backoff_factor(chain_config.backoff_factor)
        .backoff_cap(chain_config.backoff_cap)
        .commit_lag_cap(commit_lag_cap)
        .progress_msg_buffer_capacity(BufferSize::new(PROGRESS_MSG_BUFFER_BYTES))
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
    // s80: newTrades / userFills are fed from execution (each executed block's
    // fills), not read back from the DB at commit; fills are recorded only
    // while a stream subscriber exists. Set before the replica starts.
    app.set_fill_sink(Arc::new(notifier.clone()));
    // EVM forward channel (RPC -> network unicast), made before the replica consumes `app`:
    // item 7 step 2 (decision 7) commit-path re-sends ride it too.
    let (evm_fwd_tx, mut evm_fwd_rx) = tokio::sync::mpsc::unbounded_channel();
    let evm_fwd_for_app = evm_fwd_tx.clone();
    let own_vk_for_app = verifying_key.to_bytes();
    app.set_evm_forward(Arc::new(move |vk: [u8; 32], raw: Vec<u8>| {
        if vk != own_vk_for_app {
            let _ = evm_fwd_for_app.send((vk, raw));
        }
    }));
    if let Some(path) = &cli.state_hash_attest_key {
        let addr = app.set_state_hash_attest_key(load_state_hash_attest_key(path)?);
        info!(%addr, "running state hash: automatic AttestStateHash enabled");
    }
    let state_db_for_handler = state_db.clone();
    let mempool_for_handler = mempool.clone();
    // Item 7 option 0 (task 4.1, decision 2): with `TORUS_EVM_FAST_PATH` the commit prune
    // reads the published ingress view; without it today's raw-DB prune (read once here, as
    // the exec context reads it once at construction).
    let prune_on_view = torus_consensus::exec_pipeline::evm_fast_path_enabled();
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
    let rec_vote_rx = view_rec.clone();
    let rec_pc = view_rec.clone();
    let rec_timeout = view_rec.clone();
    let rec_commit = view_rec.clone();
    // Own headers loop back via broadcast self-delivery; proposal_arrival is a
    // follower metric, so filter them out by origin.
    let own_hotstuff_vk = verifying_key;
    let timeout_counter = metrics.consensus_timeout_total.clone();
    let leader_state_for_view = leader_state.clone();
    // Item 7 step 1: proposals (own and received headers) feed the next EVM
    // height's leader prediction for the RPC EVM forward.
    let leader_state_for_propose = leader_state.clone();
    let leader_state_for_header = leader_state.clone();
    // The shutdown thread exists before consensus starts, so stopping never
    // needs a new thread.
    let shutdown_worker = ShutdownWorker::spawn()?;
    let replica = ReplicaSpec::builder()
        .app(app)
        .network(network)
        .kv_store(kv_store)
        .configuration(hs_config)
        .on_start_view(move |ev: &StartViewEvent| {
            // Leader-hint fix: feed the REAL pacemaker view and its actual
            // (reputation-aware when enabled) leader into LeaderState, so the
            // RPC d2l forward hint stays correct during timeout churn instead
            // of trusting the committed_height+1 heuristic + plain IWRR.
            leader_state_for_view.observe_start_view(ev.view.int(), ev.leader);
            let closed = rec_start.start_view(ev.timestamp, ev.view.int());
            rec_start.view_entry_slack(ev.deadline_slack_secs);
            // s70: one line per closed view, joined across validators by
            // tools/matched-bench/gap_attr.py.
            if let Some(closed) =
                closed.filter(|_| hotstuff_rs::logging::body_fetch_trace_enabled())
            {
                info!("body_fetch_diag view_close: {closed}");
            }
        })
        .on_propose(move |ev: &ProposeEvent| {
            // Item 7 step 1: hotstuff heights; LeaderState converts them to Torus heights.
            leader_state_for_propose
                .observe_hotstuff_proposal(ev.proposal.view.int(), ev.proposal.block.height.int());
            rec_propose.propose(ev.timestamp, ev.proposal.block.hash.bytes())
        })
        .on_update_highest_pc(move |ev: &UpdateHighestPCEvent| {
            rec_qc.update_highest_pc(ev.timestamp, ev.highest_pc.block.bytes())
        })
        .on_receive_proposal(move |ev: &ReceiveProposalEvent| {
            rec_receive.receive_proposal(ev.timestamp);
        })
        .on_receive_proposal_header(move |ev: &ReceiveProposalHeaderEvent| {
            leader_state_for_header.observe_hotstuff_proposal(ev.view.int(), ev.height.int());
            if ev.origin != own_hotstuff_vk {
                rec_header.receive_proposal(ev.timestamp);
            }
        })
        .on_insert_block(move |ev: &InsertBlockEvent| rec_insert.insert_block(ev.timestamp))
        .on_phase_vote(move |ev: &PhaseVoteEvent| rec_vote.phase_vote(ev.timestamp))
        // s70: collector side of qc_collect (phased-mode votes during
        // validator-set updates are left out).
        .on_receive_phase_vote(move |ev: &ReceivePhaseVoteEvent| {
            if ev.phase_vote.phase.is_generic() {
                rec_vote_rx.receive_phase_vote(ev.timestamp, ev.phase_vote.view.int());
            }
        })
        .on_collect_pc(move |ev: &CollectPCEvent| {
            if ev.phase_certificate.phase.is_generic() {
                rec_pc.collect_pc(ev.timestamp, ev.phase_certificate.view.int());
            }
        })
        .on_view_timeout(move |ev: &ViewTimeoutEvent| {
            timeout_counter.inc();
            rec_timeout.view_timeout(ev.timestamp);
        })
        .on_commit_block(move |event: &CommitBlockEvent| {
            rec_commit.commit_block(event.timestamp);
            let height = find_latest_height(&state_db_for_handler);
            info!(height, "on_commit_block fired");
            if height == 0 {
                return;
            }
            if prune_on_view {
                // Item 7 option 0 (task 4.1, R9): the view's height, then its nonces, through
                // one handle (the DB marker and DB nonces when no view is published).
                mempool_for_handler.prune_committed_txs_on_view();
            } else {
                // Item 7 step 2 (decision 7): the raw DB's applied marker (absent = 0), read
                // before the state nonces the prune reads from the same DB. Not `height`: the
                // header row is written at commit, before execution.
                let executed = state_db_for_handler
                    .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
                    .ok()
                    .flatten()
                    .and_then(|v| <[u8; 8]>::try_from(v.as_slice()).ok())
                    .map_or(0, u64::from_be_bytes);
                mempool_for_handler.prune_committed_txs(executed);
            }
            latest_height_for_handler.store(height, std::sync::atomic::Ordering::Relaxed);
            match state_db_for_handler.get_cf_raw(CF_BLOCK_HEADERS, &height.to_be_bytes()) {
                Ok(Some(data)) if data.len() > 32 => {
                    if let Ok(header) = serde_json::from_slice::<serde_json::Value>(&data[32..]) {
                        notifier_for_replica.notify_new_block(header);
                    }
                }
                _ => {}
            }
        })
        .build()
        .start();
    info!("consensus replica started");

    // 9. Start RPC server (shares height counter with commit handler)
    let rpc_addr: SocketAddr = cli.rpc_addr.parse()?;
    // A storage fault reading the persisted trade-history cutoff stops the
    // node here (it would otherwise serve pruned blocks as "no trades").
    let mut rpc_server = RpcServer::try_new(
        state_db.clone(),
        mempool.clone(),
        executor,
        chain_config.chain_id,
        chain_config.epoch_length,
        notifier,
    )?;
    rpc_server.set_latest_height_handle(latest_height_shared);
    rpc_server.set_metrics(metrics.clone());
    // s80 fix 2: all-markets newTrades is off on validators unless
    // TORUS_ALL_MARKET_TRADES=1 (on for --rpc-only unless =0). Read once here.
    let all_market_trades = torus_rpc::streams::all_market_trades_allowed(
        std::env::var(torus_rpc::streams::ENV_ALL_MARKET_TRADES)
            .ok()
            .as_deref(),
        cli.rpc_only,
    );
    info!(all_market_trades, "all-markets newTrades subscriptions");
    rpc_server.set_all_market_trades(all_market_trades);
    // Anti-spam item B: per-address native request limit, ON unless
    // TORUS_ADDR_RATE_LIMIT=0. Counted for every action entering the mempool
    // (RPC, gossip, forwards), enforced at RPC ingress. In memory, reset on
    // restart.
    let (addr_limiter, bad_exempt) = torus_mempool::addr_rate::from_env();
    for entry in &bad_exempt {
        warn!(%entry, "TORUS_ADDR_RATE_EXEMPT: not an address, ignored");
    }
    match addr_limiter {
        Some(limiter) => {
            info!(
                enabled = true,
                buffer = limiter.buffer(),
                exempt = limiter.exempt_count(),
                "native ingress anti-spam: per-address request limit (TORUS_ADDR_RATE_LIMIT / _BUFFER / _EXEMPT; node-local, in memory)"
            );
            mempool.set_addr_rate_limiter(Arc::new(limiter));
        }
        None => info!(
            enabled = false,
            "native ingress anti-spam: per-address request limit OFF (TORUS_ADDR_RATE_LIMIT=0)"
        ),
    }
    // Anti-spam item D: per-IP weight limit (TORUS_RPC_IP_WEIGHT_PER_MIN,
    // default 1200, 0 = off; TORUS_RPC_IP_EXEMPT CIDRs, unset = loopback only)
    // and the per-WebSocket-connection subscription cap.
    let (ip_limiter, bad_cidrs) = torus_rpc::ip_limit::from_env();
    for entry in &bad_cidrs {
        warn!(%entry, "TORUS_RPC_IP_EXEMPT: not an IP or CIDR, ignored");
    }
    match ip_limiter {
        Some(limiter) => {
            info!(
                weight_per_min = limiter.weight_per_min(),
                exempt = ?limiter.exempt(),
                "rpc anti-spam: per-IP weight limit (TORUS_RPC_IP_WEIGHT_PER_MIN / TORUS_RPC_IP_EXEMPT)"
            );
            rpc_server.set_ip_limiter(Arc::new(limiter));
        }
        None => info!("rpc anti-spam: per-IP weight limit OFF (TORUS_RPC_IP_WEIGHT_PER_MIN=0)"),
    }
    let max_subs_per_conn = torus_rpc::ip_limit::parse_max_subs_per_conn(
        std::env::var("TORUS_RPC_MAX_SUBS_PER_CONN").ok(),
    );
    info!(
        max_subs_per_conn,
        "rpc: WebSocket subscriptions per connection (TORUS_RPC_MAX_SUBS_PER_CONN)"
    );
    rpc_server.set_max_subscriptions_per_connection(max_subs_per_conn);

    // Leader forwarding: RPC → network bridge
    let own_vk = verifying_key.to_bytes();
    let leader_state_for_rpc = leader_state.clone();
    let leader_vk_fn: Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync> = Arc::new(move || {
        leader_state_for_rpc
            .current_leader()
            .map(|vk| *vk.as_bytes())
    });
    // B1: the native forward channel is BOUNDED and carries structured
    // (leader hint, sender, parsed action) tuples — overflow is a counted
    // drop on the RPC side (`rpc_forward_dropped_full`), never unbounded
    // memory behind a stalled forwarder.
    let (fwd_tx, fwd_rx) = tokio::sync::mpsc::channel(torus_rpc::FORWARD_CHANNEL_CAP);
    // Full-body native forwards only in the no-gossip fallback: with pre-spread on,
    // gossip already delivers every body to the leader (Sprint 3.5). EVM forwarding has no
    // gossip path, so it is unconditional — wired via its own channel.
    rpc_server.set_leader_forwarding(
        own_vk,
        leader_vk_fn.clone(),
        fwd_tx,
        evm_fwd_tx,
        !cli.native_gossip,
    );
    // Item 7 step 1: EVM txs go to the leader of the next EVM height.
    // Item 7 step 2 (decision 7): the leaders of the next EVM height(s) of the tx's lane.
    let leader_state_for_evm = leader_state.clone();
    rpc_server.set_evm_targets_fn(Arc::new(move |lane| {
        let (target, vks) = leader_state_for_evm.next_evm_targets(lane);
        (target, vks.iter().map(|vk| vk.to_bytes()).collect())
    }));

    // Extract shared handles before start() consumes the server
    let latest_height_handle = rpc_server.latest_height();
    let pruned_up_to_handle = rpc_server.pruned_up_to();
    let trades_pruned_handle = rpc_server.trades_pruned_up_to();

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

    // Forwarding bridge (B1): with `TORUS_D2L_BATCH=1` (default) the
    // ForwardBatcher task coalesces the structured tuples into ONE 0xFD
    // envelope per (flush window, leader) — leader re-resolved at flush time,
    // PushScheduler-bounded, retried against the re-resolved leader, with the
    // periodic re-forward sweep healing lost envelopes. `=0` restores today's
    // per-action 0xFE forward byte-for-byte (the sender prefix + serde_json
    // canonical bytes are reconstructed exactly as `verify_one_action`
    // produced them).
    if forward_batcher::d2l_batch_enabled() {
        // Failed envelopes retry against the CURRENT leader, not the stale hint.
        network_for_fwd.set_leader_resolver(leader_vk_fn.clone());
        forward_batcher::spawn(
            fwd_rx,
            network_for_fwd,
            leader_vk_fn.clone(),
            own_vk,
            mempool.clone(),
            // Same gate as the RPC forward path above: bodies flow to the
            // leader only in no-gossip mode, and so does the healing sweep.
            !cli.native_gossip,
        );
    } else {
        info!("B1 d2l batching disabled (TORUS_D2L_BATCH=0) — per-action 0xFE forwards");
        let mut fwd_rx = fwd_rx;
        tokio::spawn(async move {
            while let Some((target_vk_bytes, sender, action)) = fwd_rx.recv().await {
                if let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&target_vk_bytes) {
                    // Byte-identical rollback: 20-byte sender ‖ canonical
                    // serde_json action bytes, one request per action.
                    let Ok(action_json) = serde_json::to_vec(&action) else {
                        continue;
                    };
                    let mut payload = Vec::with_capacity(20 + action_json.len());
                    payload.extend_from_slice(sender.as_slice());
                    payload.extend_from_slice(&action_json);
                    network_for_fwd.forward_native_action(vk, payload);
                }
            }
        });
    }

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
    if let Some(pruner_config) = pruner_settings {
        info!(
            retention_blocks = ?cli.retention_blocks,
            trade_retention_blocks = ?cli.trade_retention_blocks,
            "background pruner enabled"
        );
        let mut pruner = StatePruner::new(state_db.clone(), pruner_config, pruned_up_to_handle)
            .with_trades_pruned_handle(trades_pruned_handle);
        let latest_for_pruner = latest_height_handle.clone();
        let metrics_for_pruner = metrics.clone();
        let prune_cutoff = state_db.prune_cutoff().clone();
        info!(
            prune_mode = ?prune_cutoff.mode(),
            filter_cutoff = pruner.filter_cutoff(),
            "state pruner mode (TORUS_PRUNE_MODE)"
        );
        metrics.pruner_filter_cutoff.set(pruner.filter_cutoff() as i64);
        tokio::spawn(async move {
            let (mut files_seen, mut rows_seen) = (0u64, 0u64);
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                let current = latest_for_pruner.load(std::sync::atomic::Ordering::Relaxed);
                if current == 0 {
                    continue;
                }
                // A pass is blocking RocksDB work (the D13 trade pass scans
                // every trader), so it runs on the blocking pool, never on a
                // runtime worker that consensus / libp2p tasks need.
                let pass = tokio::task::spawn_blocking(move || {
                    let res = pruner.maybe_prune(current);
                    (pruner, res)
                })
                .await;
                let res = match pass {
                    Ok((p, res)) => {
                        pruner = p;
                        res
                    }
                    Err(e) => {
                        warn!(%e, "background pruner pass panicked; pruner stopped");
                        return;
                    }
                };
                // s132: filter-mode counters (deltas of the running totals).
                let files = pruner.files_dropped_total();
                metrics_for_pruner
                    .pruner_files_dropped
                    .inc_by(files.saturating_sub(files_seen));
                files_seen = files;
                let rows = prune_cutoff.filter_rows_dropped();
                metrics_for_pruner
                    .pruner_filter_rows_dropped
                    .inc_by(rows.saturating_sub(rows_seen));
                rows_seen = rows;
                metrics_for_pruner
                    .pruner_filter_cutoff
                    .set(pruner.filter_cutoff() as i64);
                match res {
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
        let consensus_db = dbs.consensus.db_arc();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let size = torus_state::dir_size_bytes(&data_dir);
                m.db_size_bytes.set(size as i64);
                // Section 71.8 option A: the consensus DB's CFs carry a
                // `db="consensus"` label (its cf_consensus_meta would
                // otherwise collide with the state DB's).
                for (cf_db, db_label) in [(&db, None), (&consensus_db, Some("consensus"))] {
                    for cf_name in torus_state::cf::ALL_CF_NAMES {
                        let Some(cf) = cf_db.cf_handle(cf_name) else {
                            continue;
                        };
                        let mut label = vec![("cf".to_string(), (*cf_name).to_string())];
                        if let Some(l) = db_label {
                            label.push(("db".to_string(), l.to_string()));
                        }
                        for (prop, fam) in [
                            ("rocksdb.num-files-at-level0", &m.rocksdb_l0_files),
                            ("rocksdb.cur-size-all-mem-tables", &m.rocksdb_memtable_bytes),
                            (
                                "rocksdb.estimate-pending-compaction-bytes",
                                &m.rocksdb_pending_compaction_bytes,
                            ),
                        ] {
                            if let Ok(Some(v)) = cf_db.property_int_value_cf(cf, prop) {
                                fam.get_or_create(&label).set(v as i64);
                            }
                        }
                    }
                }
                if let Ok(Some(v)) = db.property_int_value("rocksdb.block-cache-usage") {
                    m.rocksdb_block_cache_bytes.set(v as i64);
                }
            }
        });
    }

    // 12b. r3 exec-write-stall-attribution: DB-wide RocksDB runtime snapshot
    // (write-controller state, LSM totals, statistics tickers/histograms) into
    // the UNLABELLED torus_rocksdb_* gauges, on a cadence fine enough for a
    // 300 s bench cell (default every 5 s; TORUS_ROCKSDB_STATS_INTERVAL_SECS).
    {
        let m = metrics.clone();
        let sdb = state_db.clone();
        let cdb = dbs.consensus.clone();
        let every = rocksdb_stats_interval_secs(std::env::var("TORUS_ROCKSDB_STATS_INTERVAL_SECS").ok());
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(every));
            loop {
                interval.tick().await;
                let snap = sdb.runtime_stats();
                publish_rocksdb_stats(&m.rocksdb, &snap);
                // Section 71.8 option A: the consensus DB, as torus_rocksdb_consensus_*.
                publish_rocksdb_stats(&m.rocksdb_consensus, &cdb.runtime_stats());
            }
        });
    }

    // 12c. Process-allocator (jemalloc) stats into the torus_jemalloc_*_bytes
    // gauges every 5 s; not spawned on a system-allocator build.
    if allocator::JEMALLOC {
        let m = metrics.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                if let Some(s) = allocator::stats() {
                    allocator::publish(&m.jemalloc, &s);
                }
            }
        });
    }

    // 13. Wait for SIGINT (Ctrl+C) or SIGTERM (systemctl / docker / k8s stop),
    // then stop gracefully within a bound. Dropping the replica stops the
    // hotstuff threads; its `TorusApp` drop then drains the execution queue
    // (every committed block is executed and persisted) and joins the exec
    // and flush threads. With item 6 Phase 3 checkpoints on, the exec
    // thread's `ExecutionContext` drop first closes the open window into a
    // final checkpoint (durable marker = last executed height), so it lands
    // here, inside the bounded stop and before the flush below. Then
    // memtables are flushed and the WAL synced, so a restart replays neither
    // blocks nor WAL. A second signal, or the bound
    // (`TORUS_SHUTDOWN_TIMEOUT_SECS`, default 60), exits at once; recovery
    // after that is the same as after a kill.
    // Logged before the handlers are installed: if stdout is blocked (a full
    // pipe), the default action still kills the node.
    info!("node is running — SIGINT (Ctrl+C) or SIGTERM shuts it down");
    // One pair of receivers for both waits, so a second signal sent while
    // the first is handled is queued, not lost.
    let mut signals = ShutdownSignals::new()?;
    let signal = signals.recv().await;
    // From here no log is waited for (a blocked stdout would otherwise hold
    // off the deadline and the second-signal wait).
    log_with_grace(
        move || info!(%signal, "shutdown signal received, stopping..."),
        Duration::ZERO,
    );
    let limit = shutdown_timeout(std::env::var("TORUS_SHUTDOWN_TIMEOUT_SECS").ok());
    let started = std::time::Instant::now();
    let stop = move || {
        drop(replica);
        info!("consensus stopped, execution queue drained");
        flush_for_shutdown(&dbs)?;
        info!("memtables flushed, WAL synced");
        Ok(())
    };
    let second_signal = async {
        let s = signals.recv().await;
        log_with_grace(
            move || warn!(signal = %s, "second signal during shutdown"),
            Duration::ZERO,
        );
    };
    let outcome = bounded_stop(shutdown_worker, stop, limit, second_signal).await;
    // Every outcome ends with `_exit`: `exit()` would run RocksDB's exit
    // handlers, which join its background threads, so a stalled flush or
    // compaction would hold up the exit with no bound. A clean stop has
    // already flushed and synced; the others are as after a kill.
    debug_assert!(outcome.exits_at_once());
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let code = match outcome {
        StopOutcome::Clean => 0,
        _ => 1,
    };
    log_with_grace(
        move || match outcome {
            StopOutcome::Clean => info!(elapsed_ms, "shutdown complete"),
            StopOutcome::Failed(e) => error!(%e, "shutdown failed (exit 1)"),
            StopOutcome::TimedOut => error!(
                ?limit,
                "shutdown did not finish within the limit; exiting without it (exit 1)"
            ),
            StopOutcome::Interrupted => {
                error!("shutdown interrupted by a second signal (exit 1)")
            }
        },
        Duration::from_millis(500),
    );
    // SAFETY: `_exit` only ends the process; no Rust state is touched.
    unsafe { libc::_exit(code) }
}

/// The watchdog's action at the shutdown deadline: as a timed-out stop in
/// `run`, without waiting for anything (no runtime, bounded log).
fn deadline_exit() {
    log_with_grace(
        || error!("shutdown did not finish within the limit; exiting without it (exit 1)"),
        Duration::from_millis(500),
    );
    // SAFETY: `_exit` only ends the process; no Rust state is touched.
    unsafe { libc::_exit(1) };
}

/// Run `log` (the last line before `_exit`) on its own thread and wait at
/// most `grace`: a thread stuck writing to stdout holds its lock, so the
/// log may never finish. Returns whether it finished. No explicit flush:
/// stdout is line-buffered and the line ends in a newline.
fn log_with_grace(log: impl FnOnce() + Send + 'static, grace: Duration) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("node-exit-log".into())
        .spawn(move || {
            log();
            let _ = tx.send(());
        });
    spawned.is_ok() && rx.recv_timeout(grace).is_ok()
}

/// SIGINT and SIGTERM receivers. Kept for the node's lifetime: a signal
/// that arrives between two `recv` calls is queued, not lost.
struct ShutdownSignals {
    term: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
}

impl ShutdownSignals {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Self {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
        })
    }

    /// Wait for the next SIGINT or SIGTERM; returns the signal's name.
    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            Some(()) = self.term.recv() => "SIGTERM",
            Some(()) = self.int.recv() => "SIGINT",
            else => std::future::pending().await,
        }
    }
}

/// Pure parse of `TORUS_SHUTDOWN_TIMEOUT_SECS` (default 60; `0` or garbage
/// => 60).
fn shutdown_timeout(raw: Option<String>) -> Duration {
    match raw
        .as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<u64>().ok())
    {
        Some(n) if n >= 1 => Duration::from_secs(n),
        _ => Duration::from_secs(60),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StopOutcome {
    /// `stop` returned.
    Clean,
    /// `stop` returned an error or panicked.
    Failed(String),
    /// `stop` was still running at the limit.
    TimedOut,
    /// `interrupt` finished first (a second signal).
    Interrupted,
}

impl StopOutcome {
    /// Whether the process must exit without running exit handlers: always
    /// (see the end of `run`).
    fn exits_at_once(&self) -> bool {
        true
    }
}

type StopJob = Box<dyn FnOnce() -> Result<(), String> + Send>;
type StopReply = tokio::sync::oneshot::Sender<Result<(), String>>;

/// The thread that runs the shutdown, spawned at startup before consensus
/// starts: stopping never needs a new thread, whose failed spawn would drop
/// the stop (and the replica in it) on the async caller with no bound.
struct ShutdownWorker {
    jobs: std::sync::mpsc::SyncSender<(StopJob, StopReply, Duration)>,
}

impl ShutdownWorker {
    fn spawn() -> std::io::Result<Self> {
        Self::spawn_with(deadline_exit)
    }

    /// Spawn the shutdown thread and its watchdog. The watchdog is armed
    /// with the job's limit and calls `on_deadline` if the job has not
    /// finished by then: the bound holds even when the async runtime (and
    /// so `tokio::time`) is stalled.
    fn spawn_with(on_deadline: fn()) -> std::io::Result<Self> {
        let (jobs, rx) = std::sync::mpsc::sync_channel::<(StopJob, StopReply, Duration)>(1);
        let (arm_tx, arm_rx) = std::sync::mpsc::sync_channel::<Duration>(1);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
        std::thread::Builder::new()
            .name("node-shutdown-watchdog".into())
            .spawn(move || {
                if let Ok(limit) = arm_rx.recv() {
                    // Timeout: the stop is late. Disconnected: it panicked
                    // (the sender dropped unwinding), a failed stop.
                    if done_rx.recv_timeout(limit).is_err() {
                        on_deadline();
                    }
                }
            })?;
        std::thread::Builder::new()
            .name("node-shutdown".into())
            .spawn(move || {
                if let Ok((job, reply, limit)) = rx.recv() {
                    let _ = arm_tx.send(limit);
                    let res = job();
                    let _ = done_tx.send(());
                    let _ = reply.send(res);
                }
            })?;
        Ok(Self { jobs })
    }

    /// A worker whose thread is gone.
    #[cfg(test)]
    fn gone() -> Self {
        let (jobs, _) = std::sync::mpsc::sync_channel(1);
        Self { jobs }
    }
}

/// Run the blocking `stop` on the shutdown thread and wait for it, at most
/// `limit` or until `interrupt` completes. A `stop` still running is left
/// behind (the caller exits the process).
async fn bounded_stop(
    worker: ShutdownWorker,
    stop: impl FnOnce() -> Result<(), String> + Send + 'static,
    limit: Duration,
    interrupt: impl std::future::Future<Output = ()>,
) -> StopOutcome {
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(std::sync::mpsc::SendError((job, _, _))) =
        worker.jobs.send((Box::new(stop), tx, limit))
    {
        // Neither run nor drop it here (that blocks this thread with no
        // bound); the process exits next.
        std::mem::forget(job);
        return StopOutcome::Failed("shutdown thread is gone".into());
    }
    tokio::select! {
        done = tokio::time::timeout(limit, rx) => match done {
            Ok(Ok(Ok(()))) => StopOutcome::Clean,
            Ok(Ok(Err(e))) => StopOutcome::Failed(e),
            Ok(Err(_)) => StopOutcome::Failed("shutdown thread panicked".into()),
            Err(_) => StopOutcome::TimedOut,
        },
        () = interrupt => StopOutcome::Interrupted,
    }
}

/// Flush every column family's memtables to SST and fsync the WAL, so a
/// restart does not replay the WAL. Run after consensus and execution stop.
/// Covers both DBs (section 71.8 option A): the consensus DB's CFs are
/// flushed first, and the state DB's `sync_wal` syncs the consensus DB's
/// WAL (its attached peer) before its own.
fn flush_for_shutdown(dbs: &node_dbs::NodeDbs) -> Result<(), String> {
    flush_then_sync(
        || {
            for (db, what) in [
                (dbs.consensus.db_arc(), "consensus DB "),
                (dbs.state.db_arc(), ""),
            ] {
                for cf_name in torus_state::cf::ALL_CF_NAMES {
                    if let Some(cf) = db.cf_handle(cf_name) {
                        db.flush_cf(cf)
                            .map_err(|e| format!("flush {what}{cf_name}: {e}"))?;
                    }
                }
            }
            Ok(())
        },
        || dbs.state.sync_wal().map_err(|e| e.to_string()),
    )
}

/// The memtable flush, then the WAL sync, which runs even when the flush
/// failed (the WAL is then all a restart has). Either error is returned.
fn flush_then_sync(
    flush: impl FnOnce() -> Result<(), String>,
    sync: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    match (flush(), sync()) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(f), Ok(())) => Err(f),
        (Ok(()), Err(s)) => Err(format!("WAL sync: {s}")),
        (Err(f), Err(s)) => Err(format!("{f}; WAL sync: {s}")),
    }
}

/// Pure parse of `TORUS_ROCKSDB_STATS_INTERVAL_SECS` (default 5; `< 1` or
/// garbage => 5).
fn rocksdb_stats_interval_secs(raw: Option<String>) -> u64 {
    match raw.as_deref().map(str::trim).and_then(|s| s.parse::<u64>().ok()) {
        Some(n) if n >= 1 => n,
        _ => 5,
    }
}

/// Copy one [`torus_state::RocksdbRuntimeStats`] snapshot into the unlabelled
/// `torus_rocksdb_*` gauges. Ticker / histogram gauges are left untouched when
/// the DB was opened without statistics (they stay at 0, visibly).
fn publish_rocksdb_stats(
    g: &torus_telemetry::RocksdbStatMetrics,
    s: &torus_state::RocksdbRuntimeStats,
) {
    let i = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
    let f = |v: f64| if v.is_finite() { v.round() as i64 } else { 0 };
    g.memtable_bytes_all.set(i(s.memtable_bytes_all));
    g.immutable_memtables_all.set(i(s.immutable_memtables_all));
    g.l0_files_max.set(i(s.l0_files_max));
    g.pending_compaction_bytes_all.set(i(s.pending_compaction_bytes_all));
    g.delayed_write_rate.set(i(s.delayed_write_rate));
    g.write_stopped.set(i(s.write_stopped));
    g.running_compactions.set(i(s.running_compactions));
    g.running_flushes.set(i(s.running_flushes));
    if let Some(t) = &s.tickers {
        g.stall_micros.set(i(t.stall_micros));
        g.write_self.set(i(t.write_self));
        g.write_other.set(i(t.write_other));
        g.bytes_written.set(i(t.bytes_written));
        g.wal_bytes.set(i(t.wal_bytes));
        g.flush_write_bytes.set(i(t.flush_write_bytes));
        g.compact_read_bytes.set(i(t.compact_read_bytes));
        g.compact_write_bytes.set(i(t.compact_write_bytes));
        g.compaction_cpu_micros.set(i(t.compaction_cpu_micros));
    }
    if let Some(h) = &s.histograms {
        g.db_write_count.set(i(h.db_write.count));
        g.db_write_sum_micros.set(i(h.db_write.sum));
        g.db_write_p99_micros.set(f(h.db_write.p99));
        g.db_write_max_micros.set(f(h.db_write.max));
        g.write_stall_count.set(i(h.write_stall.count));
        g.write_stall_sum_micros.set(i(h.write_stall.sum));
        g.write_stall_p99_micros.set(f(h.write_stall.p99));
        g.write_stall_max_micros.set(f(h.write_stall.max));
        g.flush_count.set(i(h.flush.count));
        g.flush_sum_micros.set(i(h.flush.sum));
        g.compaction_count.set(i(h.compaction.count));
        g.compaction_sum_micros.set(i(h.compaction.sum));
    }
}

// Hex encoding helper for logging (no external hex crate needed)
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Bytes of future-view consensus messages buffered until the local view
/// reaches them (hotstuff_rs counts `size_of` the message + the sender key).
/// 1 MiB holds a full reconnect flush (256 queued sends) from each of 3
/// peers with headroom; the scaffold's 1024 held one header (s75 fix E).
const PROGRESS_MSG_BUFFER_BYTES: u64 = 1 << 20;

/// R01b: threads whose panic fail-stops the node (exit 70). The ONE list;
/// names must match the spawn sites:
/// - `hotstuff-algo`: hotstuff_rs `algorithm.rs` `Algorithm::start`
/// - `hotstuff-syncsv`: hotstuff_rs `block_sync/server.rs` `BlockSyncServer::start`
/// - `hotstuff-bodyserve`: hotstuff_rs `hotstuff/block_data_server.rs` `start`
/// - `torus-execution`: torus-consensus `app.rs` `TorusApp::new`
/// - `torus-flush-worker`: torus-consensus `exec_pipeline.rs` `FlushWorker::spawn`
///
/// The three hotstuff threads read the consensus KV store, whose read and
/// write faults panic (the hotstuff_rs `KVStore` trait is infallible).
///
/// NOT listed: `torus-async-validate` (torus-consensus `AsyncValidateWorker`).
/// It is fed raw, unauthenticated proposal bytes before any chain-id / view /
/// QC check, never reads the consensus KV store, and its DA writes are covered
/// by the mempool's 3-failure latch; listing it would let one peer's bytes
/// that hit a future panic there stop every validator at once.
const FAIL_STOP_THREADS: &[&str] = &[
    "hotstuff-algo",
    "hotstuff-syncsv",
    "hotstuff-bodyserve",
    "torus-execution",
    "torus-flush-worker",
];

/// sysexits.h EX_SOFTWARE, the node's fail-stop exit code.
const FAIL_STOP_EXIT_CODE: i32 = 70;

/// R01b A: chain a panic hook that fail-stops the node (`exit(70)`) when the
/// panicking thread is one of [`FAIL_STOP_THREADS`]. The previous hook runs
/// first, so the panic message and backtrace still print. A panic on any other
/// thread (RPC, network tasks), or one inside a recovering
/// `torus_bridge::panic_scope::catch_recoverable`, keeps today's behaviour.
///
/// It exits from the panicking thread itself rather than through the
/// `exec_failed` poller: a panic hook runs before the unwind, so this cannot
/// be lost to a stuck tokio runtime, a thread that never drops its
/// `TorusApp`, or an unwind that deadlocks. Same exit code as the poller.
fn install_fail_stop_panic_hook(exit: fn(i32)) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let thread = std::thread::current();
        let Some(name) = thread.name().filter(|n| FAIL_STOP_THREADS.contains(n)) else {
            return;
        };
        if torus_bridge::panic_scope::in_recoverable_scope() {
            return;
        }
        error!(
            thread = name,
            "FATAL: {name} thread panicked ({info}) — terminating node (fail-stop, exit {FAIL_STOP_EXIT_CODE})"
        );
        exit(FAIL_STOP_EXIT_CODE);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// R19 follow-up (owner s112): a genesis with a duplicate validator or an
    /// address in both accounts[] and evm_alloc refuses to start, naming the
    /// file and the address.
    #[test]
    fn r19_node_refuses_genesis_with_duplicate_validator_or_overlap() {
        let base: serde_json::Value =
            serde_json::from_str(include_str!("../../../devnet/genesis.json")).unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("genesis.json");
        std::fs::write(&path, base.to_string()).unwrap();
        load_genesis(&path).expect("the tracked devnet genesis loads");

        let v0 = base["validators"][0].clone();
        let v0_addr = v0["address"].as_str().unwrap().to_lowercase();
        let mut dup_validator = base.clone();
        let mut dup = v0.clone();
        dup["pubkey"] = "0x3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29".into();
        dup_validator["validators"]
            .as_array_mut()
            .unwrap()
            .push(dup);

        let overlap_addr = "0xcccccccccccccccccccccccccccccccccccccccc";
        let mut overlap = base.clone();
        overlap["evm_alloc"] = serde_json::json!({ overlap_addr: { "balance": "0x1" } });
        overlap["accounts"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({ "address": overlap_addr, "balance": "1" }));

        for (json, addr) in [
            (dup_validator, v0_addr),
            (overlap, overlap_addr.to_string()),
        ] {
            std::fs::write(&path, json.to_string()).unwrap();
            let err = load_genesis(&path).expect_err("refused").to_string();
            assert!(err.starts_with("refusing to start: genesis "), "{err}");
            assert!(err.contains(&path.display().to_string()), "{err}");
            assert!(err.contains("duplicate"), "{err}");
            assert!(err.contains(&addr), "names {addr}: {err}");
        }
    }

    /// Codex P2: a failed memtable flush must not skip the WAL sync, and the
    /// error must reach the caller.
    #[test]
    fn flush_failure_still_syncs_the_wal_and_reports_the_error() {
        let mut synced = false;
        let res = flush_then_sync(
            || Err("flush cf_accounts: injected".into()),
            || {
                synced = true;
                Ok(())
            },
        );
        assert!(synced, "WAL sync skipped after a flush failure");
        let err = res.expect_err("flush failure reported as success");
        assert!(err.contains("injected"), "{err}");

        let res = flush_then_sync(|| Err("f".into()), || Err("s".into()));
        let err = res.unwrap_err();
        assert!(err.contains('f') && err.contains('s'), "{err}");
        assert_eq!(flush_then_sync(|| Ok(()), || Ok(())), Ok(()));
    }

    /// Section 71.8 option A: the graceful stop flushes the consensus DB's
    /// memtables as well as the state DB's, and syncs both WALs, so a
    /// restart replays neither.
    #[test]
    fn shutdown_flush_covers_the_consensus_db() {
        use torus_state::cf::{ALL_CF_NAMES, CF_CONSENSUS_META, CF_NATIVE_PENDING};
        let dir = tempfile::tempdir().unwrap();
        let dbs = node_dbs::open_node_dbs(dir.path()).unwrap();
        dbs.state
            .put_cf_raw(CF_BLOCK_HEADERS, &1u64.to_be_bytes(), b"hdr")
            .unwrap();
        dbs.consensus
            .put_cf_raw(CF_CONSENSUS_META, &[9], b"pc")
            .unwrap();
        dbs.consensus
            .put_cf_raw(CF_NATIVE_PENDING, &[1u8; 32], b"body")
            .unwrap();
        let state_syncs = dbs.state.wal_sync_count().unwrap();
        let consensus_syncs = dbs.consensus.wal_sync_count().unwrap();
        flush_for_shutdown(&dbs).unwrap();
        for (db, cfs) in [
            (&dbs.state, &ALL_CF_NAMES[..]),
            (&dbs.consensus, &torus_state::CONSENSUS_DB_CFS[..]),
        ] {
            for cf in cfs {
                let h = db.cf_handle(cf).unwrap();
                let live = db
                    .inner()
                    .property_int_value_cf(h, "rocksdb.num-entries-active-mem-table")
                    .unwrap()
                    .unwrap();
                assert_eq!(live, 0, "{cf} memtable not flushed at shutdown");
            }
        }
        assert!(dbs.state.wal_sync_count().unwrap() > state_syncs);
        assert!(dbs.consensus.wal_sync_count().unwrap() > consensus_syncs);
    }

    /// Codex P2: a second SIGTERM that arrives while the first is being
    /// handled is not lost (the receivers live across both waits).
    #[tokio::test]
    async fn second_sigterm_during_shutdown_is_not_lost() {
        let mut signals = ShutdownSignals::new().unwrap();
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        let got = tokio::time::timeout(Duration::from_secs(5), signals.recv())
            .await
            .expect("first SIGTERM seen");
        assert_eq!(got, "SIGTERM");
        // Sent before the shutdown starts waiting for a second signal.
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let second = tokio::time::timeout(Duration::from_secs(2), signals.recv()).await;
        assert_eq!(second, Ok("SIGTERM"), "second SIGTERM lost");
    }

    /// A worker whose deadline does nothing (the real one exits the process).
    fn worker() -> ShutdownWorker {
        ShutdownWorker::spawn_with(|| {}).unwrap()
    }

    /// Codex P2 (round 2): a stop the shutdown thread cannot take is never
    /// run or dropped on the calling (async) thread, where dropping the
    /// replica would block in its joins with no bound and no second-signal
    /// escape. Normally it runs, and drops, on `node-shutdown`.
    #[tokio::test]
    async fn stop_never_runs_or_drops_on_the_caller() {
        struct Guard(Arc<std::sync::Mutex<Option<String>>>);
        impl Drop for Guard {
            fn drop(&mut self) {
                let name = std::thread::current().name().unwrap_or("").to_string();
                *self.0.lock().unwrap() = Some(name);
            }
        }
        let long = Duration::from_secs(30);
        let never = std::future::pending::<()>;

        let seen = Arc::new(std::sync::Mutex::new(None));
        let guard = Guard(seen.clone());
        let stop = move || {
            drop(guard);
            Ok(())
        };
        let out = bounded_stop(worker(), stop, long, never()).await;
        assert_eq!(out, StopOutcome::Clean);
        assert_eq!(seen.lock().unwrap().as_deref(), Some("node-shutdown"));

        let seen = Arc::new(std::sync::Mutex::new(None));
        let guard = Guard(seen.clone());
        let stop = move || {
            let _held = &guard;
            Ok(())
        };
        let out = bounded_stop(ShutdownWorker::gone(), stop, long, never()).await;
        assert!(matches!(out, StopOutcome::Failed(_)), "{out:?}");
        assert_eq!(
            *seen.lock().unwrap(),
            None,
            "stop dropped on the calling thread"
        );
    }

    /// Codex P2 (round 3): after a timeout or a second signal the stop is
    /// still running (e.g. a RocksDB flush stalled on I/O). `exit()` runs
    /// RocksDB's exit handlers, which join its background threads and so
    /// block on the same I/O; those outcomes must skip them (`_exit`).
    #[test]
    fn abandoned_stop_exits_without_exit_handlers() {
        assert!(StopOutcome::TimedOut.exits_at_once());
        assert!(StopOutcome::Interrupted.exits_at_once());
        // Codex round 4: a clean stop too: an unrelated stalled compaction
        // would otherwise hold up `exit(0)` after flush + WAL sync.
        assert!(StopOutcome::Clean.exits_at_once());
        assert!(StopOutcome::Failed("flush".into()).exits_at_once());
    }

    /// Codex stop review: the last log line before `_exit` must not hold up
    /// the exit (the stdout lock may be held by a thread stuck writing).
    #[test]
    fn last_log_before_exit_is_bounded() {
        let t = std::time::Instant::now();
        let done = log_with_grace(
            || std::thread::sleep(Duration::from_secs(5)),
            Duration::from_millis(200),
        );
        assert!(!done, "a stuck log reported as finished");
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        assert!(log_with_grace(|| {}, Duration::from_secs(5)));
    }

    /// Codex round 6: the deadline must not depend on the tokio timer (a
    /// worker stuck in a blocked log stalls it). The shutdown thread arms an
    /// independent watchdog: no runtime exists in this test.
    #[test]
    fn deadline_fires_without_the_async_runtime() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        static FIRED: AtomicBool = AtomicBool::new(false);
        fn fired() {
            FIRED.store(true, SeqCst);
        }
        let w = ShutdownWorker::spawn_with(fired).unwrap();
        let (reply, _rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let hung: StopJob = Box::new(move || {
            let _ = release_rx.recv();
            Ok(())
        });
        w.jobs
            .send((hung, reply, Duration::from_millis(100)))
            .unwrap();
        let t = std::time::Instant::now();
        while !FIRED.load(SeqCst) && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(FIRED.load(SeqCst), "deadline did not fire");
        drop(release_tx);

        // A stop that finishes in time does not fire it.
        static LATE: AtomicBool = AtomicBool::new(false);
        fn late() {
            LATE.store(true, SeqCst);
        }
        let w = ShutdownWorker::spawn_with(late).unwrap();
        let (reply, rx) = tokio::sync::oneshot::channel();
        w.jobs
            .send((Box::new(|| Ok(())), reply, Duration::from_millis(300)))
            .unwrap();
        assert_eq!(rx.blocking_recv().unwrap(), Ok(()));
        std::thread::sleep(Duration::from_millis(600));
        assert!(!LATE.load(SeqCst), "fired after a clean stop");
    }

    /// Codex stop review: a panicking stop disconnects the watchdog's
    /// completion channel; that must count as a failed stop (exit at once),
    /// not disarm the watchdog.
    #[test]
    fn watchdog_fires_when_the_stop_panics() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        static FIRED: AtomicBool = AtomicBool::new(false);
        fn fired() {
            FIRED.store(true, SeqCst);
        }
        let w = ShutdownWorker::spawn_with(fired).unwrap();
        let (reply, _rx) = tokio::sync::oneshot::channel();
        let boom: StopJob = Box::new(|| panic!("stop panicked"));
        w.jobs.send((boom, reply, Duration::from_secs(30))).unwrap();
        let t = std::time::Instant::now();
        while !FIRED.load(SeqCst) && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(FIRED.load(SeqCst), "watchdog disarmed by the panic");
    }

    #[test]
    fn shutdown_timeout_parse() {
        let s = Duration::from_secs;
        assert_eq!(shutdown_timeout(None), s(60));
        assert_eq!(shutdown_timeout(Some("15".into())), s(15));
        assert_eq!(shutdown_timeout(Some(" 120 ".into())), s(120));
        assert_eq!(shutdown_timeout(Some("0".into())), s(60));
        assert_eq!(shutdown_timeout(Some("soon".into())), s(60));
    }

    #[tokio::test]
    async fn bounded_stop_reports_clean_failed_timeout_and_interrupt() {
        let never = std::future::pending::<()>;
        let long = Duration::from_secs(30);
        assert_eq!(
            bounded_stop(worker(), || Ok(()), long, never()).await,
            StopOutcome::Clean
        );
        assert_eq!(
            bounded_stop(worker(), || Err("flush failed".into()), long, never()).await,
            StopOutcome::Failed("flush failed".into())
        );
        assert!(matches!(
            bounded_stop(worker(), || panic!("stop failed"), long, never()).await,
            StopOutcome::Failed(_)
        ));

        // A hung stop is abandoned at the limit, not waited for.
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let t = std::time::Instant::now();
        let hung = move || {
            let _ = release_rx.recv();
            Ok(())
        };
        let out = bounded_stop(worker(), hung, Duration::from_millis(200), never()).await;
        assert_eq!(out, StopOutcome::TimedOut);
        assert!(t.elapsed() < Duration::from_secs(5));

        // A second signal ends the wait at once.
        let (release2_tx, release2_rx) = std::sync::mpsc::channel::<()>();
        let hung = move || {
            let _ = release2_rx.recv();
            Ok(())
        };
        let interrupt = tokio::time::sleep(Duration::from_millis(50));
        let t = std::time::Instant::now();
        assert_eq!(
            bounded_stop(worker(), hung, long, interrupt).await,
            StopOutcome::Interrupted
        );
        assert!(t.elapsed() < Duration::from_secs(5));
        drop((release_tx, release2_tx));
    }

    /// R01b A: a panic on a listed thread calls the injected exit with 70; a
    /// panic on an unlisted thread (incl. `torus-async-validate`, which is fed
    /// unauthenticated proposal bytes), or one caught by a recovering
    /// `catch_recoverable` on a listed thread, does not. Only this test's own
    /// thread names are asserted, and the default hook is restored at the end
    /// so the hook does not outlive the test in a one-process run.
    #[test]
    fn r01b_fail_stop_panic_hook_exits_70_on_listed_threads_only() {
        static EXITS: std::sync::Mutex<Vec<(String, i32)>> = std::sync::Mutex::new(Vec::new());
        fn record(code: i32) {
            let name = std::thread::current().name().unwrap_or("").to_string();
            EXITS.lock().unwrap().push((name, code));
        }
        install_fail_stop_panic_hook(record);
        let run = |name: &str, f: fn()| {
            std::thread::Builder::new()
                .name(name.into())
                .spawn(f)
                .unwrap()
                .join()
        };
        assert!(run("hotstuff-algo", || panic!("injected write failure")).is_err());
        assert!(run("torus-flush-worker", || panic!("injected")).is_err());
        assert!(run("rpc-worker", || panic!("injected")).is_err());
        assert!(run("torus-async-validate", || panic!("injected")).is_err());
        assert!(run("torus-execution", || {
            let r = torus_bridge::panic_scope::catch_recoverable(|| panic!("recovered"));
            assert!(r.is_err());
        })
        .is_ok());
        let exits: Vec<_> = EXITS
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| {
                [
                    "hotstuff-algo",
                    "torus-flush-worker",
                    "rpc-worker",
                    "torus-async-validate",
                    "torus-execution",
                ]
                .contains(&n.as_str())
            })
            .cloned()
            .collect();
        // Back to the default hook (drops ours and the one it chained).
        drop(std::panic::take_hook());
        assert_eq!(
            exits,
            vec![
                ("hotstuff-algo".to_string(), 70),
                ("torus-flush-worker".to_string(), 70)
            ]
        );
    }

    /// s75 fix E: on reconnect every peer flushes up to 256 queued direct
    /// sends (torus-network `PendingSendQueue`), possibly all for views ahead
    /// of a restarted replica. The buffer must hold such a flush from each
    /// peer; at 1024 bytes it held one header or three votes, and the
    /// survivors' votes for the view the rejoiner collects were evicted
    /// (s75-mc-abcd-r1 rejoin 2: 74.6 s freeze).
    #[test]
    fn progress_buffer_holds_a_full_reconnect_flush_from_three_peers() {
        use hotstuff_rs::hotstuff::messages::{
            BlockDataResponse, NewView, PhaseVote, ProposalHeader,
        };
        use hotstuff_rs::types::crypto_primitives::VerifyingKey;
        use std::mem::size_of;
        let largest = [
            size_of::<ProposalHeader>(),
            size_of::<PhaseVote>(),
            size_of::<NewView>(),
            size_of::<BlockDataResponse>(),
        ]
        .into_iter()
        .max()
        .unwrap();
        let flush = 3 * 256 * (size_of::<VerifyingKey>() + largest) as u64;
        assert!(
            PROGRESS_MSG_BUFFER_BYTES >= flush,
            "buffer {PROGRESS_MSG_BUFFER_BYTES} B < one reconnect flush from 3 peers {flush} B"
        );
    }

    /// RocksDB keeps every SST open (max_open_files = -1) and the book CF
    /// writes 4 MiB SSTs; a systemd service starts with soft NOFILE 1024.
    /// Startup must lift the soft limit to the hard limit, and only raise.
    #[cfg(unix)]
    #[test]
    fn raise_nofile_limit_lifts_soft_to_hard_and_is_idempotent() {
        let get = || {
            let mut lim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) }, 0);
            lim
        };
        let mut lim = get();
        if lim.rlim_max < 1024 {
            eprintln!("skip: hard NOFILE {} < 1024", lim.rlim_max);
            return;
        }
        #[cfg(target_os = "macos")]
        let want = lim.rlim_max.min(MACOS_OPEN_MAX);
        #[cfg(not(target_os = "macos"))]
        let want = lim.rlim_max;
        lim.rlim_cur = 1024;
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) }, 0);

        let (old, new, hard) = raise_nofile_limit().expect("raise");
        assert_eq!((old, new, hard), (1024, want, lim.rlim_max));
        assert_eq!(get().rlim_cur, want);
        assert_eq!(
            raise_nofile_limit().expect("raise again"),
            (want, want, hard)
        );
    }

    #[test]
    fn rocksdb_stats_interval_parse() {
        assert_eq!(rocksdb_stats_interval_secs(None), 5);
        assert_eq!(rocksdb_stats_interval_secs(Some("0".into())), 5);
        assert_eq!(rocksdb_stats_interval_secs(Some("x".into())), 5);
        assert_eq!(rocksdb_stats_interval_secs(Some(" 2 ".into())), 2);
    }

    /// r3 exec-write-stall-attribution: end-to-end wiring — a real DB opened
    /// with tickers on, a few puts, one snapshot, published into the metrics,
    /// and the encoded /metrics text carries NON-ZERO unlabelled
    /// torus_rocksdb_* samples (the thing the bench sampler read as 0 before).
    #[test]
    fn rocksdb_snapshot_publishes_nonzero_unlabelled_gauges() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tuning = torus_state::DbTuning {
            stats_level: 2,
            ..Default::default()
        };
        let db = torus_state::StateDb::open_with_tuning(dir.path(), &tuning).expect("open");
        for i in 0..4u32 {
            db.put_cf_raw(torus_state::cf::CF_ACCOUNTS, &i.to_be_bytes(), &[9u8; 128])
                .expect("put");
        }
        let m = torus_telemetry::Metrics::new();
        publish_rocksdb_stats(&m.rocksdb, &db.runtime_stats());
        let text = m.encode();
        let val = |name: &str| -> i64 {
            text.lines()
                .find_map(|l| {
                    let mut it = l.split_whitespace();
                    (it.next() == Some(name)).then(|| it.next().unwrap_or("0").parse().unwrap_or(0))
                })
                .unwrap_or_else(|| panic!("{name} missing from /metrics:\n{text}"))
        };
        assert!(val("torus_rocksdb_memtable_bytes_all") > 0);
        assert!(val("torus_rocksdb_write_self") >= 4);
        assert!(val("torus_rocksdb_bytes_written") >= 4 * 128);
        assert!(val("torus_rocksdb_db_write_count") >= 4);
        assert_eq!(val("torus_rocksdb_stall_micros"), 0);
        assert_eq!(val("torus_rocksdb_write_stopped"), 0);
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
    fn native_gossip_flag_can_be_disabled() {
        // S458: `--native-gossip=false` must parse and yield false — clap's
        // inferred SetTrue action rejected the value, making the no-gossip
        // direct-to-leader mode (main.rs forward_bodies gate) unreachable.
        let cli = Cli::try_parse_from([
            "torus-node",
            "--keystore",
            "k.keystore",
            "--native-gossip=false",
        ])
        .expect("--native-gossip=false must parse");
        assert!(!cli.native_gossip);

        let cli =
            Cli::try_parse_from(["torus-node", "--keystore", "k.keystore", "--native-gossip=true"])
                .unwrap();
        assert!(cli.native_gossip);

        // Default stays ON with the flag absent.
        let cli = Cli::try_parse_from(["torus-node", "--keystore", "k.keystore"]).unwrap();
        assert!(cli.native_gossip);

        // Bare `--native-gossip` (no value) keeps working as "true".
        let cli =
            Cli::try_parse_from(["torus-node", "--keystore", "k.keystore", "--native-gossip"])
                .unwrap();
        assert!(cli.native_gossip);
    }

    #[test]
    fn exec_trust_cache_flag_can_be_disabled() {
        // Same SetTrue defect as --native-gossip: "=false" was rejected.
        let cli = Cli::try_parse_from([
            "torus-node",
            "--keystore",
            "k.keystore",
            "--exec-trust-cache=false",
        ])
        .expect("--exec-trust-cache=false must parse");
        assert!(!cli.exec_trust_cache);

        let cli = Cli::try_parse_from(["torus-node", "--keystore", "k.keystore"]).unwrap();
        assert!(cli.exec_trust_cache);
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

    fn cli(args: &[&str]) -> Cli {
        let mut all = vec!["torus-node", "--keystore", "k.keystore"];
        all.extend_from_slice(args);
        Cli::try_parse_from(all).unwrap()
    }

    /// D13: `--trade-retention-blocks` is unset by default (keep every trade
    /// row, exactly today) and is its own pruner trigger.
    #[test]
    fn trade_retention_blocks_flag_and_pruner_config() {
        // Neither: archive, no pruner.
        assert!(cli(&[]).trade_retention_blocks.is_none());
        assert!(pruner_config(&cli(&[])).unwrap().is_none());

        // Bodies only: trade history kept.
        let c = pruner_config(&cli(&["--retention-blocks", "5000"]))
            .unwrap()
            .unwrap();
        assert_eq!((c.retention_blocks, c.trade_retention_blocks), (5000, None));

        // Trades only: bodies kept (u64::MAX = cutoff 0).
        let parsed = cli(&["--trade-retention-blocks", "200000"]);
        assert_eq!(parsed.trade_retention_blocks, Some(200000));
        let c = pruner_config(&parsed).unwrap().unwrap();
        assert_eq!(
            (c.retention_blocks, c.trade_retention_blocks),
            (u64::MAX, Some(200000))
        );
        assert_eq!(
            c.prune_interval_blocks,
            PrunerConfig::default().prune_interval_blocks
        );
        assert_eq!(c.batch_size, PrunerConfig::default().batch_size);

        // Both.
        let c = pruner_config(&cli(&[
            "--retention-blocks",
            "5000",
            "--trade-retention-blocks",
            "100",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(
            (c.retention_blocks, c.trade_retention_blocks),
            (5000, Some(100))
        );

        // --archive keeps everything: either retention flag with it is an error.
        let e = pruner_config(&cli(&["--archive", "--trade-retention-blocks", "100"])).unwrap_err();
        assert!(
            e.contains("--archive") && e.contains("--trade-retention-blocks"),
            "{e}"
        );
        assert!(pruner_config(&cli(&["--archive", "--retention-blocks", "100"])).is_err());
        assert!(pruner_config(&cli(&["--archive"])).unwrap().is_none());
    }

    #[test]
    fn trade_retention_blocks_from_config_file() {
        let cfg: ConfigFile = toml::from_str("trade-retention-blocks = 7000").unwrap();
        let mut c = cli(&[]);
        apply_config_defaults(&mut c, cfg);
        assert_eq!(c.trade_retention_blocks, Some(7000));

        // The command line wins over the file.
        let cfg: ConfigFile = toml::from_str("trade-retention-blocks = 7000").unwrap();
        let mut c = cli(&["--trade-retention-blocks", "9"]);
        apply_config_defaults(&mut c, cfg);
        assert_eq!(c.trade_retention_blocks, Some(9));
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

    #[test]
    fn state_hash_attest_key_flag_and_key_file() {
        let cli = Cli::try_parse_from(["torus-node", "--keystore", "k.keystore"]).unwrap();
        assert!(cli.state_hash_attest_key.is_none(), "off by default");
        let cli = Cli::try_parse_from([
            "torus-node",
            "--keystore",
            "k.keystore",
            "--state-hash-attest-key",
            "acct.key",
        ])
        .unwrap();
        assert_eq!(cli.state_hash_attest_key, Some(PathBuf::from("acct.key")));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acct.key");
        std::fs::write(&path, format!("0x{}\n", "11".repeat(32))).unwrap();
        set_mode(&path, 0o600);
        let key = load_state_hash_attest_key(&path).unwrap();
        assert_eq!(key.to_bytes().as_slice(), &[0x11; 32]);
        std::fs::write(&path, "zz").unwrap();
        assert!(load_state_hash_attest_key(&path).is_err());
        assert!(load_state_hash_attest_key(&dir.path().join("missing")).is_err());
    }

    fn set_mode(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Review finding 6: the attest key file must be owner-only (0600 or
    /// stricter); anything group/other-accessible is refused with a clear
    /// error naming the path and the required mode.
    #[test]
    fn state_hash_attest_key_requires_owner_only_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acct.key");
        std::fs::write(&path, "22".repeat(32)).unwrap();
        for mode in [0o644, 0o640, 0o604, 0o660, 0o777] {
            set_mode(&path, mode);
            let err = load_state_hash_attest_key(&path).expect_err("group/other access must be refused");
            let msg = err.to_string();
            assert!(msg.contains("0600") && msg.contains("acct.key"), "{mode:o}: {msg}");
        }
        for mode in [0o600, 0o400] {
            set_mode(&path, mode);
            assert!(load_state_hash_attest_key(&path).is_ok(), "{mode:o}");
        }
    }

    /// Review finding 6: non-ASCII input of the right byte length must be an
    /// error, never a panic (byte slicing inside a multi-byte char).
    #[test]
    fn decode_hex_key_rejects_non_ascii_without_panicking() {
        let tricky = format!("a{}b", "\u{e9}".repeat(31)); // 64 bytes, not 64 chars
        assert_eq!(tricky.len(), 64);
        let out = std::panic::catch_unwind(|| decode_hex_key(&tricky).map_err(|e| e.to_string()));
        let err = out.expect("must not panic").expect_err("non-ASCII must be rejected");
        assert!(err.contains("hex"), "{err}");
        assert!(decode_hex_key(&"\u{0661}".repeat(32)).is_err(), "non-ASCII digits");
        assert!(decode_hex_key(&"ab".repeat(32)).is_ok());
    }
}
