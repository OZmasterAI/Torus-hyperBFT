//! Background column-family writer (O3).
//!
//! Moves node-local CF writes off the execution thread: the exec path buffers
//! raw KVs and hands each block's batch to this writer, which applies them in
//! one `WriteBatch` on its own thread. Only valid for CFs OUTSIDE the consensus
//! roots (trade history: `CF_NATIVE_TRADES` / `CF_NATIVE_USER_TRADES`) — the
//! rows land shortly after the block's atomic state flush, so a hard crash can
//! lose the last few queued batches. Replay does not re-execute applied blocks,
//! so such a gap stays a cosmetic hole in RPC trade history, never consensus
//! divergence.
//!
//! Shutdown ordering: dropping the writer closes the channel; the thread drains
//! every queued batch, then exits, and the drop joins it. The thread owns its
//! own `StateDb` clone, so the DB stays open until the drain completes.
//!
//! r3 exec-write-stall-attribution — write-group head-of-line blocking: RocksDB
//! serialises write groups per instance, so while this thread's ONE per-block
//! batch (tens of thousands of trade rows at 30k matched/s) is being applied,
//! every other writer — the exec thread's header/body puts and state flush, the
//! consensus thread's commit-time persist — waits for the WHOLE batch to land
//! (measured 60-90 ms per exec write, exactly the size of one trade batch;
//! removing the exec writes only moved the wait to the next one). Two
//! mitigations, both node-local and byte-identical in what gets stored:
//! - batches are written in CHUNKS of `TORUS_BG_WRITER_CHUNK_KVS` rows
//!   (default 2048; `0` = one batch as before), so a foreground write waits at
//!   most one chunk (~ms) instead of one block's worth of rows;
//! - chunks are written `low_pri` (`TORUS_BG_WRITER_LOW_PRI=0` disables), so
//!   write-controller back-pressure under compaction debt lands on this
//!   cosmetic writer instead of the exec / consensus threads.
//! Rows within a batch keep their order across chunks; a chunk boundary is a
//! crash-visible partial batch, which is exactly the "lose the last few rows"
//! failure mode the writer already has (cosmetic RPC history, never consensus).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

use rocksdb::{WriteBatch, WriteOptions};

use crate::db::StateDb;
use crate::error::StateError;

/// A raw write destined for a named column family. CF names are `'static`
/// consts from [`crate::cf`].
pub type RawCfKv = (&'static str, Vec<u8>, Vec<u8>);

/// Default `TORUS_BG_WRITER_CHUNK_KVS`: rows per RocksDB write group. 2048
/// small rows is a ~1-3 ms memtable insert, i.e. the longest a foreground
/// writer can be held behind this thread.
pub const DEFAULT_BG_WRITER_CHUNK_KVS: usize = 2048;

/// Pure parse of `TORUS_BG_WRITER_CHUNK_KVS`: unset / garbage => the default;
/// `0` => unchunked (one batch per block, exact-pre-r3).
pub fn parse_bg_writer_chunk_kvs(raw: Option<String>) -> usize {
    raw.as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_BG_WRITER_CHUNK_KVS)
}

/// Pure parse of `TORUS_BG_WRITER_LOW_PRI`: default ON; `0`/`false`/`off`
/// disables.
pub fn parse_bg_writer_low_pri(raw: Option<String>) -> bool {
    !matches!(
        raw.as_deref().map(str::trim),
        Some("0" | "false" | "FALSE" | "no" | "off")
    )
}

/// How the writer thread applies each queued batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BgWriterPolicy {
    /// Rows per write group; `0` = whole batch in one write.
    pub chunk_kvs: usize,
    /// Write chunks with `WriteOptions::low_pri`.
    pub low_pri: bool,
}

impl BgWriterPolicy {
    /// Read from the environment (once, at spawn).
    pub fn from_env() -> Self {
        Self {
            chunk_kvs: parse_bg_writer_chunk_kvs(std::env::var("TORUS_BG_WRITER_CHUNK_KVS").ok()),
            low_pri: parse_bg_writer_low_pri(std::env::var("TORUS_BG_WRITER_LOW_PRI").ok()),
        }
    }

    /// Pre-r3 behaviour: one normal-priority write per batch.
    pub const UNCHUNKED: Self = Self {
        chunk_kvs: 0,
        low_pri: false,
    };
}

impl Default for BgWriterPolicy {
    fn default() -> Self {
        Self {
            chunk_kvs: DEFAULT_BG_WRITER_CHUNK_KVS,
            low_pri: true,
        }
    }
}

pub struct BackgroundCfWriter {
    tx: Option<SyncSender<Vec<RawCfKv>>>,
    handle: Option<JoinHandle<()>>,
    queued: Arc<AtomicUsize>,
}

impl BackgroundCfWriter {
    /// Spawn the writer thread with the policy from the environment. `queue_cap`
    /// bounds the number of in-flight batches; a full queue makes `send` block
    /// (backpressure), so a stalled RocksDB cannot balloon memory.
    pub fn spawn(db: StateDb, thread_name: &str, queue_cap: usize) -> Self {
        Self::spawn_with_policy(db, thread_name, queue_cap, BgWriterPolicy::from_env())
    }

