export const meta = {
  name: 'exp-s58-perf-search-cont',
  description: 'Continues killed run wf_251473a7-7d3 (exp-s58-perf-search): replays its 30 finished agents from saved results, re-runs the 20 unfinished round-1 verifiers, then the later research rounds, picks, flag-gated test-first builds with Codex review and audit, integrated on exp/archy-s58-perf-search; excludes 18c s123 work',
  whenToUse: 'Continuation of the killed s58 perf-search run: same logic, finished agents replayed',
  phases: [
    { title: 'Prior art', detail: '18c s123 and in-flight exclusions, perf history, current bottlenecks', model: 'opus' },
    { title: 'Research', detail: 'lens researchers; later rounds chosen by the planner', model: 'opus' },
    { title: 'Triage', detail: 'merge duplicates, drop redo of prior work, rank', model: 'opus' },
    { title: 'Verify', detail: 'premise/gain, determinism/safety, novelty/feasibility', model: 'opus' },
    { title: 'Plan', detail: 'next-round lenses, then final picks and specs', model: 'opus' },
    { title: 'Build', detail: 'one worktree per pick, test-first, serialized cargo, Codex quick review', model: 'opus' },
    { title: 'Audit', detail: 'adversarial implementation audit and fix loop', model: 'opus' },
    { title: 'Integrate', detail: 'merge onto the exp branch, full suites, release build, doc and bench plan', model: 'opus' },
  ],
}

const A = {"repo": "/home/oz/projects/torus-economy/Torus-hyperBFT", "base_sha": "a44000e0", "base_branch": "exp/archy-s58-perf-search", "wt_root": "/home/oz/projects/wt", "target": "/home/oz/.cargo-target-exp-s58", "lock": "/home/oz/.cargo-exp-s58.lock", "results_doc": "docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md", "state_file": "/home/oz/projects/torus-economy/.claude-state.json", "max_rounds": 3, "verify_cap_r1": 12, "verify_cap_later": 8, "max_new_lenses": 6, "max_picks": 6, "s123_seed": "18c's s123 perf-experiments workflow (wf_443f7b55-fcb, 2026-10-15, base main c612dc7f; memories 13db18e4b126e788, 686c9ac32eed6242, 73ea4a840468f4fb): 21 candidates, 7 survived adversarial checks, 4 implemented as default-off env-flag branches (fetched here; notes in docs/plans/exp-s123-<slug>.md on each branch):\n1. exp/s123-parallel-cancel-all-book-pass (TORUS_PARALLEL_CANCEL_ALL=N): batched cancel-all book pass (native_executor.rs exec_cancel_all_run) on the exec worker pool, one job group per market (market_workers.rs). WIP, not building yet.\n2. exp/s123-propose-defer-on-exec-queue (TORUS_PROPOSE_EXEC_WATERMARK; control arm TORUS_EXEC_QUEUE_DEPTH): leader defers a fresh proposal while its exec queue is at a watermark (hotstuff_rs App::proposal_backpressure + torus-consensus app.rs). Two consensus-timing review fixes pending.\n3. exp/s123-exec-action-keys-parallel (TORUS_EXEC_PARALLEL_KEYS): exec trust-cache keys computed in an order-preserving parallel verify pre-pass (torus-types eip712.rs). The sort-key half was deferred, not refuted.\n4. exp/s123-defer-book-blobs-in-layer-merge (TORUS_CKPT_DEFER_BOOKS): Classic book blobs merged into the checkpoint layer in the deferred part (torus-state checkpoint_layer.rs).\nDropped or folded: phase2-sharded-prepare (replaced by the zero-code TORUS_PARALLEL_ENGINE=8 A/B, queued on archy), exec-prep-stage and exec-fused-keys (folded into 3).\nRefuted, known by name: consensus-reuse-action-hashes (mem 93695d41711b2545: seed follower PendingProposal from compact hashes, pass hashes into dispatch prune, pool hashes into mirror, skip sig attestation; hotstuff-thread Keccak only ~1% CPU), next-leader-parent-path-slim (mem 6544729f533865cb: same hash reuse plus parallel bincode decode in NativeDaStore::get_batch_timed), liq-scan-parallel-prevalue (mem 966f49e6b842cec8: parallel liq_view of the 2,048-trader liquidation window; gain below noise, Phase 5 rewrites that path). About 11 more refuted candidates are not yet recovered by name.\nOther 18c work: checkpoint slicing branch expected next (sec 74 conclusion); item 7 touches hotstuff_rs and consensus app.rs; item 6 Phase 5 (Users mutated in place, inline sums per fill) in flight; TORUS_PARALLEL_ENGINE=N exists (byte-identical) and is to be benched at 300m."}

// Continuation of killed run wf_251473a7-7d3: finished agents return their saved results.
const SAVED = Object.assign({},
  await workflow({ scriptPath: "/home/oz/.torus/projects/-home-oz-projects-torus-economy-Torus-hyperBFT/f5cdb9ec-65cf-4a0b-8575-4b093f1a4332/workflows/scripts/exp-s58-cont-data-1.js" }),
  await workflow({ scriptPath: "/home/oz/.torus/projects/-home-oz-projects-torus-economy-Torus-hyperBFT/f5cdb9ec-65cf-4a0b-8575-4b093f1a4332/workflows/scripts/exp-s58-cont-data-2.js" }))
if (Object.keys(SAVED).length !== 30) throw new Error(`expected 30 saved results, got ${Object.keys(SAVED).length}`)
const replayed = []
const live = []
function agentC(prompt, opts) {
  if (Object.prototype.hasOwnProperty.call(SAVED, opts.label)) {
    replayed.push(opts.label)
    return Promise.resolve(SAVED[opts.label])
  }
  if (!live.length) log(`First live agent: ${opts.label} (${replayed.length} finished agents replayed so far)`)
  live.push(opts.label)
  return agent(prompt, opts)
}
log(`Continuing killed run wf_251473a7-7d3: ${Object.keys(SAVED).length} saved agent results loaded`)
const R = { agentType: 'explore', model: 'opus', effort: 'max' }
const B = { agentType: 'builder', model: 'opus', effort: 'xhigh' }

const S = (props, req) => ({ type: 'object', properties: props, required: req })
const str = { type: 'string' }
const num = { type: 'number' }
const bool = { type: 'boolean' }
const arr = (it) => ({ type: 'array', items: it })
const en = (vals) => ({ type: 'string', enum: vals })

