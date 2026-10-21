//! SIGTERM is a graceful shutdown (owner s112), the same as SIGINT.
//!
//! `systemctl stop`, `docker stop` and Kubernetes send SIGTERM. These tests
//! spawn the real `torus-node` binary as a 1-validator devnet (the uniswap e2e
//! genesis shape), let it commit and execute a few blocks, send SIGTERM, and
//! check that it stops consensus, drains execution, exits 0 within a bound,
//! and restarts from the same applied state.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use torus_state::cf::CF_BLOCK_HEADERS;
use torus_state::StateDb;

/// Hard bound for the whole stop (signal to process exit).
const STOP_BOUND: Duration = Duration::from_secs(60);
/// Blocks executed before the signal is sent.
const MIN_BLOCKS: u64 = 3;

/// 1-validator genesis from `devnet/genesis.json` (as `make-ci-genesis.py`).
fn write_genesis(dir: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../devnet/genesis.json");
    let mut g: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&src).expect("read devnet genesis")).unwrap();
    let first = g["validators"][0].clone();
    g["validators"] = serde_json::Value::Array(vec![first]);
    let out = dir.join("genesis.json");
    std::fs::write(&out, serde_json::to_vec_pretty(&g).unwrap()).unwrap();
    out
}

struct Node {
    child: Child,
    log: PathBuf,
}

/// The node command (1-validator devnet in `work`), stdio not yet set.
fn node_command(work: &Path, genesis: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_torus-node"));
    cmd.arg("--genesis")
        .arg(genesis)
        .arg("--data-dir")
        .arg(work.join("data"))
        .args([
            "--validator-key",
            "0100000000000000000000000000000000000000000000000000000000000000",
            "--p2p-listen",
            "/ip4/127.0.0.1/udp/0/quic-v1",
            "--rpc-addr",
            "127.0.0.1:0",
            "--metrics-addr",
            "127.0.0.1:0",
            "--log-level",
            "info",
        ])
        .env_remove("RUST_LOG")
        // Item 6 Phase 3 knobs: each test sets what it needs.
        .env_remove("TORUS_CHECKPOINT_INTERVAL_MS")
        .env_remove("TORUS_CHECKPOINT_MAX_BYTES")
        .env_remove("TORUS_EXEC_PIPELINE")
        .env_remove("TORUS_BOOK_ROWS")
        .stdin(Stdio::null());
    cmd
}

impl Node {
    fn start(work: &Path, genesis: &Path, run: &str) -> Node {
        Node::start_env(work, genesis, run, &[])
    }

    fn start_env(work: &Path, genesis: &Path, run: &str, env: &[(&str, &str)]) -> Node {
        let log = work.join(format!("node-{run}.log"));
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let child = node_command(work, genesis)
            .envs(env.iter().copied())
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("spawn torus-node");
        Node { child, log }
    }

    /// The log without ANSI colour codes.
    fn log_text(&self) -> String {
        strip_ansi(&std::fs::read_to_string(&self.log).unwrap_or_default())
    }

    /// Highest `execution pipeline: block done height=N` in the log.
    fn executed_height(&self) -> Option<u64> {
        last_height(&self.log_text(), "execution pipeline: block done")
    }

