//! Item 7 option 0, step 0 (task 0.3, `docs/plans/item7-option0-impl.md`):
//! TEST-ONLY read probes of the layered overlay. Compiled only with the
//! `test-probes` feature (enabled by `torus-consensus`'s dev-dependency);
//! no node build reaches this module.
//!
//! A test installs a [`Probe`] on a thread ([`install`]); every read an
//! overlay makes ON THAT THREAD is then counted per (reader class, layer
//! that answered, CF). The reader class is a thread-local label the caller
//! sets ([`scope`]). Beside each layer hit the probe keeps an IN-FLIGHT
//! count: at the moment of the read it re-reads the same key from the DB and
//! counts the hit in flight iff the DB answer differs (absent, other bytes,
//! or a row the layer tombstoned). Only those hits prove that a reader
//! skipping the layer would have diverged at that read (plan R2: a parent hit
//! after a drain, or a C hit whose checkpoint already landed, is a hit and
//! not in flight).
//!
//! The probe also carries the planted faults of plan section 5.4: an overlay
//! built on a thread whose probe has [`Plant::SkipParent`] (or
//! `SkipCheckpoint`, `SkipDeferred`) reads as if that layer were absent. With
//! no probe installed nothing changes: no count, no skip.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The layer that answered a read (newest first, then the DB).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// The overlay's own pending set.
    Own,
    /// The resident rows R.
    Resident,
    /// The previous block's frozen set (pipelined parent).
    Parent,
    /// The since-checkpoint layer L, main part.
    LMain,
    /// L's deferred part (`DEFERRED_CFS`, possibly still being merged).
    LDeferred,
    /// The checkpoint in flight C.
    Checkpoint,
    /// The DB itself.
    Db,
}

impl Layer {
    /// Every layer, in read order.
    pub const ALL: [Layer; 7] = [
        Layer::Own,
        Layer::Resident,
        Layer::Parent,
        Layer::LMain,
        Layer::LDeferred,
        Layer::Checkpoint,
        Layer::Db,
    ];

    /// A layer whose answer can differ from the DB's (the in-flight layers).
    pub fn frozen(self) -> bool {
        matches!(
            self,
            Layer::Parent | Layer::LMain | Layer::LDeferred | Layer::Checkpoint
        )
    }
}

/// Who read: a thread-local label set by the caller ([`scope`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reader {
    /// No scope set.
    Other,
    /// The revm database adapter.
    Revm,
    /// The writer-precompile journal.
    Journal,
    /// The EVM lane read.
    Lane,
    /// The CoreWriter / oracle / liquidation due-checks.
    DueCheck,
    /// The native phase.
    Native,
    /// A test's own reads.
    Test,
}

/// One (reader, layer, CF) cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    /// Reads this layer answered.
    pub hits: u64,
    /// Of those, the reads whose DB answer at that moment differed.
    pub inflight: u64,
}

impl std::ops::AddAssign for Count {
    fn add_assign(&mut self, o: Self) {
        self.hits += o.hits;
        self.inflight += o.inflight;
    }
}

/// A planted fault: overlays built while it is set read without that layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plant {
    /// Skip the pipelined parent layer.
    SkipParent,
    /// Skip the checkpoint in flight C.
    SkipCheckpoint,
    /// Skip L's deferred part.
    SkipDeferred,
}

/// The layers an overlay skips (all `false` without a probe).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Skip {
    pub(crate) parent: bool,
    pub(crate) checkpoint: bool,
    pub(crate) deferred: bool,
}

/// The counters of one node (install it on the node's executing thread).
#[derive(Default)]
pub struct Probe {
    counts: Mutex<BTreeMap<(Reader, Layer, &'static str), Count>>,
    skip_parent: AtomicBool,
    skip_checkpoint: AtomicBool,
    skip_deferred: AtomicBool,
}

impl Probe {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Plant `p` (overlays built from now on, on threads with this probe).
    pub fn plant(&self, p: Plant) {
        match p {
            Plant::SkipParent => &self.skip_parent,
            Plant::SkipCheckpoint => &self.skip_checkpoint,
            Plant::SkipDeferred => &self.skip_deferred,
        }
        .store(true, Ordering::SeqCst);
    }

    fn skip(&self) -> Skip {
        Skip {
            parent: self.skip_parent.load(Ordering::SeqCst),
            checkpoint: self.skip_checkpoint.load(Ordering::SeqCst),
            deferred: self.skip_deferred.load(Ordering::SeqCst),
        }
    }

