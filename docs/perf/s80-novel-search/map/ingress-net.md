# System map: ingress-net (RPC submit, mempool, network, native-action DA)

Repo: `/home/oz/projects/torus-economy/Torus-hyperBFT` @ `eb993e3f` (read-only). Paths are relative to it.
Bench context: 3 validators on one host (loopback QUIC), `--native-gossip=true` (tools/matched-bench/test_harness.py:680),
the bench submits `torus_submitNativeActionsBin` (tools/matched-bench/run-cell.sh:1814-1816), BLOCK_CAP 400,
`TORUS_HASH_ONLY_PUSH_THRESHOLD=524288` (devnet/wsl/env.sh:48).
Current standard shape (docs/perf/ozarchy-summary-2026-10-13.md): ~190-215k matched/s, 2.6-4.6 native blk/s,
block-time p99 1.0-1.5 s, node CPU 23-30 CPU-s per 1M fills. Per action ~239 orders, ~10.8 KB canonical bytes,
~13.2 KB bincode on the wire (8k-explore-ideas L4.2/L6.3, 8k-explore-survey exec Keccak row). Estimate: ~1k
actions/s chain-wide (200k matched/s / ~190-210 fills per processed action, ozarchy-antispam doc §53c).

## 1. Architecture and data flow (one order, client to block)

Default mode = gossip pre-spread (`--native-gossip=true`, crates/torus-node/src/main.rs:181-194, :949). The
direct-to-leader (d2l) body forward is armed only with `--native-gossip=false` (main.rs:1451-1457
`forward_bodies = !cli.native_gossip`; ForwardBatcher main.rs:1508-1520). EVM txs always unicast to the leaders of
the next EVM height(s) (torus.rs:1049-1080).

### 1a. Ingress node (any validator or rpc-only node)
1. HTTP POST (jsonrpsee/hyper) on the dedicated `rpc-runtime` thread, 4 async `rpc-worker` threads
   (main.rs:1474-1497; server crates/torus-rpc/src/lib.rs:503-579; `max_connections` 512, lib.rs:55). Middleware:
   MetricsLayer + IpLimitLayer (lib.rs:507-515). Body = JSON array of hex strings (bincode payload hex-encoded,
   ~2x bytes), deserialized into `Vec<String>` (copy 1).
2. `run_submit_pipeline` (crates/torus-rpc/src/torus.rs:764-990):
   - batch cap 100 (`SUBMIT_BATCH_MAX`, lib.rs:39); permit from `Semaphore(64)`, 250 ms wait (lib.rs:30-34, 91-99;
     torus.rs:781).
   - pre-verify shed (torus.rs:802-844), in `spawn_blocking`: pool full -> shed all but oracle submissions; admission
     backlog (pooled normal >= max(10 s commit rate x 20 s, 4 x cap), mempool lib.rs:410-457) -> shed non-cancels.
     The bincode tag peek avoids a decode (torus.rs:321-327, 332).
   - verify (torus.rs:860-889): `spawn_blocking` -> `ingress_verify_pool()` (rayon, cores/2 min 2,
     torus.rs:661-676), `par_iter` over the batch. Per item `verify_one_action_with` (torus.rs:599-631): hex decode
     (copy 2, `parse_bytes` types.rs:90, for the bin format too) -> bincode decode (copy 3) -> `validate_batch_size`
     -> `validate_known_markets` (2 point reads per distinct market, torus.rs:393-466) -> `check_price_band`
     (fee-config row + oracle mark per market, torus.rs:536-563) -> `validate_with_sessions` = chain id + nonce window
     + EIP-712 struct hash over every order + secp256k1 ecrecover (or ed25519 + session row)
     (crates/torus-types/src/eip712.rs:1144-1156, 1078-1126) -> serde_json re-encode + keccak for the ACK hash only
     (torus.rs:625-630). All state reads go through `Mempool::ingress_state()`, the exec thread's published ReadView
     (mempool lib.rs:352-364).
   - admit loop, back on the async worker, serial per batch (torus.rs:900-990): `check_addr_rate`
     (lib.rs:380-405) -> forward clone (d2l only, torus.rs:936) -> `Mempool::add_native_action_presigned`.
