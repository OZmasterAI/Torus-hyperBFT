# System map: consensus and block pipeline (s80 novel search, prep)

Repo `/home/oz/projects/torus-economy/Torus-hyperBFT`, main `eb993e3f`. Paths are repo-relative. "ozarchy §N" means
section N of `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`. "survey" means `docs/perf/8k-explore-survey.md`
(177 explored items). Read-only map: nothing was built or run. Costs come from the cited bench sections, not from new
measurements. Exec-thread internals are in `map/exec.md`. This map stops at the exec channel. Memory ids
(`mem xxxxxxxx`) are prior findings checked with `search_knowledge`.

Owner-excluded areas that touch this map (see `ledger/owner-list.md`): TC-required + tip signing, dead-leader skip,
T5.1 (cadence), T5.2 (p99 floor), T5.3 / Q6 (checkpoint-window p99), a2 short timeout, M2 timestamp bound, 3 vs 4
validators, push-manifest threshold, L4.x WAN items, erasure coding, co-location, PE over WAN, qca-timers (parked),
b0 / b1 / mode 3 (parked).

## 1. Architecture and data flow

**Crates.** `crates/hotstuff_rs`: a fork of HotStuff-rs with MonadBFT additions (TCs, NEC / recovery, 2-chain
commit, speculative 1-QC bookkeeping) and a header-first "hybrid pipelining" fast path
(`docs/plans/hybrid-pipelining.md`, option A shipped; option C, speculative block production, was never built).
`crates/torus-consensus/src/app.rs` (48.7k lines): `TorusApp`, the `App` impl. It runs commit-then-execute (CTE):
produce and validate never execute. Committed blocks go to the `torus-execution` thread. `crates/torus-network`: the
libp2p `Network` impl (`bridge.rs:621-720`) and the swarm loop (`swarm.rs`). `crates/torus-node/src/main.rs:1186-1201`:
the replica configuration.

**Block shape.** One hotstuff `Block` carries 1 datum (`Data(Vec<Datum(Vec<u8>)>)`, `types/data_types.rs:198,244`).
`COMPACT_PROPOSALS = true` (`app.rs:7224`), so the datum is a bincode `CompactBlock` (`torus-types/src/lib.rs:728`):
the `TorusBlockHeader`, the native action **hashes** (32 B each, cap 400 actions, `torus-mempool/src/rate_limit.rs:97`),
inline EVM txs and core-writer actions. That is ~13 KB at cap with no EVM load. Native bodies (~10.8 KB each) travel
out of band. The proposer pre-pushes them, or sends a hash manifest above 512 KB (`TORUS_HASH_ONLY_PUSH_THRESHOLD`,
env.sh). They land in the DA store (`cf_native_pending`, consensus DB). `data_hash` = sha256 of the datum. Block hash
= H(height, justify, data_hash) (`hotstuff/implementation.rs:2183`).

**Happy path for view v** (L = leader(v), N = leader(v+1) = vote collector, n = 3, round-robin, reputation off;
`implementation.rs` is abbreviated as `:line`):

1. **Enter view.** On an AdvanceView, `pacemaker/implementation.rs:453-543` calls `update_view` (:558). The next loop
   pass runs `HotStuff::enter_view` (:563): NewView to the next leader (:652-675), collector reset, then
   `set_highest_view_entered` (1 RocksDB write).
2. **L builds** (:800-880). L needs `highest_pc.block` in its tree. If it is missing, L defers and fetches the parent
   by hash (:833-848; retried every loop pass).
   - `app.produce_block` (`app.rs:10433`) reads the parent header from the tree.
   - `build_proposal_on` (`app.rs:10215`): select (`select_block_payload` :11056), DA mirror
     (`mirror_or_drop_native_at` :9623), proposer sig attestation, encode via `PendingProposal::with_hashes`. The
     pre-proposal body push goes to `pre_proposal_tx`.
   - Then `block_tree.insert` and `block_tree.update(justify)`: 2 writes (`block_tree/accessors/internal.rs:279-363`).
