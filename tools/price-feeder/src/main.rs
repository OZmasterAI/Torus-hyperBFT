//! `price-feeder`: keygen | address | check | run (docs/plans/oracle-feeder.md).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use torus_price_feeder::config::{Config, Exchange};
use torus_price_feeder::exchange::ReqwestGet;
use torus_price_feeder::feeder::{self, Feeder};
use torus_price_feeder::fetch::{requests, Clock, Fetcher, SystemClock};
use torus_price_feeder::keyfile::{keygen, load_signer, read_passphrase};
use torus_price_feeder::node::{register_hint, startup_check, Readiness, RpcNode};
use torus_price_feeder::{health, price};

#[derive(Parser)]
#[command(name = "price-feeder", version, about = "Torus validator oracle price feeder")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a NEW hot signer keystore and print its address and the
    /// registration command (run that with the VALIDATOR keystore).
    Keygen {
        #[arg(long)]
        keystore: PathBuf,
        /// Read the passphrase from this file instead of prompting.
        #[arg(long)]
        passphrase_file: Option<PathBuf>,
    },
    /// Print the signer address of the configured key.
    Address {
        #[arg(long)]
        config: PathBuf,
    },
    /// Startup check + one fetch/aggregate; prints a per-market table. Never submits.
    Check {
        #[arg(long)]
        config: PathBuf,
    },
    /// Run the feeder (until ctrl-c).
    Run {
        #[arg(long)]
        config: PathBuf,
    },
}

fn passphrase(file: Option<PathBuf>) -> Result<String, String> {
    match file {
        Some(p) => read_passphrase(&p),
        None => {
            let a = rpassword::read_password_from_tty(Some("Signer keystore passphrase: "))
                .map_err(|e| e.to_string())?;
            let b = rpassword::read_password_from_tty(Some("Repeat passphrase: ")).map_err(|e| e.to_string())?;
            if a != b {
                return Err("passphrases differ".into());
            }
            Ok(a)
        }
    }
}

async fn check(cfg: Config) -> Result<(), String> {
    let key = load_signer(&cfg)?;
    let signer = torus_wallet::keystore::address_from_key(&key);
    let node = RpcNode::new(&cfg.rpc_url)?;
    let report = startup_check(&cfg, &node, signer).await?;
    println!("signer     {signer:#x}");
    println!("validator  {:#x}", cfg.validator_address);
    match &report.readiness {
        Readiness::Ready => println!("status     ready"),
        Readiness::Idle(m) => println!("status     IDLE: {m}"),
    }
    for w in &report.warnings {
        println!("warning    {w}");
    }
    let http = Arc::new(ReqwestGet::new()?);
    let mut fetcher = Fetcher::default();
    let clock = SystemClock;
    let timeout = std::time::Duration::from_millis(cfg.fetch_timeout_ms);
    fetcher.fetch_all(&http, &clock, &requests(&cfg), timeout).await;
    let now = clock.now_ms();
    println!("\nvenues");
    for ex in Exchange::ALL {
        match fetcher.venue(ex) {
            None => println!("  {:8} (not used)", ex.name()),
            Some(v) => match &v.last_error {
                None => println!("  {:8} ok, {} ms, {} tickers", ex.name(), v.latency_ms.unwrap_or(0), v.quotes.len()),
                Some(e) => println!("  {:8} ERROR {e}", ex.name()),
            },
        }
    }
    let rules = feeder::rules(&cfg);
    let rate = feeder::usdt_usd(&cfg, &fetcher, now);
    println!("\nmarkets");
    for m in &cfg.markets {
        let (samples, missing) = feeder::market_samples(&cfg, &fetcher, m);
        let listed = if report.listed.contains(&m.market_id) { "" } else { " [NOT LISTED: skipped]" };
        match price::aggregate(&samples, &cfg.weights_for(m), rate, now, &rules) {
            price::Outcome::Price { price, sources, weight } => {
                println!("  {:>5} {:8} {price} ({sources} sources, weight {weight}){listed}", m.market_id, m.base_asset)
            }
            o => println!("  {:>5} {:8} OMITTED: {}{listed}", m.market_id, m.base_asset, o.reason().unwrap_or_default()),
        }
        if !missing.is_empty() {
            println!("        missing symbols: {}", missing.join(", "));
        }
    }
    println!("\n(check never submits)");
    Ok(())
}

async fn run(cfg: Config) -> Result<(), String> {
    let key = load_signer(&cfg)?;
    let signer = torus_wallet::keystore::address_from_key(&key);
    let node = RpcNode::new(&cfg.rpc_url)?;
    // Fatal misconfiguration stops here; a non-active validator only idles.
    let report = startup_check(&cfg, &node, signer).await?;
    for w in &report.warnings {
        tracing::warn!("{w}");
    }
    if let Readiness::Idle(m) = &report.readiness {
        tracing::warn!("validator not active, feeder idles until it is: {m}");
    }
    let listener = tokio::net::TcpListener::bind(&cfg.health_listen)
        .await
        .map_err(|e| format!("health_listen {}: {e}", cfg.health_listen))?;
    tracing::info!(signer = %format!("{signer:#x}"), health = %cfg.health_listen, "price feeder starting");
    let feeder = Feeder::new(cfg, Arc::new(ReqwestGet::new()?), node, SystemClock, key);
    tokio::spawn(health::serve(listener, feeder.status(), SystemClock));
    feeder
        .run(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let r = match cli.command {
        Command::Keygen { keystore, passphrase_file } => passphrase(passphrase_file)
            .and_then(|pw| keygen(&keystore, &pw))
            .map(|addr| {
                println!("signer address: {addr:#x}");
                println!("keystore:       {}", keystore.display());
                println!("\nRegister it once with the VALIDATOR's EVM keystore:\n  {}", register_hint(addr));
            }),
        Command::Address { config } => Config::load(&config)
            .and_then(|c| load_signer(&c))
            .map(|k| println!("{:#x}", torus_wallet::keystore::address_from_key(&k))),
        Command::Check { config } => match Config::load(&config) {
            Ok(c) => check(c).await,
            Err(e) => Err(e),
        },
        Command::Run { config } => match Config::load(&config) {
            Ok(c) => run(c).await,
            Err(e) => Err(e),
        },
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
