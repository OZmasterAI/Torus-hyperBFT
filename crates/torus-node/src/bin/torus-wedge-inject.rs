//! `torus-wedge-inject` — DEVNET/TEST ONLY. Fabricates a QC'd-poison-ancestry
//! hole in a node's consensus tree so the `torus-unwedge` recovery flow can be
//! proven end-to-end on a live devnet (organic formation is not reproducible
//! on current binaries — the S444+ healing layers defeat it). See
//! `hotstuff_rs::block_tree::recovery::inject_hole_for_testing`.
//!
//! NEVER run this against a chain you care about.
//!
//! Node must be stopped (exclusive RocksDB open). Without --hash it picks a
//! victim and prints its hex hash; pass that hash to every OTHER node so the
//! fleet shares one hole (a node that keeps the block would heal its peers).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use hotstuff_rs::block_tree::recovery;
use hotstuff_rs::types::data_types::CryptoHash;
use torus_consensus::RocksKVStore;
use torus_state::StateDb;

#[derive(Parser)]
#[command(
    name = "torus-wedge-inject",
    about = "DEVNET ONLY: inject a consensus-tree hole to test the unwedge flow"
)]
struct Cli {
    /// Node data directory (node must be STOPPED).
    #[arg(long)]
    data_dir: PathBuf,

    /// Delete this specific block (hex, from a prior pick on another node).
    #[arg(long)]
    hash: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let target = match &cli.hash {
        Some(hs) => match hex::decode(hs) {
            Ok(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                Some(CryptoHash::new(arr))
            }
            _ => {
                eprintln!("torus-wedge-inject: --hash must be 32 bytes of hex");
                return ExitCode::from(1);
            }
        },
        None => None,
    };

    // Section 71.8 option A: the HotStuff store is `<data_dir>/consensus-db`.
    let path = torus_state::consensus_db_path(&cli.data_dir);
    if !path.is_dir() {
        eprintln!("torus-wedge-inject: no consensus DB at {}", path.display());
        return ExitCode::from(1);
    }
    let db = match StateDb::open_consensus(&path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("torus-wedge-inject: exclusive open (is the node stopped?): {e}");
            return ExitCode::from(1);
        }
    };
    let kv = RocksKVStore::new(db.db_arc());

    match recovery::inject_hole_for_testing(kv, target) {
        Ok(victim) => {
            println!("{}", hex::encode(victim.bytes()));
            ExitCode::from(0)
        }
        Err(e) => {
            eprintln!("torus-wedge-inject: {e:?}");
            ExitCode::from(1)
        }
    }
}
