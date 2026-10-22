# System map: crypto, serialization, runtime (Torus-hyperBFT main `eb993e3f`, read-only, 2026-10-22)

Scope: `crates/torus-types` (EIP-712, action hashing, recovery), `crates/torus-node` (startup, threads, runtimes,
allocator), wire/storage formats, compression, build profile. All paths are relative to
`/home/oz/projects/torus-economy/Torus-hyperBFT`. "M" = measured (source given), "E" = estimate (formula given).

Baseline numbers used throughout (M, `~/bench-results-matched/ozarchy-9j2-std-r2-CL21`, val1, 303 s load,
Classic CL21, the owner-chosen config; section 124 of `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`):
264.5k matched/s, 22.1 native blk/s, 63 actions/blk, 11.4k fills/blk, block p50/p99 11.4/~475 ms,
node CPU 5.94 cores (user 5.47 + sys 0.47), CPU-s/1M fills 23.2, exec block 37.4 ms, exec busy ~0.88 wall.
Bench signs every action with EIP-712 secp256k1 (`tools/bench-throughput/src/main.rs:136-142`, default
`eip712`), 400 orders per `PlaceOrderBatch` (`tools/matched-bench/run-cell.sh:29,1815`), `--format bin`.

## 1. Architecture and data flow

### 1.1 Crypto primitives in use
| primitive | crate / backend | used for | file:line |
|---|---|---|---|
| Keccak-256 | alloy-primitives `asm-keccak` -> keccak-asm 0.1.6 / sha3-asm (cryptogams scalar x86-64; no AVX dispatch) | EIP-712 hashes, action content hash, trust-cache key, sort hash, state roots, level hashes | `Cargo.toml:66-80` (measured 206->248 MB/s on 18c, comment) |
| Keccak-256 (pure Rust) | `sha3` 0.10.8 / `keccak` 0.1.6 | mode-2 level sponge cache only (not Classic), epoch, EVM bloom | `crates/torus-core/src/order_book.rs:1121,3918`; `torus-economics/src/epoch.rs:4`; `torus-evm/src/bloom.rs:2` |
| secp256k1 ECDSA recover | `k256` 0.13.4 (pure Rust; `ecdsa` 0.16.9 `recover_from_prehash` ends with a full `verify_prehash`) | native-action sender recovery | `crates/torus-types/src/eip712.rs:901-917` |
| secp256k1 (C) | `secp256k1` 0.31.1 / -sys 0.11 (libsecp256k1) | linked via revm-precompile (EVM ecrecover) only; 0.30.0 also via alloy-consensus | `cargo tree -i secp256k1@0.31.1` (revm-precompile 32.1.0) |
| ed25519 | `ed25519-dalek` 2.2 (`batch`), curve25519-dalek 4.1.3 (runtime AVX2 via cpufeatures) | session-key actions (`verify_batch`), HotStuff votes/QCs, libp2p identity, gossipsub envelopes | `eip712.rs:1641`; `crates/hotstuff_rs/src/types/signed_messages.rs:62`; `crates/torus-network/src/behaviour.rs:135` |
| SHA-256 | `sha2` 0.10.9 (cpufeatures SHA-NI) | HotStuff block hash, proposal `data_hash` | `crates/hotstuff_rs/src/types/crypto_primitives.rs:15-16`, `types/block.rs:73-81`; `crates/torus-consensus/src/app.rs:9463-9467` |
| SHA-512 | `sha2` | inside ed25519 sign/verify (gossipsub envelopes sign full payload) | libp2p-gossipsub 0.49.4 |
| QUIC AEAD | rustls 0.23 + ring 0.17 (ChaCha20-Poly1305 first in libp2p-tls 0.6.2) | all p2p transport | libp2p-quic 0.13 |
| Reed-Solomon GF(2^8) | `reed-solomon-erasure` 6.0.0, `simd-accel` NOT enabled (pure Rust tables) despite "SIMD-accelerated" comment | erasure shards (T3.1; owner-excluded) | `Cargo.toml:124-130`, `crates/torus-state/Cargo.toml:25` |

