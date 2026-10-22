//! s80 close-merge-removal (`TORUS_S80_CLOSE_STACK`): a window close that
//! stacks the frozen blocks n-1 and n on top of L instead of merging them
//! into L on the exec thread. The flush worker writes the stack as one batch
//! in order (L, then n-1, then n): RocksDB applies batch entries in order,
//! so last-write-wins reproduces the merge and the DB state is identical.
//!
//! * `ubench_close_merge_vs_stack` (`#[ignore]`): the premise microbench
//!   (E-side merge ms vs stack, W-side batch build ms, batch bytes, DB write
//!   ms) at the std and deep (L6.1 %100 book blob) shapes; asserts the two
//!   DBs end byte-identical. Run under `LD_PRELOAD=/usr/lib/libjemalloc.so`.

use std::sync::Arc;
use std::time::Instant;

use rocksdb::WriteBatch;
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDERS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS,
};
use torus_state::db::StateDb;
use torus_state::{CheckpointLayer, FrozenPending, NativeStateOverlay, StateBackend};

fn temp_db() -> (StateDb, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (db, dir)
}

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const ALL_TEST_CFS: [&str; 5] = [
    CF_NATIVE_POSITIONS,
    CF_NATIVE_BALANCES,
    CF_NATIVE_ORDERS,
    CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_MARKETS,
];

fn env_usize(k: &str, d: usize) -> usize {
    std::env::var(k)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(d)
}

/// A 52-byte position-like key: market (8) + account (20) + 24 bytes of id.
fn position_key(i: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(52);
    k.extend_from_slice(&(i % 300).to_be_bytes());
    k.extend_from_slice(&i.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_be_bytes());
    k.extend_from_slice(&[0u8; 12]);
    k.extend_from_slice(&i.to_be_bytes());
    k.extend_from_slice(&[7u8; 16]);
    k
}

fn value(rng: &mut Rng, len: usize) -> Vec<u8> {
    let b = rng.next();
    (0..len).map(|i| (b >> (i % 8 * 8)) as u8 ^ i as u8).collect()
}

/// One block's frozen set at the measured shape: `positions` position rows
/// (80% re-touching keys of L's range, 20% new), `positions / 4` balances,
/// `positions / 6` orders rows, ~1% deletes, plus `books` Classic book blobs
/// of `book_bytes` each (the L6.1 %100 full save).
fn block_set(
    db: &StateDb,
    rng: &mut Rng,
    height: u64,
    l_keys: u64,
    positions: usize,
    books: usize,
    book_bytes: usize,
) -> Arc<FrozenPending> {
    let o = NativeStateOverlay::new(db.clone());
    for _ in 0..positions {
        let i = if rng.below(5) == 0 {
            l_keys + rng.below(l_keys)
        } else {
            rng.below(l_keys)
        };
        let k = position_key(i);
        if rng.below(100) == 0 {
            o.delete_cf_raw(CF_NATIVE_POSITIONS, &k).unwrap();
        } else {
            let v = value(rng, 100);
            o.put_cf_raw(CF_NATIVE_POSITIONS, &k, &v).unwrap();
        }
    }
    for _ in 0..positions / 4 {
        let k = rng.below(l_keys / 4).to_be_bytes();
        let v = value(rng, 64);
        o.put_cf_raw(CF_NATIVE_BALANCES, &k, &v).unwrap();
    }
    for _ in 0..positions / 6 {
        let mut k = rng.below(1 << 40).to_be_bytes().to_vec();
        k.extend_from_slice(&[1u8; 24]);
        if rng.below(3) == 0 {
            o.delete_cf_raw(CF_NATIVE_ORDERS, &k).unwrap();
        } else {
            let v = value(rng, 90);
            o.put_cf_raw(CF_NATIVE_ORDERS, &k, &v).unwrap();
        }
    }
    for m in 0..books as u64 {
        let v = value(rng, book_bytes);
        o.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &m.to_be_bytes(), &v).unwrap();
    }
    o.put_cf_raw(CF_NATIVE_MARKETS, &height.to_be_bytes(), b"m").unwrap();
    o.freeze(height)
}