3. **L broadcasts the header** (`finish_leader_proposal` :897, `broadcast_proposal_as_header` :975).
   - L keeps the body in `pending_bodies` (:981) and serializes it into the swarm `block_store` (`bridge.rs:713`).
   - The `ProposalHeader` goes over **gossipsub** (`swarm.rs:2381-2430`; self loopback at :2383). The direct fan is
     opt-in.
   - Body push with the header is off (`TORUS_BODY_PUSH_MAX_BYTES=0`, :3627).
   - With `TORUS_DEFER_COMMIT_FEED` off (the default), any commit feed from step 2's update runs **before** the
     broadcast (:908-912).
4. **Follower receives the header** (`on_receive_proposal_header` :2129).
   - Checks: equivocation (:2147-2181), hash, TC / NEC, justify `is_correct`, then `safe_pc` or the pending-bypass
     lock clause (:2236-2270).
   - **Lock before vote:** `update_locks_only` (:2356, 1 write, no commit).
   - Arms `awaiting_body_vote` (:2387).
   - Sends a `BlockDataRequest` to the origin (:2435). This is a request-response round trip.
5. **Body arrives.** A `BlockDataResponse` on the progress channel, served off-thread by `hotstuff-bodyserve`
   (`block_data_server.rs:68`).
   - `on_receive_block_data_response` (:2497) calls `vote_if_body_held` (:2651), then `app.check_block_data`, then
     `check_proposal_data` (`app.rs:10103`).
   - `check_proposal_data` decodes the datum and checks counts and the **parent link**. The parent must be
     **inserted in the tree**: `NotInTree` returns `Missing` (`app.rs:3242`). Then it checks the timestamp and DA
     presence (`pin_native_da` + `native_da_missing`, `torus-mempool/src/lib.rs:1814,1920`).
   - If `Held`, `cast_header_vote` (:2740) persists the vote state (:2768; 1 write, default `WriteOptions`,
     `kv_store.rs:160-195`). It sends the vote to leader(v+1) **and** leader(v+2) (:1201; `roles.rs:118,150`).
6. **Insert right after the vote:** `try_insert_body` (:2858).
   - `app.validate_block` (`app.rs:10483`): parent link, hash, decode + DA reconstruct of all bodies
     (`decode_proposal_and_ensure_durable` :9787).
   - `insert`, then `update(justify)`: lock, and the **2-chain commit** of the grandparent when
     `justify.view == parent_justify.view + 1` (`block_tree/invariants.rs:565-635`, rule :617).
   - **Commit feed** (:2958), then `drain_deferred_bodies` (:3035).
7. **N collects votes** (`on_receive_phase_vote` :1641).
   - Quorum = `floor(2P/3)+1` (`types/validator_set.rs:153`). With 3 equal stakes that is **3 of 3**.
   - QC, `advance_highest_pc_from_remote`, `update(QC)` (commits the grandparent), **commit feed** (:1727),
     speculative 1-QC mark (:1737, 1 write), **then** AdvanceView broadcast (:1750-1762).
8. **N changes view only through a received AdvanceView.** That is its own, looped back through
   swarm → inbound queue → poller → algo, or a peer's. A collected PC never calls `update_view` (s70; mem aa29872a).
   - On the next pass, the pacemaker tick broadcasts **a second AdvanceView** for the same PC
     (`pacemaker/implementation.rs:196-215`).
   - Every replica re-broadcasts the first AdvanceView it receives in a view (:505-527).
   - Then `enter_view(v+1)`, which needs B_v inserted (step 2).
