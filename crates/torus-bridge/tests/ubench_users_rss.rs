//! Item 6 Phase 4 step 6 (`docs/plans/item6-phase4-impl.md` "Step 6", gate
//! "RAM per user", section 1): resident bytes per user of the `Users` record
//! set, orders excluded.
//!
//! `ubench_users_rss` (ignored) builds synthetic users into the record set
//! the way R's rebuild does (`users_bench::UsersProbe::build`, a cold build
//! from raw rows of both CFs) and reports the bytes it allocates per user:
//! light users (2 positions) and active users (10 positions), each with a
//! balance row and a `cvlm` row, at 100k and 1M users (`UB_USERS`, comma
//! separated, default `100000,1000000`). Positions sit in distinct markets of
//! 300 (the standard bench shape), 1 in 10 Isolated; addresses are hashed
//! (random key order, as real addresses). Then every record gets a sums entry
//! (the steady state after step 3: one per valued trader, never cleared).
//!
//! Allocator stats: the test binary runs on the system allocator (jemalloc is
//! compiled into `torus-node` only, `main.rs`'s `#[global_allocator]`), so a
//! counting global allocator here keeps three live-byte counters per
//! allocation: `req` (bytes asked for), `glibc` (`malloc_usable_size` + the
//! 8-byte chunk header: what glibc holds) and `jemalloc` (jemalloc 5's size
//! class of the request for small sizes <= 14 KiB, the page-rounded request
//! above: what the node's allocator holds, slab fragmentation not counted).
//! Plus the process RSS delta (informational: glibc keeps freed build
//! temporaries). The pass criterion uses the jemalloc column with sums:
//! light <= ~0.6 KB, active <= ~2.3 KB per user (section 1). The table marks
//! each row ok / OVER; the test asserts only loose sanity bounds.
//!
//!   cargo test --release -p torus-bridge --test ubench_users_rss -- --ignored --nocapture
//!   UB_USERS=100000 cargo test --release -p torus-bridge --test ubench_users_rss -- --ignored --nocapture
//!
//! Run with `--ignored` (not `--include-ignored`): the counters are process
//! wide; every test here takes `LOCK` so they do not overlap.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::sync::Mutex;

use alloy_primitives::Address;
use torus_bridge::native_executor::users_bench::UsersProbe;
use torus_core::position::{cum_volume_key, position_key, MarginType, NativeBalance, Position};
use torus_types::{FixedPoint, MarketId};

// ---------------------------------------------------------------------------
// Counting allocator
// ---------------------------------------------------------------------------

struct Counting;

static REQ: AtomicIsize = AtomicIsize::new(0);
static GLIBC: AtomicIsize = AtomicIsize::new(0);
static JE: AtomicIsize = AtomicIsize::new(0);
static LIVE: AtomicIsize = AtomicIsize::new(0);

#[cfg(target_os = "linux")]
extern "C" {
    fn malloc_usable_size(ptr: *mut std::ffi::c_void) -> usize;
}

/// glibc's chunk of a live allocation: usable size + the size field.
fn glibc_bytes(ptr: *mut u8, size: usize) -> isize {
    #[cfg(target_os = "linux")]
    // SAFETY: `ptr` is a live allocation of the system allocator (glibc).
    let usable = unsafe { malloc_usable_size(ptr.cast()) };
    #[cfg(not(target_os = "linux"))]
    let usable = {
        let _ = ptr;
        size
    };
    (usable.max(size) + 8) as isize
}

/// jemalloc 5's size class (x86_64: 16-byte quantum, 4 KiB pages, 4 classes
/// per doubling) for small sizes; a large allocation (> 14 KiB) holds whole
/// pages and only its touched pages are resident: the page-rounded request.
fn jemalloc_bytes(size: usize) -> usize {
    const SMALL_MAX: usize = 14 * 1024;
    match size {
        0..=8 => 8,
        9..=128 => size.div_ceil(16) * 16,
        129..=SMALL_MAX => {
            let lg = usize::BITS - 1 - (size - 1).leading_zeros();
            let delta = 1usize << (lg - 2);
            size.div_ceil(delta) * delta
        }
        _ => size.div_ceil(4096) * 4096,
    }
}

fn count(ptr: *mut u8, size: usize, sign: isize) {
    REQ.fetch_add(sign * size as isize, Relaxed);
    GLIBC.fetch_add(sign * glibc_bytes(ptr, size), Relaxed);
    JE.fetch_add(sign * jemalloc_bytes(size) as isize, Relaxed);
    LIVE.fetch_add(sign, Relaxed);
}

// SAFETY: every call forwards to `System` unchanged; the counters only read
// the pointers it returns, while they are live.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            count(p, layout.size(), 1);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            count(p, layout.size(), 1);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(ptr, layout.size(), -1);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old = (layout.size(), glibc_bytes(ptr, layout.size()));
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            REQ.fetch_add(new_size as isize - old.0 as isize, Relaxed);
            GLIBC.fetch_add(glibc_bytes(p, new_size) - old.1, Relaxed);
            JE.fetch_add(jemalloc_bytes(new_size) as isize - jemalloc_bytes(old.0) as isize, Relaxed);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Tests share the process-wide counters: one at a time.
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug)]
struct Snap {
    req: isize,
    glibc: isize,
    je: isize,
    live: isize,
    rss: isize,
}

