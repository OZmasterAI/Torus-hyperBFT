//! The order-book storage mode is a genesis field (`consensus.book_mode`,
//! owner s131): every node of a chain runs the mode its genesis names.
//! `TORUS_BOOK_ROWS` may only repeat it (deprecated); a value that disagrees,
//! or a DB whose `__book_mode__` marker names another mode, refuses to start.
//!
//! These tests spawn the real `torus-node` binary as a 1-validator devnet (the
//! `sigterm_shutdown.rs` shape) and read the marker row from the data dir
//! after the node exited.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use torus_state::cf::CF_NATIVE_MARKETS;
use torus_state::StateDb;

/// Blocks executed before a running node is stopped.
const MIN_BLOCKS: u64 = 3;

/// 1-validator genesis from `devnet/genesis.json`; `book_mode` = the
/// `consensus.book_mode` value (`None` = field absent).
fn write_genesis(dir: &Path, name: &str, book_mode: Option<serde_json::Value>) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../devnet/genesis.json");
    let mut g: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&src).expect("read devnet genesis")).unwrap();
    let first = g["validators"][0].clone();
    g["validators"] = serde_json::Value::Array(vec![first]);
    match book_mode {
        Some(v) => g["consensus"]["book_mode"] = v,
        None => assert!(g["consensus"].get("book_mode").is_none()),
    }
    let out = dir.join(name);
    std::fs::write(&out, serde_json::to_vec_pretty(&g).unwrap()).unwrap();
    out
}

fn mode3() -> Option<serde_json::Value> {
    Some(serde_json::json!("level_authority_chunked"))
}

struct Node {
    child: Child,
    log: PathBuf,
}

impl Node {
    /// Start a node on `data` (`genesis` None = no `--genesis` flag) with
    /// `book_rows` as `TORUS_BOOK_ROWS` (None = unset).
    fn start(
        work: &Path,
        data: &str,
        genesis: Option<&Path>,
        book_rows: Option<&str>,
        run: &str,
    ) -> Node {
        let log = work.join(format!("node-{data}-{run}.log"));
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_torus-node"));
        if let Some(g) = genesis {
            cmd.arg("--genesis").arg(g);
        }
        cmd.arg("--data-dir")
            .arg(work.join(data))
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
            .env_remove("TORUS_CHECKPOINT_INTERVAL_MS")
            .env_remove("TORUS_CHECKPOINT_MAX_BYTES")
            .env_remove("TORUS_EXEC_PIPELINE")
            .env_remove("TORUS_BOOK_ROWS")
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        if let Some(v) = book_rows {
            cmd.env("TORUS_BOOK_ROWS", v);
        }
        let child = cmd.spawn().expect("spawn torus-node");
        Node { child, log }
    }

    fn log_text(&self) -> String {
        strip_ansi(&std::fs::read_to_string(&self.log).unwrap_or_default())
    }

