//! `torus-unwedge` — offline consensus-state surgery for QC'd-poison-ancestry
//! wedges (S459). See `docs/plans/unwedge-tool.md` and
//! `hotstuff_rs::block_tree::recovery` for the mechanism.
//!
//! Modes (mutually exclusive):
//!   --inspect (default)        read-only classification; safe on a LIVE node
//!                              (sees data as of the last RocksDB flush).
//!   --export-pc <FILE>         read-only; writes the best recovery-PC
//!                              candidate (Borsh bytes) for distribution to
//!                              nodes that lack one.
//!   --apply --pc-file <FILE>   surgery. Opens the DB exclusively — the node
//!                              MUST be stopped (RocksDB LOCK enforces this).
//!                              Prints the plan and exits 2 unless --yes.
//!
//! Exit codes: 0 ok | 1 error | 2 apply refused (missing --yes).

use std::path::PathBuf;
use std::process::ExitCode;

use borsh::{BorshDeserialize, BorshSerialize};
use clap::Parser;
use hotstuff_rs::block_tree::recovery::{self, RecoveryError, WedgeReport};
use hotstuff_rs::hotstuff::types::PhaseCertificate;
use torus_consensus::RocksKVStore;
use torus_state::StateDb;

#[derive(Parser)]
#[command(
    name = "torus-unwedge",
    about = "Offline consensus block-tree surgery for permanently wedged Torus nodes"
)]
struct Cli {
    /// Node data directory (the RocksDB holding cf_consensus_meta).
    #[arg(long)]
    data_dir: PathBuf,

    /// Read-only classification (default mode).
    #[arg(long)]
    inspect: bool,

    /// Read-only: export the best recovery-PC candidate to FILE (Borsh bytes).
    #[arg(long, value_name = "FILE")]
    export_pc: Option<PathBuf>,

    /// Perform the surgery (requires --pc-file; node must be stopped).
    #[arg(long)]
    apply: bool,

    /// Recovery PC to install (from --export-pc on a node that has one).
    #[arg(long, value_name = "FILE")]
    pc_file: Option<PathBuf>,

    /// Actually write. Without this, --apply only prints the plan (exit 2).
    #[arg(long)]
    yes: bool,

    /// Emit machine-readable JSON instead of prose.
    #[arg(long)]
    json: bool,
}

fn hex32(h: &hotstuff_rs::types::data_types::CryptoHash) -> String {
    hex::encode(h.bytes())
}

fn report_json(r: &WedgeReport) -> serde_json::Value {
    serde_json::json!({
        "committed_frontier_height": r.committed_frontier_height.int(),
        "committed_frontier_hash": hex32(&r.committed_frontier_hash),
        "committed_retained": r.committed_retained,
        "uncommitted_count": r.uncommitted.len(),
        "uncommitted": r.uncommitted.iter().map(hex32).collect::<Vec<_>>(),
        "hole_count": r.holes.len(),
        "holes": r.holes.iter().map(hex32).collect::<Vec<_>>(),
        "pc_candidate_count": r.pc_candidates.len(),
        "pc_candidate_views": r.pc_candidates.iter().map(|pc| pc.view.int()).collect::<Vec<_>>(),
    })
}

fn print_report(r: &WedgeReport, json: bool) {
    if json {
        println!("{}", report_json(r));
    } else {
        println!(
            "committed frontier : height {} hash {}",
            r.committed_frontier_height.int(),
            hex32(&r.committed_frontier_hash)
        );
        println!("committed retained : {}", r.committed_retained);
        println!("uncommitted blocks : {}", r.uncommitted.len());
        println!("holes (lost parents): {}", r.holes.len());
        for h in &r.holes {
            println!("    missing {}", hex32(h));
        }
        println!("recovery-PC candidates: {}", r.pc_candidates.len());
        for pc in &r.pc_candidates {
            println!("    PC view {} -> frontier", pc.view.int());
        }
        if r.uncommitted.is_empty() && r.holes.is_empty() {
            println!("verdict: tree is CLEAN (nothing to unwedge)");
        } else if r.pc_candidates.is_empty() {
            println!(
                "verdict: WEDGED, no local recovery PC — import one via --apply --pc-file \
                 (export from a node that has a candidate)"
            );
        } else {
            println!("verdict: WEDGED, recovery PC harvestable locally (--export-pc)");
        }
    }
}

