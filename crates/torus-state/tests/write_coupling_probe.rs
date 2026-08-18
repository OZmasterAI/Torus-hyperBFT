//! Scratch probe (ignored): does a small write on one CF stall behind a
//! concurrent multi-MB write batch on another CF of the SAME RocksDB instance?
//! Run: cargo test --release -p torus-state --test write_coupling_probe -- --ignored --nocapture
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use torus_state::cf::{CF_CONSENSUS_META, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_PENDING};
use torus_state::StateDb;

fn pct(v: &mut Vec<f64>, p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p) as usize]
}

fn run(label: &str, big_writer: bool, separate_db: bool) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(StateDb::open(&dir.path().join("state")).unwrap());
    let small_db = if separate_db {
        Arc::new(StateDb::open(&dir.path().join("cons")).unwrap())
    } else {
        db.clone()
    };
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = vec![];
    if big_writer {
        let db2 = db.clone();
        let stop2 = stop.clone();
        handles.push(std::thread::spawn(move || {
            let mut i = 0u64;
            let mut big_ms = vec![];
            while !stop2.load(Ordering::Relaxed) {
                // ~30 MB batch: 60k keys x 512 B (books-like), like an exec flush.
                let mut wb = rocksdb::WriteBatch::default();
                let raw = db2.db_arc();
                let cf = raw.cf_handle(CF_NATIVE_ORDER_BOOKS).unwrap();
                let val = vec![(i & 0xff) as u8; 512];
                for k in 0..60_000u64 {
                    let key = ((i % 50) * 60_000 + k).to_be_bytes();
                    wb.put_cf(cf, key, &val);
                }
                let t = Instant::now();
                db2.write(wb).unwrap();
                big_ms.push(t.elapsed().as_secs_f64() * 1e3);
                i += 1;
                // exec cadence: write ~40% of the time
                std::thread::sleep(Duration::from_millis(60));
            }
            let mut b = big_ms;
            eprintln!(
                "  big writer: n={} mean={:.1}ms p50={:.1} p90={:.1}",
                b.len(),
                b.iter().sum::<f64>() / b.len() as f64,
                pct(&mut b, 0.5),
                pct(&mut b, 0.9)
            );
        }));
        // second big writer: DA-body-like 2 MB batches
        let db3 = db.clone();
        let stop3 = stop.clone();
        handles.push(std::thread::spawn(move || {
            let mut i = 0u64;
            while !stop3.load(Ordering::Relaxed) {
                let mut wb = rocksdb::WriteBatch::default();
                let raw = db3.db_arc();
                let cf = raw.cf_handle(CF_NATIVE_PENDING).unwrap();
                let val = vec![7u8; 20_000];
                for k in 0..100u64 {
                    wb.put_cf(cf, (i * 100 + k).to_be_bytes(), &val);
                }
                db3.write(wb).unwrap();
                i += 1;
                std::thread::sleep(Duration::from_millis(300));
            }
        }));
    }
    // small writer = consensus thread: ~5 small writes per 250ms view
    let mut lat = vec![];
    let t0 = Instant::now();
    let mut n = 0u64;
    while t0.elapsed() < Duration::from_secs(8) {
        let mut wb = rocksdb::WriteBatch::default();
        let raw = small_db.db_arc();
        let cf = raw.cf_handle(CF_CONSENSUS_META).unwrap();
        wb.put_cf(cf, format!("blk{n}").as_bytes(), &vec![1u8; 4096]);
        wb.put_cf(cf, b"highest_pc", &n.to_be_bytes());
        wb.put_cf(cf, b"children", &vec![2u8; 64]);
        let t = Instant::now();
        small_db.write(wb).unwrap();
        lat.push(t.elapsed().as_secs_f64() * 1e3);
        n += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }
    let mut l = lat;
    eprintln!(
        "{label}: small writes n={} mean={:.2}ms p50={:.2} p90={:.2} p99={:.2} max={:.2}",
        l.len(),
        l.iter().sum::<f64>() / l.len() as f64,
        pct(&mut l, 0.5),
        pct(&mut l, 0.9),
        pct(&mut l, 0.99),
        pct(&mut l, 1.0)
    );
}

#[test]
#[ignore]
fn write_coupling_probe() {
    run("baseline (no big writer)", false, false);
    run("shared DB + big writer", true, false);
    run("separate DB + big writer", true, true);
}