    fn wait_executed_above(&mut self, floor: u64) -> u64 {
        let start = Instant::now();
        loop {
            if let Some(h) = self.executed_height().filter(|h| *h > floor) {
                return h;
            }
            if let Ok(Some(st)) = self.child.try_wait() {
                panic!("node exited early ({st}):\n{}", self.tail());
            }
            assert!(
                start.elapsed() < Duration::from_secs(120),
                "no executed block above {floor} in 120 s:\n{}",
                self.tail()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn signal(&self, sig: libc::c_int) {
        let rc = unsafe { libc::kill(self.child.id() as libc::pid_t, sig) };
        assert_eq!(rc, 0, "kill({sig}) failed");
    }

    /// Wait for exit within `bound`; on timeout SIGKILL the node and fail.
    fn wait_exit(&mut self, bound: Duration) -> (ExitStatus, Duration) {
        let start = Instant::now();
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                return (st, start.elapsed());
            }
            if start.elapsed() > bound {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "node did not exit within {bound:?} of the signal:\n{}",
                    self.tail()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn tail(&self) -> String {
        let text = self.log_text();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(40)..].join("\n")
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn last_height(log: &str, marker: &str) -> Option<u64> {
    log.lines()
        .filter(|l| l.contains(marker))
        .filter_map(|l| {
            let rest = &l[l.find("height=")? + "height=".len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .max()
}

/// Graceful stop on `sig`: exit 0 within [`STOP_BOUND`], consensus stopped
/// and the execution queue drained (deliberate stop, no fail-stop).
fn assert_graceful_stop(node: &mut Node, sig: libc::c_int, name: &str) {
    node.signal(sig);
    let (status, took) = node.wait_exit(STOP_BOUND);
    let log = node.log_text();
    assert_eq!(
        status.code(),
        Some(0),
        "{name}: node must exit 0, got {status} after {took:?}:\n{}",
        node.tail()
    );
    assert!(
        log.contains("shutdown signal received") && log.contains(&format!("signal={name}")),
        "{name}: no shutdown line naming the signal:\n{}",
        node.tail()
    );
    assert!(
        log.contains("execution pipeline thread shutting down"),
        "{name}: execution pipeline was not stopped deliberately:\n{}",
        node.tail()
    );
    assert!(
        log.contains("shutdown complete"),
        "{name}: no shutdown-complete line:\n{}",
        node.tail()
    );
    assert!(
        !log.contains("FATAL"),
        "{name}: fatal line on shutdown:\n{}",
        node.tail()
    );
}

#[test]
fn sigterm_stops_the_node_gracefully_and_exits_zero() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path());
    let mut node = Node::start(work.path(), &genesis, "a");
    node.wait_executed_above(MIN_BLOCKS);
    assert_graceful_stop(&mut node, libc::SIGTERM, "SIGTERM");
}

#[test]
fn sigint_still_stops_the_node_gracefully() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path());
    let mut node = Node::start(work.path(), &genesis, "a");
    node.wait_executed_above(MIN_BLOCKS);
    assert_graceful_stop(&mut node, libc::SIGINT, "SIGINT");
}

/// Applied height (latest stored header) and that header's bytes.
fn applied_head(data: &Path) -> (u64, Vec<u8>) {
    let db = StateDb::open(data).expect("reopen data dir after the node exited");
    let h = torus_rpc::find_latest_height(&db);
    let hdr = db
        .get_cf_raw(CF_BLOCK_HEADERS, &h.to_be_bytes())
        .unwrap()
        .expect("header at the applied height");
    (h, hdr)
}

#[test]
fn restart_after_sigterm_resumes_from_the_same_applied_state() {
    // Per-block flush pinned (`0`): step 6 made 15 s checkpoints the
    // validator default; `sigterm_with_checkpoints_restarts_with_zero_replay`
    // covers that path, this test keeps the per-block restart covered.
    const PER_BLOCK: &[(&str, &str)] = &[("TORUS_CHECKPOINT_INTERVAL_MS", "0")];
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path());
    let data = work.path().join("data");

    let mut node = Node::start_env(work.path(), &genesis, "a", PER_BLOCK);
    node.wait_executed_above(MIN_BLOCKS);
    assert_graceful_stop(&mut node, libc::SIGTERM, "SIGTERM");
    // Every block the node executed is the stored head: nothing was dropped
    // mid-flight and nothing executed after the head was written.
    let executed = node.executed_height().unwrap();
    drop(node);
    let (head, head_hdr) = applied_head(&data);
    assert_eq!(
        head, executed,
        "stored head must be the last executed block"
    );

    let mut node = Node::start_env(work.path(), &genesis, "b", PER_BLOCK);
    let resumed = node.wait_executed_above(head);
    let log = node.log_text();
    assert!(
        !log.contains("boot reconciliation delivered"),
        "graceful stop left committed blocks undelivered:\n{}",
        node.tail()
    );
    assert!(
        !log.contains("initializing from genesis"),
        "restart re-ran genesis:\n{}",
        node.tail()
    );
    assert_graceful_stop(&mut node, libc::SIGTERM, "SIGTERM");
    drop(node);

    let db = StateDb::open(&data).unwrap();
    let again = db
        .get_cf_raw(CF_BLOCK_HEADERS, &head.to_be_bytes())
        .unwrap()
        .expect("pre-restart head still stored");
    assert_eq!(
        again, head_hdr,
        "header at height {head} changed across the restart"
    );
    assert!(torus_rpc::find_latest_height(&db) >= resumed);
}

/// The native applied-height marker (the durable marker a restart replays
/// from), read from the stopped node's data dir; absent (nothing durable
/// past genesis) reads as 0.
fn durable_marker(data: &Path) -> u64 {
    use torus_state::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
    let db = StateDb::open(data).expect("reopen data dir after the node exited");
    db.get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
        .unwrap()
        .map_or(0, |raw| {
            u64::from_be_bytes(raw.as_slice().try_into().expect("8-byte marker"))
        })
}

/// Item 6 Phase 3: with coalesced checkpoints (15 s interval, so no periodic
/// checkpoint closes before the signal), SIGTERM must close the open window
/// into a final checkpoint inside the graceful stop, before the CF flush and
/// WAL sync. The durable marker then equals the last executed height and the
/// restart replays zero blocks.
#[test]
fn sigterm_with_checkpoints_restarts_with_zero_replay() {
    const CKPT: &[(&str, &str)] = &[("TORUS_CHECKPOINT_INTERVAL_MS", "15000")];
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path());
    let data = work.path().join("data");

    let mut node = Node::start_env(work.path(), &genesis, "a", CKPT);
    node.wait_executed_above(MIN_BLOCKS);
    assert!(
        node.log_text().contains("coalesced state checkpoints ON"),
        "checkpoint mode not active:\n{}",
        node.tail()
    );
    assert_graceful_stop(&mut node, libc::SIGTERM, "SIGTERM");
    assert!(
        !node
            .log_text()
            .contains("final checkpoint at shutdown failed"),
        "final checkpoint failed:\n{}",
        node.tail()
    );
    let executed = node.executed_height().unwrap();
    drop(node);
    assert_eq!(
        durable_marker(&data),
        executed,
        "durable marker after SIGTERM must be the last executed height \
         (the final checkpoint did not land)"
    );

    let mut node = Node::start_env(work.path(), &genesis, "b", CKPT);
    node.wait_executed_above(executed);
    let log = node.log_text();
    assert!(
        !log.contains("execution gap detected, replaying"),
        "restart replayed blocks after SIGTERM:\n{}",
        node.tail()
    );
    assert!(
        !log.contains("boot reconciliation delivered"),
        "graceful stop left committed blocks undelivered:\n{}",
        node.tail()
    );
    assert!(
        !log.contains("initializing from genesis"),
        "restart re-ran genesis:\n{}",
        node.tail()
    );
    // Zero replay: no height at or below the restored one executed again.
    let re_executed: Vec<u64> = log
        .lines()
        .filter(|l| l.contains("execution pipeline: block done"))
        .filter_map(|l| last_height(l, "execution pipeline: block done"))
        .filter(|h| *h <= executed)
        .collect();
    assert!(
        re_executed.is_empty(),
        "restart re-executed heights {re_executed:?} (restored {executed})"
    );
    assert_graceful_stop(&mut node, libc::SIGTERM, "SIGTERM");
}

/// Section 71.8 option A on the real binary: after a run (15 s checkpoints,
/// so the applied marker is written by the final checkpoint batch), the
/// HotStuff state is in `<data>/consensus-db` and not in the state DB, and the
/// checkpoint's state meta key is in the state DB and not in the consensus
/// DB. A restart resumes from that HotStuff state: no genesis re-init, the
/// chain grows, and the highest voted view does not go backwards.
#[test]
fn hotstuff_state_lives_in_the_consensus_db_and_restart_resumes_from_it() {
    use hotstuff_rs::block_tree::variables::{
        APP_FED_BLOCK_HEIGHT, COMMITTED_VALIDATOR_SET, HIGHEST_COMMITTED_BLOCK,
        HIGHEST_VIEW_PHASE_VOTED,
    };
    use torus_state::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
    const CKPT: &[(&str, &str)] = &[("TORUS_CHECKPOINT_INTERVAL_MS", "15000")];
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path());
    let data = work.path().join("data");
    let voted_view = |cdb: &StateDb| -> u64 {
        let raw = cdb
            .get_cf_raw(CF_CONSENSUS_META, &HIGHEST_VIEW_PHASE_VOTED)
            .unwrap()
            .expect("the validator voted");
        u64::from_le_bytes(raw[..8].try_into().unwrap())
    };

    let mut node = Node::start_env(work.path(), &genesis, "a", CKPT);
    node.wait_executed_above(MIN_BLOCKS);
    assert_graceful_stop(&mut node, libc::SIGTERM, "SIGTERM");
    let executed = node.executed_height().unwrap();
    drop(node);
    let voted = {
        let state = StateDb::open(&data).unwrap();
        let cdb = StateDb::open_consensus(&torus_state::consensus_db_path(&data)).unwrap();
        for key in [
            COMMITTED_VALIDATOR_SET,
            HIGHEST_COMMITTED_BLOCK,
            HIGHEST_VIEW_PHASE_VOTED,
            APP_FED_BLOCK_HEIGHT,
        ] {
            assert!(
                cdb.get_cf_raw(CF_CONSENSUS_META, &key).unwrap().is_some(),
                "HotStuff key {key:?} missing in consensus-db"
            );
            assert!(
                state.get_cf_raw(CF_CONSENSUS_META, &key).unwrap().is_none(),
                "HotStuff key {key:?} found in the state DB"
            );
        }
        assert!(
            state
                .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
                .unwrap()
                .is_some(),
            "applied marker missing in the state DB"
        );
        assert!(
            cdb.get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
                .unwrap()
                .is_none(),
            "applied marker found in consensus-db"
        );
        voted_view(&cdb)
    };
    assert_eq!(durable_marker(&data), executed);

    let mut node = Node::start_env(work.path(), &genesis, "b", CKPT);
    node.wait_executed_above(executed);
    assert!(
        !node.log_text().contains("initializing from genesis"),
        "restart re-ran genesis:\n{}",
        node.tail()
    );
    assert_graceful_stop(&mut node, libc::SIGTERM, "SIGTERM");
    drop(node);
    let cdb = StateDb::open_consensus(&torus_state::consensus_db_path(&data)).unwrap();
    assert!(
        voted_view(&cdb) > voted,
        "highest voted view did not advance from the restored vote state"
    );
}

/// Section 71.8 option A, restart rule on the real binary: an old-layout
/// data dir (HotStuff state in the state DB) is refused with a clear error
/// and a non-zero exit, and `consensus-db` is not created.
#[test]
fn old_layout_data_dir_refuses_to_start() {
    use hotstuff_rs::block_tree::variables::COMMITTED_VALIDATOR_SET;
    use torus_state::cf::CF_CONSENSUS_META;
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path());
    let data = work.path().join("data");
    {
        let db = StateDb::open(&data).unwrap();
        db.put_cf_raw(CF_CONSENSUS_META, &COMMITTED_VALIDATOR_SET, b"old-layout")
            .unwrap();
    }
    let mut node = Node::start(work.path(), &genesis, "old");
    let (status, _) = node.wait_exit(STOP_BOUND);
    let log = node.log_text();
    assert!(!status.success(), "old layout started:\n{}", node.tail());
    assert!(
        log.contains("refusing to start") && log.contains("before section 71.8 option A"),
        "no clear old-layout error:\n{}",
        node.tail()
    );
    assert!(
        !data.join("consensus-db").exists(),
        "old-layout dir was changed"
    );
}