    /// Spawn the writer thread with an explicit [`BgWriterPolicy`].
    pub fn spawn_with_policy(
        db: StateDb,
        thread_name: &str,
        queue_cap: usize,
        policy: BgWriterPolicy,
    ) -> Self {
        let (tx, rx) = sync_channel::<Vec<RawCfKv>>(queue_cap);
        let queued = Arc::new(AtomicUsize::new(0));
        let drained = queued.clone();
        let handle = std::thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || {
                // recv() returns every queued batch even after the sender is
                // dropped, then errors — drain-on-shutdown falls out for free.
                while let Ok(kvs) = rx.recv() {
                    if let Err(e) = write_kvs_chunked(&db, &kvs, policy) {
                        tracing::error!(%e, n = kvs.len(), "background CF writer: batch failed");
                    }
                    drained.fetch_sub(1, Ordering::Relaxed);
                }
            })
            .expect("spawn background CF writer thread");
        Self {
            tx: Some(tx),
            handle: Some(handle),
            queued,
        }
    }

    /// Queue a batch. If the writer is gone the batch is handed back so the
    /// caller can write it synchronously instead of losing it.
    pub fn send(&self, kvs: Vec<RawCfKv>) -> Result<(), Vec<RawCfKv>> {
        let Some(tx) = &self.tx else {
            return Err(kvs);
        };
        self.queued.fetch_add(1, Ordering::Relaxed);
        tx.send(kvs).map_err(|e| {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            e.0
        })
    }

    /// Batches queued but not yet written (telemetry).
    pub fn queued_batches(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }
}