9. **CTE: `on_committed_block`** (`app.rs:10601-10776`).
   - Decodes the datum. Takes the cached body if its hashes match (`cached_matches_compact` :1986; this hashes every
     body when the cache is cold). Otherwise `ExecSource::Compact`.
   - Commit-manifest put (:10731), `last_header` advance, EVM reinsert / resend, then `enqueue_for_execution`
     (:11211).
   - `dispatch_to_exec` (:11421): persist ticket to `torus-commit-persist` (T5.3), **Keccak rehash of every native
     body** (:11505-11513), mempool `remove_committed_native` (:11516), DA GC hand-off.
   - Then a **blocking `sync_channel(64)` send** (:11621; `EXEC_QUEUE_DEPTH` `rate_limit.rs:355`). Only
     `TORUS_EXEC_NONBLOCKING_DISPATCH=1` (:11571) turns a full channel into a park instead of a block.
   - `execution_loop` (:6814) waits for the ticket, then executes.

**Followers commit only on body insertion.** The header path only locks (:2356), and an AdvanceView only advances
`highest_pc`. So at a follower, B_{v-2} commits when B_v's body is inserted. Order-to-commit is about 2 views plus one
insertion.

## 2. Threads / pipeline, and what is serial

| thread | role | serial with |
|---|---|---|
| `hotstuff-algo` (`algorithm.rs:153`) | **The whole consensus state machine and every App callback**: produce, check, validate (with DA reconstruct), commit feed (with the blocking exec send), block-tree reads and writes, pacemaker, sync processing | everything below that it hands off to |
| poller (unnamed, `networking/receiving.rs:56`) | pops the `LibP2PNetwork` inbound VecDeque into the progress / sync / block-data mpsc channels; **sleeps 250 µs when empty** (:95) | one hop per inbound message |
| `hotstuff-bodyserve` | serves `BlockDataRequest` from a BlockTreeCamera snapshot (s72 option 3, on by default) | misses fall back to algo |
| `hotstuff-syncsv` + sync worker | block-sync server and fetch | fallback only |
| event bus | Event handlers (view-phase metrics, RPC commit handler). Events carry cloned `Block`s (`events.rs:106-117`) | clones made on algo |
| tokio swarm | gossipsub, `/torus/direct`, QUIC; consensus broadcasts, votes, bodies, native gossip; 0.34-0.44 cores / node (survey §0) | per-peer queue 512, shared with native batches (`behaviour.rs:75`) |
| `torus-execution` (`app.rs:8335`) | executes committed blocks strictly in order; busy 0.88-0.91 at saturation (ozarchy §118) | algo blocks on its full channel |
| `torus-commit-persist` (`app.rs:724`) | durable header + body write (T5.3) | exec waits on the ticket |
| `torus-async-validate` (`app.rs:7758`) | decode + DA prevalidate; `TORUS_ASYNC_VALIDATE` **off**, never benched on (survey §5) | n/a |
| `torus-da-recovery`, `-da-gc`, `-manifest-prune`, `-flush-worker` | DA heal, RH4 GC, manifest prune, exec flush | off the critical path |

**Algorithm loop** (`algorithm.rs:190-312`): one inbound message per pass. Each pass does, in order:
- shutdown check;
- pacemaker `tick` (:201; 2-4 uncached tree reads);
- `enter_view` if the view is outdated or a proposal is deferred (:207);
- `run_pending_commit_feed`;
- up to 128 sync blocks (:228);
- deferred bodies, then the block-data channel;
- `poll_progress_and_retry` (:314): `tick_body_retries` (:490: header re-send, body / justify / missing-PC /
  missing-data retries, `tick_body_vote`), then `recv` with deadline = min(view deadline, now+10 ms while anything is
  pending (:336), next re-send), then dispatch;
- every 1 s, a reconcile / `on_reconcile_tick` (:287-311).

**Serial critical path per view.** Everything runs on one thread per node, and n = 3 needs all three votes.
- L: `[B_{v-1} inserted] -> produce (select, mirror, attest, encode) -> insert + update writes -> header`.
- Followers: `header -> BlockDataRequest RTT -> check (needs B_{v-1} INSERTED locally) -> vote`. Then
  `insert B_v = validate (DA reconstruct) + update + commit feed of B_{v-2} (blocking exec send)`.
