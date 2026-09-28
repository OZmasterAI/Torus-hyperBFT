//! Small fresh-DB qualification, not a throughput or power-loss test.
#![cfg(unix)]

use rocksdb::WriteBatch;
use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use torus_state::cf::{
    CF_CONSENSUS_META, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::db::parse_max_total_wal_mib;
use torus_state::{DbTuning, StateDb};

const MIB: u64 = 1 << 20;
const ROWS: u64 = 64;
const VALUE_BYTES: usize = 64 * 1024;

#[test]
fn wal_budget_parser_preserves_defaults_and_checks_overflow() {
    assert_eq!(DbTuning::default().max_total_wal_size, None);
    assert_eq!(parse_max_total_wal_mib(None), None);
    for value in [
        "0",
        "000",
        "",
        "-1",
        "+1",
        "1.5",
        "1_000",
        "１２",
        "18446744073709551615",
        "17592186044416",
    ] {
        assert_eq!(parse_max_total_wal_mib(Some(value)), None, "{value}");
    }
    assert_eq!(parse_max_total_wal_mib(Some(" 1024 ")), Some(1024 * MIB));
    assert_eq!(
        parse_max_total_wal_mib(Some("17592186044415")),
        Some(17592186044415 * MIB)
    );
}

fn persisted_wal_option(path: &Path) -> u64 {
    let mut files: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("OPTIONS-")
        })
        .collect();
    files.sort();
    fs::read_to_string(files.last().expect("persisted OPTIONS file"))
        .unwrap()
        .lines()
        .find_map(|line| {
            let (name, value) = line.trim().split_once('=')?;
            (name.trim() == "max_total_wal_size").then(|| value.trim().parse().unwrap())
        })
        .expect("max_total_wal_size in RocksDB OPTIONS")
}

#[test]
fn wal_budget_unset_zero_and_explicit_options_are_persisted() {
    for (budget, expected) in [(None, 0), (Some(0), 0), (Some(MIB), MIB)] {
        let dir = tempfile::tempdir().unwrap();
        let tuning = DbTuning {
            max_total_wal_size: budget,
            ..Default::default()
        };
        let _db = StateDb::open_with_tuning(dir.path(), &tuning).unwrap();
        assert_eq!(persisted_wal_option(dir.path()), expected);
    }
}

fn cold_sst_bytes(db: &StateDb) -> u64 {
    db.inner()
        .property_int_value_cf(
            db.cf_handle(CF_NATIVE_MARKETS).unwrap(),
            "rocksdb.total-sst-files-size",
        )
        .unwrap()
        .unwrap()
}

fn write_state_and_marker(db: &StateDb, height: u64, payload: &[u8]) {
    let mut batch = WriteBatch::default();
    batch.put_cf(
        db.cf_handle(CF_NATIVE_BALANCES).unwrap(),
        height.to_be_bytes(),
        payload,
    );
    batch.put_cf(
        db.cf_handle(CF_NATIVE_BALANCES).unwrap(),
        b"atomic-state",
        height.to_be_bytes(),
    );
    batch.put_cf(
        db.cf_handle(CF_CONSENSUS_META).unwrap(),
        META_NATIVE_APPLIED_HEIGHT,
        height.to_be_bytes(),
    );
    db.write(batch).unwrap();
}