/// Item 6 Phase 3 step 6: the compiled default. A validator started with
/// `TORUS_CHECKPOINT_INTERVAL_MS` unset runs coalesced checkpoints every 15 s
/// (the gate's interval); before step 6 unset meant the per-block flush.
#[test]
fn env_unset_validator_runs_15s_checkpoints() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path());
    let mut node = Node::start(work.path(), &genesis, "a");
    node.wait_executed_above(MIN_BLOCKS);
    let log = node.log_text();
    assert!(
        log.contains("coalesced state checkpoints ON"),
        "env unset: a validator must default to checkpoints:\n{}",
        node.tail()
    );
    assert!(
        log.contains("interval: Some(15s)"),
        "env unset: the compiled default interval must be 15 s:\n{}",
        node.tail()
    );
    assert_graceful_stop(&mut node, libc::SIGTERM, "SIGTERM");
}

/// `TORUS_CHECKPOINT_INTERVAL_MS=0` still selects today's per-block flush.
#[test]
fn explicit_zero_interval_keeps_the_per_block_flush() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path());
    let mut node = Node::start_env(
        work.path(),
        &genesis,
        "a",
        &[("TORUS_CHECKPOINT_INTERVAL_MS", "0")],
    );
    node.wait_executed_above(MIN_BLOCKS);
    assert!(
        !node.log_text().contains("coalesced state checkpoints ON"),
        "interval 0 must keep the per-block flush:\n{}",
        node.tail()
    );
    assert_graceful_stop(&mut node, libc::SIGTERM, "SIGTERM");
}