- N: `3rd vote -> QC -> update -> commit feed -> AdvanceView -> loopback -> enter v+1 -> needs B_v inserted -> produce`.

So the commit feed sits on the view path twice: at N between QC and AdvanceView, and at each follower inside the
parent insertion that gates its next vote.

## 3. Hot paths with cost

Current main, code defaults, standard shape (hsdb-300m, 300 markets, cap 400, MAX_IN_FLIGHT 4, commit tail, one
host). **268.8k matched/s, 15.47 native blk/s, block p50 / p99 14 / 659 ms, 0 timeouts, exec 55 ms per native block,
~16k fills per block** (ozarchy §113, main `654f047c`).

| path | where | cost | source |
|---|---|---|---|
| Commit feed on algo (`on_committed_block` hook) | `app.rs:10601`, called from :1727 / :2958 / :911 | 16-19 ms per call; **260-265 ms per s** of algo wall | ozarchy §106 (`fd2557c3`) |
| Collector `qc_to_advance` (QC to own view entry) | `implementation.rs:1641-1762` + loopback | 28-41 ms per view, **94-96% of it the commit feed**; 1/3 of the 85-123 ms commit interval; 218 ms per s | §118 (qca-timers binaries, parked) |
| ↳ blocking exec send | `app.rs:11621` | 7-13 ms per block (0.39-0.45 ms per 1k fills); algo blocked 19-26% of wall in 6 of 9 windows | §118; survey §0 (s90.3) |
| ↳ enqueue minus send (materialize, Keccak rehash, prune, GC) | `app.rs:11421-11560` | 11-17 ms per block (0.55 ms per 1k fills); dispatch Keccak 11.2-12.5 ms per native block = 41-46% of algo on-CPU | §118; survey §0 / §6 (s92.2) |
| ↳ manifest put / datum decode | `app.rs:10731`, :10632-10666 | 2.9-3.9 / 1.7-2.5 ms per block | §118 |
| Leader `propose_delay` | `implementation.rs:800-880` | 21.6 ms (block_build 20.9 = select 9.7 + mirror 9.0) | §106, out of checkpoint windows |
| `qc_collect` (own proposal to own QC) / `vote_gather` | | 49.5 / 26.8 ms | §106 |
| Follower `proposal_arrival` / `vote_delay` / `insert_persist` | :2129-2440, :2651-2790, :2858 | 30.2 / 8.7 / 10.7 ms | §106 |
| `validate_block` (DA reconstruct; DA wait) | `app.rs:10483`, :9787 | 3.7 (6.6; 20.5) ms | §106 |
| Rest of `qc_to_advance` (14 steps, incl. loopback 0.35 and swarm command 0.14) | | 1.6-1.7 ms per view | §118 |
| Algo on-CPU | | 36-44 ms per native block, busy ~22% | survey §0 (s81.4, s92.2) |
| Vote-state write | `internal.rs:645` | 0.05-0.06 ms | §106 |
| Exec thread (the binding resource) | `app.rs:6814` | 55 ms per native block, busy 0.88-0.91; matched/s pinned near offered load | §113, §118 |
| Deep book (1M resting) | exec `save_books` | 3.42 → 15.76 blk/s, p50 193 → 13 ms with L3.1 (now on by default) | §110 |

**Reading: blk/s is an equilibrium, not a ceiling.** Exec saturates, and the blocking exec send paces consensus. A
slower view makes bigger blocks, and those make the next feed slower. In §118, the same env ran at 8.0 blk/s on one
build and 14.4 on another.

For contrast, the historical regime: at 1.2-1.3 blk/s (cap 200, 10 markets, September) a view took 600-700 ms and
propose was about half of it (`docs/perf/s70-qc-split-2026-09-27.md`, `s72-parent-body-2026-09-28.md`). Most "no
gain" consensus verdicts come from that regime: s65 / s66 defer feed, s70 local advance, s72 fix D. The empty-block
floor was 102 ms on a 4-val devnet (`docs/reports/sprint-s395-report.md:11`).

## 4. Knobs and current defaults