// Invoked only by the parent below, with a new temp directory and explicit
// options. No process-global environment mutation in the test runner.
#[test]
fn wal_budget_child() {
    let Some(root) = std::env::var_os("WAL_BUDGET_FIXTURE_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let budget: u64 = std::env::var("WAL_BUDGET_FIXTURE_BYTES")
        .unwrap()
        .parse()
        .unwrap();
    let path = root.join("db");
    assert!(
        !path.exists(),
        "fixture must never open an existing database"
    );
    let tuning = DbTuning {
        max_total_wal_size: (budget > 0).then_some(budget),
        ..Default::default()
    };
    let db = StateDb::open_with_tuning(&path, &tuning).unwrap();
    assert_eq!(persisted_wal_option(&path), budget);
    db.put_cf_raw(CF_NATIVE_MARKETS, b"cold-sentinel", b"retained")
        .unwrap();
    for height in 1..=ROWS {
        write_state_and_marker(&db, height, &vec![height as u8; VALUE_BYTES]);
    }
    if budget > 0 {
        // Writes above exceed the WAL threshold but not the 128MiB hot-CF
        // memtable or 1GiB aggregate budget. No explicit CF/WAL flush is used.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let reason = fs::read_to_string(path.join("LOG"))
                .unwrap_or_default()
                .contains("Flushing all column families with data in WAL number");
            if reason
                && cold_sst_bytes(&db) > 0
                && db.runtime_stats().tickers.unwrap().flush_write_bytes > 0
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "automatic oldest-WAL flush did not finish"
            );
            thread::sleep(Duration::from_millis(25));
        }
    } else {
        assert_eq!(
            cold_sst_bytes(&db),
            0,
            "control must retain the cold memtable"
        );
        assert_eq!(db.runtime_stats().tickers.unwrap().flush_write_bytes, 0);
    }
    // An acknowledged tail batch after observed flushing: no clean DB close,
    // manual flush, or fsync can manufacture this test's recovery result.
    write_state_and_marker(&db, ROWS + 1, b"tail");
    let evidence = serde_json::json!({"pid": std::process::id(), "budget_bytes": budget,
        "height": ROWS + 1, "cold_sst_bytes": cold_sst_bytes(&db)});
    fs::write(
        root.join("ready.tmp"),
        serde_json::to_vec(&evidence).unwrap(),
    )
    .unwrap();
    fs::rename(root.join("ready.tmp"), root.join("ready.json")).unwrap();
    // Fail boundedly if the controller fails; the parent requires SIGKILL.
    thread::sleep(Duration::from_secs(60));
    panic!("controller did not kill fixture child");
}

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn wal_budget_automatic_flush_and_atomic_batch_survive_sigkill() {
    use std::os::unix::process::ExitStatusExt;
    for budget in [0, MIB] {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("child.log");
        let log = fs::File::create(&log_path).unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "wal_budget_child", "--nocapture"])
            .env("WAL_BUDGET_FIXTURE_ROOT", dir.path())
            .env("WAL_BUDGET_FIXTURE_BYTES", budget.to_string())
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        // Other node-local knobs must not force a control flush or change this
        // fixture's threshold. This affects the child only, not parallel tests.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("TORUS_") {
                cmd.env_remove(name);
            }
        }
        let mut child = KillOnDrop(cmd.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(40);
        let ready = dir.path().join("ready.json");
        while !ready.exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child exited: {}",
                fs::read_to_string(&log_path).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "child timed out: {}",
                fs::read_to_string(&log_path).unwrap()
            );
            thread::sleep(Duration::from_millis(25));
        }
        let evidence: serde_json::Value =
            serde_json::from_slice(&fs::read(ready).unwrap()).unwrap();
        assert_eq!(evidence["pid"].as_u64(), Some(child.0.id() as u64));
        assert_eq!(evidence["budget_bytes"].as_u64(), Some(budget));
        assert_eq!(evidence["height"].as_u64(), Some(ROWS + 1));
        assert_eq!(evidence["cold_sst_bytes"].as_u64().unwrap() > 0, budget > 0);
        child.0.kill().unwrap(); // Child::kill is SIGKILL on Unix.
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        let tuning = DbTuning {
            max_total_wal_size: (budget > 0).then_some(budget),
            ..Default::default()
        };
        let db = StateDb::open_with_tuning(&dir.path().join("db"), &tuning).unwrap();
        assert_eq!(
            db.get_cf_raw(CF_NATIVE_MARKETS, b"cold-sentinel").unwrap(),
            Some(b"retained".to_vec())
        );
        for height in 1..=ROWS {
            assert_eq!(
                db.get_cf_raw(CF_NATIVE_BALANCES, &height.to_be_bytes())
                    .unwrap(),
                Some(vec![height as u8; VALUE_BYTES])
            );
        }
        assert_eq!(
            db.get_cf_raw(CF_NATIVE_BALANCES, &(ROWS + 1).to_be_bytes())
                .unwrap(),
            Some(b"tail".to_vec())
        );
        let marker = db
            .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
            .unwrap();
        assert_eq!(marker, Some((ROWS + 1).to_be_bytes().to_vec()));
        assert_eq!(
            db.get_cf_raw(CF_NATIVE_BALANCES, b"atomic-state").unwrap(),
            marker
        );
    }
}
