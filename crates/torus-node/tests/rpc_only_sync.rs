//! An `--rpc-only` node catches up with and follows a live chain through one
//! bootstrap peer (real `torus-node` binaries, debug build).
//!
//! Testnet wallets read from `--rpc-only` nodes (owner s138, RPC freshness
//! option a), and fresh / archive nodes sync the same way. V0 and V1 run a
//! 2-validator chain; R (`--rpc-only`) starts once the chain is a few hundred
//! blocks ahead, dials only V0, and must catch up and keep up.
//!
//! Not a 1-validator chain: a debug 1-validator chain commits ~45 blocks/s
//! with ~90 consensus gossip messages/s from its single author. That is more
//! than a debug node's swarm loop drains on a slow or loaded runner (~120
//! events/s on ubuntoz), so R's connection always has a gossip event ready and
//! libp2p's `Connection::poll` never reaches stream open/accept: every
//! request-response on it (block sync, by-hash fetches) starves and R never
//! syncs. It also exceeds the 50 msg/s per-author consensus rate limit.
//! Neither happens at release speed or testnet rates.
#![cfg(unix)]

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;

const BOUND: Duration = Duration::from_secs(120);
/// The devnet key of `devnet/genesis.json` validator `i` (0-based).
fn validator_key(i: u8) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[0] = i + 1;
    key
}

/// 2-validator genesis from `devnet/genesis.json`.
fn genesis_text() -> String {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../devnet/genesis.json");
    let mut g: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&src).expect("read devnet genesis")).unwrap();
    let two: Vec<serde_json::Value> = g["validators"].as_array().unwrap()[..2].to_vec();
    g["validators"] = serde_json::Value::Array(two);
    serde_json::to_string_pretty(&g).unwrap()
}

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
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

struct Node {
    child: Child,
    log: PathBuf,
}

impl Node {
    fn start(work: &Path, name: &str, genesis: &Path, port: u16, extra: &[String]) -> Node {
        let log = work.join(format!("{name}.log"));
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_torus-node"))
            .arg("--genesis")
            .arg(genesis)
            .arg("--data-dir")
            .arg(work.join(format!("data-{name}")))
            .args([
                "--p2p-listen",
                &format!("/ip4/127.0.0.1/udp/{port}/quic-v1"),
                "--p2p-private-addrs",
                "--rpc-addr",
                "127.0.0.1:0",
                "--metrics-addr",
                "127.0.0.1:0",
                "--log-level",
                // Diagnostics: e.g. `info,libp2p_request_response=debug`.
                &std::env::var("TORUS_TEST_LOG_LEVEL").unwrap_or_else(|_| "info".into()),
            ])
            .args(extra)
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

    /// How often each failure symptom appears in the log.
    fn symptom_counts(&self) -> String {
        const KEYS: [&str; 6] = [
            "max sub-streams",
            "Timeout",
            "timed out",
            "OutboundFailure",
            "at capacity",
            "cannot sync",
        ];
        let text = self.log_text();
        KEYS.iter()
            .map(|k| format!("{k}={}", text.matches(k).count()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn assert_running(&mut self) {
        if let Ok(Some(st)) = self.child.try_wait() {
            panic!("node exited early ({st}):\n{}", self.tail());
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Diagnostics: keep every log, pass or fail.
        if let Ok(dir) = std::env::var("TORUS_TEST_KEEP_LOGS") {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::copy(
                &self.log,
                Path::new(&dir).join(self.log.file_name().unwrap()),
            );
        }
    }
}

/// Poll `cond` until it holds (or panic after [`BOUND`], keeping the logs).
fn wait_until(
    work: &Path,
    nodes: &mut [&mut Node],
    what: &str,
    mut cond: impl FnMut(&[&mut Node]) -> bool,
) {
    let start = Instant::now();
    loop {
        if cond(nodes) {
            return;
        }
        for n in nodes.iter_mut() {
            n.assert_running();
        }
        if start.elapsed() > BOUND {
            let keep = std::env::temp_dir().join(format!("rpc-only-sync-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&keep);
            let tails: Vec<String> = nodes
                .iter()
                .map(|n| {
                    let name = n.log.file_name().unwrap();
                    let _ = std::fs::copy(&n.log, keep.join(name));
                    format!(
                        "--- {} height={:?} {}\n--- tail:\n{}",
                        n.log.display(),
                        n.executed_height(),
                        n.symptom_counts(),
                        n.tail()
                    )
                })
                .collect();
            panic!(
                "{what}: not within {BOUND:?} (logs kept in {}; work {})\n{}",
                keep.display(),
                work.display(),
                tails.join("\n")
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn rpc_only_node_catches_up_and_follows_through_one_bootstrap_peer() {
    let work = tempfile::tempdir().unwrap();
    let genesis = work.path().join("genesis.json");
    std::fs::write(&genesis, genesis_text()).unwrap();

    let v0_peer = torus_network::bridge::peer_id_from_verifying_key(
        &SigningKey::from_bytes(&validator_key(0)).verifying_key(),
    );
    let v0_port = free_udp_port();
    let v0_addr = format!("/ip4/127.0.0.1/udp/{v0_port}/quic-v1/p2p/{v0_peer}");
    let validator = |i: u8, peers: &str| {
        vec![
            "--validator-key".to_string(),
            hex::encode(validator_key(i)),
            // Explicit (possibly empty): never the compiled testnet seeds.
            format!("--p2p-peers={peers}"),
        ]
    };
    let mut v0 = Node::start(work.path(), "v0", &genesis, v0_port, &validator(0, ""));
    let mut v1 = Node::start(
        work.path(),
        "v1",
        &genesis,
        free_udp_port(),
        &validator(1, &v0_addr),
    );

    // Let the chain get ahead, so R starts as a node that must catch up
    // through several block-sync batches (128 blocks each).
    wait_until(
        work.path(),
        &mut [&mut v0, &mut v1],
        "the chain executes blocks",
        |n| n[0].executed_height().is_some_and(|h| h >= 300),
    );

    let mut r = Node::start(
        work.path(),
        "r",
        &genesis,
        free_udp_port(),
        &["--rpc-only".to_string(), format!("--p2p-peers={v0_addr}")],
    );

    // Catch up: R reaches the height V0 had when R started...
    let target = v0.executed_height().unwrap();
    wait_until(
        work.path(),
        &mut [&mut v0, &mut v1, &mut r],
        &format!("r catches up (target {target})"),
        |n| n[2].executed_height().is_some_and(|h| h >= target),
    );

    // ...and follows, twice: V0 executes blocks beyond both nodes' previous
    // heights (so a stalled chain fails here), then R reaches that new height.
    let mut prev = target;
    for round in 0..2 {
        prev = prev
            .max(v0.executed_height().unwrap_or(0))
            .max(r.executed_height().unwrap_or(0));
        wait_until(
            work.path(),
            &mut [&mut v0, &mut v1, &mut r],
            &format!("the chain executes new blocks (round {round}, above {prev})"),
            |n| n[0].executed_height().is_some_and(|h| h > prev),
        );
        let target = v0.executed_height().unwrap();
        assert!(target > prev);
        prev = target;
        wait_until(
            work.path(),
            &mut [&mut v0, &mut v1, &mut r],
            &format!("r follows (round {round}, target {target})"),
            |n| n[2].executed_height().is_some_and(|h| h >= target),
        );
    }
}