const PRIOR_SCHEMA = S({
  summary: str,
  items: arr(S({ name: str, kind: str, subsystem: str, change: str, outcome: str, source: str, avoid: bool }, ['name', 'kind', 'change', 'outcome', 'avoid'])),
  unrecovered: str,
}, ['summary', 'items'])
const BOTTLE_SCHEMA = S({
  metrics: str, headline: str,
  bottlenecks: arr(S({ area: str, regime: str, size: str, evidence: str, source: str }, ['area', 'size', 'evidence', 'source'])),
  artifacts: arr(S({ path: str, contents: str }, ['path', 'contents'])),
  noise_rules: str, host: str,
}, ['metrics', 'headline', 'bottlenecks', 'artifacts', 'noise_rules', 'host'])
const CAND = S({
  slug: str, title: str, mechanism: str, code_locations: arr(str), evidence: str, regime: str,
  metric_targets: arr(str), est_gain: str, est_gain_pct_matched: num, confidence: en(['low', 'medium', 'high']),
  determinism: str, flag: str, impl_size: str, test_plan: str, novelty_check: str, risks: str,
}, ['slug', 'title', 'mechanism', 'code_locations', 'evidence', 'est_gain', 'est_gain_pct_matched', 'confidence', 'determinism', 'flag', 'impl_size', 'test_plan', 'novelty_check'])
const CAND_SCHEMA = S({ candidates: arr(CAND), notes: str }, ['candidates'])
const TRIAGE_SCHEMA = S({
  fresh: arr(S({ id: str, merged_from: arr(str), score: num, note: str }, ['id', 'score', 'note'])),
  dropped: arr(S({ id: str, reason: str, prior_item: str }, ['id', 'reason'])),
}, ['fresh', 'dropped'])
const VERDICT_SCHEMA = S({
  keep: bool, severity: en(['fatal', 'major', 'minor', 'none']), summary: str, verified: str, refuted: str,
  corrected_gain: str, corrected_gain_pct_matched: num, constraints: str, closest_prior: str,
}, ['keep', 'severity', 'summary', 'corrected_gain', 'constraints'])
const LENS = S({ key: str, title: str, focus: str }, ['key', 'title', 'focus'])
const PLAN_SCHEMA = S({ continue: bool, rationale: str, lenses: arr(LENS) }, ['continue', 'rationale', 'lenses'])
const PICK_SCHEMA = S({
  picks: arr(S({
    id: str, slug: str, title: str, flag: str, files: arr(str), spec: str, tests_first: str, acceptance: str,
    microbench: str, expected_gain: str, bench_measure: str, constraints: str, conflicts: str,
  }, ['id', 'slug', 'title', 'flag', 'files', 'spec', 'tests_first', 'acceptance', 'expected_gain', 'constraints'])),
  knob_arms: arr(S({ name: str, env: str, rationale: str }, ['name', 'env', 'rationale'])),
  not_picked: arr(S({ id: str, reason: str }, ['id', 'reason'])),
  rationale: str,
}, ['picks', 'knob_arms', 'not_picked', 'rationale'])
const BUILD_SCHEMA = S({
  status: en(['done', 'partial', 'failed']), branch: str, worktree: str, head: str, flag: str, files_changed: str,
  tests: str, clippy_fmt: str, codex: str, microbench: str, notes: str, open_issues: str,
}, ['status', 'branch', 'worktree', 'head', 'tests', 'codex', 'notes'])
const AUDIT_SCHEMA = S({
  ok: bool, flag_off_identical: bool, deterministic: bool, tests_adequate: bool,
  blocking: arr(S({ issue: str, where: str, fix: str }, ['issue', 'where', 'fix'])),
  nonblocking: str, summary: str,
}, ['ok', 'flag_off_identical', 'deterministic', 'tests_adequate', 'blocking', 'summary'])
const INTEG_SCHEMA = S({
  status: en(['done', 'partial', 'failed']), branch: str, head: str, worktree: str,
  merged: arr(S({ slug: str, flag: str, merge_commit: str }, ['slug', 'merge_commit'])),
  skipped: arr(S({ slug: str, reason: str }, ['slug', 'reason'])),
  tests: str, clippy_fmt: str, release_build: str, doc: str, bench_plan: str, open_issues: str,
}, ['status', 'branch', 'head', 'merged', 'skipped', 'tests', 'doc', 'bench_plan'])

const short = (s, n) => { const t = String(s || ''); return t.length > n ? t.slice(0, n) + '...' : t }
const slugify = (s) => (String(s || '').toLowerCase().replace(/[^a-z0-9-]/g, '-').replace(/-+/g, '-').replace(/^-|-$/g, '').slice(0, 40)) || 'exp'

const CTX = `CONTEXT
- Project: Torus-hyperBFT, a Rust HotStuff-family BFT chain with a native perp order-book DEX. Key crates: hotstuff_rs; torus-consensus (src/app.rs); torus-bridge (native_executor.rs, market_workers.rs, exec_pool.rs, liquidation_step.rs); torus-core (order_book.rs); torus-state (backend.rs, checkpoint_layer.rs, incremental.rs, native_trie.rs, native_da.rs, bg_writer.rs); torus-mempool; torus-network; torus-types (eip712.rs); torus-rpc; torus-node.
- Repo ${A.repo}. main = ${A.base_sha} (just pulled; includes section 74 / 71.8 option A: HotStuff store and DA store in their own RocksDB). The primary checkout stays on main: never switch its branch or edit files in it.
- Machine: ozarchy ("archy"), 32 threads, 62 GB RAM, also the bench host (18c: 3 validator nodes share one 32-thread host, so extra threads per node compete). Bench harness: tools/matched-bench (README.md, run-cell.sh, campaign drivers); cells at 10 markets (10m) and 300 markets (300m). Results doc ${A.results_doc} (sections up to 74); docs/perf/bench-archive/; other docs/perf/*.md; plans in docs/plans/.
- Metrics: matched/s (headline throughput), native blk/s (blockspeed), exec block_ms, block p99, CPU-s/1M matched, RSS, bytes written, consensus timeouts. AGREE (all nodes reach identical state) must hold in every cell.
- Reference points to re-check, not trust blindly: n=4 paired CI about +-1.3% matched/s (sec 67.3); at 300m about +3.0% matched/s per -10.2% exec block_ms (sec 67.4); 300m is exec-bound; 10m is wave-shaped (sec 70.5).
- Look before grepping: .torus/GRAPH_REPORT.md, the indexer (code_search, code_graph) and memory (search_knowledge with project torus-hyperbft).`

const PA_S123 = `${CTX}

TASK (read-only research): Build the complete exclusion list of perf work that 18c (the other machine) has done or is doing, so a new perf search does not redo it.
1. 18c's s123 perf-experiments workflow. Already known:
${A.s123_seed}
Recover as many of the 21 candidates by name as you can: search_knowledge with many phrasings ('adversarial check of candidate', 's123', 'c612dc7f', 'keep=false', 'REJECT', 'Selection 2026-10-15', 'experiment-s123', 'refuted', plus topic words: cancel-all, exec queue, trust-cache keys, book blobs, liquidation, action hashes, DA decode, sort key, prepare, settle, trie, checkpoint, select, mempool, funding, trades, network), get_memory on every relevant hit, and read the 4 notes files with git show <branch>:docs/plans/exp-s123-<slug>.md (they also mention ideas they considered and rejected). For each: what it proposed (files, flag), verdict and why, source.
2. 18c's other in-flight or announced perf work: checkpoint slicing (expected after section 74), item 7, item 6 Phase 5 steps, anything on remote branches (git branch -r --sort=-committerdate | head -40; git log -3 origin/<b>) or in docs/plans that is planned but not merged, and recent 18c memories (machine:18c, last 7 days) about perf plans.
3. archy's own queue: next_steps in ${A.state_file} (e.g. the TORUS_PARALLEL_ENGINE A/B).
Return one item per idea: name, kind (s123-implemented | s123-dropped | s123-refuted | in-flight | queued | planned), change, outcome/status, source, avoid (true for anything a new search must not redo). In 'unrecovered' say plainly which of the 21 s123 candidates you could not identify. Do not edit files; do not run cargo.`