| knob | default | where |
|---|---|---|
| `timeout_base_ms` (`max_view_time`) | 1200 (genesis; devnet the same) | `torus-node/src/main.rs:464,1189`; `devnet/genesis.json:8` |
| backoff factor / cap (multiplier `factor^min(view-(qc+1), cap)`) | 2 / 8 | `main.rs:465-466`; `pacemaker/implementation.rs:805-817,1227-1240` |
| `commit_lag_backoff_cap` (S470) | genesis 0 (off); bench `TORUS_COMMIT_LAG_BACKOFF_CAP=8`; grace 4 | `main.rs:490-517`; `pacemaker/implementation.rs:1248`; `run-cell.sh:614-621` |
| pacemaker epoch length | genesis `consensus.epoch_length`: devnet 100 views, code default 100,000; epoch-change view = `view % len == 0` | `main.rs:1188`; `devnet/genesis.json:7`; `pacemaker/implementation.rs:1280` |
| schedule rebase clamp | rebase when the entered view's deadline is more than 2 × mvt × mult away, so the effective timeout is ~1.2-2.4 s | `pacemaker/implementation.rs:607-615` |
| `TORUS_HEADER_RESEND_MS` | 250 (0 = off) | `implementation.rs:3651` |
| `TORUS_ROUND_SKIP` (rejoin) | on; max 16 views | :3645; `pacemaker/implementation.rs:1276` |
| `TORUS_DEFER_COMMIT_FEED` / `TORUS_DEFER_PARENT_FEED` | off / off | :3634-3642 |
| `TORUS_BODY_PUSH_MAX_BYTES` | 0 (off); hard cap 64 KiB | :3614, :3627 |
| `TORUS_BODY_SERVE_THREAD` | on | `replica.rs:645` |
| `TORUS_BODY_BEFORE_EXPIRY` | off | `algorithm.rs:182` |
| `TORUS_PROPOSER_EXEC_WATERMARK` | off | `implementation.rs:464-490` |
| body fetch: retry / origin tries / total / slow / deadline | 100 ms / 2 / 9 / 500 ms / 4 s | :3578-3596 |
| vote re-check on `Missing` | **20 ms** | :3601 |
| missing-data retry / deadline / cap | 500 ms / 3 s / 64 | :3598-3607 |
| algo recv short deadline while pending | 10 ms | `algorithm.rs:336` |
| poller idle sleep | 250 µs | `receiving.rs:95` |
| progress message buffer | 1 MiB | `main.rs:2059` |
| block sync: limit / advertise / response / blacklist / min view diff / trigger | 128 / 10 s / 3 s / 10 s / 2 / 60 s (futile backoff to 480 s) | `main.rs:1194-1199`; `block_sync/client.rs:199` |
| `TORUS_EXEC_NONBLOCKING_DISPATCH` | off (blocking send) | `app.rs:1157-1206` |
| exec channel depth | 64 | `rate_limit.rs:355` |
| `TORUS_EXEC_THROTTLE_WATERMARKS` | unset (no pacing) | `app.rs:1046-1080` |
| `TORUS_ASYNC_VALIDATE` | off | `app.rs:996-1006` |
| native caps per block: actions / orders / bytes | 400 / 200k / 12 MB | `rate_limit.rs:97,250,309` |
| `TORUS_CONSENSUS_DIRECT_FAN` / `_GOSSIP_MIRROR` | off / on | `torus-network/src/config.rs:105-130` |
| consensus gossip rate limit (per author, no env) | 50 msg/s, bucket 50 | `config.rs:280`; `peer_scoring.rs:171-205`; `swarm.rs:1420` |
| gossipsub | per-peer queue 512; Strict signing; message id = author + seqno | `behaviour.rs:75,85-97` |
| `TORUS_ROCKSDB_PIPELINED_WRITE` | 1 (bench `RECORD_ENV`; node defaults said to match since plan 9.13) | `run-cell.sh:590-621` |
| reputation leader selection | off | `main.rs:469` |
| stakes | equal (bench `VALIDATOR_STAKES` unset), so the quorum is 3 / 3 | `run-cell.sh:546` |

