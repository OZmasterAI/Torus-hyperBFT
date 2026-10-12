// spawnbench: cost of std::thread::scope spawn+join (the node's per-block idiom) vs a persistent pool, on this host.
// Usage: spawnbench <threads_per_scope> <scopes> <work_us> [bg_threads]
//   threads_per_scope: workers spawned per scope (like one parallel match/settle/drain step)
//   work_us:           busy work per worker (0 = empty closure: pure spawn/join overhead)
//   bg_threads:        idle background threads parked in the process (the node has ~100+), default 0
// Prints per spawned thread: process CPU (user+sys, getrusage), caller wall in spawn() calls, wall of the whole scope minus
// work, and the same for a persistent pool (channel + condvar-free: one mpsc per worker, a shared done-counter).
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

fn rusage() -> (f64, f64) {
    let mut ru: libc_rusage = unsafe { std::mem::zeroed() };
    unsafe { getrusage(0, &mut ru) };
    let t = |tv: Timeval| tv.sec as f64 + tv.usec as f64 * 1e-6;
    (t(ru.utime), t(ru.stime))
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Timeval {
    sec: i64,
    usec: i64,
}
#[repr(C)]
struct libc_rusage {
    utime: Timeval,
    stime: Timeval,
    rest: [i64; 14],
}
extern "C" {
    fn getrusage(who: i32, usage: *mut libc_rusage) -> i32;
}

fn spin(us: u64) {
    if us == 0 {
        return;
    }
    let t = Instant::now();
    let d = Duration::from_micros(us);
    while t.elapsed() < d {
        std::hint::spin_loop();
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let k: usize = a[1].parse().unwrap();
    let n: usize = a[2].parse().unwrap();
    let w: u64 = a[3].parse().unwrap();
    let bg: usize = a.get(4).map(|s| s.parse().unwrap()).unwrap_or(0);
    let (_bgtx, bgrx) = mpsc::channel::<()>();
    let bgrx = Arc::new(std::sync::Mutex::new(bgrx));
    for _ in 0..bg {
        let r = bgrx.clone();
        std::thread::spawn(move || {
            let _ = r.lock().unwrap().recv();
        });
    }
    // touch a big heap so the mm has many VMAs/pages, as the node's (~4 GB RSS) does
    let mut ballast: Vec<Vec<u8>> = Vec::new();
    for _ in 0..256 {
        ballast.push(vec![1u8; 4 << 20]);
    }
    // ---- scoped spawn ----
    let (u0, s0) = rusage();
    let t0 = Instant::now();
    let mut spawn_wall = Duration::ZERO;
    for _ in 0..n {
        std::thread::scope(|s| {
            let mut hs = Vec::with_capacity(k);
            for _ in 0..k {
                let ts = Instant::now();
                hs.push(s.spawn(move || spin(w)));
                spawn_wall += ts.elapsed();
            }
            for h in hs {
                h.join().unwrap();
            }
        });
    }
    let wall = t0.elapsed().as_secs_f64();
    let (u1, s1) = rusage();
    let tot = (n * k) as f64;
    let work = (n * k) as f64 * w as f64 * 1e-6;
    println!(
        "scope k={k} n={n} work_us={w} bg={bg}: per thread cpu_us={:.2} (user {:.2} sys {:.2}) excl work; caller spawn() wall_us={:.2}; scope wall per scope ms={:.4} (ideal work {:.4})",
        ((u1 - u0) + (s1 - s0) - work) / tot * 1e6,
        (u1 - u0 - work) / tot * 1e6,
        (s1 - s0) / tot * 1e6,
        spawn_wall.as_secs_f64() / tot * 1e6,
        wall / n as f64 * 1e3,
        w as f64 * 1e-3
    );
    // ---- persistent pool ----
    let done = Arc::new(AtomicUsize::new(0));
    let mut txs = Vec::new();
    let mut hs = Vec::new();
    for _ in 0..k {
        let (tx, rx) = mpsc::channel::<(u64, std::thread::Thread)>();
        let d = done.clone();
        txs.push(tx);
        hs.push(std::thread::spawn(move || {
            while let Ok((w, t)) = rx.recv() {
                spin(w);
                d.fetch_add(1, Ordering::AcqRel);
                t.unpark();
            }
        }));
    }
    let (u0, s0) = rusage();
    let t0 = Instant::now();
    for i in 0..n {
        for tx in &txs {
            tx.send((w, std::thread::current())).unwrap();
        }
        while done.load(Ordering::Acquire) < (i + 1) * k {
            std::thread::park();
        }
    }
    let wall = t0.elapsed().as_secs_f64();
    let (u1, s1) = rusage();
    println!(
        "pool  k={k} n={n} work_us={w} bg={bg}: per task cpu_us={:.2} (user {:.2} sys {:.2}) excl work; round wall per round ms={:.4}",
        ((u1 - u0) + (s1 - s0) - work) / tot * 1e6,
        (u1 - u0 - work) / tot * 1e6,
        (s1 - s0) / tot * 1e6,
        wall / n as f64 * 1e3
    );
    drop(txs);
    for h in hs {
        h.join().unwrap();
    }
    std::hint::black_box(&ballast);
}