const PA_HIST = `${CTX}

TASK (read-only research): Build a compact map of perf work ALREADY DONE on Torus-hyperBFT so a new search does not repeat it: merged optimizations, measured no-gain experiments, refuted ideas, shelved items, and existing TORUS_* env knobs with their measured outcome.
Sources: ${A.results_doc} (sections 1-74: skim headings, read each conclusion); the other docs/perf/*.md (action-hash-scratch, body-fetch, cancel-batch, cap100 gap attribution, consensus-thread attribution, exec-pipeline, load-improvements, matched-200k, native-da-body-scratch, packed-trade-history, s63/s76 docs, ...); docs/plans/*.md (plan outcomes); git log --oneline main | grep -iE 'perf|exp|speed|fast|parallel|batch|cache' (merged perf work); memory (search_knowledge: 'shelved-gain sweep' = cb3604fd9dd0ffe9, 'no gain', 'refuted', 'A/B result', 'campaign RESULT', 'owner decision perf', 'reverted').
Knobs: rg -o --no-filename 'TORUS_[A-Z0-9_]+' crates | sort -u; per knob record default, purpose and any measured result.
Return one item per idea or knob (kind: merged | no-gain | refuted | shelved | knob) with subsystem one of: exec-matching, exec-pipeline, state-rocksdb-checkpoint-trie, consensus-hotstuff, mempool-ingress-verify, network-da, alloc-hash-serde, liquidation-adl-funding-margin, trades-history-rpc, bench-harness. avoid=true for merged, no-gain and refuted items; avoid=false for shelved items or knobs worth revisiting because the baseline changed (say why in outcome). Keep each item short; completeness beats prose. Do not edit files; do not run cargo.`

const PA_BOTTLE = `${CTX}

TASK (read-only research): Describe where time and CPU go on current main ${A.base_sha} under the matched-bench workload, so researchers aim at real bottlenecks.
Read: ${A.results_doc} sections 60-74 (esp. 67.x exec-bound at 300m; 70.5/70.6 per-wave cycle and end-of-block worker; 71.x consensus-thread in-window write waits by call site and target DB; 73 admission sweep and jemalloc decay; 74 own-DB A/B: the remaining stall is the state-DB write queue, commit-manifest put +25 ms per call; out-of-window block p99 ~940 ms; ~1.2 s p99 floor); docs/perf/bench-archive/ (per-campaign tools/analyst dirs: per-thread CPU, perf stacks or flamegraph text, timers, keccak-by-thread); tools/matched-bench/README.md (what a cell is: markets, load shape, tail depth, duration; how each metric is computed); memory for the latest campaign results (ozarchy-hsdb, p4j4, p4swap10, liq2).
Return: metrics (definitions and how computed); headline (latest numbers at 10m and 300m with dates and sources: matched/s, native blk/s, exec block_ms, block p99, CPU-s/1M, RSS); bottlenecks (ranked: thread or phase, ms per block or CPU share, regime, evidence, source; say what bounds throughput at 10m vs 300m); artifacts (exact paths researchers can mine and what each holds); noise_rules (CI sizes, n, decision rules such as any 5% rule); host (nodes per host, threads, pools). Do not edit files; do not run cargo or profilers.`

const fmtItems = (items) => (items || []).map(i => `- [${i.avoid ? 'AVOID' : 'revisit?'}] ${i.name} (${i.kind}${i.subsystem ? ', ' + i.subsystem : ''}): ${i.change} => ${i.outcome}${i.source ? ' [' + i.source + ']' : ''}`).join('\n')
const fmtBottle = (b) => [
  'Metrics: ' + b.metrics,
  'Headline: ' + b.headline,
  'Bottlenecks:\n' + (b.bottlenecks || []).map(x => `- ${x.area}${x.regime ? ' [' + x.regime + ']' : ''}: ${x.size}. ${x.evidence} [${x.source}]`).join('\n'),
  'Profile artifacts:\n' + (b.artifacts || []).map(x => `- ${x.path}: ${x.contents}`).join('\n'),
  'Noise and decision rules: ' + b.noise_rules,
  'Host: ' + b.host,
].join('\n')

phase('Prior art')
log(`Base ${A.base_branch} = main ${A.base_sha}; worktrees under ${A.wt_root}; cargo target ${A.target}, lock ${A.lock}`)
const prior = await parallel([
  () => agentC(PA_S123, { ...R, label: 'prior:18c-s123', phase: 'Prior art', schema: PRIOR_SCHEMA }),
  () => agentC(PA_HIST, { ...R, label: 'prior:history', phase: 'Prior art', schema: PRIOR_SCHEMA }),
  () => agentC(PA_BOTTLE, { ...R, label: 'prior:bottlenecks', phase: 'Prior art', schema: BOTTLE_SCHEMA }),
])
const pa1 = prior[0], pa2 = prior[1], pa3 = prior[2]
const BRIEF = [
  '== 18c s123 seed (authoritative) ==\n' + A.s123_seed,
  '== 18c and in-flight work ==\n' + (pa1 ? pa1.summary + '\n' + fmtItems(pa1.items) + (pa1.unrecovered ? '\nNot recovered: ' + pa1.unrecovered : '') : '(prior-art agent failed: rely on the seed above and check memory yourself)'),
  '== Perf history: merged, no-gain, refuted, shelved, knobs ==\n' + (pa2 ? pa2.summary + '\n' + fmtItems(pa2.items) : '(history agent failed: check docs/perf and memory yourself)'),
  '== Current bottlenecks on main ==\n' + (pa3 ? fmtBottle(pa3) : '(bottleneck agent failed: read sections 67-74 yourself)'),
].join('\n\n')
log(`Prior art: ${pa1 ? pa1.items.length : 0} 18c/in-flight items, ${pa2 ? pa2.items.length : 0} history items, ${pa3 ? pa3.bottlenecks.length : 0} bottlenecks; brief ${BRIEF.length} chars`)