### 1.2 What gets hashed per native action (400-order `PlaceOrderBatch`)
- EIP-712 struct hash: 400 `PlaceOrderItem` hashes (320 B preimage = 3 Keccak-f permutations each), then
  `ordersHash` over 12.8 KB (95 perms), 2 typehash keccaks recomputed per call, signing hash, address
  keccak: ~1,300 perms per action (`eip712.rs:349-420`, `893-898`; perm counts from mem 0a51b36b/4823ab93).
- Domain separator: cached `OnceLock` (`eip712.rs:215-235`) but `recover_sender_for_chain` calls the
  uncached `eip712_domain_separator_for_chain` every recover (`eip712.rs:1028`; ~6 keccak calls).
- Canonical bytes `C` (`lib.rs:1103`, 45 B/order without cloid, `lib.rs:1055-1086`) -> ~133 perms per pass:
  - content hash `keccak(C||nonce||tag||sig)` (`lib.rs:1379-1413`, F-2 binds the signature),
  - trust-cache key `keccak(C||nonce||v||r||s)` (`lib.rs:1433-1459`; differs from content hash only by the
    0x00 tag byte after the common prefix), `None` for session actions,
  - sort hash `keccak(C)` shared-absorbed with the cache key at exec when `|C|>=136` (`lib.rs:1464-1510`).
- RPC ack hash: `keccak(serde_json(action))` even for binary ingress (`crates/torus-rpc/src/torus.rs:624-631`),
  ~64 KB JSON = 470-530 perms per 400-order action (mem 0a51b36b).

### 1.3 Per-action lifecycle on one node (bench shape)
| stage | thread | crypto / encode work | file:line |
|---|---|---|---|
| RPC ingress (`submitNativeActionsBin`) | rpc-worker -> spawn_blocking -> `torus-ingress-verify-*` | hex decode of JSON-RPC string, bincode decode, `validate_known_markets` (RocksDB get per distinct market), price band, struct hash + ecrecover (`validate_with_sessions`), JSON ack serialize + keccak | `torus.rs:291-299, 599-631, 764-890` |
| pool insert | ingress pool thread (mempool write lock) | trust-cache key + content hash | `crates/torus-mempool/src/native_pool.rs:398,414` |
| DA mirror | same | `action.clone()` into pending batch; `put_batch`: content hash + bincode per action | `crates/torus-mempool/src/lib.rs:1994-2020`; `crates/torus-state/src/native_da.rs:536-538` |
| gossip out | same, then swarm task | `bincode((sender, action))` -> raw gossipsub batch (no zstd since s364), envelope ed25519 sign over whole payload on the swarm task | `mempool/src/lib.rs:1460-1482`; `torus-network/src/swarm.rs:820-850` |
| gossip in | QUIC conn task (envelope verify) -> swarm task (bincode decode) -> `torus-gossip-verify-*` | DA mirror (clone + content hash + bincode), cache key, trust-cache lookup, struct hash + ecrecover on miss, pool insert (content hash) | `swarm.rs:1547-1580`; `crates/torus-node/src/main.rs:988-1040`; `mempool/src/lib.rs:1347-1390` |
| forward to leader (D2L, 0xFD) | forward batcher task | `bincode::serialized_size` + `serialize_into` (two walks) | `crates/torus-node/src/forward_batcher.rs:84-100`; `swarm.rs:159-167` |
| proposal build (leader) | hotstuff-algo | deep clone of selected actions + content hash per action; `CompactBlock` bincode; SHA-256 `data_hash` | `app.rs:9624-9645, 7238-7244, 9463` |
| commit | hotstuff-algo | content hash per action again (mempool remove), DA GC pin hashes when fence on | `app.rs:11500-11515, 9838-9850` |
| commit persist | torus-commit-persist | tagged bincode body record (JSON legacy decode only) | `app.rs:716-740`; `crates/torus-state/src/block_body.rs:1-20` |
| exec verify | torus-execution (keys pre-pass serial; phase 1 on global rayon pool only on miss) | sort hash + cache key per action, RwLock FifoCache lookup, session Phase 2 serial (struct hash + ed25519 batch) | `app.rs:5370-5420`; `eip712.rs:1443-1655` |
| wire `/2.0` request-response | swarm/conn tasks | borsh `try_to_vec` + zstd-3 per stream (per peer), decompress bounded by path cap | `crates/torus-network/src/codec.rs:152, 203-285` |

