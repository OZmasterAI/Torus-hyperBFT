//! P0.0 phase 4: the real `torus-node` binary binds its data dir to the
//! genesis file it was initialized with. A 1-validator devnet runs a few
//! blocks and stops; a restart with a changed consensus field refuses to
//! start naming the field; a restart with the same genesis reformatted
//! starts and keeps producing blocks; a further restart on the same bytes
//! takes the stored digest (s140), and the changed file is still refused.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(120);

/// 1-validator genesis value from `devnet/genesis.json`.
fn genesis_value() -> serde_json::Value {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../devnet/genesis.json");
    let mut g: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&src).expect("read devnet genesis")).unwrap();
    let first = g["validators"][0].clone();
    g["validators"] = serde_json::Value::Array(vec![first]);
    g
}

fn write(dir: &Path, name: &str, text: String) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

struct Node {
    child: Child,
    log: PathBuf,
}

impl Node {
    fn start(work: &Path, genesis: &Path, run: &str) -> Node {
        let log = work.join(format!("node-{run}.log"));
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_torus-node"))
            .arg("--genesis")
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
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("spawn torus-node");
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

    /// Highest `execution pipeline: block done height=N` in the log.
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
                start.elapsed() < BOUND,
                "no executed block above {floor} in {BOUND:?}:\n{}",
                self.tail()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let start = Instant::now();
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                return st;
            }
            if start.elapsed() > BOUND {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("node did not exit within {BOUND:?}:\n{}", self.tail());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn stop(&mut self) {
        let rc = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
        assert_eq!(rc, 0, "kill(SIGTERM) failed");
        let st = self.wait_exit();
        assert_eq!(
            st.code(),
            Some(0),
            "graceful stop, got {st}:\n{}",
            self.tail()
        );
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

#[test]
fn changed_genesis_refuses_to_start_and_a_reformatted_one_starts() {
    let work = tempfile::tempdir().unwrap();
    let g = genesis_value();
    let original = write(
        work.path(),
        "genesis.json",
        serde_json::to_string_pretty(&g).unwrap(),
    );

    let mut node = Node::start(work.path(), &original, "a");
    let h = node.wait_executed_above(2);
    assert!(
        node.log_text()
            .contains("genesis digest stored: this data dir is bound"),
        "fresh boot did not store the digest:\n{}",
        node.tail()
    );
    node.stop();

    // A consensus parameter edited after the first boot: refused, named.
    let mut edited = g.clone();
    let t = edited["consensus"]["timeout_base_ms"].as_u64().unwrap();
    edited["consensus"]["timeout_base_ms"] = (t + 100).into();
    let edited = write(
        work.path(),
        "genesis-edited.json",
        serde_json::to_string_pretty(&edited).unwrap(),
    );
    let mut node = Node::start(work.path(), &edited, "b");
    let st = node.wait_exit();
    let log = node.log_text();
    assert_eq!(
        st.code(),
        Some(1),
        "changed genesis must refuse:\n{}",
        node.tail()
    );
    assert!(
        log.contains("refusing to start: genesis file")
            && log.contains("changed: consensus.timeout_base_ms."),
        "refusal names the field:\n{}",
        node.tail()
    );
    assert!(
        !log.contains("execution pipeline: block done"),
        "{}",
        node.tail()
    );

    // The same genesis, compact: starts and continues the chain.
    let compact = write(
        work.path(),
        "genesis-compact.json",
        serde_json::to_string(&g).unwrap(),
    );
    let mut node = Node::start(work.path(), &compact, "c");
    let h = node.wait_executed_above(h);
    let log = node.log_text();
    assert!(
        log.contains("genesis file matches the stored genesis digest")
            && !log.contains("not digested again"),
        "new file bytes are digested in full:\n{}",
        node.tail()
    );
    node.stop();

    // s140: a restart on the same bytes takes the stored digest (no full
    // digest of the file) and continues the chain.
    let mut node = Node::start(work.path(), &compact, "d");
    node.wait_executed_above(h);
    assert!(
        node.log_text().contains(
            "genesis file matches the stored genesis digest (the file's bytes are the ones \
             the digest was computed from: not digested again)"
        ),
        "restart on unchanged bytes:\n{}",
        node.tail()
    );
    node.stop();

    // After a cached restart the edited file is still refused.
    let mut node = Node::start(work.path(), &edited, "e");
    let st = node.wait_exit();
    let log = node.log_text();
    assert_eq!(st.code(), Some(1), "changed genesis must refuse:\n{}", node.tail());
    assert!(
        log.contains("changed: consensus.timeout_base_ms."),
        "refusal names the field:\n{}",
        node.tail()
    );
}