const ROUND1 = [
  { key: 'match-core', title: 'Matching engine and order-book data structures',
    focus: 'torus-core order_book.rs (place_order, cancel, modify, fill generation, price-level containers, Classic book) and the per-action path into it from torus-bridge native_executor.rs. Find per-order CPU you can remove: redundant lookups, allocation per fill or level, key hashing, cloning of orders/fills, linear scans over levels or orders, repeated fee/margin math per fill, cache-unfriendly layout. Size each against the measured exec cost per block at 300m (exec-bound). Outputs must stay bit-identical.' },
  { key: 'exec-pipeline', title: 'Native block execution pipeline and parallelism',
    focus: 'torus-bridge native_executor.rs, market_workers.rs, exec_pool.rs, parallel settle and trade defer, and how torus-consensus app.rs hands blocks to exec (dispatch_and_execute, execution_loop). Map the phases of one native block with measured ms (sections 67-70: phase1/phase2, settle, end-of-block worker, per-wave cycle), find serial sections and idle workers (Amdahl), overlap between block N tail and block N+1 head, and work that can leave the critical path. Several nodes share the host threads. Do NOT redo 18c s123 picks (parallel cancel-all pass, parallel exec action-keys pre-pass, sharded prepare) or the queued TORUS_PARALLEL_ENGINE A/B.' },
  { key: 'state-write', title: 'State write path, RocksDB and checkpointing',
    focus: 'torus-state backend.rs overlay and write batches, bg_writer.rs, checkpoint_layer.rs, incremental.rs state hash, native_trie.rs, commit manifest, CF layout, and RocksDB options (write buffers, WAL, compaction, block cache, bloom filters, pipelined or unordered writes, write_buffer_manager, rate limiter, sync). Section 74: the remaining stall is the state-DB write queue (commit-manifest put +25 ms per call). Find ways to cut write amplification, avoid synchronous waits on consensus or exec threads, or batch/merge writes. NOT checkpoint slicing (18c in flight), NOT TORUS_CKPT_DEFER_BOOKS (18c s123), NOT 71.8 option A (merged).' },
  { key: 'consensus-cadence', title: 'Consensus cadence, proposal timing and the block p99 floor',
    focus: 'hotstuff_rs implementation.rs (tick, on_receive_msg, pacemaker, proposal/vote/QC paths, view timeouts) and torus-consensus app.rs (leader select and propose, produce_block, validate, commit feed, exec handoff). Find what sets the block interval and its tail: the ~1.2 s block p99 floor outside checkpoint windows (sec 74: out-of-window p99 ~940 ms), the 10m wave shape (sec 70.5: full 400-action blocks then idle empty blocks), timer and poll granularity (e.g. ~10 ms polls), blocking work on the consensus thread, vote/QC latency. Do NOT redo propose-defer-on-exec-queue (TORUS_PROPOSE_EXEC_WATERMARK, TORUS_EXEC_QUEUE_DEPTH), consensus-reuse-action-hashes or next-leader-parent-path-slim (refuted), s66 TORUS_DEFER_COMMIT_FEED or s72 defer parent feed (no gain).' },
  { key: 'ingress-verify', title: 'Ingress, mempool selection, signature verification and block packing',
    focus: 'torus-rpc ingress, torus-mempool (lib.rs, native_pool.rs select_for_block_with_senders_excluding, rate limit and trust cache), torus-types eip712.rs (signature recovery and verification cost, batching, caching), and block packing policy (actions-per-block caps, select limits) and its effect on matched/s versus blocks/s. Find per-action CPU on the leader select/propose path and the follower validate path, redundant verification between ingress and exec, and packing knobs that raise matched/s without hurting p99. Do NOT redo exec-action-keys-parallel (parallel verify pre-pass; its sort-key half is deferred to 18c) or measured anti-spam work.' },
  { key: 'network-da', title: 'Block body propagation, DA and networking',
    focus: 'torus-network (swarm.rs, gossipsub, QUIC transport, body send paths and traces), torus-state native_da.rs and erasure.rs, follower body fetch, header/body split, serialization and compression of bodies. Measured: body-late views (33-48% of views in s76), DA decode 12-16 ms mean on late views, body-fetch queue-time diagnostics (docs/perf/body-*). Find latency on the critical path from leader proposal to follower vote: encode/decode cost, copies, extra round trips, send-queue behavior, chunk sizes. Do NOT redo parallel DA decode (refuted in s123) or s76 flush-on-miss.' },
  { key: 'alloc-hash-serde', title: 'Allocation, hashing and serialization across threads',
    focus: 'Cross-cutting CPU: allocation (clones, Vec/HashMap growth, Box per item), jemalloc behavior (compiled in; decay knob measured in sec 73), hashing (ahash already used for exec maps; Keccak ~0.07-0.09 cores on the hotstuff thread), bincode/serde encode/decode of actions, blocks, state rows and trade/history rows. Use existing perf stacks in docs/perf/bench-archive to find top self-time in alloc/free/memcpy/hash/serde and the call sites responsible; also RSS growth sources. Do NOT redo seeded ahash (s82 / P2-5) or jemalloc compile-in.' },
  { key: 'profile-mining', title: 'Profile mining: hottest unaddressed code',
    focus: 'Start from data, not code: the latest perf stacks, per-thread CPU and timers in docs/perf/bench-archive/ (e.g. ozarchy-p3cloidprof-tools, ozarchy-hsdb-tools, p4j4, liq2) and the latest results sections. Rank functions and threads by self and inclusive CPU at 300m and 10m; for each top entry check whether a prior or in-flight item addresses it (brief); for the unaddressed ones find the code-level reason it is hot and propose a fix. Report the evidence path and share for every candidate.' },
  { key: 'locks-threads-tail', title: 'Locks, thread pools, scheduling and tail stalls',
    focus: 'Lock contention on hot paths (executor memo Mutex, overlay pending RwLock, mempool locks, channel handoffs), pool sizing and oversubscription with several nodes on one 32-thread host (exec pool, rayon global pool used by eip712 phase 1, RPC ingress pool, bg writers, RocksDB background jobs), wakeup latency between consensus and exec threads, fsync/compaction/flush stalls and checkpoint windows that set block p99. Known: CPU pinning no-gain (s75); P2-3 spawn cost small (41 spawns per native block).' },
  { key: 'per-block-background', title: 'Per-block maintenance: liquidation, funding, ADL, margin, history, pruning',
    focus: 'Work that runs every block or every N blocks regardless of user actions: liquidation scans (liquidation_step.rs), ADL, funding, mark/oracle updates, margin sums, cancel-all bookkeeping, trade history, action status and stream records, pruning and retention passes. Find work that can be skipped when inputs did not change, made incremental, batched across blocks, or moved off the exec critical path while staying deterministic. Known: liq-scan-parallel-prevalue rejected by 18c; Phase 4 persistent sums merged; Phase 5 (Users mutated in place, inline sums per fill) in flight, so do not collide with it.' },
]

const VLENSES = [
  { key: 'gain', text: `LENS: premise and gain. Verify every factual claim against the code on main ${A.base_sha} and the measured data. Recompute the gain yourself from measured numbers, not from the candidate's estimate: which cost is removed, on which thread, is that thread on the critical path in this regime, and what that does to matched/s, blk/s, block_ms, p99 and CPU-s/1M. Refute (severity fatal) if the premise is false on current main, if the removed cost is much smaller than claimed, or if the corrected gain is below bench noise with no measurable CPU-s/1M, p99 or RSS benefit. If you cannot confirm the gain from data, keep=false.` },
  { key: 'safety', text: 'LENS: determinism and safety. Every node must produce byte-identical state and state hash for the same blocks whatever the flag value and thread timing; with the flag unset behavior must equal main; consensus safety and liveness (timeouts, view changes, leader behavior) must not regress; crash/restart, fail-stop and fault paths must stay correct; concurrency must be sound (ordering, locks, Send/Sync, panics in workers). Refute (fatal) only if it cannot be made safe; otherwise keep and list the exact constraints the implementation must meet.' },
  { key: 'novelty', text: 'LENS: novelty and feasibility. Compare against the brief, memory (search_knowledge, several phrasings), git history (git log --all --oneline -S or -G on the key symbol, recent branches) and docs/perf plus docs/plans. Refute (fatal) if it repeats or substantially overlaps 18c s123 experiments or rejected candidates, any measured no-gain or refuted item, the queued TORUS_PARALLEL_ENGINE A/B, or in-flight work (checkpoint slicing, item 7, Phase 5): name the prior item and its source. Also judge whether it is implementable as a default-off node-local env-flag experiment of reasonable size with tests, and note file conflicts with likely sibling experiments.' },
]