3. `add_native_action_presigned` (crates/torus-mempool/src/lib.rs:1302-1328): nonce window (60 s, eip712.rs:49) ->
   `action.clone()` (copy 4) -> `submit_native_action` -> `verified_cache_key` (canonical bytes + keccak #1) ->
   `submit_native_action_inner` (lib.rs:1504-1586): duty-exempt check, `check_funded` (native balance / account /
   positions reads, funded.rs; 1 TRS by default) -> **global pool write lock** (lib.rs:1534) ->
   `NativePool::insert_with_restash_key` (native_pool.rs:408-502): `compute_action_hash` (keccak #2) and
   `bincode::serialized_size` INSIDE the lock, dedup on (sender, hash), per-sender cap 512, pool cap 65,536, BTreeMap
   insert keyed `(priority, sender, nonce, seq)` (native_pool.rs:16-50) + expiry, hash and cancel-head indexes ->
   addr-limiter charge -> trust-cache seed (`verified_senders` FifoCache, lib.rs:250-264, 585).
4. `mirror_to_da` (lib.rs:1994-2027): clone into `da_pending` (copy 5); at 128 bodies or 50 ms age
   (lib.rs:178-180) the CALLING thread runs `NativeDaStore::put_batch` (crates/torus-state/src/native_da.rs:527-555:
   `compute_action_hash` again (keccak #3) + bincode encode (copy 6) + one RocksDB WriteBatch on the consensus DB,
   docs/plans/hs-da-own-db-impl.md) and wakes the global arrival condvar.
5. `gossip_native_action` (lib.rs:1460-1488): `bincode::serialize(&(sender, action))` (copy 7) -> `try_send` on the
   bounded(8192) native gossip channel (bridge.rs:260; drop counted when full).
6. Swarm task (one tokio task, swarm.rs:1119-1418) appends to `native_batch`, flushes on the 50 ms timer / 1024
   actions / 128 KiB frame budget (swarm.rs:660-679, 1360-1415; `max_tx_message_size` config.rs:277) -> gossipsub
   publish on the raw v1 topic (`MessageAuthenticity::Signed`, `ValidationMode::Strict`, behaviour.rs:85-97; ed25519
   sign per frame) -> QUIC to the explicit validator peers (swarm.rs:1232-1240).
7. RPC reply: per-item ack hash (keccak of canonical JSON, a different identity from `compute_action_hash`).

### 1b. Every other validator (gossip receiver)
8. Swarm event loop: size gate, `deserialize_native_batch` + per-entry `bincode::deserialize` (copy 1) INLINE on the
   loop (swarm.rs:1521-1590) -> `native_action_inbound`, an **unbounded** mpsc (swarm.rs:408, bridge.rs:218).
9. Gossip inbound task (main.rs:989-1037): one tokio task; per action spawns onto the `torus-gossip-verify` rayon
   pool (cores/2), or runs inline once 1024 are in flight (main.rs:1010). `add_native_action_from_gossip`
   (lib.rs:1347-1390): `mirror_to_da` FIRST (clone + maybe put_batch), `verified_cache_key` (keccak), trust-cache
   lookup, on a miss full `recover_sender` (EIP-712 over all orders + ecrecover, eip712.rs:1019-1031), sender must
   equal the claim -> `admit_gossip` (lib.rs:1417-1458: batch size, nonce window) -> the same insert as step 3
   (keccak + `serialized_size` under the pool write lock). The claim-trusting path
   (`add_native_action_from_gossip_trusted`, lib.rs:1392) has no production caller.
   0xFD direct envelopes (pre-proposal push <= 512 KB, or d2l forward batches) reach the same inbound channel after a
   bincode decode on the swarm loop (swarm.rs:1683-1701); 0xFE legacy per-action JSON forward (swarm.rs:1727-1741).

### 1c. Leader (proposer), hotstuff-algo thread
10. `produce_block` -> `build_proposal_on` (crates/torus-consensus/src/app.rs:10215-10425) -> `select_block_payload`
    (app.rs:11056-11205): catch-up gate (lag > 64 -> empty, app.rs:10987), in-flight exclusion set =
    `pending_proposals` hashes + hash ledger (app.rs:11091-11097), exec-backlog pacing tier (off unless
    `TORUS_EXEC_THROTTLE_WATERMARKS`), then `select_native_for_block_with_senders_excluding` (mempool
    lib.rs:2143-2171): **synchronous `flush_da_mirrors`** of the whole ingress buffer, expiry eviction under the
    write lock, then a read-locked walk (native_pool.rs:564-608, 668-680): cancels oldest-first up to 25 %, then
    (priority, sender, nonce) order, 64 per sender per block, budgets 400 actions / 200k orders / 12 MB
    (deterministic prefix); every pick is cloned out (copy A).
11. `mirror_or_drop_native_at` (app.rs:9623-9706): clones the actions again (copy B, app.rs:9636),
    `compute_action_hash` per action, DA-GC pin, `mirror_missing_native_to_da` (MultiGet presence, writes only absent
    bodies; lib.rs:1944-1966), optional shard custody (off, `TORUS_SHARD_CUSTODY`, app.rs:938, 9748-9770).
12. `generate_sig_attestation` (crates/torus-bridge/src/proposer.rs:252-278): bincode re-encode of every body +
    SHA-256 + ed25519 sign (app.rs:10315-10323).
13. Header carries `native_action_count`; the datum is a CompactBlock of hashes (`COMPACT_PROPOSALS = true`,
    app.rs:7224); `PendingProposal` keeps the full block for 10 heights (app.rs:10407). Pre-proposal bundle
    `try_send` on `sync_channel(4)` (app.rs:10373-10384, main.rs:1059) -> std thread ->
    `broadcast_pre_proposal_actions` (bridge.rs:431-446, 159-182): `serialized_size`; > 512 KB (always at bench
    size, ~3-4 MB per block) -> hash manifest (keccak per action again) -> swarm `fan_native_push` through the
    PushScheduler (32 in flight, queue 512, swarm.rs:241-320) + a 3-bundle re-push ring.
14. HotStuff side: the block is borsh-encoded and stored in the swarm `block_store` (bridge.rs:713-720; hotstuff
    implementation.rs:984, 2110), header broadcast, 250 ms header re-send.

### 1d. Follower vote and commit (hotstuff-algo)
15. Manifest receive on the swarm loop: decode + one `NativeDaStore::contains` point lookup per hash
    (swarm.rs:2632-2670, 1702-1726) -> pull only the missing bodies from the proposer (`/torus/native-da/2.0`, 16
    hashes per request, bridge.rs:88). Serves run off-loop via `spawn_blocking`, max 8 (swarm.rs:613-658, 2012-2040).
16. Vote-time `check_proposal_data` (app.rs:10103-10200): absorb fetched bodies, DA-GC pin, `native_da_missing`
    (MultiGet, flush only on a miss; lib.rs:1920-1938) -> Held / Missing (no vote yet).
17. `validate_block` decode -> `reconstruct_native_actions_hot_core` (app.rs:9324-9430): one MultiGet + bincode decode
    of every body (lib.rs:1728-1755, native_da.rs:606-636), one 20 ms wake-on-arrival slice (app.rs:7897-7898), then
    misses go to `DaRecoveryWorker` (app.rs:8926) and the vote is MissingData (re-proposal next view).
18. Commit -> `dispatch_to_exec` (app.rs:11421): `compute_action_hash` of every action again (app.rs:11505-11513),
    `remove_committed_native` (restash under the read lock, removal under the write lock; lib.rs:2207-2223), exec
    `sync_channel(64)`; exec re-verifies through the trust cache (eip712.rs:1443, app.rs:5382).

## 2. Threads / pipeline and what is serial

| stage | thread(s) | parallel? | serial point / queue |
|---|---|---|---|
| HTTP + JSON parse | `rpc-worker` x4 async (main.rs:1478) | per connection | 512 conns; 64 permits |
| shed screen | tokio blocking pool (`rpc-worker`, 72-75 threads seen, ozarchy §69.4 off-CPU) | per request | none |
| verify | `torus-ingress-verify-*` rayon, cores/2 | per item (`par_iter`) | one batch per permit |
| admit (hash, insert, DA, gossip encode) | the async `rpc-worker` that awaited verify | **serial per batch** | pool `RwLock` (global), `da_pending` Mutex, consensus-DB write group |
| gossip batch + publish | swarm task (main tokio runtime) | **one event loop** for all p2p | biased select: cmds > events > DA-serve done > EVM tx > batch flush > native in (swarm.rs:1179-1259) |
| gossip / 0xFD decode, manifest planning | swarm task | **serial** | inline bincode decode, inline `contains` |
| gossip ingest dispatch | one tokio task (main.rs:1012) | spawns | unbounded inbound mpsc; inline at 1024 in flight |
| gossip verify + insert | `torus-gossip-verify-*` rayon, cores/2 | per action | pool `RwLock`, DA buffer, DB write group |
| select / mirror / attest / encode | `hotstuff-algo` | **serial (consensus thread)** | pool read lock; sync DA flush; MultiGet |
| pre-proposal push encode | std thread (main.rs:1062-1066) | 1 | `sync_channel(4)`, then the swarm loop |
| DA serve | tokio blocking, <= 8 (swarm.rs:613) | yes | all-empty answer at the cap |
| DA recovery pull | `DaRecoveryWorker` thread (app.rs:8926) | 1 | ~1 s budget per handoff |
| follower DA check + reconstruct | `hotstuff-algo` (async-validate worker exists, off) | serial | MultiGet + decode |
| dispatch hashing + pool prune | `hotstuff-algo` | serial | exec `sync_channel(64)` |
| block-data serve | `BlockDataServer` thread (on, hotstuff block_data_server.rs:120) | 1 | misses fall back to the algo thread |

Serial resources every ingested action crosses: the pool RwLock (once per node), the swarm loop (once per node per
gossip frame), the consensus-DB write queue (a DA batch per node), and per block the hotstuff-algo thread. On one host
the exec thread is the binding resource (survey "Reading"); ingress threads matter through CPU contention on the
shared host and through consensus-thread time on leader and followers.

## 3. Hot paths with measured / estimated cost

| # | hot path | cost | source |
|---|---|---|---|
| H1 | gossip-verify pool (EIP-712 struct hash + ecrecover on every non-ingress validator, n-1 times per action) | 23.6-24.1 % of val1 user cycles (300 mkts); s77: 1.72 cores over 3 nodes at ~64k matched/s, 7.03 CPU-s/1M after hash dedup; Keccak 57.7 % of the thread; Keccak 2.19 ms per 1k fills | ozarchy doc §69.5; s77-s80 doc §1-2; ozarchy §63 Keccak table |
| H2 | ingress-verify pool | 14.0-14.4 % of user cycles; 3.83 CPU-s/1M (s77); bs500 microbench 5.8 ms/action = hex 1.15 + parse 0.71 + sig 2.43 + JSON ack ser 1.14 + keccak 0.40; at 300 markets `validate_known_markets` is 28.3 % of the thread (~221 distinct markets per batch, ~0.25 cores/node) | ozarchy §69.5; s77 §2; plans/ingress-cpu-supply.md; memory c6513199 |
| H3 | tokio main runtime (libp2p: QUIC, gossipsub sign/verify, AEAD, bincode, zstd) | 9.3-9.6 % of user cycles; 0.34-0.44 cores/node: QUIC 19.5 %, ed25519 verify 19 %, AEAD 11 %, sign 11 %, gossipsub 12 %, bincode 7.6 %, zstd 6.6 % | ozarchy §69.5; survey network row (memory dabcab1c) |
| H4 | rpc-worker admit (cache key, insert hash, DA put_batch hash + encode) | ~3 % of node CPU (s77, /proc); Keccak 0.08 cores | s77 §1; ozarchy §63 table |
| H5 | leader block_build on hotstuff-algo | select 20-25 ms per proposal incl. the sync DA flush (one node 29-60 ms on main), mirror 14-22 ms, attest 7.5 ms (cap 200, s68), encode ~1 ms | ozarchy §53c, §53d.5, §82.4; s68 doc |
| H6 | follower reconstruct before vote / validate | cap 200: MultiGet ~6 ms + decode 12-16 ms (the 27-34 ms flush was removed by s76 flush-on-miss); est. ~1.5-2x at 300-400 actions | s76-validate-timers doc |
| H7 | dispatch `compute_action_hash` recompute on hotstuff-algo | 11.2-12.5 ms per native block, 41-46 % of consensus-thread CPU | survey consensus row (s81.4, s92.2) |
| H8 | hotstuff-algo total | 56.6 ms on-CPU per block (base) | ozarchy §82.4 |
| H9 | DA bodies written on every node (consensus DB) | 17.0 GB written per node per 600 s cell (~28 MB/s incl. WAL and compaction) | ozarchy §82.4 base |
| H10 | DA write stalls in checkpoint windows (before DA got its own DB) | gossip verify 12.55 s / RPC 3.24 s of WriteThread waits over 5 windows, max 615 / 493 ms | ozarchy §71.3 (hs-da-own-db: in-window DB wait -91 %) |
| H11 | pre-warm pulls | ~0.93 pulls/s per node, ~46 bodies each | 8k-explore-ideas L4.3 (8i P0 logs) |

Latency (s77, val0): at half load (24k matched/s, 22 blk/s) admit p50 8 ms, commit 160, exec 213, fills visible
241 ms; p99 127 ms / 2.2 s / 2.5 s / 3.7 s (view timeouts). Full load: exec queue pinned at 64 blocks, exec age ~71 s;
the 16 s admit age is a bench artifact (5k senders, 256 in flight). Paced order-to-commit p50 159 ms (s88, L1.1).
Estimates from code (not measured): gossip adds 0-50 ms of batching (const timer) plus verify for the ~2/3 of actions
admitted away from the next leader; a body crosses ~7 copies on the ingress node, ~3 on each receiver, +2 clones per
selected action on the leader; keccak over the full canonical bytes runs 3x on the ingress node, 2x on each receiver,
+3x on the leader (mirror, manifest, dispatch) and 1x on every follower at dispatch.

## 4. Knobs and current defaults

RPC (crates/torus-rpc): `TORUS_RPC_MAX_CONNS` 512 (lib.rs:55-60); `SUBMIT_PERMITS` 64, `SUBMIT_QUEUE_TIMEOUT` 250 ms,
`SUBMIT_BATCH_MAX` 100 (consts, lib.rs:30-39); `TORUS_RPC_MAX_RESPONSE_MB` 10; `TORUS_RPC_IP_WEIGHT_PER_MIN` /
`TORUS_RPC_IP_EXEMPT` (ip_limit.rs:286-287; bench exports 1200); `FORWARD_CHANNEL_CAP` 65,536 (lib.rs:243); ingress
verify pool cores/2 (const, torus.rs:661-676); rpc runtime 4 workers (const, main.rs:1478).

Mempool (crates/torus-mempool): `TORUS_NATIVE_TOTAL_BLOCK_CAP` 400 (rate_limit.rs:97-109);
`TORUS_NATIVE_ORDERS_PER_BLOCK_CAP` 200,000 (:250-267); `TORUS_NATIVE_BLOCK_BYTES_CAP` 12 MB (:309-321);
`TORUS_NATIVE_PER_BLOCK_CAP` 64 per sender per block (:50-62); pool 65,536 / per sender 512 (consts :115, :219);
`TORUS_ADMISSION_HORIZON_MS` 20,000, rate window 10 s, floor 4 x cap (:126-175); `TORUS_ADMISSION_EXEC_AWARE` off
(:167); `TORUS_CANCEL_BLOCK_SHARE_PCT` 25 (:202-213); `TORUS_VERIFIED_SENDER_CACHE_CAP` derived, 64,000 at cap 400
(:349-399); `TORUS_INGRESS_MIN_COLLATERAL` 1 TRS (funded.rs); `TORUS_ADDR_RATE_LIMIT/_BUFFER/_EXEMPT`
(addr_rate.rs:256-260; bench sets LIMIT=1); `ORACLE_PENDING_PER_VALIDATOR` 4; DA mirror 128 bodies / 50 ms (consts,
lib.rs:178-180); DA fail-stop after 3 failed writes (lib.rs:185); nonce window 60 s (eip712.rs:49).

Network (crates/torus-network): `--native-gossip` true; `TORUS_D2L_BATCH` 1, `_MS` 25, `_MAX_BYTES` 512 KiB,
`TORUS_D2L_REFORWARD_MS` 2000 (forward_batcher.rs:13-23; inert in gossip mode); native gossip batch 50 ms / 1024
(consts, swarm.rs:660-661); `max_tx_message_size` 128 KiB, `max_consensus_message_size` 1 MiB (config.rs:276-277);
`GOSSIP_MAX_TRANSMIT_SIZE` 2 MiB, direct / native-da 8 MB, block-data 16 MB (caps.rs); `TORUS_GOSSIP_QUEUE_LEN` 512
(behaviour.rs:75); gossipsub heartbeat 100 ms (:63); `TORUS_HASH_ONLY_PUSH_THRESHOLD` 512 KiB (bridge.rs:97, clamped to
`TORUS_DIRECT_PUSH_BODY_BYTES` 8 MB); push 32 in flight / queue 512 (swarm.rs:241-246); DA fetch chunk 16
(bridge.rs:88); DA serve 8 in flight (swarm.rs:613); QUIC streams 512 per connection (bridge.rs:44); req/resp timeout
10 s (behaviour.rs:154-209); inbound consensus queue 10,000 (swarm.rs:3030); native gossip channel 8192
(bridge.rs:260); gossip verify inline at 1024 (main.rs:1010); pre-proposal `sync_channel(4)` (main.rs:1059);
`TORUS_CONSENSUS_DIRECT_FAN` off / `_GOSSIP_MIRROR` on (config.rs:114-128); consensus rate limit 50/s per author, tx
100/s per peer (config.rs:279-283); `TORUS_MESH_WATCHDOG_GRACE_MS` 5000, `_TICK_MS` 1000,
`TORUS_MESH_STALE_CONN_KICK` on.
Consensus-side DA: `TORUS_SHARD_CUSTODY` off (app.rs:938); reconstruct slice 1 x 20 ms (app.rs:7897-7898);
`TORUS_ASYNC_VALIDATE` off; `TORUS_BODY_SERVE_THREAD` on; `TORUS_HEADER_RESEND_MS` 250; `TORUS_DA_BODY_RETENTION`
derived 100,128; `TORUS_DA_GC_INLINE` off (worker); `TORUS_EXEC_THROTTLE_WATERMARKS` unset.

## 5. Known limits and failure modes

- The single-host rig is the ceiling: 3 nodes at ~4.5 cores each + bench on 18 vCPU, load1 36-40; closed-loop bench,
  so offered load follows RPC latency (s76 doc). The verify pools (H1 + H2, ~38 % of user cycles) compete with the
  exec thread for cores; s352: sharing the global rayon pool took block time from 529 to 4034 ms (torus.rs:651-660).
- At full load the exec channel pins at 64; ingress then sheds via the admission limit and nonce expiry (s76: cap 200
  expired 2-9 % of submitted, cap 400 ~0). Exec-aware admission at 2-5 s horizons lost 33-51 % (survey §10).
- Big bodies: bs~500 view timeouts on full-body dissemination -> hash manifest + pre-warm pull; cap 1000 wedged
  (s365); `max sub-streams reached` 2,537x at bs=500 -> stream window 512 + bounded push (bridge.rs:36-44,
  swarm.rs:236-250).
- S387: manifest pull storms starved consensus while serves ran on the loop -> off-loop serves, pull only missing.
- The gossipsub per-peer queue (libp2p 5000) let consensus messages sit behind native batches: 21.5 -> 1.15 blk/s
  collapse -> queue 512 (behaviour.rs:65-75). The consensus rate limiter dropped leader headers -> 250 ms re-send (s77).
- s339: relayed oversized gossip got a validator banned -> author-only penalties (swarm.rs:1521-1540).
- S458: the gossip-off (d2l) canary wedged the testnet -> pre-spread stays the default.
- A missing body at vote or validate -> MissingData, re-proposal next view: a p99 latency event, not a fork.
- Shard custody on: -21.6 % matched/s, 140 ms per follower validate (ozarchy §82.4).
- Selection order (priority, sender, nonce) favors low addresses (s64 bug 5); an arrival-order key collapsed to 3.4k
  matched/s (s66, native_pool.rs:31-34).
- Unbounded queues: `native_action_inbound`, the swarm command channel, EVM inbound (bridge.rs:218-259); the swarm
  `block_store` is insert-only (swarm.rs:402, 2473-2479).
- WAN: dissemination and RTT (227-502 ms legs) bind, not CPU (survey); WAN items are owner-held (owner list).

## 6. Corners that look unexamined (observations only)

1. **The proposer attestation is computed but no longer checked.** `generate_sig_attestation` runs on hotstuff-algo
   for every leader block (app.rs:10315-10323): bincode re-encode of every body + SHA-256 + ed25519 sign
   (proposer.rs:252-278); s68 measured 7.5 ms per leader block at cap 200. `verify_sig_attestation` lost its only
   production caller in `b6f6aca1` (s84 decision 1, validate_body_checks_core removed); the header field is read only
   in tests (grep of `sig_attestation`). The survey still lists "attest over action hashes" as an open
   consensus-format change, which assumes the check exists.
2. **The proposer's select path always flushes the whole ingress DA buffer synchronously** (lib.rs:2151, inside
   `block_build_select`: 20-25 ms, 29-60 ms on one node), although `mirror_missing_native_to_da` (app.rs:9673)
   re-checks presence and writes the absent bodies; the follower path already moved to flush-on-miss (s76). 18c noted
   that the timer conflates the flush with selection (ozarchy §53d.5); it has not been split since.
3. **The same bytes are hashed again and again.** The pool entry stores `action_hash` (native_pool.rs:244) and the
   restash key, but selection returns only `(sender, action)`; the leader recomputes the hash in mirror
   (app.rs:9637-9641) and the manifest (bridge.rs:166-169), and every node again at dispatch (app.rs:11505-11513, H7).
   Ingest computes it under the pool write lock (native_pool.rs:414) and again in `put_batch` (native_da.rs:536). The
   survey tracks only the dispatch hash; memory c6513199 notes dispatch discards the pooled verified sender.
4. **The pool write-lock hold includes a ~10.8 KB keccak + `serialized_size`** (native_pool.rs:414-420 under
   lib.rs:1534). RPC admit, the cores/2 gossip-verify threads and the consensus thread (evict + select) all take this
   lock; there is no lock-wait metric, and selection clones up to 400 actions while holding the read lock.
5. **Verify is paid n times per action.** Every non-ingress validator re-derives the sender (main.rs:1019 ->
   lib.rs:1347-1390) although the author is an authenticated validator peer; H1 is ~1.7x H2 in CPU. Backlog L5.1-L5.3
   lower the cost of one verify, not the count.
6. **The swarm loop decodes and reads the DB inline**: gossip batch decode (swarm.rs:1543-1560), 0xFD envelope
   decode (:1683-1701), one RocksDB `contains` per manifest hash (:2652-2656, ~300-400 per block). DA serves were moved
   off-loop for the same reason (S387); these were not. Flagged in July (memory 841d7e57, "never implemented").
7. **Synchronous RocksDB writes run on async RPC workers.** The admit loop (torus.rs:900-990) runs on one of 4 async
   workers and performs the due `put_batch` inline (lib.rs:2016-2025); before the DB split this cost 3.24 s of
   WriteThread waits (max 493 ms) in checkpoint windows. Since hs-da-own-db, DA batches (up to 128 bodies, ~1.7 MB)
   share the consensus-DB write group with QC / lock / vote writes; that interplay has not been measured since the move.
8. **Followers re-read bodies they already hold decoded in their pool.** Reconstruct does a MultiGet + bincode decode
   of every body (H6), although gossip already put the same `SignedNativeAction` in the local pool. "Pool-first
   reconstruction" appears only as an open one-liner (s64 #10 / s65).
9. **The ack hash is a third identity.** `keccak(serde_json(action))` (torus.rs:625-630) exists only for the RPC reply
   and costs a full JSON encode per admitted action (old microbench: 1.54 of 5.8 ms at bs500), next to
   `compute_action_hash` and `verified_cache_key`. Binary ingress still hex-decodes (types.rs:90) and rides a JSON
   string array; ingress-cpu-supply.md's "bin drops hex" does not match the code.
10. **The gossip frame shape is fixed in code.** The 50 ms timer and the 1024 count are consts (swarm.rs:660-661); the
    128 KiB frame cap is shared with the EVM tx topic (config.rs:277), so one frame carries at most ~9 bench actions,
    and each frame is ed25519-signed and verified (H3: sign 11 %, verify 19 %). The timer sits on the
    admit-to-leader path.
11. **The swarm `block_store` grows without bound** (swarm.rs:2473-2479): every own proposal is borsh-encoded on
    hotstuff-algo and cloned into it (bridge.rs:713-720; implementation.rs:984, 2110), yet this binary requests block
    data through HotStuffMessage (bridge.rs:684-689); only the legacy `/torus/block-data/1.0` handler reads it.
    Estimate: tens of MB per hour per node.
12. **Survey status has drifted.** T2.1 (gossip verify pool, main.rs:989-1037), T2.2 (coalesced DA mirror,
    lib.rs:178-180, 1994-2058) and T2.3 (incremental BTreeMap order, native_pool.rs:16-50) are shipped in code but
    listed as "status not recorded" in 8k-explore-survey §2, §6 and §10.
13. **Whole blocks are retained per height.** `pending_proposals` keeps whole TorusBlocks (with bodies) for 10 heights on
    leaders and validators (app.rs:10072-10076, 10407); at ~3-4 MB of bodies per block that is ~30-40 MB live per
    node, cloned on insert.
14. **Gossip inbound is unbounded.** Backpressure from verify never reaches the swarm (unbounded mpsc,
    bridge.rs:218). At >= 1024 in flight the single dispatch task verifies inline, so the queue grows while the swarm
    keeps decoding.

Already explored or owner-held (do not re-propose): WAN L4.1-L4.4, the push-manifest threshold, erasure follow-ups,
shard custody, T5.x p99 (owner list); L5.1 item-hash memo, L5.2 known-key ECDSA, L5.3 session path, native
pre-spread over direct zstd (`TORUS_NATIVE_PRESPREAD_DIRECT`), the ingress market-spec cache (memory c6513199 /
bfbd7612), DA blob files, FIFO DA (dead), exec-aware admission, arrival-order selection (dead), leader body push with
the header (off).