Formats: bincode 1.3.3 default (fixint, u64 lengths, u32 enum tags) for actions on gossip, DA, 0xFD
envelopes, `CompactBlock`/`TorusBlock` datum, body records; borsh 0.10 for hotstuff_rs messages; borsh 1 for
network codec frames and Classic book blobs; serde_json for RPC, `CF_SESSIONS` rows
(`crates/torus-state/src/db.rs:1458-1468`, `backend.rs:163-173`, `read_view.rs:370-378`), block header
records (`app.rs:2108-2110`), legacy body records. CBOR only for `/torus/sync/1.0` (`behaviour.rs:203-206`).

## 2. Threads, pipeline, and what is serial

Process: `#[tokio::main]` multi-thread runtime, workers = host cores (`main.rs:560`); 32 on archy. Startup order
(`main.rs:594-1360`): tracing + fail-stop panic hook (`:596-598`) -> RLIMIT_NOFILE raise (`:603-609`, `:521-555`)
-> key load -> `open_node_dbs` (`:715`) -> genesis binding -> `set_network_chain_id` (`:806`) -> components
(mempool, network `:946`, app) -> gossip-verify pool + inbound task (`:988-1040`) -> pre-proposal push thread
(`:1062`) -> replica start (`:1275-1356`) -> RPC runtime (`:1474-1490`) -> metrics, pruner, stats -> signal wait.

| thread / pool | count on a 32-thread host | role | serial? | file:line |
|---|---|---|---|---|
| tokio main workers (+ blocking pool) | 32 (only 11-12 ever run, mem db0cb68d) | libp2p swarm task (single task: behaviour, publish sign, batch decode), gossip inbound loop, misc tasks | swarm event loop is ONE task | `main.rs:560`; `torus-network/src/bridge.rs:333-344` |
| rpc-runtime + rpc-worker | 1 + 4 (+ own blocking pool) | jsonrpsee; each submit = spawn_blocking that blocks on `ingress_verify_pool().install` | no | `main.rs:1474-1490`; `torus.rs:860-890` |
| torus-ingress-verify-* | max(2, host/2) = 16 | RPC verify (rayon) | per batch parallel | `torus.rs:668-683` |
| torus-gossip-verify-* | max(2, host/2) = 16 | gossip ingest verify, 1 rayon job per action, inline on tokio task past 1024 in flight | no (fallback serial) | `main.rs:996-1032` |
| rayon global pool | 32 (lazy) | exec phase-1 recover on trust-cache miss (`VerifyMode::Default`), optional exec key hashing | n/a | `eip712.rs:1188-1318` |
| verify-* | only if `TORUS_PARALLEL_VERIFY=N>=2` | dedicated exec verify pool | n/a | `eip712.rs:1251-1263` |
| torus-exec-pool-* | max(host, TORUS_MATCH_WORKERS, TORUS_SETTLE_WORKERS) = 32 | match + settle pass A jobs (static LPT chunks) | phases fork/join from exec thread | `crates/torus-bridge/src/exec_pool.rs:20-58` |
| hotstuff-algo | 1 | consensus: propose/build, validate, vote, commit feed | YES (all consensus) | `crates/hotstuff_rs/src/algorithm.rs:150-156` |
| torus-execution | 1 | executes committed blocks (64-deep `sync_channel`) | YES (the throughput bottleneck) | `app.rs:8327-8336` |
| torus-flush-worker | 1 | state DB writes, checkpoint drain | YES | `crates/torus-consensus/src/exec_pipeline.rs:829-831` |
| torus-commit-persist | 1 | durable committed-block persist | YES | `app.rs:716-740` |
| others (long-lived) | ~10 | hotstuff-syncsv, hotstuff-bodyserve, torus-da-gc, torus-da-recovery, torus-end-resident, torus-manifest-prune, torus-ban-writer, bg_writer, torus-async-validate (flag), pre-proposal push | each serial | `main.rs:2061-2083` list; `crates/torus-state/src/bg_writer.rs:225` |
| per-event | per checkpoint (15 s) | torus-ckpt-merge, torus-ckpt-drop | | `app.rs:436-437, 493` |
| RocksDB bg | 4 jobs x 2 DBs | flush/compaction (Lz4 + bottommost Zstd) | | `crates/torus-state/src/db.rs:792, 844-845` |