const seen = []
const survivors = []
const rejected = []
let backlog = []
const rounds = []
const lensesUsed = []

const seenText = () => seen.map(x => `- ${x.id} ${x.slug}: ${x.title} [${x.status}${x.reason ? ': ' + short(x.reason, 200) : ''}]`).join('\n')
const survText = () => survivors.map(s => `- ${s.cand.slug}: ${s.cand.title} | est ${short(s.cand.est_gain, 160)} | ${s.verdicts.map(v => v.lens + '=' + (v.keep ? 'keep' : 'refute') + ' (' + short(v.corrected_gain, 100) + ')').join('; ')}`).join('\n')
const rejText = () => rejected.map(r => `- ${r.slug}: ${r.title} [${r.stage}] ${short(r.reason, 260)}`).join('\n')
const backText = () => backlog.map(x => `- ${x.cand.slug}: ${x.cand.title} (triage score ${x.cand.triage_score || 0})`).join('\n')

function researchPrompt(lens, round) {
  const extra = round > 1
    ? `\nEARLIER ROUNDS (do not repeat survivors or rejected ideas):\nSurvivors:\n${survText() || '(none)'}\nRejected or dropped:\n${rejText() || '(none)'}\n`
    : ''
  return `${CTX}

TASK: You are a performance researcher (read-only). Find NEW, evidence-backed optimizations that raise matched/s and native blk/s, or cut exec block_ms, block p99, CPU-s/1M or RSS on current main ${A.base_sha}.
Round ${round}. Your lens: ${lens.title}
${lens.focus}

PRIOR-WORK BRIEF (authoritative: never propose an AVOID item, especially 18c's s123 experiments and their rejected candidates, nor queued or in-flight work; a revisit? item only if you show what changed since it was measured):
${BRIEF}
${extra}
METHOD
1. Ground every claim in code on main (path:line) and in measured data (doc section, bench-archive file, memory id). Check the code does not already do it.
2. Estimate the gain with arithmetic from measured numbers (ms or CPU share removed -> metric), name the regime (10m and/or 300m) and compare with the noise.
3. Self-refute before reporting: already tried? premise true on current main? deterministic (every node computes byte-identical state; a node-local flag may change how, never what)? collides with in-flight work?
4. Each idea must be implementable as a node-local env flag, default off, with flag-off behavior identical to main.
5. Read-only: do not edit files; do not run cargo, tests, benches or profilers (one cargo job at a time on this machine, reserved for builders).
Return at most 4 candidates, best first, each with a unique kebab-case slug. Fewer is fine; zero is fine. No weak or speculative padding.`
}

function triagePrompt(round, pool) {
  return `${CTX}

TASK: Triage judge for round ${round} of a perf-idea search (read-only; no edits, no cargo).
1. Merge duplicates in this pool (same mechanism or same code path): keep one representative id in fresh and list the others in merged_from.
2. Drop candidates that redo prior work: any AVOID item in the brief (18c s123 experiments and rejected candidates, the queued TORUS_PARALLEL_ENGINE A/B, checkpoint slicing, Phase 5 or item 7 work, measured no-gain items) or anything already handled in earlier rounds (list below). Name the prior item.
3. Drop candidates with no code location, a premise you can see is false, or a gain clearly below noise with no measurable CPU-s/1M, p99 or RSS benefit.
4. Score the rest 0-100 (expected gain x confidence / effort) with a one-line note.
Spot-check code only to settle duplicate, redo or premise questions; full verification comes later. Items marked carried=true were triaged before but not yet verified: re-rank them with the new ones. Every pool id must appear exactly once: as a fresh representative, inside one merged_from, or in dropped.

PRIOR-WORK BRIEF:
${BRIEF}

ALREADY HANDLED IN EARLIER ROUNDS:
${seenText() || '(none)'}

POOL (JSON):
${JSON.stringify(pool)}`
}

function verifyPrompt(cand, dupes, v) {
  return `${CTX}

TASK: Adversarially verify one perf candidate. Read-only: do not edit files, do not run cargo. Try to REFUTE it.
${v.text}

PRIOR-WORK BRIEF:
${BRIEF}

CANDIDATE (JSON):
${JSON.stringify(cand)}
${dupes.length ? 'MERGED DUPLICATES (extra evidence, JSON):\n' + JSON.stringify(dupes) : ''}`
}

function plannerPrompt(round) {
  return `${CTX}

TASK: Plan the next round of a dynamic perf-idea search (read-only; no edits, no cargo). Round ${round} just finished.
First save one memory summarizing round ${round}: every verified candidate with keep or reject and the one-line reason (tags: project:torus-hyperbft,machine:ozarchy,experiment-s58,type:decision,area:backend).
Then decide whether another round is likely to find NEW, verifiable wins of meaningful size. If yes, propose up to ${A.max_new_lenses} lenses: unexplored angles (subsystems, cross-cutting effects, workload regimes, measurement blind spots, the existing-knob space), deeper dives where survivors cluster (the next-largest measured cost in the same phase), or reframings of rejected ideas that fix the reason they failed. Never repeat a rejected idea or an AVOID item. Each lens: key (new kebab-case), title, focus (3-6 sentences: what to examine, which files, which measured cost to attack, what to avoid). If returns are diminishing, set continue=false.

PRIOR-WORK BRIEF:
${BRIEF}

LENSES USED SO FAR:
${lensesUsed.join('\n')}

SURVIVORS SO FAR:
${survText() || '(none)'}

REJECTED OR DROPPED SO FAR:
${rejText() || '(none)'}

TRIAGED BUT NOT YET VERIFIED (carried into the next triage):
${backText() || '(none)'}`
}

function pickPrompt(survJson) {
  return `${CTX}

TASK: Choose the experiments to implement now (read-only; read code as needed to write precise specs; no edits, no cargo). Each pick is built in its own worktree on branch exp/archy-s58-<slug> off ${A.base_branch} (= main ${A.base_sha}) behind its own node-local env flag (default off, flag-off identical to main), then all are merged onto ${A.base_branch}.
Rules: up to ${A.max_picks} picks, ordered by expected gain x confidence / effort; honor every verifier constraint; avoid pairs that rewrite the same functions (or say how to sequence them in conflicts); nothing that redoes 18c s123 work or other AVOID items. Zero-code experiments (existing env knobs or config values worth benching) go in knob_arms, not picks. Slugs kebab-case, unique, at most 40 chars. Flag names TORUS_<SOMETHING> not already used in the code (check with grep).
For each pick write a spec a builder can follow without re-researching: exact files and functions (path:line on ${A.base_sha}), the change, the flag and how it is read (once, cached), tests to write first (what fails, or what equivalence they prove, including flag-on vs flag-off identical results), acceptance criteria, an optional microbench (only if cheap and meaningful), the expected gain and how the bench should see it (bench_measure).
Afterwards save one memory listing the picks, knob arms and not-picked survivors with reasons (tags: project:torus-hyperbft,machine:ozarchy,experiment-s58,type:decision,area:backend).

PRIOR-WORK BRIEF:
${BRIEF}

VERIFIED SURVIVORS (JSON, with verdicts and constraints):
${survJson}`
}