/// L at the measured shape: `l_keys` positions + `l_keys / 4` balances,
/// built as the node builds it: many block-sized frozen sets merged in
/// turn (`chunks` of them, keys in random order), so its tree nodes are
/// scattered over the heap rather than laid out in key order.
fn build_l(db: &StateDb, rng: &mut Rng, l_keys: u64, chunks: u64) -> CheckpointLayer {
    let mut l = CheckpointLayer::new();
    let per = l_keys.div_ceil(chunks);
    for c in 0..chunks {
        let o = NativeStateOverlay::new(db.clone());
        for j in 0..per {
            // A permutation of 0..l_keys spread over the chunks.
            let i = (j * chunks + c) % l_keys;
            let v = value(rng, 100);
            o.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(i), &v).unwrap();
            if i % 4 == 0 {
                let v = value(rng, 64);
                o.put_cf_raw(CF_NATIVE_BALANCES, &(i / 4).to_be_bytes(), &v).unwrap();
            }
        }
        l.merge(&o.freeze(1 + c));
    }
    l
}

/// Evict the caches between phases (the node runs a whole block between
/// two touches of L): write a 256 MB buffer.
fn thrash(buf: &mut [u8]) {
    for (i, b) in buf.iter_mut().enumerate().step_by(64) {
        *b = b.wrapping_add(i as u8);
    }
    std::hint::black_box(&buf);
}