Per-block scoped spawns on the exec path: all `torus_exec_thread_spawns_*` counters are 0 during load except
`end_resident 1` (M, 9j2 std r2 val1 metrics; sites in `crates/torus-state/src/spawn_count.rs:12-33`).
Bench host: 3 nodes + client on 32 HW threads, ~140 threads per node; load1 avg 35.6, max 52.4 (M, 9j2 summary).

Serial points relevant to this map: (a) exec thread's verify key pre-pass (keccak per action, no pool unless
`TORUS_EXEC_PARALLEL_KEYS=1`), (b) exec Phase 2 for session actions (struct hash + ed25519 batch, serial),
(c) hotstuff-algo content hashing at build/commit, (d) the single swarm task (publish sign, batch decode),
(e) mempool write lock around pool insert (gossip/ingress threads park on it, mem 12d0cc8f).

## 3. Hot paths with cost

| path | cost | per-what | source |
|---|---|---|---|
| exec verify (keys pre-pass + cache lookups; phase 1 skipped on all-hit) | 2.57 ms/blk mean = 6.8% of 37.4 ms exec block; 71.5% of blocks <1 ms, 95.3% <16 ms, max <64 ms | exec thread, critical path | M: `torus_exec_verify_seconds` 18.28 s / 7,117 blk (9j2 std r2 val1) |
| exec keys pre-pass keccak | ~63 x 134 perms x ~1,200 cyc ~= 2.6 ms/blk | matches the timer above | E: perms mem 0a51b36b, cryptogams Ryzen 8.8 c/b |
| trust cache | hits 476,069, misses 294,104 (= gossip received 293,926); exec misses ~0 | per 303 s | M: 9j2 metrics |
| ecrecover volume | 487/s RPC + 970/s gossip = ~1,457 recoveries/s/node (the 1-per-action-per-node floor) | node | M: 147,691 submits, 293,926 gossip received / 303 s |
| k256 recover CPU | ~310-350 us each under load; inside recover: trailing `verify_prehash` ~48%, lincomb ~39% | per recover | M-derived, s124 (main a44000e0, mem 93fd4bf0/4864a390) |
| recover total | 0.41-0.43 cores/node at 1,179/s (s124); scaled to 1,457/s ~0.5 cores | node | M (s124) / E (scaled) |
| EIP-712 struct hash | ~1,300 perms ~= 0.4 ms per 400-order action; ~0.4-0.6 cores/node at 1,457/s | node | E (perm count x c/perm); s124 M: keccak 53.5% of gossip-verify, 33% of ingress-verify |
| verify pools total | gossip-verify 0.94-0.97, ingress-verify 0.76-0.84 cores/node (of 5.1-5.65) | node | M, s124 hsdb-300m io-threads (older main; eip712 place-order path unchanged since) |
| ingress-verify split | keccak 33%, validate_known_markets 28%, rayon idle 20.6%, recovery 17.6%, hex decode 6.6-7%, JSON ack serde 4.5-4.7% | ingress pool | M, s124/s123 (mem 4864a390, 5fa915a3) |
| gossip-verify split | keccak/struct hash 53.5%, recovery 32.4%, mirror_to_da 6.8%, rayon idle 3.6% | gossip pool | M, mem 4864a390 |
| other per-action keccak passes (cache key, pool content hash, DA content hash, build hash, commit hash) | ~5 x 133 perms ~= 0.2 ms/action/node -> ~0.3 cores/node, spread over 4 threads | node | E |
| RPC submit verify wall | mean 6.6 ms (`..._cpu_seconds`, which is wall inside spawn_blocking) / 11.6 ms (`..._seconds`) per submit | per action | M: 980 s and 1,710 s / 147,691 (metric semantics mem 4864a390) |
| hotstuff-algo | 11.7 ms on-CPU + 1.8 ms runqueue wait per committed block; block_build 9.3-9.5, validate 2.2-2.4, on_committed 5.4-5.8, mempool_rm 1.6-1.9, commit_persist 4.6 ms | per block | M: 9j2 `run.log` CONSENSUS lines + schedstat |
| content hashing on hotstuff-algo | ~63 x 133 perms ~= 2.5 ms per pass (build, commit) | per block | E; s123 measured hotstuff Keccak 0.07-0.09 cores at older main (mem 93695d41) |
| torus-execution | 31.6 ms on-CPU + 2.97 ms runqueue wait per committed block | per block | M: 9j2 schedstat |
| tokio workers | 0.435 cores/node: QUIC non-crypto 0.088, scheduler 0.072, gossipsub envelope verify 0.059, QUIC AEAD 0.037, envelope sign 0.033 (~40% of swarm-task busy), batch bincode decode 0.030, zstd 0.020 | node | M, s124 r3 (mem db0cb68d) |
| SHA-512 (ed25519 envelopes) | 1.9% of node samples | node | M, mem db0cb68d |
| wire compression | direct 133.4 MB -> 32.9 MB (4.05x); native-da 358.9 MB -> 29.4 MB (12.2x) per 303 s | val1 | M: `torus_wire_compression_bytes` |
| gossip bytes | ~4.7 MB/s/node published raw, >94% of QUIC bytes | node | M, mem db0cb68d |
| jemalloc vs glibc | SAT matched/s tie, blk/s +14.9%, p99 -7.2%, CPU-s/1M +2.66%, RSS growth -44% | A/B | M, section 56 (commit 655e874a) |
| Rust alloc shim | 0.39-0.42% of exec / pool samples | thread | M, mem b5a9511e |
| FixedPoint i128 `p / SCALE` | `__divti3` libcall: exec 1.31%, pool 2.59% | thread | M, mem 4a421c03; u64 fast path useless (products >= 2^64), mem 81ee744a |