## 5. Known limits and failure modes

- **Exec-bound pacing.** A full exec channel blocks the algo thread inside the commit feed (`app.rs:11621`).
  - Fix D (s72) moved the feed after the broadcast. The leader then stalled 1.35 s without serving bodies, which led
    to justify-fetch exhaustion, sync fallback and a timeout. The body-serve thread now covers serving.
  - Throttle watermarks cost 8-44% (s77-s80). Nonblocking dispatch is opt-in only.
- **Unanimous QC at n = 3.** One slow replica (busy in validate or feed) delays every QC.
  - The backup vote recipient (`roles.rs:131-163`) only adds liveness when leader(w+1) is down.
  - A 4-val devnet with 1 of 4 dead ran more than 4x slower (s442). Dead-leader skip and 3 vs 4 validators are
    owner items.
- **Consensus gossip rate limiter** (known: §77, mem 8d5ce8bc, 10a86b6c).
  - Each author sends ~2 consensus gossip messages per view: the header, plus 2 AdvanceViews when it collects (:1756
    and pacemaker :196-215). The bucket is 50 msg/s per author.
  - Above ~25 views/s, headers get dropped and the view times out at ~1.2 s (idle and small-block regimes).
  - Mitigated by the 250 ms header re-send. Drop metric `torus_consensus_rate_limited_dropped_total` (`0af69a76`).
  - Root fix designed (per-class buckets, s126), not built.
- **Gossipsub head-of-line.** Headers share the per-peer queue with 128 KB native batches. The direct fan (B2) is
  opt-in.
- **Commit wedge (S470).** QCs outrun commits when a justify body is pending. Mitigations: commit-lag backoff (bench
  cap 8), by-hash justify fetch (:3352), missing-PC fetch (:3510), parked header (:362-395).
- **Body / DA misses.** The vote waits for `Held` and re-checks every 20 ms.
  - Missing parent: by-hash fetch (:3387) with a 4 s deadline, then sync (60 s trigger with futile backoff).
  - MissingData: retried every 500 ms for up to 3 s, at most 64 tracked; then sync.
- **Cadence bistability** (§118). Small fixed costs move the equilibrium, and two builds of the same code differ (8z
  vs 9f).
- **Timeout tails.** The deadline grid is absolute per pacemaker epoch, with a rebase clamp. A dead view costs about
  1.2-2.4 s, more under backoff (multiplier up to 2^8). Block p99 of 640-920 ms is unattributed outside checkpoint
  windows (T5.2, excluded).
- **Crash windows.**
  - Commit write before feed: covered by S444 boot / live reconcile (`algorithm.rs:168-178,287-311`,
    `committed_feed.rs:106`) and the F-1 manifest watchdog (`app.rs:11280-11293`).
  - The vote-state write uses default `WriteOptions` (WAL, no fsync, `kv_store.rs:191`). It is safe across a
    process crash; power-loss safety is not established here (observation only).
- **Equivocation.** Speculative rollback is log-only in CTE (`app.rs:10779`). Slashing needs on-chain evidence.

## 6. Corners that look unexamined (observations, not proposals)

Each item was checked against survey §1-§13, `8k-explore-ideas.md` and memory. "Known-open" marks items listed
elsewhere.

1. **Proposer body maps are never pruned.**
   - `HotStuff.pending_bodies` (`implementation.rs:124`) gets an insert on every own proposal (:981, :2107). Its
     only remover, `take_pending_body` (:547), has **no caller** (repo grep).
   - The swarm `block_store` (`swarm.rs:402`, insert :2473) also never evicts.
   - mem f774b73e (s72) noted "never pruned" in passing. Nothing records its size or a fix.
   - Rough size: ~13 KB datum × ~5 own proposals/s at 15.5 blk/s ≈ 0.13 MB/s ≈ 0.5 GB/h per node for both maps
     together. Not checked against RSS; the soaks report RSS slopes but do not attribute them.
