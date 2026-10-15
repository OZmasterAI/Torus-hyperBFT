//! Paced mode (`--rate`) and the signals run-cell.sh sends it: the harness
//! stops tx-flood with SIGTERM when the bench exits and gives it 10 s before a
//! SIGKILL, so tx-flood must stop at once, at any point of its run.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_torus-tx-flood");

/// A loopback URL nothing listens on (connections are refused at once).
fn closed_port_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    format!("http://127.0.0.1:{port}")
}

/// One keep-alive connection of the stub: nonce 0 for every sender, every raw
/// tx pooled.
fn serve(stream: TcpStream) {
    let mut out = stream.try_clone().unwrap();
    let mut rd = BufReader::new(stream);
    loop {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if rd.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let l = line.trim_end().to_ascii_lowercase();
            if l.is_empty() {
                break;
            }
            if let Some(v) = l.strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; len];
        if rd.read_exact(&mut body).is_err() {
            return;
        }
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let result = match req["method"].as_str() {
            Some("eth_getTransactionCount") => serde_json::json!("0x0"),
            Some("eth_sendRawTransaction") => serde_json::json!(format!("0x{}", "00".repeat(32))),
            _ => serde_json::Value::Null,
        };
        let reply =
            serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": result}).to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
            reply.len()
        );
        if out.write_all(resp.as_bytes()).is_err() {
            return;
        }
    }
}

/// A JSON-RPC stub on 127.0.0.1 that pools every tx.
fn stub_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s));
        }
    });
    url
}

fn sigterm(child: &Child) {
    let ok = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap()
        .success();
    assert!(ok, "kill -TERM failed");
}

/// Waits up to `limit` for the child; kills it and returns None on a timeout.
fn wait_for(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let t0 = Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return Some(s);
        }
        if t0.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// SIGTERM while the start nonces cannot be read (the node is down) ends the
/// run at once, with rc 1 (nothing was sent). RED before: the signal handlers
/// were installed but not polled while the fetch retried for ~10 s.
#[test]
fn sigterm_while_fetching_the_start_nonces_exits_at_once() {
    let mut child = Command::new(BIN)
        .args([
            "--rpc-urls",
            &closed_port_url(),
            "--rate",
            "5",
            "--accounts",
            "1",
        ])
        .args(["--fanout", "pinned"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1_500));
    let t0 = Instant::now();
    sigterm(&child);
    let status = wait_for(&mut child, Duration::from_secs(5))
        .expect("tx-flood ignored SIGTERM while fetching its start nonces");
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    assert_eq!(status.code(), Some(1), "interrupted before the first send");
}

/// SIGTERM mid-run (run-cell.sh at bench exit): the run stops, the final
/// stats name the signal, and the exit code is 0.
#[test]
fn sigterm_mid_run_writes_the_stats_and_exits_zero() {
    let dir = std::env::temp_dir().join(format!("tx-flood-sig-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stats = dir.join("stats.json");
    let mut child = Command::new(BIN)
        .args(["--rpc-urls", &stub_url(), "--rate", "20", "--accounts", "2"])
        .args([
            "--fanout",
            "pinned",
            "--stats-file",
            stats.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1_500));
    sigterm(&child);
    let status = wait_for(&mut child, Duration::from_secs(5)).expect("tx-flood ignored SIGTERM");
    assert_eq!(status.code(), Some(0));
    let s: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&stats).unwrap()).unwrap();
    assert_eq!(s["stopped_by"], "SIGTERM", "{s}");
    assert!(s["accepted"].as_u64().unwrap() >= 10, "{s}");
    assert_eq!(s["fanout"], "pinned");
    std::fs::remove_dir_all(&dir).unwrap();
}