Crypto+hash share of node CPU: ~1.2-1.4 of 5.94 cores (~20-24%), almost all off the exec critical path
(E: sum of the rows above; s124 M: Keccak = #1 node function, 24.4% of node cycles at 10m).

## 4. Knobs and current defaults

| knob | default | effect | file:line |
|---|---|---|---|
| `--exec-trust-cache` | true | exec reuses ingress/gossip-verified senders | `main.rs:195-207` |
| `TORUS_VERIFIED_SENDER_CACHE_CAP` | max(16,384, 64 x block cap x 5/2) = 32,000 at cap 200 | FIFO trust cache size | `crates/torus-mempool/src/rate_limit.rs:349-372` |
| `EXEC_QUEUE_DEPTH` (const) | 64 | exec channel depth; cache floor | `rate_limit.rs:355`; `app.rs:8331` |
| `TORUS_PARALLEL_VERIFY` | unset = global rayon pool; `0/1` serial; `N>=2` dedicated pool | exec phase-1 recover | `eip712.rs:1188-1263` |
| `TORUS_EXEC_PARALLEL_KEYS` | off; only for >=16 actions | exec keys pre-pass on the pool | `eip712.rs:1222-1249` |
| gossip / ingress verify pool sizes | max(2, host/2), no env | ingest parallelism | `main.rs:996-1006`; `torus.rs:668-683` |
| `GOSSIP_VERIFY_MAX_INFLIGHT` (const) | 1024 | past it, verify inline on the tokio task | `main.rs:1010-1028` |
| exec pool size | max(host, `TORUS_MATCH_WORKERS`, `TORUS_SETTLE_WORKERS`) | match/settle pool | `exec_pool.rs:20-36` |
| tokio main / RPC workers | host cores (`#[tokio::main]`) / 4 (const) | runtimes | `main.rs:560, 1477-1478` |
| `RAYON_NUM_THREADS` | unset (= host) | global rayon pool | rayon-core 1.13 |
| allocator | feature `jemalloc` default on (5.3.1, unprefixed, also serves RocksDB C++); `MALLOC_CONF` unset in benches | heap | `crates/torus-node/Cargo.toml` `[features]`; `main.rs:37-42`; `allocator.rs:1-60` |
| `ZSTD_WIRE_LEVEL` (const) | 3 on `/torus/{direct,block-data,native-da}/2.0` | wire compression | `codec.rs:152`; `behaviour.rs:140-185` |
| gossipsub | Strict validation, Signed authenticity, queue 512 (`TORUS_GOSSIP_QUEUE_LEN`), raw payloads | gossip | `behaviour.rs:75-97, 135` |
| `TORUS_D2L_BATCH`, `_MS`, `_MAX_BYTES`, `_REFORWARD_MS` | 1, 25 ms, 512 KiB, 2000 ms | leader forward batching | `forward_batcher.rs:13-24` |
| RocksDB codecs | Lz4, bottommost Zstd; `TORUS_ROCKSDB_WAL_COMPRESSION` off; `TORUS_ROCKSDB_APPEND_CF_CODEC` today; bg jobs 4 | storage CPU | `db.rs:415-427, 775-799, 844-845, 896-907` |
| `COMPACT_PROPOSALS` (const) | true | hash-only proposals | `app.rs:7225` |
| `TORUS_LEVEL_HASH_CACHE` | 256 MB, mode 2 only (Classic unaffected) | level sponge cache | `crates/torus-bridge/src/native_executor.rs:3289-3330, 3605-3620` |
| `TORUS_PARALLEL_BUCKET_HASH` | 8 threads (scoped) | trie bucket rehash | `crates/torus-state/src/native_trie.rs:683, 1095-1120` |
| `HOTSTUFF_CPUS` | unset (harness-only taskset of hotstuff-algo) | pinning | `tools/matched-bench/run-cell.sh:183-190, 1578-1640` |
| Cargo profile | no `[profile.release]` anywhere (opt 3, cgu 16, thin-local LTO, panic=unwind, x86-64 baseline target); `.cargo/config.toml`: clang + mold, dev line-tables-only; bench RUSTFLAGS add `force-frame-pointers=yes` + release line-tables (mem 4a421c03) | codegen | `Cargo.toml` (150 lines, none); `.cargo/config.toml` |

## 5. Known limits and failure modes

- Exec-bound: exec ~0.88 busy (wall) at 22 blk/s. Crypto touches the exec critical path only via the keys
  pre-pass (2.57 ms/blk) and, for session-signed actions, Phase 2. Crypto-pool savings reach matched/s only
  through CPU contention (s124 estimates: libsecp ~0 matched/s, keccak-x4 ~+0.2%).
- Shared global rayon pool starvation (s352: block 529 -> 4,034 ms) is why ingress/gossip have dedicated pools
  (`torus.rs:659-667`); the exec still uses the global pool for phase 1 (`VerifyMode::Default`).
- Gossip flood: past 1,024 queued verifies the inbound task verifies inline on a tokio worker (`main.rs:1023-1028`),
  serializing ingest at one ecrecover at a time.
- Trust-cache determinism rules: key commits to the full signature; session actions are never cached
  (`lib.rs:1415-1438`); a cache miss at exec costs a full recover on the critical path. Any faster ECDSA
  backend must add an explicit low-S check (k256 verify rejects high-S, libsecp recover does not;
  consensus-critical via ingress cache seeding; mem 85aaf758).
- Signature gap: EIP-712 `PlaceOrder`/`PlaceOrderItem` encode only the order-type tag, not stop
  `trigger`/`limit` (`eip712.rs:349-365` vs canonical `lib.rs:1060-1070`), so a relay can alter stop prices
  under a valid signature (mem 293ded97, s124, "not yet confirmed by 18c"; still present on eb993e3f). Fix =
  typehash change = fresh genesis + client signer update.
- Allocator: jemalloc retains RSS (+0.9..1.8 GB vs live heap, sections 57, 61-62); `background_thread` +
  `dirty_decay_ms:1000` cut RSS 1.72 GiB/node but cost CPU-s/1M +4.6%, matched/s -2.5% (section 62);
  section 73: decay costs throughput.
- `panic = "abort"` is not an option: `exec_pool::run_jobs` and `panic_scope::catch_recoverable` rely on
  unwinding (`exec_pool.rs:42-58`); fail-stop list `main.rs:2061-2083`.
- Build prerequisites: `sha3-asm` needs perl + C compiler and panics on unsupported arches (`Cargo.toml:70-78`).
- Bench topology: 3 validators share one 32-thread host, every pool is sized to the host; runqueue wait is
  15.7% of hotstuff-algo on-CPU and 9.4% of exec on-CPU (M, 9j2 schedstat). Production (one validator per
  host) sees ~3x less oversubscription, so thread-sizing results may not transfer.
- Wire: gossip ships raw because gossipsub cannot negotiate per peer (zstd topic removed s364, DoS);
  `/2.0` zstd is negotiated per peer and stays mixed-binary safe.

## 6. Corners that look unexamined (observations, not proposals)

Method: memory `search_knowledge` (project torus-hyperbft) + git log + ledger branches; "no hit" means no
memory or branch found for the specific angle.

1. Session-signed regime is unmeasured and serial at exec. The bench never signs sessions in current cells.
   For session actions the exec thread runs, per action per block: `get_session` + `serde_json` decode of
   `SessionData` (`db.rs:1463`, `read_view.rs:374`), the full EIP-712 struct hash (~1,300 perms per
   400-order batch) and the ed25519 batch (`eip712.rs:1600-1650`), with nothing cached (`lib.rs:1433-1438`).
   E: 63 session actions/blk x ~0.45 ms ~= 28 ms/blk on a 37 ms exec block. The signature-validity part
   (pubkey, signing hash, sig) is a pure function; only existence/expiry/scope is state-dependent, yet the
   two are coupled. The July session-owner cache (mem d472420e, perf/p3-throughput) memoized only the DB read
   and is not on main (`app.rs:5385-5398` reads `overlay.get_session`).
2. Five-plus keccak passes over the same canonical bytes per action per node (cache key, pool content hash,
   DA content hash, proposal-build hash, commit hash; sort+cache share one absorb only at exec). The content
   hash and the cache key share the `C||nonce` prefix and differ by one tag byte (`lib.rs:1388-1412` vs
   `1443-1459`); a shared absorb exists for sort+cache (`lib.rs:1482-1510`) but not for content+cache at
   ingress/gossip. s123 rejected the consensus-side reuse on CPU grounds (mem 93695d41); the ingress/gossip
   pair and the DA re-hash (pool already stores `action_hash`, mem b3427219) have no candidate.
3. Re-encode churn at ingress. Binary ingress arrives as hex text in JSON-RPC (hex decode 6.6-7% of
   ingress-verify), is bincode-decoded, then re-serialized as JSON (ack hash, `torus.rs:629`), bincode for
   gossip (`mempool/src/lib.rs:1468`), bincode for DA (`native_da.rs:538`) and bincode again in 0xFD
   envelopes after a separate `serialized_size` walk (`swarm.rs:163-166`, `forward_batcher.rs:93`). bincode 1
   is deterministic, so the client's bytes equal `bincode::serialize(action)`; they are dropped after decode.
   No memory hit on reusing them. The JSON ack keccak is client-visible API (owner/API call).
4. jemalloc tuning beyond decay: `MALLOC_CONF` has only been tried for decay/background_thread. No memory
   hits for `narenas`/`percpu_arena`, `tcache_max`, `thp`/`metadata_thp`, `oversize_threshold`. Per-action
   allocations are large: a 400-order `Vec<PlaceOrderParams>` (~40-50 KB in memory, E) is above jemalloc
   5.3's default tcache size limit (32 KiB), and it is cloned on every DA mirror (`mempool/src/lib.rs:1997`)
   and proposal build (`app.rs:9636-9637`). These are env-only A/Bs (no code).
5. Pool sizing below host size, but not the aggressive version: `TORUS_CORE_BUDGET=5` lost -67.7%
   (`docs/perf/matched-200k-campaign-2026-08-18.md:132`), whole-node pinning -5.7% (s75), bulk nice refuted
   (mem 12d0cc8f). Still untried: right-sizing only the idle-heavy pools (tokio main 32 workers with 11-12
   active; global rayon 32 nearly idle; exec pool 32 fixed by host) or pinning just torus-execution (only
   `HOTSTUFF_CPUS` exists). Caveat: bench-only oversubscription (section 5).
6. Exec keys pre-pass: `TORUS_EXEC_PARALLEL_KEYS` is on main, default off, never benched (s123 built "code
   only", mem daa76a69); 2.57 ms/blk is on the exec critical path today. Competes with the s124
   "exec-carried-verified-senders" KEEP (mem 5a8a9cc5), which predates the 63-action block shape (it modelled
   16.8 ms/blk at 400 actions/blk).
7. Build flags other than LTO/opt-level: no `target-cpu` (x86-64 baseline: no BMI2/ADX/AVX2 codegen in
   k256 field math, i128 FixedPoint, hashbrown, bincode), no PGO/BOLT (llvm-profdata 22.1.8 matches rustc's
   LLVM, mem 4a421c03). Adjacent to the parked "opt-level A/B" item, which the owner may count as the same
   thing.
8. EIP-712 format: a 400-order batch costs ~1,300 perms because each item gets its own 320-byte struct
   hash, but `ordersHash` is an opaque `bytes32` (`eip712.rs:401-420`), so wallets cannot display the items
   anyway. Hashing `C` once would be ~133 perms (~9x fewer). This changes the signing format (fresh genesis,
   client signer, the trading-app signer is already parked as outdated). s65 "PlaceOrderBatch v2 signing
   format" was deprioritised, never refuted (mem 4823ab93). Could be bundled with the stop-price fix
   (section 5). Owner call.
9. zstd per stream: `write_length_prefixed_borsh_zstd` copies the payload (`try_to_vec`) and compresses
   once per peer (`codec.rs:261-285`); `DirectPayload::Shared` (`codec.rs:10-14`) shares only the raw Vec.
   Cheap at n=3 (zstd 0.020 cores/node), but it grows linearly with validator count (post-mainnet N=11).
10. Small items: the domain separator is recomputed on every recover (`eip712.rs:1028`; ~0.5% of a batch,
    larger share for single-order actions, still far below ecrecover); typehashes are re-keccaked per call
    (`eip712.rs:368-372, 402-407`); `CF_SESSIONS` and header records are JSON (`app.rs:2108-2110`);
    reed-solomon runs without `simd-accel` (owner-excluded area); the mode-2 level sponge uses pure-Rust
    `sha3` instead of keccak-asm (`order_book.rs:1121, 3918`; Classic unaffected).

Already examined; do not re-propose as new (status in brackets):
- libsecp256k1 recover backend + k256 no-trailing-verify arm [s124 KEEP, CPU-s/1M -5..-6%, matched/s ~0,
  unbuilt; mem 4864a390, 85aaf758].
- AVX2 4-lane Keccak for batch items [s124 KEEP, CPU-s/1M -4..-5%, unbuilt; mem 0a51b36b, 4823ab93].
- Fat LTO + cgu=1 [s124 KEEP with gain refuted, ~+1.4% matched/s; mem b5a9511e]. opt-level A/B [owner-parked].
- RocksDB HW CRC32C via CXXFLAGS [s124 KEEP, -0.8..-1.0% CPU-s/1M; mem 77748bc9, 4a421c03].
- Ingress market-spec memo [s124 r1; mem db71a3ab, c6513199]. Exec-carried verified senders [s124 KEEP; mem 5a8a9cc5].
- Native pre-spread over direct + zstd; QUIC AES-GCM [s124 r3 KEEP / dropped; mem db0cb68d, 8b1039de].
- Consensus reuse of action hashes [s123 rejected, CPU only; mem 93695d41]. Typehash hoist, domain-separator
  cache, shared absorb [done: s77, section 92 step 8e].
- FixedPoint `__divti3` u64 fast path / chunked division [s104 dropped; mem 81ee744a]. foldhash in exec maps [done P2-5].
- jemalloc adoption and decay [done / rejected; sections 56-62, 73]. CPU pinning [s75 -5.7%]. Core budget
  [-67.7%]. Bulk nice [refuted]. Verify-pool parking executor [s124 r3, rank 27].
- Owner-excluded: erasure/shard custody (T3.1), push-manifest threshold, running state hash / batch 10,
  T5.3 p99 attribution, PE over WAN, Block-STM (post-testnet).