fn scan(db: &StateDb) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for cf in ALL_TEST_CFS {
        for (k, v) in StateBackend::iterate_cf(db, cf, None).unwrap() {
            let mut key = cf.as_bytes().to_vec();
            key.push(0);
            key.extend_from_slice(&k);
            out.push((key, v));
        }
    }
    out
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// The premise microbench. Env: `CS_L_KEYS` (700000), `CS_L_CHUNKS` (98
/// block-sized merges build L), `CS_POSITIONS` (5500 per block),
/// `CS_BOOKS` (300), `CS_BOOK_KB` (367 = deep; 44 = std), `CS_ITERS` (6).
/// Each iteration builds a fresh L (untimed), evicts the caches, then
/// times one arm's close on E, its batch build on W and the DB write; the
/// arms alternate first (ABBA) and each frees its batch before the other.
#[test]
#[ignore = "microbench: cargo test --release -p torus-state --test ckpt_close_stack -- --ignored --nocapture"]
fn ubench_close_merge_vs_stack() {
    let l_keys = env_usize("CS_L_KEYS", 700_000) as u64;
    let chunks = env_usize("CS_L_CHUNKS", 98) as u64;
    let positions = env_usize("CS_POSITIONS", 5_500);
    let books = env_usize("CS_BOOKS", 300);
    let book_bytes = env_usize("CS_BOOK_KB", 367) * 1024;
    let iters = env_usize("CS_ITERS", 6);
    let (scratch, _sd) = temp_db();
    let mut rng = Rng(0x5EED_CAFE);
    let mut cache = vec![0u8; 256 << 20];
    let p = block_set(&scratch, &mut rng, 1000, l_keys, positions, 0, 0);
    let c = block_set(&scratch, &mut rng, 1001, l_keys, positions, books, book_bytes);
    // [merged, stacked]: E ms, W build ms, W write ms, batch bytes.
    let mut e = [Vec::new(), Vec::new()];
    let mut wb = [Vec::new(), Vec::new()];
    let mut wr = [Vec::new(), Vec::new()];
    // The whole W checkpoint flush (`flush_checkpoint`: build, distinct
    // dirty counts, prune / scanned-delete filters, write) into a 2nd DB.
    let mut wf = [Vec::new(), Vec::new()];
    let mut bytes = [0usize; 2];
    let mut scans: [Option<Vec<(Vec<u8>, Vec<u8>)>>; 2] = [None, None];
    // The previous closes' layers are freed before the next (steady state:
    // jemalloc reuses the freed extents, as after the dropper on the node).
    let mut keep: Vec<CheckpointLayer> = Vec::new();
    for it in 0..iters + 1 {
        let order: [usize; 2] = if matches!(it % 4, 0 | 3) { [0, 1] } else { [1, 0] };
        for arm in order {
            keep.clear();
            // Both arms of an iteration build the same L (the scans compare).
            let mut lrng = Rng(0x1A7E_5EED ^ (it as u64 + 1).wrapping_mul(0x9E37_79B9));
            let mut layer = build_l(&scratch, &mut lrng, l_keys, chunks);
            if it == 0 && arm == order[0] {
                eprintln!(
                    "shape: L entries {} bytes {:.1} MB; p bytes {:.2} MB; c bytes {:.2} MB",
                    layer.entry_count(),
                    layer.bytes() as f64 / 1e6,
                    p.state_bytes() as f64 / 1e6,
                    c.state_bytes() as f64 / 1e6
                );
            }
            thrash(&mut cache);
            let t = Instant::now();
            if arm == 0 {
                layer.merge(&p);
                layer.merge(&c);
            } else {
                layer.push_frozen(p.clone());
                layer.push_frozen(c.clone());
            }
            let em = t.elapsed().as_secs_f64() * 1e3;
            thrash(&mut cache);
            let (db, _d) = temp_db();
            let t = Instant::now();
            let mut batch = WriteBatch::default();
            layer.append_to_batch(&db, &mut batch).unwrap();
            let wm = t.elapsed().as_secs_f64() * 1e3;
            bytes[arm] = batch.size_in_bytes();
            let t = Instant::now();
            db.write(batch).unwrap();
            let wrm = t.elapsed().as_secs_f64() * 1e3;
            thrash(&mut cache);
            let (db2, _d2) = temp_db();
            let rows = torus_state::running_hash::ChainRows::default();
            let height = layer.top_height().unwrap();
            let t = Instant::now();
            layer.flush_checkpoint(None, &rows, &db2, height, None, None).unwrap();
            let wfm = t.elapsed().as_secs_f64() * 1e3;
            if it == 0 {
                scans[arm] = Some(scan(&db));
                assert!(scan(&db2) == scans[arm].clone().unwrap(), "flush_checkpoint != batch");
            } else {
                e[arm].push(em);
                wb[arm].push(wm);
                wr[arm].push(wrm);
                wf[arm].push(wfm);
                eprintln!(
                    "iter {it} {}: E {em:.3} ms | W build {wm:.2} ms | W write {wrm:.1} ms | W flush_checkpoint {wfm:.1} ms",
                    ["merged", "stacked"][arm]
                );
            }
            keep.push(layer);
        }
        if it == 0 {
            assert!(scans[0] == scans[1], "stacked batch != merged batch");
            scans = [None, None];
        }
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (em, es) = (median(&mut e[0].clone()), median(&mut e[1].clone()));
    let (wm, ws) = (median(&mut wb[0].clone()), median(&mut wb[1].clone()));
    eprintln!(
        "MEDIAN E merge {em:.2} ms (mean {:.2}) | E stack {es:.4} ms | W build merged {wm:.2} stacked {ws:.2} ms (delta {:+.2}, means {:.2}/{:.2}) | W write merged {:.1} stacked {:.1} ms | batch bytes merged {} stacked {} ({:+.2}%)",
        mean(&e[0]),
        ws - wm,
        mean(&wb[0]),
        mean(&wb[1]),
        median(&mut wr[0].clone()),
        median(&mut wr[1].clone()),
        bytes[0],
        bytes[1],
        (bytes[1] as f64 / bytes[0] as f64 - 1.0) * 100.0
    );
    eprintln!(
        "MEDIAN W flush_checkpoint merged {:.1} stacked {:.1} ms (means {:.1} / {:.1})",
        median(&mut wf[0].clone()),
        median(&mut wf[1].clone()),
        mean(&wf[0]),
        mean(&wf[1])
    );
}
/// The read-side cost of a stacked C (s80): while C is in flight (W's
/// batch build + write, ~10-13 blocks per close at 22 blk/s), every point
/// read that misses the parent and L probes C. A stacked C probes head,
/// n, n-1 then the base instead of one merged map. Times `CS_READS` point
/// reads of positions (a third in the base only, a third in n-1 / n, a
/// third absent: a DB miss) through an overlay over each C, ABBA, at the
/// std / deep shape (`CS_BOOK_KB`). Prints ns per read for both arms.
#[test]
#[ignore = "microbench: cargo test --release -p torus-state --test ckpt_close_stack -- --ignored --nocapture"]
fn ubench_reads_through_stacked_c() {
    let l_keys = env_usize("CS_L_KEYS", 700_000) as u64;
    let chunks = env_usize("CS_L_CHUNKS", 98) as u64;
    let positions = env_usize("CS_POSITIONS", 5_500);
    let books = env_usize("CS_BOOKS", 300);
    let book_bytes = env_usize("CS_BOOK_KB", 367) * 1024;
    let reads = env_usize("CS_READS", 200_000);
    let rounds = env_usize("CS_ITERS", 6);
    let (db, _d) = temp_db();
    let mut rng = Rng(0x5EED_CAFE);
    let p = block_set(&db, &mut rng, 1000, l_keys, positions, 0, 0);
    let c = block_set(&db, &mut rng, 1001, l_keys, positions, books, book_bytes);
    let mut lrng = Rng(0x1A7E_5EED);
    let base = build_l(&db, &mut lrng, l_keys, chunks);
    let mut merged = base.duplicate();
    merged.merge(&p);
    merged.merge(&c);
    let mut stacked = base;
    stacked.push_frozen(p.clone());
    stacked.push_frozen(c.clone());
    let arms = [Arc::new(merged), Arc::new(stacked)];
    // The keys: base-only, tail (n-1 / n), absent (beyond both ranges).
    let tail_keys: Vec<Vec<u8>> = [&p, &c]
        .iter()
        .flat_map(|f| {
            let o = NativeStateOverlay::with_layers(db.clone(), Some((*f).clone()), None, None);
            o.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap().into_iter().map(|(k, _)| k)
        })
        .collect();
    let mut krng = Rng(77);
    let keys: Vec<Vec<u8>> = (0..reads)
        .map(|i| match i % 3 {
            0 => position_key(krng.below(l_keys)),
            1 => tail_keys[krng.below(tail_keys.len() as u64) as usize].clone(),
            _ => position_key(3 * l_keys + krng.below(l_keys)),
        })
        .collect();
    let mut cache = vec![0u8; 256 << 20];
    let mut ns = [Vec::new(), Vec::new()];
    for r in 0..rounds + 1 {
        let order: [usize; 2] = if matches!(r % 4, 0 | 3) { [0, 1] } else { [1, 0] };
        for arm in order {
            let o = NativeStateOverlay::with_layers(db.clone(), None, None, Some(arms[arm].clone()));
            thrash(&mut cache);
            let t = Instant::now();
            let mut found = 0usize;
            for k in &keys {
                found += o.get_cf_raw(CF_NATIVE_POSITIONS, k).unwrap().is_some() as usize;
            }
            let per = t.elapsed().as_nanos() as f64 / reads as f64;
            std::hint::black_box(found);
            if r > 0 {
                ns[arm].push(per);
                eprintln!("round {r} {}: {per:.1} ns/read (found {found})", ["merged", "stacked"][arm]);
            }
        }
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    eprintln!(
        "MEDIAN read merged {:.1} ns stacked {:.1} ns (means {:.1} / {:.1}, delta {:+.1} ns/read)",
        median(&mut ns[0].clone()),
        median(&mut ns[1].clone()),
        mean(&ns[0]),
        mean(&ns[1]),
        mean(&ns[1]) - mean(&ns[0])
    );
}

// ---- correctness: a stacked close == a merged close --------------------

/// The CFs of the random test (keys 0..KEYS each); the oracle CF carries
/// submission-prefixed keys so the prune path is exercised.
const RAND_CFS: [&str; 6] = [
    CF_NATIVE_POSITIONS,
    CF_NATIVE_BALANCES,
    CF_NATIVE_ORDERS,
    CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_MARKETS,
    torus_state::cf::CF_NATIVE_ORACLE,
];
const KEYS: u64 = 24;

fn rand_key(cf: &str, i: u64) -> Vec<u8> {
    let mut k = Vec::new();
    if cf == torus_state::cf::CF_NATIVE_ORACLE {
        k.extend_from_slice(torus_state::cf::ORACLE_SUBMISSION_PREFIX);
    }
    k.extend_from_slice(&(i % 3).to_be_bytes());
    k.extend_from_slice(&i.to_be_bytes());
    k
}

/// A random block: put / delete / put chains over a small key space (the
/// same key often written, deleted and written again across blocks).
fn rand_block(db: &StateDb, rng: &mut Rng, height: u64, ops: usize) -> Arc<FrozenPending> {
    let o = NativeStateOverlay::new(db.clone());
    for _ in 0..ops {
        let cf = RAND_CFS[rng.below(RAND_CFS.len() as u64) as usize];
        let k = rand_key(cf, rng.below(KEYS));
        if rng.below(3) == 0 {
            o.delete_cf_raw(cf, &k).unwrap();
        } else {
            let len = 1 + rng.below(40) as usize;
            let v = value(rng, len);
            o.put_cf_raw(cf, &k, &v).unwrap();
        }
    }
    o.freeze(height)
}

/// Seed the DB below the window with a value for most keys (so tombstones
/// matter).
fn seed_db(db: &StateDb, rng: &mut Rng) {
    for cf in RAND_CFS {
        for i in 0..KEYS {
            if rng.below(4) != 0 {
                let v = value(rng, 8);
                db.put_cf_raw(cf, &rand_key(cf, i), &v).unwrap();
            }
        }
    }
}

fn scan_cfs(db: &StateDb, cfs: &[&str]) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for cf in cfs {
        for (k, v) in StateBackend::iterate_cf(db, cf, None).unwrap() {
            out.push((cf.to_string(), k, v));
        }
    }
    out
}

/// Every read the node makes of C answers the same for a stacked C and
/// a merged C: point reads through an overlay (C in flight), the ReadView,
/// `lookup_row`, prefix scans, `prefix_exists` and `iterate_cf_from`.
fn assert_reads_equal(db: &StateDb, merged: &Arc<CheckpointLayer>, stacked: &Arc<CheckpointLayer>, what: &str) {
    let om = NativeStateOverlay::with_layers(db.clone(), None, None, Some(merged.clone()));
    let os = NativeStateOverlay::with_layers(db.clone(), None, None, Some(stacked.clone()));
    let top = merged.top_height().unwrap();
    let vm = torus_state::ReadView::new(db.clone(), top, Vec::new(), Some(merged.clone()));
    let vs = torus_state::ReadView::new(db.clone(), top, Vec::new(), Some(stacked.clone()));
    for cf in RAND_CFS {
        for i in 0..KEYS {
            let k = rand_key(cf, i);
            assert_eq!(merged.lookup_row(cf, &k), stacked.lookup_row(cf, &k), "{what}: lookup_row {cf} {i}");
            assert_eq!(om.get_cf_raw(cf, &k).unwrap(), os.get_cf_raw(cf, &k).unwrap(), "{what}: overlay get {cf} {i}");
            assert_eq!(vm.get_cf_raw(cf, &k).unwrap(), vs.get_cf_raw(cf, &k).unwrap(), "{what}: view get {cf} {i}");
        }
        assert_eq!(om.iterate_cf(cf, None).unwrap(), os.iterate_cf(cf, None).unwrap(), "{what}: overlay scan {cf}");
        assert_eq!(vm.iterate_cf(cf, None).unwrap(), vs.iterate_cf(cf, None).unwrap(), "{what}: view scan {cf}");
        for p in 0..3u64 {
            let mut prefix = rand_key(cf, 0);
            prefix.truncate(prefix.len() - 16);
            prefix.extend_from_slice(&p.to_be_bytes());
            assert_eq!(
                om.iterate_cf(cf, Some(&prefix)).unwrap(),
                os.iterate_cf(cf, Some(&prefix)).unwrap(),
                "{what}: overlay prefix scan {cf} {p}"
            );
            assert_eq!(
                om.prefix_exists(cf, &prefix).unwrap(),
                os.prefix_exists(cf, &prefix).unwrap(),
                "{what}: prefix_exists {cf} {p}"
            );
            assert_eq!(
                om.iterate_cf_from(cf, &prefix, 5).unwrap(),
                os.iterate_cf_from(cf, &prefix, 5).unwrap(),
                "{what}: iterate_cf_from {cf} {p}"
            );
        }
    }
}

/// (a) Random put/delete/put sequences: a close that stacks n-1 and n on L
/// writes the same DB (every CF, incl. the marker and the stale mark) as
/// the close that merges them, answers every read the same while in
/// flight, and has the same top. Coalesced books put on top after the
/// stack (L3.1 `put_raw`) land on top in both.
#[test]
fn stacked_close_equals_merged_close_random() {
    for seed in 1..=60u64 {
        let mut rng = Rng(0x9E37_79B9 ^ seed.wrapping_mul(0x1000_0000_01B3));
        let (scratch, _s) = temp_db();
        let (dbm, _dm) = temp_db();
        let (dbs, _ds) = temp_db();
        let mut seed_rng = Rng(seed + 7);
        seed_db(&dbm, &mut seed_rng);
        let mut seed_rng = Rng(seed + 7);
        seed_db(&dbs, &mut seed_rng);
        let mut l = CheckpointLayer::new();
        let blocks = 1 + rng.below(4);
        for h in 1..=blocks {
            l.merge(&rand_block(&scratch, &mut rng, h, 30));
        }
        let p = rand_block(&scratch, &mut rng, blocks + 1, 30);
        let c = rand_block(&scratch, &mut rng, blocks + 2, 30);
        let drain = seed % 5 == 0; // a drain stacks the top block only
        let mut merged = l.duplicate();
        let mut stacked = l;
        if !drain {
            merged.merge(&p);
            stacked.push_frozen(p.clone());
        }
        merged.merge(&c);
        stacked.push_frozen(c.clone());
        assert!(stacked.is_stacked());
        if seed % 3 == 0 {
            for m in 0..3u64 {
                let k = rand_key(CF_NATIVE_ORDER_BOOKS, rng.below(KEYS));
                let v = value(&mut rng, 16 + m as usize);
                merged.put_raw(CF_NATIVE_ORDER_BOOKS, k.clone(), v.clone()).unwrap();
                stacked.put_raw(CF_NATIVE_ORDER_BOOKS, k, v).unwrap();
            }
        }
        assert_eq!(merged.top_height(), stacked.top_height(), "seed {seed}: top");
        assert!(stacked.bytes() >= merged.bytes(), "seed {seed}: bytes bound");
        let (merged, stacked) = (Arc::new(merged), Arc::new(stacked));
        assert_reads_equal(&dbm, &merged, &stacked, &format!("seed {seed}"));
        let height = merged.top_height().unwrap();
        let rows = torus_state::running_hash::ChainRows::default();
        let sm = merged.flush_checkpoint(None, &rows, &dbm, height, None, None).unwrap();
        let ss = stacked.flush_checkpoint(None, &rows, &dbs, height, None, None).unwrap();
        assert_eq!(sm.dirty_entries_by_cf, ss.dirty_entries_by_cf, "seed {seed}: dirty counts");
        let all: Vec<&str> = torus_state::cf::ALL_CF_NAMES.to_vec();
        assert!(scan_cfs(&dbm, &all) == scan_cfs(&dbs, &all), "seed {seed}: checkpoint DB differs");
    }
}

/// (b) The bare batch: `append_to_batch` of a stacked layer applies to
/// the same DB as the merged layer's (put -> delete -> put chains).
#[test]
fn stacked_append_to_batch_equals_merged() {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed * 31 + 1);
        let (scratch, _s) = temp_db();
        let (dbm, _dm) = temp_db();
        let (dbs, _ds) = temp_db();
        let mut seed_rng = Rng(seed);
        seed_db(&dbm, &mut seed_rng);
        let mut seed_rng = Rng(seed);
        seed_db(&dbs, &mut seed_rng);
        let mut l = CheckpointLayer::new();
        l.merge(&rand_block(&scratch, &mut rng, 1, 40));
        let p = rand_block(&scratch, &mut rng, 2, 40);
        let c = rand_block(&scratch, &mut rng, 3, 40);
        let mut merged = l.duplicate();
        merged.merge(&p);
        merged.merge(&c);
        let mut stacked = l;
        stacked.push_frozen(p);
        stacked.push_frozen(c);
        let mut bm = WriteBatch::default();
        merged.append_to_batch(&dbm, &mut bm).unwrap();
        let mut bs = WriteBatch::default();
        stacked.append_to_batch(&dbs, &mut bs).unwrap();
        dbm.write(bm).unwrap();
        dbs.write(bs).unwrap();
        assert!(scan_cfs(&dbm, &RAND_CFS) == scan_cfs(&dbs, &RAND_CFS), "seed {seed}");
        // A duplicate of a stacked layer is the same stack.
        let dup = stacked.duplicate();
        assert_eq!(dup.top_height(), stacked.top_height());
        assert_eq!(dup.entry_count(), stacked.entry_count());
    }
}