const setupRules = (wt) => `- Cargo: other builders run in parallel and this machine allows one cargo job at a time. Run EVERY cargo command from ${wt} as:
  flock ${A.lock} env CARGO_TARGET_DIR=${A.target} cargo <args>
  For anything longer than a minute use Bash run_in_background and wait for its completion notification. Never kill a cargo or rustc process you did not start. Check df -h /home before the first build; stop and report if under 25 GB free.
- Edit .rs files through Bash (python3 exact-string replacement, heredoc or git apply), not the Edit/Write tools: Edit/Write on .rs files start the session's rust-analyzer, which runs its own cargo check outside the lock. Markdown files may use Write.
- Tests per TESTING.md (nextest output flags; -E 'rdeps(<crate>)' for leaf crates; -p <crate> plus cargo check --workspace --tests -q for core crates).
- rm/rmdir: never with an unguarded shell variable; literal absolute paths or "\${VAR:?}" only.
- Do not push. Commit messages end with the line: Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`

function buildPrompt(p) {
  const wt = `${A.wt_root}/exp-s58-${p.slug}`
  const br = `exp/archy-s58-${p.slug}`
  return `${CTX}

TASK: Implement one perf experiment, test-first, and commit it on its own branch.

EXPERIMENT ${p.slug}: ${p.title}
Flag: ${p.flag} (node-local env var, read once and cached, default off)
Files: ${(p.files || []).join(', ')}
Spec:
${p.spec}
Tests to write first:
${p.tests_first}
Acceptance:
${p.acceptance}
Verifier constraints (must hold):
${p.constraints}
${p.microbench ? 'Optional microbench:\n' + p.microbench : ''}
Expected gain: ${p.expected_gain}
${p.conflicts ? 'Possible conflicts with sibling experiments: ' + p.conflicts : ''}

SETUP
- Worktree: git -C ${A.repo} worktree add ${wt} -b ${br} ${A.base_branch}. If it already exists from an earlier attempt, reuse it. If git reports a lock, wait a few seconds and retry. Work only in ${wt}; never touch the primary checkout, other worktrees or other branches.
${setupRules(wt)}

STEPS
1. Read the spec's code on ${A.base_sha}. If the premise is false or it cannot be made deterministic, stop and return status failed with the reason.
2. Write the tests first and run them: they must fail for the right reason, or (equivalence tests) pin current behavior.
3. Implement. Flag unset: same code path and results as main. Flag set: byte-identical state results across nodes and runs (add a flag-on vs flag-off equivalence test where the spec asks).
4. Prove: targeted tests pass; cargo clippy on the changed crates with 0 new warnings; cargo fmt --check clean. Optional microbench: indicative only, through the lock; report numbers.
5. Add docs/plans/exp-archy-s58-${p.slug}.md (what changed, why with evidence, flag, tests, expected gain, how to bench it, risks). Commit on ${br} with subject "perf(exp): <what> (${p.flag}, default off)".
6. Codex quick review of the whole branch (background, allow ~20 min): cd ${wt} && mkdir -p /tmp/claude-$(id -u) && torus-codex review --base ${A.base_branch} > /tmp/claude-$(id -u)/codex-s58-${p.slug}.md 2>/dev/null. Read the file. Fix every P0/P1 and each valid P2 test-first, re-run the affected tests, commit the fixes. Record each finding and its disposition (if Codex is unavailable, say so).
Return the structured result; head = final commit sha.`
}

function auditPrompt(p, b, n) {
  const wt = `${A.wt_root}/exp-s58-${p.slug}`
  return `${CTX}

TASK: Adversarially audit one implemented perf experiment before it is merged onto ${A.base_branch} (audit ${n}). Read-only: do not edit files and do not run cargo (one cargo job at a time, reserved for builders); git and reading are fine.
Worktree ${wt}, branch exp/archy-s58-${p.slug}. Diff: git -C ${wt} diff ${A.base_branch}...HEAD
Experiment: ${p.title}; flag ${p.flag}
Spec:
${p.spec}
Constraints:
${p.constraints}
Builder report (JSON):
${JSON.stringify(b)}

Check, with path:line evidence:
1. Flag unset: behavior identical to main (no changed path; at most a cached flag read on the hot path).
2. Flag set: deterministic state across nodes and runs (iteration order, float math, thread timing, error, fault and fail-stop paths, panics in workers, partial failure).
3. Consensus safety and liveness unaffected or explicitly bounded.
4. Tests prove the claims (would they fail on a wrong implementation? flag-on vs flag-off equivalence where relevant?).
5. It implements the spec and attacks the measured cost; no dead or debug code; notes file present.
6. Codex findings handled correctly.
ok=false only with concrete blocking issues (where, why, fix). Style nits are nonblocking.`
}

function fixPrompt(p, audit, n) {
  const wt = `${A.wt_root}/exp-s58-${p.slug}`
  return `${CTX}

TASK: Fix blocking audit findings on perf experiment ${p.slug} (fix round ${n}) in the existing worktree ${wt} (branch exp/archy-s58-${p.slug}). Work only there.
${setupRules(wt)}

Blocking findings (JSON):
${JSON.stringify(audit.blocking)}
Fix each test-first (a test that fails before the fix where possible), re-run the affected tests, keep clippy and fmt clean, update docs/plans/exp-archy-s58-${p.slug}.md if behavior changed, and commit. If a finding is wrong, say why with evidence instead of changing code.
Spec for reference:
${p.spec}
Return the structured result; head = final commit sha.`
}