fn rss_bytes() -> isize {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: isize = statm.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    pages * 4096
}

fn snap() -> Snap {
    Snap { req: REQ.load(Relaxed), glibc: GLIBC.load(Relaxed), je: JE.load(Relaxed), live: LIVE.load(Relaxed), rss: rss_bytes() }
}

impl std::ops::Sub for Snap {
    type Output = Snap;
    fn sub(self, o: Snap) -> Snap {
        Snap { req: self.req - o.req, glibc: self.glibc - o.glibc, je: self.je - o.je, live: self.live - o.live, rss: self.rss - o.rss }
    }
}

// ---------------------------------------------------------------------------
// Synthetic users
// ---------------------------------------------------------------------------

/// Markets of the standard bench shape.
const MARKETS: u64 = 300;

fn splitmix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// User `i`'s address: hashed, so key order is random in `i`.
fn trader(i: u64) -> Address {
    let mut b = [0u8; 20];
    for (j, chunk) in b.chunks_mut(8).enumerate() {
        let h = splitmix(i.wrapping_mul(3).wrapping_add(j as u64)).to_be_bytes();
        chunk.copy_from_slice(&h[..chunk.len()]);
    }
    Address::from(b)
}

/// User `i`'s `k` distinct markets of `1..=MARKETS`: a start and a stride
/// coprime with 300.
fn markets(i: u64, k: usize) -> Vec<MarketId> {
    const STRIDES: [u64; 8] = [7, 11, 13, 17, 19, 23, 29, 31];
    let h = splitmix(i ^ 0x5555);
    let stride = STRIDES[(h % 8) as usize];
    let mut ms: Vec<MarketId> = (0..k as u64).map(|j| 1 + (h / 8 + j * stride) % MARKETS).collect();
    ms.sort_unstable();
    ms
}

fn fp(x: u64) -> FixedPoint {
    FixedPoint::from_raw(i128::from(x))
}

/// The raw rows of `n` users with `k` positions each, a balance row and a
/// `cvlm` row: (positions CF, balances CF), as R holds them.
#[allow(clippy::type_complexity)]
fn synth(n: u64, k: usize) -> (BTreeMap<Vec<u8>, Vec<u8>>, BTreeMap<Vec<u8>, Vec<u8>>) {
    let (mut pos, mut bal) = (BTreeMap::new(), BTreeMap::new());
    for i in 0..n {
        let t = trader(i);
        for (j, m) in markets(i, k).into_iter().enumerate() {
            let h = splitmix(i.wrapping_mul(1_000).wrapping_add(j as u64));
            let size = 1 + h % 1_000_000;
            let price = 1_000 + (h >> 20) % 100_000;
            let isolated = (h >> 40).is_multiple_of(10);
            let p = Position {
                trader: t,
                market_id: m,
                is_long: h & 1 == 0,
                size: fp(size),
                entry_price: fp(price),
                cost_basis: fp(size * price),
                realized_pnl: FixedPoint::from_raw(i128::from((h >> 8) % 10_000) - 5_000),
                isolated_margin: if isolated { fp(size * price / 10) } else { FixedPoint::ZERO },
                margin_type: if isolated { MarginType::Isolated } else { MarginType::Cross },
            };
            pos.insert(position_key(&t, m).to_vec(), borsh::to_vec(&p).expect("encode position"));
        }
        let h = splitmix(i ^ 0xBA1A);
        let b = NativeBalance { available: fp(h % 1_000_000_000), order_margin: fp((h >> 32) % 1_000_000) };
        bal.insert(t.to_vec(), borsh::to_vec(&b).expect("encode balance"));
        bal.insert(cum_volume_key(&t).to_vec(), fp(1 + (h >> 16) % 1_000_000_000).raw().to_be_bytes().to_vec());
    }
    (pos, bal)
}

/// Check that `p` holds the users of `synth(n, k)`, all regular.
fn check_regular(p: &UsersProbe, n: u64, k: usize) {
    let n = n as usize;
    assert_eq!(p.regular(), n, "every synthetic user has a record");
    assert_eq!(p.raw_rows(), 0, "no synthetic row is irregular");
    assert_eq!(p.positions(), n * k, "positions on records");
    assert_eq!(p.balances_and_volumes(), (n, n), "balance and cvlm rows on records");
    assert_eq!(p.traders_and_holders(), (n, n * k), "E2 traders and adl C2 holders");
    assert_eq!(p.rows_and_bytes().0, n * (k + 2), "rows held");
}