/// A deferred book mode (genesis `consensus.book_mode` = mode 3) cannot run
/// with checkpoints (owner s109, task 1.2). With the interval unset the
/// compiled default yields (per-block flush, a warning) instead of refusing
/// to start; an explicit interval still refuses.
#[test]
fn env_unset_interval_yields_to_a_deferred_book_mode() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path());
    let mut g: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&genesis).unwrap()).unwrap();
    g["consensus"]["book_mode"] = serde_json::json!("level_authority_chunked");
    std::fs::write(&genesis, serde_json::to_vec_pretty(&g).unwrap()).unwrap();
    let mut node = Node::start(work.path(), &genesis, "a");
    node.wait_executed_above(MIN_BLOCKS);
    let log = node.log_text();
    assert!(
        !log.contains("coalesced state checkpoints ON"),
        "a deferred book mode must not run checkpoints:\n{}",
        node.tail()
    );
    assert!(
        log.contains("compiled default checkpoint interval is off"),
        "the yield must be logged:\n{}",
        node.tail()
    );
    assert_graceful_stop(&mut node, libc::SIGTERM, "SIGTERM");
}

/// Codex round 5: with stdout blocked (a full pipe nobody reads, e.g. a
/// stalled log shipper), every log call blocks. SIGTERM must still end the
/// process within the shutdown bound: no log may stand between the signal
/// and the deadline / second-signal wait.
#[test]
fn sigterm_exits_within_the_bound_when_stdout_is_blocked() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path());
    let mut child = node_command(work.path(), &genesis)
        .env("TORUS_SHUTDOWN_TIMEOUT_SECS", "2")
        .stdout(Stdio::piped()) // never read: fills, then every write blocks
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn torus-node");
    // Running, logging every block: the 64 KiB pipe is full well before this.
    std::thread::sleep(Duration::from_secs(15));
    assert!(child.try_wait().unwrap().is_none(), "node exited early");
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0);
    let start = Instant::now();
    let exited = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break Some(st);
        }
        if start.elapsed() > Duration::from_secs(30) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let took = start.elapsed();
    let _ = child.kill();
    let _ = child.wait();
    let st = exited.expect("node still running 30 s after SIGTERM with stdout blocked");
    // The deadline itself ended the stop: 2 s timeout + 8 s slack for a
    // loaded host (the 30 s above only guards against a hang).
    assert!(
        took <= Duration::from_secs(2 + 8),
        "exit {took:?} after SIGTERM; the 2 s shutdown timeout was not enforced"
    );
    // Every stop step logs, so with stdout blocked the stop cannot finish:
    // only the forced-timeout path (exit 1) can end the process.
    assert_eq!(
        st.code(),
        Some(1),
        "expected the forced-timeout exit 1, got {st} after {took:?}"
    );
}