    fn tail(&self) -> String {
        let text = self.log_text();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(40)..].join("\n")
    }

    fn executed_height(&self) -> Option<u64> {
        self.log_text()
            .lines()
            .filter(|l| l.contains("execution pipeline: block done"))
            .filter_map(|l| {
                let rest = &l[l.find("height=")? + "height=".len()..];
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            })
            .max()
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

    /// The node must exit by itself (a refused start) within 60 s.
    fn wait_refused(&mut self) -> ExitStatus {
        let start = Instant::now();
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                return st;
            }
            if start.elapsed() > Duration::from_secs(60) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "node did not refuse to start (still running after 60 s):\n{}",
                    self.tail()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// SIGTERM and wait for exit 0.
    fn stop(&mut self) {
        let rc = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
        assert_eq!(rc, 0);
        let start = Instant::now();
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                assert_eq!(st.code(), Some(0), "stop: {st}:\n{}", self.tail());
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "no exit after SIGTERM:\n{}",
                self.tail()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
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

/// The `__book_mode__` marker byte(s) in a stopped node's data dir.
fn marker(work: &Path, data: &str) -> Option<Vec<u8>> {
    let db = StateDb::open(&work.join(data)).expect("reopen data dir after the node exited");
    db.get_cf_raw(CF_NATIVE_MARKETS, b"__book_mode__").unwrap()
}

/// Run a node until it executed a few blocks, then stop it; returns its log.
fn run_some_blocks(
    work: &Path,
    data: &str,
    genesis: Option<&Path>,
    book_rows: Option<&str>,
    run: &str,
) -> String {
    let mut node = Node::start(work, data, genesis, book_rows, run);
    node.wait_executed_above(MIN_BLOCKS);
    node.stop();
    node.log_text()
}

/// A refused start: non-zero exit, no executed block, and every `needle` in
/// the log. Returns the log.
fn assert_refused(
    work: &Path,
    data: &str,
    genesis: Option<&Path>,
    book_rows: Option<&str>,
    run: &str,
    needles: &[&str],
) -> String {
    let mut node = Node::start(work, data, genesis, book_rows, run);
    let st = node.wait_refused();
    let log = node.log_text();
    assert!(
        !st.success(),
        "{run}: refused start must exit non-zero, got {st}:\n{}",
        node.tail()
    );
    assert!(
        node.executed_height().is_none(),
        "{run}: executed a block before refusing:\n{}",
        node.tail()
    );
    for n in needles {
        assert!(log.contains(n), "{run}: log lacks {n:?}:\n{}", node.tail());
    }
    log
}

/// Genesis without the field = Classic (every existing genesis, incl. the
/// testnet's): the node runs and writes the Classic marker.
#[test]
fn genesis_without_book_mode_runs_classic() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path(), "genesis.json", None);
    let log = run_some_blocks(work.path(), "data", Some(&genesis), None, "a");
    assert!(
        log.contains("book mode: classic"),
        "no book mode line:\n{log}"
    );
    assert_eq!(marker(work.path(), "data"), Some(vec![0]));
}

/// Genesis mode 3, env unset: the executor runs mode 3 (marker 3), and a
/// restart with the same genesis keeps it. A restart without `--genesis`
/// refuses (P0.0 phase 4: the data dir is bound to its genesis file), so the
/// default chain config can never replace the genesis mode either.
#[test]
fn genesis_mode_3_runs_mode_3_without_the_env() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path(), "genesis.json", mode3());
    let log = run_some_blocks(work.path(), "data", Some(&genesis), None, "a");
    assert!(
        log.contains("book mode: level authority, chunked level hash")
            && log.contains("genesis consensus.book_mode"),
        "no genesis book mode line:\n{log}"
    );
    assert!(
        !log.contains("TORUS_BOOK_ROWS is deprecated"),
        "no env, no deprecation warning:\n{log}"
    );
    assert_eq!(marker(work.path(), "data"), Some(vec![3]));
    // Restart without --genesis: refused (the dir is bound to the genesis
    // file), after the book mode was read from the DB marker; nothing written.
    let log = assert_refused(
        work.path(),
        "data",
        None,
        None,
        "b",
        &["refusing to start", "no --genesis was given"],
    );
    assert!(
        log.contains("book mode: level authority, chunked level hash"),
        "no-genesis restart reads the DB marker's mode:\n{log}"
    );
    assert_eq!(marker(work.path(), "data"), Some(vec![3]));
    // Restart with the same genesis: still mode 3.
    let log = run_some_blocks(work.path(), "data", Some(&genesis), None, "c");
    assert!(
        log.contains("book mode: level authority, chunked level hash"),
        "restart mode:\n{log}"
    );
    assert_eq!(marker(work.path(), "data"), Some(vec![3]));
}

