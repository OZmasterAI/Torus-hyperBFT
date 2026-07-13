// Criterion micro-bench for the erasure codec (crates/torus-state/src/erasure.rs).
// Isolates pure CPU cost of encode / reconstruct(k-of-n worst case) / verify_shard x k,
// swept over body size. Fleet/network-free, deterministic. For the 9a0806a regression check.
use std::time::Duration;
use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use torus_state::erasure::{encode, reconstruct, verify_shard, ErasureParams};

fn make_body(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    let mut x: u32 = 0x9e3779b9;
    for b in v.iter_mut() {
        x ^= x << 13; x ^= x >> 17; x ^= x << 5;
        *b = (x & 0xff) as u8;
    }
    v
}

const SIZES: &[(usize, &str)] = &[
    (4 * 1024, "4KB"), (64 * 1024, "64KB"), (512 * 1024, "512KB"), (4 * 1024 * 1024, "4MB"),
];

fn bench_encode(c: &mut Criterion) {
    let params = ErasureParams::for_validator_set(3); // k=2, n=3
    let mut g = c.benchmark_group("erasure_encode");
    for &(sz, label) in SIZES {
        let body = make_body(sz);
        g.throughput(Throughput::Bytes(sz as u64));
        g.bench_with_input(BenchmarkId::from_parameter(label), &body, |bch, body| {
            bch.iter(|| { let enc = encode(black_box(body), params).expect("encode"); black_box(enc); });
        });
    }
    g.finish();
}

fn bench_reconstruct(c: &mut Criterion) {
    let params = ErasureParams::for_validator_set(3);
    let mut g = c.benchmark_group("erasure_reconstruct_k_of_n");
    for &(sz, label) in SIZES {
        let body = make_body(sz);
        let enc = encode(&body, params).expect("encode");
        g.throughput(Throughput::Bytes(sz as u64));
        g.bench_with_input(BenchmarkId::from_parameter(label), &enc, |bch, enc| {
            bch.iter_batched(
                || {
                    let mut sparse: Vec<Option<Vec<u8>>> = vec![None; enc.params.n];
                    for i in (enc.params.n - enc.params.k)..enc.params.n { sparse[i] = Some(enc.shards[i].clone()); }
                    sparse
                },
                |sparse| { let out = reconstruct(black_box(sparse), enc.params, enc.body_len).expect("reconstruct"); black_box(out); },
                BatchSize::SmallInput,
            );
        });
    }
    g.finish();
}

fn bench_verify_shard(c: &mut Criterion) {
    let params = ErasureParams::for_validator_set(3);
    let mut g = c.benchmark_group("erasure_verify_shard_x_k");
    for &(sz, label) in SIZES {
        let body = make_body(sz);
        let enc = encode(&body, params).expect("encode");
        let proofs: Vec<_> = (0..enc.params.k).map(|i| enc.proof(i)).collect();
        g.throughput(Throughput::Bytes(sz as u64));
        g.bench_with_input(BenchmarkId::from_parameter(label), &enc, |bch, enc| {
            bch.iter(|| { for i in 0..enc.params.k { let ok = verify_shard(enc.erasure_root, i, &enc.shards[i], &proofs[i]); black_box(ok); } });
        });
    }
    g.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(3));
    targets = bench_encode, bench_reconstruct, bench_verify_shard
}
criterion_main!(benches);
