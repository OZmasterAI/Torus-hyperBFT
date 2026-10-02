//! s84: a `log::Log` that counts the s83 drill wedge log lines emitted by the replicas
//! (one test binary = one process = one logger).

use std::sync::atomic::{AtomicU64, Ordering};

/// Counts the s83 wedge log lines emitted by the replicas.
pub(crate) struct SignatureLog {
    justify_exhausted: AtomicU64,
    sync_no_progress: AtomicU64,
    justify_unknown_drop: AtomicU64,
    body_exhausted: AtomicU64,
}

pub(crate) static SIGNATURE: SignatureLog = SignatureLog {
    justify_exhausted: AtomicU64::new(0),
    sync_no_progress: AtomicU64::new(0),
    justify_unknown_drop: AtomicU64::new(0),
    body_exhausted: AtomicU64::new(0),
};

impl log::Log for SignatureLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn || metadata.target().contains("block_sync")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = record.args().to_string();
        if line.contains("justify fetch exhausted") {
            self.justify_exhausted.fetch_add(1, Ordering::Relaxed);
        } else if line.contains("made no progress") {
            self.sync_no_progress.fetch_add(1, Ordering::Relaxed);
        } else if line.contains("body fetch exhausted") {
            self.body_exhausted.fetch_add(1, Ordering::Relaxed);
        } else if line.contains("justify_block_known=false") {
            self.justify_unknown_drop.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn flush(&self) {}
}

pub(crate) fn signature() -> String {
    format!(
        "justify_fetch_exhausted={} body_fetch_exhausted={} sync_made_no_progress={} \
         header_drop_justify_unknown={}",
        SIGNATURE.justify_exhausted.load(Ordering::Relaxed),
        SIGNATURE.body_exhausted.load(Ordering::Relaxed),
        SIGNATURE.sync_no_progress.load(Ordering::Relaxed),
        SIGNATURE.justify_unknown_drop.load(Ordering::Relaxed),
    )
}

/// Install [`SIGNATURE`] as the process logger (Info and up for block sync, Warn and up otherwise).
pub(crate) fn install() {
    let _ = log::set_logger(&SIGNATURE);
    log::set_max_level(log::LevelFilter::Info);
}