impl Drop for BackgroundCfWriter {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Apply `kvs` in `policy.chunk_kvs`-row write groups (all rows in one group
/// when `chunk_kvs == 0`), in order. Chunks are written with `low_pri` per the
/// policy. Stops at the first failing chunk (rows before it are already
/// durable; the caller logs, and the writer's loss model already tolerates a
/// partial tail).
fn write_kvs_chunked(db: &StateDb, kvs: &[RawCfKv], policy: BgWriterPolicy) -> Result<(), StateError> {
    if kvs.is_empty() {
        return Ok(());
    }
    let mut wo = WriteOptions::default();
    wo.set_low_pri(policy.low_pri);
    let chunk = if policy.chunk_kvs == 0 { kvs.len() } else { policy.chunk_kvs };
    for part in kvs.chunks(chunk) {
        let mut batch = WriteBatch::default();
        for (cf_name, key, value) in part {
            batch.put_cf(db.cf_handle(cf_name)?, key, value);
        }
        db.write_with(batch, &wo)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cf::{CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES};

    fn open_test_db() -> (tempfile::TempDir, StateDb) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db = StateDb::open(dir.path()).expect("open db");
        (dir, db)
    }

    #[test]
    fn drains_all_queued_batches_on_drop() {
        let (_dir, db) = open_test_db();
        let writer = BackgroundCfWriter::spawn(db.clone(), "test-cf-writer", 8);

        for i in 0..5u8 {
            writer
                .send(vec![
                    (CF_NATIVE_TRADES, vec![i], vec![i, i]),
                    (CF_NATIVE_USER_TRADES, vec![i, 0xFF], vec![i]),
                ])
                .expect("send");
        }
        drop(writer); // closes channel, drains, joins

        for i in 0..5u8 {
            assert_eq!(
                db.get_cf_raw(CF_NATIVE_TRADES, &[i]).unwrap(),
                Some(vec![i, i]),
                "trade row {i} must be durable after drop"
            );
            assert_eq!(
                db.get_cf_raw(CF_NATIVE_USER_TRADES, &[i, 0xFF]).unwrap(),
                Some(vec![i]),
                "user-trade row {i} must be durable after drop"
            );
        }
    }

    #[test]
    fn later_batch_wins_for_same_key() {
        let (_dir, db) = open_test_db();
        let writer = BackgroundCfWriter::spawn(db.clone(), "test-cf-writer", 8);
        writer
            .send(vec![(CF_NATIVE_TRADES, b"k".to_vec(), b"old".to_vec())])
            .expect("send");
        writer
            .send(vec![(CF_NATIVE_TRADES, b"k".to_vec(), b"new".to_vec())])
            .expect("send");
        drop(writer);
        assert_eq!(
            db.get_cf_raw(CF_NATIVE_TRADES, b"k").unwrap(),
            Some(b"new".to_vec()),
            "single-producer FIFO ordering: idempotent replay overwrite applies in order"
        );
    }

    #[test]
    fn bounded_queue_applies_backpressure_without_loss() {
        // cap << N: the bounded `sync_channel` forces the producer to BLOCK
        // (backpressure) instead of buffering unboundedly — memory stays bounded to
        // ~cap in-flight batches, and every batch still lands after drain. This is
        // the L3 post-flush-writer bound (queue never grows without limit).
        let (_dir, db) = open_test_db();
        let writer = BackgroundCfWriter::spawn(db.clone(), "test-cf-writer", 4);
        let n = 500u32;
        for i in 0..n {
            writer
                .send(vec![(CF_NATIVE_TRADES, i.to_be_bytes().to_vec(), vec![i as u8])])
                .expect("send must block under backpressure, never error/drop");
        }
        drop(writer); // drain + join
        for i in 0..n {
            assert_eq!(
                db.get_cf_raw(CF_NATIVE_TRADES, &i.to_be_bytes()).unwrap(),
                Some(vec![i as u8]),
                "batch {i} must be durable despite cap=4 backpressure"
            );
        }
    }

    #[test]
    fn queued_batches_returns_to_zero() {
        let (_dir, db) = open_test_db();
        let writer = BackgroundCfWriter::spawn(db.clone(), "test-cf-writer", 8);
        writer
            .send(vec![(CF_NATIVE_TRADES, b"a".to_vec(), b"1".to_vec())])
            .expect("send");
        // Wait for the writer to consume it: deadline-bounded (a bare 1000x
        // yield_now spin was flaky on a loaded box — the writer thread simply
        // was not scheduled within 1000 yields).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while writer.queued_batches() != 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(writer.queued_batches(), 0);
    }

    // ---- r3 exec-write-stall-attribution: chunked, low-pri writes ----

    #[test]
    fn bg_writer_policy_env_parse() {
        assert_eq!(parse_bg_writer_chunk_kvs(None), DEFAULT_BG_WRITER_CHUNK_KVS);
        assert_eq!(parse_bg_writer_chunk_kvs(Some("garbage".into())), DEFAULT_BG_WRITER_CHUNK_KVS);
        assert_eq!(parse_bg_writer_chunk_kvs(Some("0".into())), 0);
        assert_eq!(parse_bg_writer_chunk_kvs(Some(" 512 ".into())), 512);
        assert!(parse_bg_writer_low_pri(None));
        assert!(parse_bg_writer_low_pri(Some("1".into())));
        assert!(!parse_bg_writer_low_pri(Some("0".into())));
        assert!(!parse_bg_writer_low_pri(Some("off".into())));
        let d = BgWriterPolicy::default();
        assert_eq!(d.chunk_kvs, DEFAULT_BG_WRITER_CHUNK_KVS);
        assert!(d.low_pri);
        assert_eq!(BgWriterPolicy::UNCHUNKED.chunk_kvs, 0);
    }

    /// A batch larger than one chunk must land completely, in order, and be
    /// byte-identical to the unchunked write — chunking changes only how many
    /// write groups the rows ride, never what is stored.
    #[test]
    fn chunked_batch_lands_all_rows_in_order() {
        let (_dir, db) = open_test_db();
        let policy = BgWriterPolicy {
            chunk_kvs: 7,
            low_pri: true,
        };
        let writer = BackgroundCfWriter::spawn_with_policy(db.clone(), "test-cf-writer", 8, policy);
        // 100 rows, and a duplicate key whose LATER value must win (order
        // preserved across the chunk boundary at 7/14/...).
        let mut kvs: Vec<RawCfKv> = (0..100u32)
            .map(|i| (CF_NATIVE_TRADES, i.to_be_bytes().to_vec(), vec![i as u8]))
            .collect();
        kvs.push((CF_NATIVE_TRADES, 3u32.to_be_bytes().to_vec(), b"late".to_vec()));
        writer.send(kvs).expect("send");
        drop(writer);
        for i in 0..100u32 {
            let want = if i == 3 { b"late".to_vec() } else { vec![i as u8] };
            assert_eq!(
                db.get_cf_raw(CF_NATIVE_TRADES, &i.to_be_bytes()).unwrap(),
                Some(want),
                "row {i} must be durable and carry the in-order value"
            );
        }
    }

    /// `write_kvs_chunked` splits into ceil(n / chunk) write groups: prove it
    /// via the RocksDB write.self ticker (each solo group = one write.self).
    #[test]
    fn chunked_write_issues_one_group_per_chunk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tuning = crate::db::DbTuning {
            stats_level: 1,
            ..Default::default()
        };
        let db = StateDb::open_with_tuning(dir.path(), &tuning).expect("open");
        let kvs: Vec<RawCfKv> = (0..25u32)
            .map(|i| (CF_NATIVE_TRADES, i.to_be_bytes().to_vec(), vec![1]))
            .collect();
        let before = db.runtime_stats().tickers.unwrap().write_self;
        write_kvs_chunked(&db, &kvs, BgWriterPolicy { chunk_kvs: 10, low_pri: false }).unwrap();
        let after = db.runtime_stats().tickers.unwrap().write_self;
        assert_eq!(after - before, 3, "25 rows / chunk 10 => 3 write groups");
        let before = after;
        write_kvs_chunked(&db, &kvs, BgWriterPolicy::UNCHUNKED).unwrap();
        let after = db.runtime_stats().tickers.unwrap().write_self;
        assert_eq!(after - before, 1, "unchunked => exactly one write group");
    }
}