/// (c) Guards: a stacked layer refuses a merge (L is never stacked), and
/// at most `MAX_STACKED` blocks stack.
#[test]
fn stacked_layer_refuses_merge_and_overfill() {
    let (scratch, _s) = temp_db();
    let mut rng = Rng(5);
    let blocks: Vec<_> = (1..=4).map(|h| rand_block(&scratch, &mut rng, h, 5)).collect();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut l = CheckpointLayer::new();
        l.push_frozen(blocks[0].clone());
        l.merge(&blocks[1]);
    }));
    assert!(r.is_err(), "merge into a stacked layer must panic");
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut l = CheckpointLayer::new();
        for b in blocks.iter().take(torus_state::MAX_STACKED + 1) {
            l.push_frozen(b.clone());
        }
    }));
    assert!(r.is_err(), "more than MAX_STACKED stacked blocks must panic");
}

/// (d) `put_raw` on a stacked layer (the L3.1 coalesced books at the
/// close) goes on top of the stack: an overwrite of a head row keeps the
/// byte count exact (no underflow), `bytes()` is base + stacked blocks +
/// head, and reads and the batch see the newest value.
#[test]
fn stacked_put_raw_head_bytes_and_precedence() {
    let (scratch, _s) = temp_db();
    let mut rng = Rng(11);
    let mut l = CheckpointLayer::new();
    l.merge(&rand_block(&scratch, &mut rng, 1, 20));
    let base_bytes = l.bytes();
    let c = rand_block(&scratch, &mut rng, 2, 20);
    let c_bytes = c.state_bytes();
    l.push_frozen(c);
    assert_eq!(l.bytes(), base_bytes + c_bytes);
    let k = rand_key(CF_NATIVE_ORDER_BOOKS, 3);
    l.put_raw(CF_NATIVE_ORDER_BOOKS, k.clone(), vec![1; 40]).unwrap();
    l.put_raw(CF_NATIVE_ORDER_BOOKS, k.clone(), vec![2; 8]).unwrap();
    assert_eq!(l.bytes(), base_bytes + c_bytes + k.len() + 8);
    assert_eq!(l.lookup_row(CF_NATIVE_ORDER_BOOKS, &k), Some(Some(vec![2; 8])));
    let (db, _d) = temp_db();
    let mut batch = WriteBatch::default();
    l.append_to_batch(&db, &mut batch).unwrap();
    db.write(batch).unwrap();
    assert_eq!(db.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &k).unwrap(), Some(vec![2; 8]));
}
