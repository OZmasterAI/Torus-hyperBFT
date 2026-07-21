# Bench & throughput handoff (freeshapka)

Consolidated bench contributions on one branch, as requested — do whatever you want with it.

## crates/torus-state/benches/erasure.rs
Criterion micro-bench for the erasure codec: encode / reconstruct (k-of-n worst case) /
verify_shard×k across body sizes 4KB–4MB. Fleet-free, deterministic. Adds a `criterion`
dev-dep + `[[bench]]` entry to crates/torus-state/Cargo.toml — no src changes.
Run: `cargo bench -p torus-state --bench erasure`.

## bench/loopback-throughput/
Isolated 3-validator loopback throughput study (WAN-free, to isolate the execution/commit ceiling).
- FINDINGS.md — node-sourced scoreboard + per-phase timing + the off-thread-commit recommendation
- run-bench2.sh — Prometheus-counter sampler (reproduce the node-sourced numbers)
- gen-multimarket-genesis.py — multi-market genesis generator
Headline: ~90.6k matched fills/s sustained on a 3-val loopback; the wall is the exec-thread serial
commit (state root already optimal ~5.3ms). Same copy is on torus-hyperbft-data @ bench/loopback-throughput-findings.

## bench/native-orders-grid-20260713/
Earlier native-orders throughput grid (baseline raw results — markets×block-cap×batch sweep).

Note: no core-node / consensus code was changed here — this is benches + measurement harness +
config-tuning findings. The one code lever identified (off-thread state commit) is described in
FINDINGS.md, not implemented (it's consensus-validated → a protocol-level change, your call).