/// The generator gives regular users (records, no raw row) whose rows the
/// record set re-encodes byte for byte (the cold build loses nothing).
#[test]
fn synthetic_users_are_regular_and_round_trip() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for k in [2, 10] {
        let (pos, bal) = synth(500, k);
        let p = UsersProbe::build(&pos, &bal);
        check_regular(&p, 500, k);
        assert_eq!(p.encode_rows(true), pos, "positions CF round trip (k {k})");
        assert_eq!(p.encode_rows(false), bal, "balances CF round trip (k {k})");
        for i in 0..500 {
            let ms = markets(i, k);
            assert!(ms.windows(2).all(|w| w[0] < w[1]), "user {i}: {k} distinct markets");
            assert!(ms.iter().all(|m| (1..=MARKETS).contains(m)));
        }
    }
}

/// jemalloc's size classes as the counter rounds them.
#[test]
fn jemalloc_size_classes() {
    let classes = [(1, 8), (8, 8), (9, 16), (17, 32), (112, 112), (129, 160), (176, 192), (257, 320), (14336, 14336), (14337, 16384)];
    for (size, class) in classes {
        assert_eq!(jemalloc_bytes(size), class, "size {size}");
    }
}

/// Counting a build: the record set's allocation is at least its positions'
/// and records' bytes, and dropping it frees all of it.
#[test]
fn counter_sees_the_record_set() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (pos, bal) = synth(2000, 10);
    let before = snap();
    let p = UsersProbe::build(&pos, &bal);
    let built = snap() - before;
    assert!(built.req as usize >= 2000 * 10 * 112, "positions alone: {built:?}");
    drop(p);
    let after = snap() - before;
    assert_eq!((after.req, after.je, after.live), (0, 0, 0), "drop frees the record set");
}

// ---------------------------------------------------------------------------
// The ubench
// ---------------------------------------------------------------------------

struct Shape {
    name: &'static str,
    positions: usize,
    /// Section 1's target, bytes per user (jemalloc, with sums).
    target: usize,
}

const SHAPES: [Shape; 2] =
    [Shape { name: "light", positions: 2, target: 600 }, Shape { name: "active", positions: 10, target: 2300 }];

#[test]
#[ignore = "ubench: cargo test --release -p torus-bridge --test ubench_users_rss -- --ignored --nocapture"]
fn ubench_users_rss() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sizes: Vec<u64> = std::env::var("UB_USERS")
        .unwrap_or_else(|_| "100000,1000000".into())
        .split(',')
        .map(|s| s.trim().parse().expect("UB_USERS: comma separated user counts"))
        .collect();
    println!("USERS_RSS bytes per user of the Users record set (orders excluded); allocator: counting over System,");
    println!("USERS_RSS columns: req = asked for, glibc = usable + 8 B header, jemalloc = jemalloc 5 size class (the node's allocator),");
    println!("USERS_RSS +sums = with a sums entry on every record; rss = process RSS delta (informational); logical = key + value bytes of the rows");
    println!(
        "USERS_RSS {:<6} {:>9} {:>4} | {:>7} {:>7} {:>8} {:>7} | {:>12} {:>12} {:>7} | {:>7} {:>7} {:>8} {:>6} {:>5}",
        "shape", "users", "pos", "req", "glibc", "jemalloc", "allocs", "+sums glibc", "+sums jemal", "target", "rss", "+rss", "logical", "map_lf", "pass"
    );
    for n in sizes {
        for s in &SHAPES {
            let k = s.positions;
            let (pos, bal) = synth(n, k);
            let s0 = snap();
            let mut p = UsersProbe::build(&pos, &bal);
            let s1 = snap();
            check_regular(&p, n, k);
            assert_eq!(p.fill_sums(1), n as usize);
            let s2 = snap();
            assert_eq!(p.sums_at(1), n as usize);
            let (b, w) = (s1 - s0, s2 - s0);
            let per = |x: isize| x as f64 / n as f64;
            let logical = p.rows_and_bytes().1 as f64 / n as f64;
            let lf = n as f64 / p.map_capacity() as f64;
            let pass = if per(w.je) <= s.target as f64 { "ok" } else { "OVER" };
            println!(
                "USERS_RSS {:<6} {:>9} {:>4} | {:>7.0} {:>7.0} {:>8.0} {:>7.2} | {:>12.0} {:>12.0} {:>7} | {:>7.0} {:>7.0} {:>8.0} {:>6.2} {:>5}",
                s.name,
                n,
                k,
                per(b.req),
                per(b.glibc),
                per(b.je),
                per(b.live),
                per(w.glibc),
                per(w.je),
                s.target,
                per(b.rss),
                per(w.rss),
                logical,
                lf,
                pass
            );
            // Loose sanity only (not the gate): the positions alone, and at
            // most ~4x the target.
            assert!(per(b.req) >= (k * 112) as f64, "{} {n}: {:.0} B/user below its positions", s.name, per(b.req));
            assert!(per(w.je) <= (4 * s.target) as f64, "{} {n}: {:.0} B/user, over 4x the target", s.name, per(w.je));
            drop(p);
            let freed = snap() - s0;
            assert_eq!(freed.live, 0, "drop frees the record set");
            drop((pos, bal));
        }
    }
}