function integratePrompt(done, failed, knobArms, allCands) {
  const wt = `${A.wt_root}/exp-s58-perf-search`
  return `${CTX}

TASK: Integrate the finished perf experiments onto ${A.base_branch} and make it bench-ready.
SETUP: git -C ${A.repo} worktree add ${wt} ${A.base_branch} (reuse if it exists). Work only there.
${setupRules(wt)}

1. Merge each experiment below in the listed order: git merge --no-ff exp/archy-s58-<slug> -m "Merge exp/archy-s58-<slug>: <title> (<FLAG>, default off)". Resolve conflicts so each experiment keeps flag-off identity and its own semantics; if two cannot coexist, keep the earlier one and report the other as skipped.
2. Prove per TESTING.md "Before every commit": cargo nextest run --workspace with the output flags and cargo test --doc; plus cargo clippy --workspace --all-targets (0 new warnings vs main ${A.base_sha}; compare against main only if warnings appear) and cargo fmt --check. Fix integration breakage test-first. Report counts.
3. Release build: cargo build --release -p torus-node (through the lock) to prove the bench binary builds; report the binary path and md5.
4. Write docs/plans/exp-archy-s58-perf-search.md: (a) table of merged experiments (flag, what, expected gain, files, tests, Codex and audit status); (b) skipped or failed experiments and why; (c) the zero-code knob arms; (d) every candidate of the search with its verdict and reason (list below); (e) a bench plan for the bench agents: control = all experiment flags unset vs each flag alone, then all-on; 300m and 10m; ABBA n>=4; 600 s cells; metrics matched/s, native blk/s, exec block_ms, block p99, CPU-s/1M, RSS, consensus timeouts; AGREE every cell; per arm, which metric should move and by how much. Commit it.
5. Save one memory: branch head, merged experiments with flags, suite results, doc path (tags: project:torus-hyperbft,machine:ozarchy,experiment-s58,type:decision,area:backend).

EXPERIMENTS TO MERGE (in order, JSON):
${JSON.stringify(done)}

NOT MERGEABLE (JSON):
${JSON.stringify(failed)}

ZERO-CODE KNOB ARMS (JSON):
${JSON.stringify(knobArms)}

ALL CANDIDATES AND VERDICTS:
${allCands}`
}

function judge(vs) {
  if (vs.length < 2) return { ok: false, unverified: true, reason: 'fewer than 2 verifiers returned' }
  const fatal = vs.find(v => !v.keep && v.severity === 'fatal')
  if (fatal) return { ok: false, reason: `${fatal.lens}: ${fatal.summary}` }
  const keeps = vs.filter(v => v.keep).length
  if (keeps < 2) return { ok: false, reason: vs.filter(v => !v.keep).map(v => `${v.lens}: ${v.summary}`).join(' | ') }
  return { ok: true }
}

// ---------------- research rounds (dynamic, until dry) ----------------
let lenses = ROUND1
for (let round = 1; round <= A.max_rounds && lenses.length > 0; round++) {
  phase('Research')
  lensesUsed.push(...lenses.map(l => `- r${round} ${l.key}: ${l.title}`))
  const res = await parallel(lenses.map(l => () =>
    agentC(researchPrompt(l, round), { ...R, label: `research:r${round}:${l.key}`, phase: 'Research', schema: CAND_SCHEMA })))
  const found = []
  res.forEach((r, i) => {
    if (!r || !r.candidates) return
    r.candidates.forEach((c, j) => found.push({ ...c, id: `r${round}-${lenses[i].key}-${j + 1}`, lens: lenses[i].key, round }))
  })
  const carried = backlog.map(x => ({ ...x.cand, carried: true }))
  const carriedDupes = {}
  backlog.forEach(x => { carriedDupes[x.cand.id] = x.dupes || [] })
  backlog = []
  const pool = found.concat(carried)
  log(`Round ${round}: ${found.length} new candidates from ${res.filter(Boolean).length}/${lenses.length} researchers, ${carried.length} carried`)
  const byId = {}
  pool.forEach(c => { byId[c.id] = c })
  let fresh = []
  if (pool.length) {
    phase('Triage')
    const tri = await agentC(triagePrompt(round, pool), { ...R, label: `triage:r${round}`, phase: 'Triage', schema: TRIAGE_SCHEMA })
    if (tri) {
      const placed = new Set()
      for (const d of tri.dropped || []) {
        const c = byId[d.id]
        if (!c || placed.has(d.id)) continue
        placed.add(d.id)
        const reason = `${d.reason}${d.prior_item ? ' (prior: ' + d.prior_item + ')' : ''}`
        rejected.push({ id: c.id, slug: c.slug, title: c.title, round, stage: 'triage', reason })
        seen.push({ id: c.id, slug: c.slug, title: c.title, status: 'dropped', reason })
      }
      for (const f of tri.fresh || []) {
        const c = byId[f.id]
        if (!c || placed.has(f.id)) continue
        placed.add(f.id)
        const dupes = []
        for (const x of f.merged_from || []) {
          if (x === f.id || !byId[x] || placed.has(x)) continue
          placed.add(x)
          dupes.push(byId[x])
          seen.push({ id: x, slug: byId[x].slug, title: byId[x].title, status: `merged into ${c.slug}` })
        }
        fresh.push({ cand: { ...c, triage_score: f.score, triage_note: f.note }, dupes: (carriedDupes[c.id] || []).concat(dupes) })
      }
      const missed = pool.filter(c => !placed.has(c.id))
      if (missed.length) {
        log(`Triage r${round} did not place ${missed.length} candidates; they go to verification at score 0: ${missed.map(c => c.slug).join(', ')}`)
        missed.forEach(c => fresh.push({ cand: { ...c, triage_score: 0, triage_note: 'not placed by triage' }, dupes: carriedDupes[c.id] || [] }))
      }
      fresh.sort((a, b) => (b.cand.triage_score || 0) - (a.cand.triage_score || 0))
    } else {
      log(`Triage r${round} failed; verifying without dedup`)
      fresh = pool.map(c => ({ cand: c, dupes: carriedDupes[c.id] || [] }))
    }
  }
  const cap = round === 1 ? A.verify_cap_r1 : A.verify_cap_later
  const toVerify = fresh.slice(0, cap)
  const over = fresh.slice(cap)
  if (over.length) {
    backlog = backlog.concat(over)
    log(`Round ${round}: ${over.length} triaged candidates over the verify cap (${cap}) carried to the next triage: ${over.map(x => x.cand.slug).join(', ')}`)
  }
  phase('Verify')
  const judged = await parallel(toVerify.map(x => () =>
    parallel(VLENSES.map(v => () =>
      agentC(verifyPrompt(x.cand, x.dupes, v), { ...R, label: `verify:${x.cand.slug}:${v.key}`, phase: 'Verify', schema: VERDICT_SCHEMA })
        .then(r => (r ? { lens: v.key, ...r } : null))))
      .then(vs => ({ ...x, verdicts: vs.filter(Boolean) }))))
  let newSurv = 0
  for (const j of judged.filter(Boolean)) {
    const v = judge(j.verdicts)
    const c = j.cand
    if (v.ok) {
      survivors.push(j)
      newSurv++
      seen.push({ id: c.id, slug: c.slug, title: c.title, status: 'survived' })
    } else if (v.unverified) {
      backlog.push(j)
    } else {
      rejected.push({ id: c.id, slug: c.slug, title: c.title, round, stage: 'verify', reason: v.reason })
      seen.push({ id: c.id, slug: c.slug, title: c.title, status: 'rejected', reason: v.reason })
    }
  }
  rounds.push({ round, lenses: lenses.map(l => l.key), new_candidates: found.length, verified: toVerify.length, survived: newSurv })
  log(`Round ${round}: ${newSurv} of ${toVerify.length} survived; totals ${survivors.length} survivors, ${rejected.length} rejected or dropped, ${backlog.length} unverified`)
  if (round >= 2 && newSurv === 0) { log(`Round ${round} produced no new survivor: research is dry, stopping`); break }
  if (round >= A.max_rounds) break
  phase('Plan')
  const plan = await agentC(plannerPrompt(round), { ...R, label: `plan:r${round}`, phase: 'Plan', schema: PLAN_SCHEMA })
  if (!plan || !plan.continue) { log(`Planner stops after round ${round}: ${plan ? short(plan.rationale, 300) : 'planner failed'}`); break }
  const usedKeys = new Set(rounds.flatMap(r => r.lenses))
  lenses = (plan.lenses || []).slice(0, A.max_new_lenses).map((l, i) => {
    let k = slugify(l.key || `lens-${round}-${i}`)
    if (usedKeys.has(k)) k = `${k.slice(0, 34)}-r${round + 1}`
    usedKeys.add(k)
    return { ...l, key: k }
  })
  log(`Round ${round + 1} lenses: ${lenses.map(l => l.key).join(', ')} (${short(plan.rationale, 200)})`)
}
if (backlog.length) log(`Unverified at the end of research (reported, not built): ${backlog.map(x => x.cand.slug).join(', ')}`)

