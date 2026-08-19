//! r3 exec-write-stall-attribution — write-group head-of-line probe (ignored,
//! timing). Reproduces the mechanism the chunked background writer targets:
//! a foreground thread doing SMALL puts (the exec thread's header / body put)
//! while a background thread streams block-sized trade batches into the SAME
//! RocksDB instance. Measures the foreground put latency (mean / p90 / max)
//! with the pre-r3 policy (one write group per 60k-row batch) vs the r3
//! default (2048-row low-pri chunks). Run on a quiet box:
//!
//!   CARGO_TARGET_DIR=... cargo test --release -p torus-state \
//!     --test bg_writer_hol_probe -- --ignored --nocapture
//!
//! Not asserted as a pass/fail bound (timing); prints both so the mechanism
//! is visible in-crate without a devnet cell.

use std::time::{Duration, Instant};

use torus_state::cf::{CF_BLOCK_HEADERS, CF_NATIVE_TRADES};
use torus_state::{BackgroundCfWriter, BgWriterPolicy, DbTuning, RawCfKv, StateDb};

fn batch(block: u32, rows: usize) -> Vec<RawCfKv> {
    (0..rows)
        .map(|i| {
            let mut k = Vec::with_capacity(12);
            k.extend_from_slice(&block.to_be_bytes());
            k.extend_from_slice(&(i as u64).to_be_bytes());
            (CF_NATIVE_TRADES, k, vec![0xAB; 96])
        })
        .collect()
}

fn probe(policy: BgWriterPolicy, blocks: u32, rows: usize) -> (f64, f64, f64, u64, u64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let tuning = DbTuning {
        stats_level: 1,
        ..Default::default()
    };
    let db = StateDb::open_with_tuning(dir.path(), &tuning).expect("open");
    let writer = BackgroundCfWriter::spawn_with_policy(db.clone(), "probe-bg-writer", 8, policy);

    let fg_db = db.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let fg = std::thread::spawn(move || {
        let mut lat = Vec::new();
        let mut i = 0u64;
        while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
            let t = Instant::now();
            fg_db
                .put_cf_raw(CF_BLOCK_HEADERS, &i.to_be_bytes(), &[0x11; 900])
                .expect("fg put");
            lat.push(t.elapsed().as_secs_f64() * 1000.0);
            i += 1;
            std::thread::sleep(Duration::from_millis(5));
        }
        lat
    });

    let t0 = Instant::now();
    for b in 0..blocks {
        writer.send(batch(b, rows)).expect("send");
    }
    drop(writer); // drain
    let bg_secs = t0.elapsed().as_secs_f64();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut lat = fg.join().unwrap();
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = lat.len().max(1);
    let mean = lat.iter().sum::<f64>() / n as f64;
    let p90 = lat[(n * 9 / 10).min(n - 1)];
    let max = *lat.last().unwrap_or(&0.0);
    let tk = db.runtime_stats().tickers.unwrap();
    eprintln!(
        "policy={policy:?}: bg {blocks}x{rows} rows in {bg_secs:.2}s; fg small-put n={n} mean={mean:.2}ms p90={p90:.2}ms max={max:.2}ms; write.self={} write.other={} stall_us={}",
        tk.write_self, tk.write_other, tk.stall_micros
    );
    (mean, p90, max, tk.write_other, tk.stall_micros)
}

#[test]
#[ignore]
fn bg_writer_hol_probe_unchunked_vs_chunked() {
    let blocks = 20;
    let rows = 60_000;
    let (m0, p0, x0, _, _) = probe(BgWriterPolicy::UNCHUNKED, blocks, rows);
    let (m1, p1, x1, _, _) = probe(BgWriterPolicy::default(), blocks, rows);
    eprintln!(
        "SUMMARY fg small-put ms: unchunked mean={m0:.2} p90={p0:.2} max={x0:.2} | chunked mean={m1:.2} p90={p1:.2} max={x1:.2}"
    );
}