    /// Every non-zero cell.
    pub fn counts(&self) -> BTreeMap<(Reader, Layer, &'static str), Count> {
        self.counts.lock().unwrap().clone()
    }

    /// The sum over the cells `f` selects.
    pub fn total(&self, f: impl Fn(Reader, Layer, &str) -> bool) -> Count {
        let mut out = Count::default();
        for ((r, l, cf), c) in self.counts.lock().unwrap().iter() {
            if f(*r, *l, cf) {
                out += *c;
            }
        }
        out
    }

    /// Forget every count (the plants stay).
    pub fn reset(&self) {
        self.counts.lock().unwrap().clear();
    }

    fn add(&self, reader: Reader, layer: Layer, cf: &'static str, inflight: bool) {
        let mut c = self.counts.lock().unwrap();
        let cell = c.entry((reader, layer, cf)).or_default();
        cell.hits += 1;
        cell.inflight += u64::from(inflight);
    }

    /// A per-(reader, layer) table: hits / in flight, summed over CFs.
    pub fn report(&self) -> String {
        let mut by: BTreeMap<(Reader, Layer), Count> = BTreeMap::new();
        for ((r, l, _), c) in self.counts.lock().unwrap().iter() {
            *by.entry((*r, *l)).or_default() += *c;
        }
        let mut out = String::new();
        for ((r, l), c) in by {
            out.push_str(&format!(
                "{r:?}/{l:?}: {} hits, {} in flight\n",
                c.hits, c.inflight
            ));
        }
        out
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Arc<Probe>>> = const { RefCell::new(None) };
    static READER: Cell<Reader> = const { Cell::new(Reader::Other) };
}

/// Restores the previously installed probe on drop.
pub struct Installed(Option<Arc<Probe>>);

impl Drop for Installed {
    fn drop(&mut self) {
        let prev = self.0.take();
        ACTIVE.with(|a| *a.borrow_mut() = prev);
    }
}

/// Count this thread's overlay reads into `probe` until the guard drops.
pub fn install(probe: Arc<Probe>) -> Installed {
    Installed(ACTIVE.with(|a| a.borrow_mut().replace(probe)))
}

/// Restores the previous reader label on drop.
pub struct Scoped(Reader);

impl Drop for Scoped {
    fn drop(&mut self) {
        READER.with(|r| r.set(self.0));
    }
}

/// Label this thread's reads `reader` until the guard drops.
pub fn scope(reader: Reader) -> Scoped {
    Scoped(READER.with(|r| r.replace(reader)))
}

/// Whether a probe is installed on this thread.
pub fn active() -> bool {
    ACTIVE.with(|a| a.borrow().is_some())
}

/// The layers an overlay built now on this thread skips.
pub(crate) fn skip() -> Skip {
    ACTIVE.with(|a| a.borrow().as_ref().map(|p| p.skip()).unwrap_or_default())
}

/// Count one read answered by `layer`; `inflight` is evaluated only when a
/// probe is installed (it re-reads the DB).
pub(crate) fn record(cf: &'static str, layer: Layer, inflight: impl FnOnce() -> bool) {
    let probe = ACTIVE.with(|a| a.borrow().clone());
    if let Some(p) = probe {
        let reader = READER.with(|r| r.get());
        let inflight = layer.frozen() && inflight();
        p.add(reader, layer, cf, inflight);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn o0_probe_counts_only_while_installed_and_scopes_nest() {
        let p = Probe::new();
        record("x", Layer::Db, || true);
        assert!(p.counts().is_empty(), "nothing installed: nothing counted");
        {
            let _i = install(p.clone());
            record("x", Layer::Db, || true);
            {
                let _s = scope(Reader::Native);
                record("x", Layer::Parent, || true);
                record("x", Layer::Parent, || false);
            }
            record("x", Layer::Parent, || true);
        }
        record("x", Layer::Parent, || true);
        let c = p.counts();
        assert_eq!(
            c[&(Reader::Other, Layer::Db, "x")],
            Count {
                hits: 1,
                inflight: 0
            },
            "the DB is never in flight"
        );
        assert_eq!(
            c[&(Reader::Native, Layer::Parent, "x")],
            Count {
                hits: 2,
                inflight: 1
            }
        );
        assert_eq!(
            c[&(Reader::Other, Layer::Parent, "x")],
            Count {
                hits: 1,
                inflight: 1
            },
            "the scope ended"
        );
        assert_eq!(c.len(), 3);
        assert_eq!(skip(), Skip::default());
        p.plant(Plant::SkipParent);
        let _i = install(p);
        assert!(skip().parent && !skip().checkpoint && !skip().deferred);
    }
}
