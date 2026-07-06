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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

use rocksdb::WriteBatch;

use crate::db::StateDb;
use crate::error::StateError;

/// A raw write destined for a named column family. CF names are `'static`
/// consts from [`crate::cf`].
pub type RawCfKv = (&'static str, Vec<u8>, Vec<u8>);

pub struct BackgroundCfWriter {
    tx: Option<SyncSender<Vec<RawCfKv>>>,
    handle: Option<JoinHandle<()>>,
    queued: Arc<AtomicUsize>,
}

impl BackgroundCfWriter {
    /// Spawn the writer thread. `queue_cap` bounds the number of in-flight
    /// batches; a full queue makes `send` block (backpressure), so a stalled
    /// RocksDB cannot balloon memory.
    pub fn spawn(db: StateDb, thread_name: &str, queue_cap: usize) -> Self {
        let (tx, rx) = sync_channel::<Vec<RawCfKv>>(queue_cap);
        let queued = Arc::new(AtomicUsize::new(0));
        let drained = queued.clone();
        let handle = std::thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || {
                // recv() returns every queued batch even after the sender is
                // dropped, then errors — drain-on-shutdown falls out for free.
                while let Ok(kvs) = rx.recv() {
                    if let Err(e) = write_kvs(&db, &kvs) {
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

fn write_kvs(db: &StateDb, kvs: &[RawCfKv]) -> Result<(), StateError> {
    let mut batch = WriteBatch::default();
    for (cf_name, key, value) in kvs {
        batch.put_cf(db.cf_handle(cf_name)?, key, value);
    }
    db.write(batch)
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
    fn queued_batches_returns_to_zero() {
        let (_dir, db) = open_test_db();
        let writer = BackgroundCfWriter::spawn(db.clone(), "test-cf-writer", 8);
        writer
            .send(vec![(CF_NATIVE_TRADES, b"a".to_vec(), b"1".to_vec())])
            .expect("send");
        // Wait for the writer to consume it (bounded spin, no sleep-forever).
        for _ in 0..1000 {
            if writer.queued_batches() == 0 {
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(writer.queued_batches(), 0);
    }
}