const allCandsText = () => [
  'SURVIVED:', survText() || '(none)',
  'REJECTED OR DROPPED:', rejText() || '(none)',
  'TRIAGED, NOT VERIFIED:', backText() || '(none)',
].join('\n')

const baseResult = () => ({
  continued_from: 'wf_251473a7-7d3',
  replayed: replayed.length,
  rounds,
  survivors: survivors.map(s => ({ slug: s.cand.slug, title: s.cand.title, est_gain: short(s.cand.est_gain, 200), corrected: s.verdicts.map(v => `${v.lens}: ${short(v.corrected_gain, 160)}`) })),
  rejected: rejected.map(r => ({ slug: r.slug, title: r.title, stage: r.stage, reason: short(r.reason, 240) })),
  backlog: backlog.map(x => ({ slug: x.cand.slug, title: x.cand.title })),
})

// ---------------- picks ----------------
phase('Plan')
if (!survivors.length) {
  log('No candidate survived verification; nothing to build.')
  return { ...baseResult(), picks: [], note: 'no survivors' }
}
const survJson = JSON.stringify(survivors.map(s => ({ id: s.cand.id, candidate: s.cand, merged_duplicates: s.dupes.map(d => ({ slug: d.slug, evidence: d.evidence })), verdicts: s.verdicts })))
const pk = await agentC(pickPrompt(survJson), { ...R, label: 'pick', phase: 'Plan', schema: PICK_SCHEMA })
if (!pk || !pk.picks || !pk.picks.length) {
  log('Picker returned no picks.')
  return { ...baseResult(), picks: [], knob_arms: pk ? pk.knob_arms : [], not_picked: pk ? pk.not_picked : [], note: pk ? short(pk.rationale, 600) : 'picker failed' }
}
if (pk.picks.length > A.max_picks) log(`Picker returned ${pk.picks.length} picks; building the first ${A.max_picks}, not building: ${pk.picks.slice(A.max_picks).map(p => p.slug).join(', ')}`)
const used = new Set()
const picks = pk.picks.slice(0, A.max_picks).map(p => {
  const s = slugify(p.slug || p.id)
  let k = s
  let n = 2
  while (used.has(k)) { k = `${s.slice(0, 37)}-${n++}` }
  used.add(k)
  return { ...p, slug: k, files: p.files || [] }
})
log(`Picks: ${picks.map(p => p.slug + ' (' + p.flag + ')').join(', ')}; knob arms: ${(pk.knob_arms || []).map(k => k.name).join(', ') || 'none'}`)

// ---------------- build -> audit -> fix (per pick, independent) ----------------
async function buildAndAudit(p) {
  let b = await agentC(buildPrompt(p), { ...B, label: `build:${p.slug}`, phase: 'Build', schema: BUILD_SCHEMA })
  if (!b) { log(`build ${p.slug}: no result`); return { pick: p, build: null, audit: null, ok: false, fixRounds: 0 } }
  if (b.status === 'failed') { log(`build ${p.slug}: failed: ${short(b.notes, 200)}`); return { pick: p, build: b, audit: null, ok: false, fixRounds: 0 } }
  let audit = null
  let fixRounds = 0
  for (let n = 1; n <= 3; n++) {
    audit = await agentC(auditPrompt(p, b, n), { ...R, label: `audit:${p.slug}:${n}`, phase: 'Audit', schema: AUDIT_SCHEMA })
    if (!audit || audit.ok || n === 3) break
    log(`audit ${p.slug} #${n}: ${(audit.blocking || []).length} blocking; fixing`)
    const f = await agentC(fixPrompt(p, audit, n), { ...B, label: `fix:${p.slug}:${n}`, phase: 'Build', schema: BUILD_SCHEMA })
    if (!f) break
    fixRounds = n
    b = { ...b, ...f }
    if (f.status === 'failed') break
  }
  const ok = !!(audit && audit.ok && b.status !== 'failed')
  log(`experiment ${p.slug}: ${ok ? 'ready' : 'NOT ready'} (build ${b.status}, audit ${audit ? (audit.ok ? 'ok' : 'blocking') : 'none'}, fix rounds ${fixRounds})`)
  return { pick: p, build: b, audit, ok, fixRounds }
}

phase('Build')
const built = await parallel(picks.map(p => () => buildAndAudit(p)))

// ---------------- integrate ----------------
const okBuilt = built.filter(x => x && x.ok).sort((a, b) => picks.indexOf(a.pick) - picks.indexOf(b.pick))
const notOk = built.filter(x => x && !x.ok)
let integ = null
if (okBuilt.length) {
  phase('Integrate')
  const done = okBuilt.map(x => ({ slug: x.pick.slug, title: x.pick.title, flag: x.pick.flag, branch: x.build.branch, head: x.build.head, expected_gain: x.pick.expected_gain, bench_measure: x.pick.bench_measure || '', tests: x.build.tests, codex: x.build.codex, audit: x.audit ? x.audit.summary : '' }))
  const failed = notOk.map(x => ({ slug: x.pick.slug, title: x.pick.title, flag: x.pick.flag, status: x.build ? x.build.status : 'no result', reason: x.audit ? ((x.audit.blocking || []).map(y => y.issue).join(' | ') || x.audit.summary) : (x.build ? x.build.notes : 'builder returned nothing') }))
  integ = await agentC(integratePrompt(done, failed, pk.knob_arms || [], allCandsText()), { ...B, label: 'integrate', phase: 'Integrate', schema: INTEG_SCHEMA })
} else {
  log('No experiment passed build and audit; skipping integration.')
}

return {
  ...baseResult(),
  picks: picks.map(p => ({ slug: p.slug, title: p.title, flag: p.flag, expected_gain: short(p.expected_gain, 200) })),
  knob_arms: pk.knob_arms || [],
  not_picked: pk.not_picked || [],
  builds: built.map(x => x ? ({
    slug: x.pick.slug, flag: x.pick.flag, ok: x.ok,
    status: x.build ? x.build.status : 'no result',
    branch: x.build ? x.build.branch : '', head: x.build ? x.build.head : '',
    tests: x.build ? short(x.build.tests, 400) : '',
    codex: x.build ? short(x.build.codex, 400) : '',
    audit: x.audit ? short(x.audit.summary, 400) : '',
    fix_rounds: x.fixRounds,
  }) : null),
  integration: integ,
}