/// Integer form of the field (3) is the same chain as the name.
#[test]
fn genesis_book_mode_as_number_runs_that_mode() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path(), "genesis.json", Some(serde_json::json!(1)));
    let log = run_some_blocks(work.path(), "data", Some(&genesis), None, "a");
    assert!(
        log.contains("book mode: order rows"),
        "no order-rows line:\n{log}"
    );
    assert_eq!(marker(work.path(), "data"), Some(vec![1]));
}

/// `TORUS_BOOK_ROWS` that disagrees with genesis (or is not a mode) refuses to
/// start, naming both settings; nothing is executed.
#[test]
fn env_disagreeing_with_genesis_refuses_to_start() {
    let work = tempfile::tempdir().unwrap();
    let g3 = write_genesis(work.path(), "genesis-3.json", mode3());
    let g0 = write_genesis(work.path(), "genesis-0.json", None);
    for (i, (genesis, env)) in [(&g3, "0"), (&g3, "2"), (&g0, "3"), (&g0, "1"), (&g3, "x")]
        .into_iter()
        .enumerate()
    {
        let data = format!("data{i}");
        assert_refused(
            work.path(),
            &data,
            Some(genesis),
            Some(env),
            "a",
            &[
                "refusing to start",
                "TORUS_BOOK_ROWS",
                "consensus.book_mode",
            ],
        );
    }
}

/// `TORUS_BOOK_ROWS` equal to the genesis mode is accepted with a
/// deprecation warning; empty = unset.
#[test]
fn env_agreeing_with_genesis_starts_with_a_deprecation_warning() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path(), "genesis.json", mode3());
    let log = run_some_blocks(work.path(), "data", Some(&genesis), Some(" 3 "), "a");
    assert!(
        log.contains("TORUS_BOOK_ROWS is deprecated"),
        "no deprecation warning:\n{log}"
    );
    assert_eq!(marker(work.path(), "data"), Some(vec![3]));
    let log = run_some_blocks(work.path(), "data", Some(&genesis), Some(""), "b");
    assert!(
        !log.contains("TORUS_BOOK_ROWS is deprecated"),
        "empty = unset:\n{log}"
    );
    assert_eq!(marker(work.path(), "data"), Some(vec![3]));
}

/// A DB written under one mode, restarted with a genesis that names another
/// (the genesis file swapped, already applied): refuses to start.
#[test]
fn db_marker_disagreeing_with_genesis_refuses_to_start() {
    let work = tempfile::tempdir().unwrap();
    let g0 = write_genesis(work.path(), "genesis-0.json", None);
    let g3 = write_genesis(work.path(), "genesis-3.json", mode3());
    run_some_blocks(work.path(), "data", Some(&g0), None, "a");
    assert_eq!(marker(work.path(), "data"), Some(vec![0]));
    assert_refused(
        work.path(),
        "data",
        Some(&g3),
        None,
        "b",
        &["refusing to start", "book-mode marker"],
    );
    // and the env cannot talk it into the DB's mode either
    assert_refused(
        work.path(),
        "data",
        Some(&g3),
        Some("0"),
        "c",
        &["refusing to start", "TORUS_BOOK_ROWS"],
    );
    assert_eq!(
        marker(work.path(), "data"),
        Some(vec![0]),
        "a refused start writes nothing"
    );
}

/// Two nodes on the same genesis cannot end up in different modes: the one
/// whose operator env pins another mode refuses to start; without it both
/// run the genesis mode.
#[test]
fn two_nodes_with_the_same_genesis_cannot_diverge_on_mode() {
    let work = tempfile::tempdir().unwrap();
    let genesis = write_genesis(work.path(), "genesis.json", mode3());
    run_some_blocks(work.path(), "a", Some(&genesis), None, "1");
    assert_refused(
        work.path(),
        "b",
        Some(&genesis),
        Some("0"),
        "1",
        &["refusing to start"],
    );
    run_some_blocks(work.path(), "b", Some(&genesis), None, "2");
    assert_eq!(marker(work.path(), "a"), Some(vec![3]));
    assert_eq!(marker(work.path(), "b"), Some(vec![3]));
}