fn best_candidate(r: &WedgeReport) -> Option<&PhaseCertificate> {
    // Any candidate certifies the same frontier; prefer the highest view
    // (freshest evidence, and monotonic vs peers' stored PCs).
    r.pc_candidates.iter().max_by_key(|pc| pc.view.int())
}

fn run(cli: Cli) -> Result<u8, String> {
    let modes = [cli.inspect, cli.export_pc.is_some(), cli.apply];
    if modes.iter().filter(|m| **m).count() > 1 {
        return Err("pick ONE of --inspect / --export-pc / --apply".into());
    }

    if cli.apply {
        let pc_path = cli
            .pc_file
            .as_ref()
            .ok_or("--apply requires --pc-file <FILE>")?;
        let pc_bytes =
            std::fs::read(pc_path).map_err(|e| format!("read {}: {e}", pc_path.display()))?;
        let pc = PhaseCertificate::try_from_slice(&pc_bytes)
            .map_err(|e| format!("decode {}: {e}", pc_path.display()))?;

        // Exclusive open: fails on RocksDB LOCK if the node is still running.
        let db = StateDb::open(&cli.data_dir)
            .map_err(|e| format!("exclusive open (is the node stopped?): {e}"))?;
        let kv = RocksKVStore::new(db.db_arc());

        let report = recovery::inspect(kv.clone()).map_err(fmt_recovery_err)?;
        print_report(&report, cli.json);
        println!(
            "plan: prune {} uncommitted block(s), clear {} hole(s), install PC view {} at frontier",
            report.uncommitted.len(),
            report.holes.len(),
            pc.view.int()
        );
        if !cli.yes {
            println!("dry run — re-run with --yes to write");
            return Ok(2);
        }

        let surgery = recovery::apply(kv, &pc).map_err(fmt_recovery_err)?;
        if cli.json {
            println!(
                "{}",
                serde_json::json!({
                    "applied": true,
                    "pruned": surgery.pruned,
                    "holes_cleared": surgery.holes_cleared,
                    "recovery_pc_view": surgery.recovery_pc_view.int(),
                    "frontier_height": surgery.frontier_height.int(),
                    "frontier_hash": hex32(&surgery.frontier_hash),
                })
            );
        } else {
            println!(
                "APPLIED: pruned {} block(s), cleared {} hole(s); highest/locked PC now view {} \
                 at frontier height {} ({}). verify_after: OK",
                surgery.pruned,
                surgery.holes_cleared,
                surgery.recovery_pc_view.int(),
                surgery.frontier_height.int(),
                hex32(&surgery.frontier_hash)
            );
        }
        return Ok(0);
    }

    // Read-only modes.
    let db = StateDb::open_read_only(&cli.data_dir)
        .map_err(|e| format!("read-only open: {e}"))?;
    let kv = RocksKVStore::new(db.db_arc());
    let report = recovery::inspect(kv).map_err(fmt_recovery_err)?;

    if let Some(out) = &cli.export_pc {
        let pc = best_candidate(&report).ok_or(
            "no recovery-PC candidate in this tree — try another node (the poison \
             proposer usually has one)",
        )?;
        let bytes = pc
            .try_to_vec()
            .map_err(|e| format!("serialize recovery PC: {e}"))?;
        std::fs::write(out, &bytes).map_err(|e| format!("write {}: {e}", out.display()))?;
        println!(
            "exported recovery PC (view {}, {} bytes) -> {}",
            pc.view.int(),
            bytes.len(),
            out.display()
        );
        return Ok(0);
    }

    print_report(&report, cli.json);
    Ok(0)
}

fn fmt_recovery_err(e: RecoveryError) -> String {
    match e {
        RecoveryError::BlockTree(err) => format!("block tree error: {err:?}"),
        RecoveryError::NoCommittedBlock => "no committed block (empty/uninitialized tree)".into(),
        RecoveryError::PcWrongBlock => {
            "recovery PC does not certify this node's committed frontier".into()
        }
        RecoveryError::PcWrongPhase => {
            "recovery PC phase cannot serve as a block justify (need Generic/Decide)".into()
        }
        RecoveryError::PcBadSignatures => {
            "recovery PC signature quorum invalid against the committed validator set".into()
        }
        RecoveryError::VerifyFailed(why) => format!("post-apply verification FAILED: {why}"),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("torus-unwedge: {msg}");
            ExitCode::from(1)
        }
    }
}