2. **Votes wait in 20 ms steps when the parent or DA bodies arrive late.**
   - If the child's body arrives before its parent is inserted (or before its DA bodies),
     `vote_if_body_held` sets recheck = now + 20 ms (:2678, :2695).
   - A later parent insertion does not re-trigger the vote: `drain_deferred_bodies` (:3035) never calls
     `vote_if_body_held`. DA absorption does not re-trigger it either.
   - `tick_body_vote` returns early until the re-check is due (:2806).
   - With unanimity, this delay is on the QC path. No counter records which votes went through the re-check.
3. **Collector runs the commit feed before the view change** (:1727 runs before :1756).
   - The feed is 94-96% of `qc_to_advance` (§118).
   - `TORUS_DEFER_COMMIT_FEED` covers only the leader path (:897-973).
   - The survey lists the collector variant "broadcast before feed" merged with "local advance".
   - Every defer / advance verdict (s65 / s66 / s70 / s72) was measured at 1.2-1.3 blk/s. Today's 8-16 blk/s, where
     the feed is ~1/3 of the commit interval, was never measured. Neighbour: "proposer commit on advance view"
     (unverified, mem 8b1039de / 8ab1bc0e).
4. **A follower's vote waits for the parent to be inserted, not just held.**
   - `check_parent_link` reads the parent header from the tree (`app.rs:3025-3039,3242`).
   - Insertion does three things in one call (`try_insert_body` :2858-2990): validate, DA reconstruct, and the
     grandparent's feed with the blocking exec send.
   - So `vote_delay` inherits feed time and exec backpressure, even when the parent datum is already in hand (pending
     header or deferred body).
   - The check itself is an owner decision (s84 decision 2, mem ef79ac07). This corner is only about where the parent
     header comes from.
   - Known nearby: validate off-thread plans (BS-4a, async-validate, validation-worker), none benched on.
5. **Followers Keccak-hash every committed body twice.**
   - First pass: the `cached_matches_compact` cold path hashes every body to compare against the compact hashes
     (`app.rs:1993-2001`).
   - Second pass: `dispatch_to_exec` hashes them all again (:11505-11513).
   - The leader skips the first pass via `with_hashes`.
   - Known-open: "reuse CompactBlock hashes in dispatch" (s64 #4, survey §6). The compare-only first pass is not
     listed.
   - Both passes run inside `enqueue minus send` (11-17 ms per block), which sits inside `qc_to_advance` on the
     collector.
6. **Block-tree getters are uncached in the loop.**
   - `highest_pc`, `locked_pc`, `validator_set_state` and `highest_committed_block_height` each do a RocksDB get plus
     a borsh decode on every call (`internal.rs:1019-1045`).
   - Callers include the pacemaker tick, the block-sync tick (`client.rs:183-187`) and the handlers.
   - `contains()` (:1056) materializes the full block with all its datums (`pluggables.rs:37-65,133-151`).
   - Small today: all non-feed steps take 1.6 ms per view (§118). The cost scales with datum size, for example inline
     EVM txs in big-lane blocks.
7. **The leader's self-vote travels the network loopback** (when the defer flag is off).
   - Path: broadcast → self-enqueue in swarm (`swarm.rs:2383`) → poller (250 µs sleep) → algo.
   - An inline self-vote exists only on the defer path (:950-970).
   - Cheap now (loopback ~0.35 ms), but it is on the unanimity path.
8. **35-38% of views are entered via a peer's AdvanceView** (§118).
   - A leader that entered this way runs its `enter_view` update, with commit and feed, before sending its header
     (:908-912, defer off).
   - No data splits out how much of propose time this costs.
9. **Instrumentation gaps on the view path.** There is no counter for:
   - votes cast immediately vs via re-check;
   - which replica's vote completed each QC (the unanimity laggard);
   - parent-insert wait inside `vote_delay`;
   - AdvanceView publishes per view.
