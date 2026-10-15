export const meta = {
  name: 'exp-s58-perf-search-xl',
  description: 'XL dynamic perf search on Torus-hyperBFT (matched/s, blockspeed, p99, CPU): 6 prior-art agents, 20-lens research with planner-driven rounds overlapping verification, 4-lens adversarial verify, judge-panel picks, spec and spec critique, flag-gated test-first builds with Codex review, 2-lens audit and fix loop, integration on one exp branch and an interaction audit; excludes 18c s123 work',
  whenToUse: 'Large perf search ending in verified, built and audited experiments on one experimental branch',
  phases: [
    { title: 'Prior art', detail: '18c s123 and in-flight exclusions, perf history, bottlenecks, profile table, hot-path map, external techniques', model: 'opus' },
    { title: 'Baseline', detail: 'test baseline of main: pre-existing failures and clippy count, so they never block the run', model: 'opus' },
    { title: 'Research', detail: '20 lenses in round 1; later rounds chosen by the planner while verification runs', model: 'opus' },
    { title: 'Triage', detail: 'merge duplicates, drop redo of prior work, rank', model: 'opus' },
    { title: 'Verify', detail: 'gain, safety, novelty, measurability; any fatal kills, 3 of 4 must keep', model: 'opus' },
    { title: 'Plan', detail: 'next-round lenses from coverage and verdicts', model: 'opus' },
    { title: 'Pick', detail: 'two independent rankings and a reconciler', model: 'opus' },
    { title: 'Spec', detail: 'spec writer and adversarial spec critic per pick', model: 'opus' },
    { title: 'Build', detail: 'one worktree per pick, test-first, cargo slots, Codex quick review, fixes', model: 'opus' },
    { title: 'Audit', detail: 'safety and correctness auditors per experiment', model: 'opus' },
    { title: 'Integrate', detail: 'merge onto the exp branch, full suites, release build, doc, bench plan, interaction audit', model: 'opus' },
  ],
}

const A = args
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

// ---------------- XL: extra prior-art agents (the first three calls stay identical to run wf_251473a7-7d3 so their results can be reused) ----------------
const PROFILE_SCHEMA = S({
  summary: str,
  hot: arr(S({ symbol: str, thread: str, regime: str, share: str, code: str, source: str, addressed_by: str }, ['symbol', 'share', 'source'])),
  notes: str,
}, ['summary', 'hot'])
const EXTERNAL_SCHEMA = S({
  summary: str,
  techniques: arr(S({ name: str, source: str, mechanism: str, applies_to: str, fit: str }, ['name', 'source', 'mechanism', 'applies_to', 'fit'])),
}, ['summary', 'techniques'])
const HOTPATH_SCHEMA = S({
  summary: str,
  steps: arr(S({ step: str, thread: str, code: str, handoff: str, measured: str, source: str }, ['step', 'thread', 'code'])),
}, ['summary', 'steps'])

const PA_PROFILE = `${CTX}

TASK (read-only research): Build a data-first table of where CPU goes, from existing profiles, so researchers attack measured hot spots.
Sources: docs/perf/bench-archive/ (per-campaign *-tools and analyst dirs: perf report or flamegraph text, per-thread CPU, timers, keccak-by-thread; prefer the newest campaigns, e.g. ozarchy-hsdb, p4j4, p4swap10, liq2, p3cloidprof), the latest results sections (${A.results_doc} sections 60-74) and memory. Do not run perf, profilers, cargo or benches; read existing files only.
Return hot: one row per hot function or thread (symbol, thread, regime 10m or 300m, share of node CPU or of its thread, the responsible call site as path:line on main ${A.base_sha}, the source file, and which known item, if any, already addresses it). Rank by share; give at least the top 40 entries if the data has them. In notes, say which profiles are stale relative to main and what data is missing.`

const PA_EXTERNAL = `${CTX}

TASK (read-only research, web allowed): Collect proven techniques from other high-performance chains and engines that could transfer to Torus-hyperBFT. Use WebSearch and WebFetch (or the toolshed firecrawl tools) and memory. Topics: matching-engine throughput (Hyperliquid HyperCore, dYdX v4, LMAX disruptor, Phoenix, OpenBook), parallel execution (Block-STM, Solana Sealevel, Sui), consensus pipelining and latency (HotStuff-2, Jolteon/DiemBFT, Narwhal/Bullshark, Mysticeti, HyperBFT), state storage and commitment (RocksDB tuning for blockchains, Reth/Erigon MDBX, NOMT, QMDB, Firewood), signature verification (batch verification, precomputation, secp256k1 recovery cost), networking (QUIC tuning, erasure-coded broadcast, Turbine).
For each technique: name, source URL, mechanism, the Torus component it maps to (crate and file; read the code to confirm the mapping), and fit (does it suit the current architecture as a node-local experiment, what measured cost it would cut, what blocks it). Depth over breadth: 15-30 techniques with real mappings beat a long generic list. Do not edit files; do not run cargo.`

const PA_HOTPATH = `${CTX}

TASK (read-only research): Map the critical path of one block on current main ${A.base_sha} so every researcher shares the same picture.
Trace: leader select (mempool) -> proposal build -> header/body broadcast and DA -> follower validate and vote -> QC -> commit -> dispatch to exec -> native block execution phases -> state overlay and write (bg writer, checkpoint layer, state hash) -> trade, history and status records -> next proposal. For each step give the function(s) with path:line, the thread (consensus, exec, workers, bg writer, network), the handoff (channel, lock, await, poll interval) and the measured time per block where the docs have it (cite the section or file). Note queues and polls that add latency, and where the 10m and 300m regimes differ. Do not edit files; do not run cargo.`

const fmtProfile = (p) => p.summary + '\n' + (p.hot || []).map(h => `- ${h.symbol}${h.thread ? ' [' + h.thread + ']' : ''}${h.regime ? ' (' + h.regime + ')' : ''}: ${h.share}${h.code ? ' at ' + h.code : ''}${h.addressed_by ? ' | addressed by: ' + h.addressed_by : ''} [${h.source}]`).join('\n') + (p.notes ? '\nNotes: ' + p.notes : '')
const fmtExternal = (e) => e.summary + '\n' + (e.techniques || []).map(t => `- ${t.name}: ${t.mechanism} -> ${t.applies_to}; fit: ${t.fit} [${t.source}]`).join('\n')
const fmtHotpath = (h) => h.summary + '\n' + (h.steps || []).map(s => `- ${s.step} [${s.thread}] ${s.code}${s.handoff ? ' | handoff: ' + s.handoff : ''}${s.measured ? ' | ' + s.measured : ''}${s.source ? ' [' + s.source + ']' : ''}`).join('\n')

const cargoLine = (slot) => A.targets.length > 1
  ? `flock ${A.locks[slot]} env CARGO_TARGET_DIR=${A.targets[slot]} CARGO_BUILD_JOBS=${A.jobs_per_slot} NEXTEST_TEST_THREADS=${A.jobs_per_slot} cargo <args>`
  : `flock ${A.locks[0]} env CARGO_TARGET_DIR=${A.targets[0]} cargo <args>`
const commonRules = `  For anything longer than a minute use Bash run_in_background and wait for its completion notification. Never kill a cargo or rustc process you did not start. Check df -h /home before the first build; stop and report if under 25 GB free.
- Edit .rs files through Bash (python3 exact-string replacement, heredoc or git apply), not the Edit/Write tools: Edit/Write on .rs files start the session's rust-analyzer, which runs its own cargo check outside the locks. Markdown files may use Write.
- Tests per TESTING.md (nextest output flags; -E 'rdeps(<crate>)' for leaf crates; -p <crate> plus cargo check --workspace --tests -q for core crates).
- rm/rmdir: never with an unguarded shell variable; literal absolute paths or "\${VAR:?}" only.
- Do not push. Commit messages end with the line: Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
const setupRulesXL = (wt, slot) => `- Cargo: other builders run in parallel; this machine runs at most ${A.targets.length} cargo job(s) at once, one per slot, and you use slot ${slot + 1}. Run EVERY cargo command from ${wt} as:
  ${cargoLine(slot)}
${commonRules}`
const setupRulesAll = (wt) => `- Cargo: you need the whole machine, so hold every slot lock. Run EVERY cargo command from ${wt} as:
  ${A.locks.map(l => 'flock ' + l).join(' ')} env CARGO_TARGET_DIR=${A.targets[0]} cargo <args>
${commonRules}`
let BASE_TXT = '(baseline still running: treat a failing test as pre-existing if it also fails on main in the baseline worktree)'
const preexistRules = () => `PRE-EXISTING TEST FAILURES ON MAIN ${A.base_sha} (baseline run; NOT caused by any experiment):
${BASE_TXT}
Rules: (1) A failure listed above is not yours: do not fix it, do not stop, do not lower your status because of it; mention it in open_issues. (2) For any other failure, re-run that test alone; if it still fails, run the same command in the baseline worktree ${BASELINE_WT} (main, through your slot lock): failing there too means pre-existing, so report it and continue; passing there means your change caused it, so fix it. (3) Status failed is only for an experiment that cannot work (false premise, cannot be made deterministic); never for pre-existing failures, flaky tests, review noise or tooling trouble. (4) Always finish by returning the structured result; never end your turn waiting on something.`
const auditPreexist = () => `Pre-existing failures on main (baseline; never blocking):
${BASE_TXT}
A test failure is blocking only if the evidence shows it is new versus main.`
const fmtBaseline = (bl) => `baseline ${bl.status} at ${bl.head}; nextest: ${bl.nextest}; doc tests: ${bl.doctest}; clippy on main: ${bl.clippy}\n` + ((bl.failing || []).length ? bl.failing.map(f => `- ${f.test}${f.crate ? ' [' + f.crate + ']' : ''} (${f.kind}): ${short(f.message, 200)}`).join('\n') : '(no failing tests on main)') + (bl.notes ? '\nNotes: ' + bl.notes : '')
const BASELINE_SCHEMA = S({
  status: en(['done', 'partial', 'failed']), head: str, nextest: str, doctest: str, clippy: str,
  failing: arr(S({ test: str, crate: str, kind: en(['fails', 'flaky']), message: str }, ['test', 'kind', 'message'])),
  notes: str,
}, ['status', 'head', 'nextest', 'doctest', 'clippy', 'failing'])
const BASELINE_SLOT = A.targets.length > 1 ? 1 : 0
const BASELINE_WT = `${A.wt_root}/exp-${A.ns}-perf-search`
const BASELINE_PROMPT = `${CTX}

TASK: Record the test baseline of main ${A.base_sha} so later agents can tell pre-existing failures from failures their change causes. Do not fix anything, do not edit source files, do not commit.
SETUP: git -C ${A.repo} worktree add ${BASELINE_WT} ${A.base_branch} (reuse the worktree if it exists; the branch already exists at ${A.base_sha}). Work only there; its HEAD must be ${A.base_sha}. This worktree later becomes the integration worktree: leave it clean.
${setupRulesXL(BASELINE_WT, BASELINE_SLOT)}

STEPS
1. cargo nextest run --workspace with the TESTING.md output flags, then cargo test --doc as in TESTING.md. Record both summary lines.
2. Re-run every failing test alone twice (cargo nextest run -p <crate> -E 'test(=<name>)' with the output flags): fails every time -> kind fails; passes at least once -> kind flaky. Keep the first lines of the assertion message.
3. cargo clippy --workspace --all-targets and record the warning count and the exact command, so builders can prove "0 new warnings".
Return the structured result. Finishing with failures listed is expected and fine: status done means the baseline was recorded, whatever it contains.`

phase('Baseline')
const baselineP = agent(BASELINE_PROMPT, { ...B, label: 'baseline', phase: 'Baseline', schema: BASELINE_SCHEMA }).catch(err => { log(`baseline agent error: ${err && err.message || err}`); return null })

phase('Prior art')
log(`XL run: base ${A.base_branch} = main ${A.base_sha}; namespace ${A.ns}; ${A.targets.length} cargo slot(s)`)
const prior = await parallel([
  () => agent(PA_S123, { ...R, label: 'prior:18c-s123', phase: 'Prior art', schema: PRIOR_SCHEMA }),
  () => agent(PA_HIST, { ...R, label: 'prior:history', phase: 'Prior art', schema: PRIOR_SCHEMA }),
  () => agent(PA_BOTTLE, { ...R, label: 'prior:bottlenecks', phase: 'Prior art', schema: BOTTLE_SCHEMA }),
  () => agent(PA_PROFILE, { ...R, label: 'prior:profiles', phase: 'Prior art', schema: PROFILE_SCHEMA }),
  () => agent(PA_EXTERNAL, { ...R, label: 'prior:external', phase: 'Prior art', schema: EXTERNAL_SCHEMA }),
  () => agent(PA_HOTPATH, { ...R, label: 'prior:hotpath', phase: 'Prior art', schema: HOTPATH_SCHEMA }),
])
const pa1 = prior[0], pa2 = prior[1], pa3 = prior[2], pa4 = prior[3], pa5 = prior[4], pa6 = prior[5]
const BRIEF = [
  '== 18c s123 seed (authoritative) ==\n' + A.s123_seed,
  '== 18c and in-flight work ==\n' + (pa1 ? pa1.summary + '\n' + fmtItems(pa1.items) + (pa1.unrecovered ? '\nNot recovered: ' + pa1.unrecovered : '') : '(prior-art agent failed: rely on the seed above and check memory yourself)'),
  '== Perf history: merged, no-gain, refuted, shelved, knobs ==\n' + (pa2 ? pa2.summary + '\n' + fmtItems(pa2.items) : '(history agent failed: check docs/perf and memory yourself)'),
  '== Current bottlenecks on main ==\n' + (pa3 ? fmtBottle(pa3) : '(bottleneck agent failed: read sections 67-74 yourself)'),
  '== Profile table (existing data) ==\n' + (pa4 ? fmtProfile(pa4) : '(profile agent failed: mine docs/perf/bench-archive yourself if your lens needs it)'),
  '== Critical path of one block ==\n' + (pa6 ? fmtHotpath(pa6) : '(hot-path agent failed)'),
  '== External techniques ==\n' + (pa5 ? fmtExternal(pa5) : '(external-techniques agent failed)'),
  '== Sibling run in flight (do not duplicate its builds) ==\n' + (A.sibling || '(none)'),
].join('\n\n')
log(`Prior art: ${pa1 ? pa1.items.length : 0} 18c/in-flight items, ${pa2 ? pa2.items.length : 0} history items, ${pa3 ? pa3.bottlenecks.length : 0} bottlenecks, ${pa4 ? pa4.hot.length : 0} profile rows, ${pa6 ? pa6.steps.length : 0} path steps, ${pa5 ? pa5.techniques.length : 0} external techniques; brief ${BRIEF.length} chars`)

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
ROUND1.push(
  { key: 'speculative-exec', title: 'Pipelining and early or speculative execution',
    focus: 'How far execution trails consensus and whether work can start earlier: execute a block when its QC (or even the proposal) arrives instead of at commit, overlap block N+1 execution with block N state commit, double-buffer the overlay, assemble the next proposal while exec finishes. Look at the hotstuff_rs commit rule (which QC triggers commit), torus-consensus app.rs dispatch_to_exec, execution_loop and commit feed, and the torus-state overlay. Speculative work must be discarded deterministically on a view change. Do NOT redo propose-defer-on-exec-queue (18c) or s66 TORUS_DEFER_COMMIT_FEED / s72 defer parent feed (no gain); say how your idea differs.' },
  { key: 'state-root', title: 'State hash and trie computation cost',
    focus: 'torus-state incremental.rs and native_trie.rs (apply_native_dirty, build_native_trie_to_cf): how the per-block state hash is computed, which hashes are recomputed per block, node fan-out, batching of dirty keys, parallel subtree hashing, caching of intermediate nodes, hash function cost. Size against exec block_ms and CPU-s/1M. The state root value and format are consensus-visible and must not change; only how they are computed. Note Phase 3 findings (digest ~0.3 CPU-s/1M) and any merged trie work in the brief.' },
  { key: 'action-mix', title: 'Fast paths for the dominant action types',
    focus: 'Find the matched-bench action mix (tools/bench-throughput and the matched-bench generator: place, cancel, modify, cancel-all, IOC, market shares and taker ratio) and the per-type cost in the native executor. Look for fast paths for the dominant types: resting orders that never cross, cancels of orders placed in the same block, modifies that keep priority, batched actions per trader, early rejection before expensive validation. Results must stay bit-identical.' },
  { key: 'observability-tax', title: 'Logging, tracing, metrics and telemetry on hot paths',
    focus: 'Cost of tracing spans and log statements (formatting even when filtered, span creation in per-action loops), metrics (telemetry counters and histograms with locks or contended atomics), diagnostic timers added in perf work (body_send_trace, validate timers, exec timers), and RPC streams or explorer side effects executed inline. Find hot-loop instrumentation that can be made cheaper or gated without losing what the bench harness reads (check which metrics and log lines tools/matched-bench parses before removing anything).' },
  { key: 'data-layout', title: 'Data layout, key encoding and cache efficiency',
    focus: 'Struct sizes and layouts of hot types (orders, fills, positions, users, book levels, state keys), key encoding for RocksDB and in-memory maps (string vs fixed-width, address hashing, interning), Vec<u8> vs fixed arrays, Arc and clone patterns, locality in the book and in per-block caches. Find measurable wins in CPU-s/1M or RSS that keep outputs identical; avoid on-disk format changes unless node-local and migration-free.' },
  { key: 'build-codegen', title: 'Build profile and code generation',
    focus: 'The release profile and build flags of the bench binary: lto, codegen-units, opt-level, panic strategy, overflow checks, target-cpu, PGO or BOLT, allocator features, RocksDB build options (portable, SSE/AVX, jemalloc), mold. Check Cargo.toml [profile.*], .cargo/config.toml, the bench build in tools/matched-bench (run-cell.sh), and memory and docs for profile experiments already done. Experiments here are build profiles or features rather than env flags; determinism must hold (no float-dependent state; identical results across nodes built with different CPU flags).' },
  { key: 'knob-defaults', title: 'Existing knobs and defaults at the current baseline',
    focus: 'Inventory the TORUS_* env knobs and node config parameters (and compiled defaults) that affect throughput or latency: queue depths, worker counts, batch sizes, block caps, write buffer and WAL sizes, timeouts, poll intervals, cache sizes. For each, find whether it was benched at the current baseline (sections 60-74) or only on older code. Propose zero-code bench arms (value changes) with a measured rationale, and code changes only where an obviously mis-sized constant lacks a knob. Do NOT include TORUS_PARALLEL_ENGINE (already queued).' },
  { key: 'measurement', title: 'Measurement blind spots and load-generator limits',
    focus: 'tools/matched-bench and tools/bench-throughput: is the load generator (signing, RPC submission, pacing) or the harness itself a limit on matched/s at 10m or 300m? Which node costs are invisible in the current metrics? Where does noise come from (host contention, page cache, compaction timing, warm-up), and what would cut the CI so smaller real wins become visible? Candidates may be harness or generator changes (no node change) or extra node-local metrics; say what each unlocks.' },
  { key: 'node-asymmetry', title: 'Leader/follower asymmetry and duplicated work across nodes',
    focus: 'Sections 70.5 and 70.6 compare node A and node B per-wave cycles and the end-of-block worker. Find work that only the leader or only followers do on the critical path, work every node repeats that could be cheaper on followers (re-verification of data already bound by the QC or DA, re-serialization, re-hashing), and asymmetries that make the slowest node set the pace. Safety first: never skip a check that guards consensus or state correctness; a follower may reuse only what is already cryptographically bound. consensus-reuse-action-hashes and next-leader-parent-path-slim were refuted by 18c: do not repeat them.' },
  { key: 'external-techniques', title: 'Transfer techniques from other high-performance chains and engines',
    focus: 'Start from the external-techniques section of the brief (do more web research with WebSearch/WebFetch if needed) and map proven techniques (Hyperliquid HyperCore/HyperBFT, Block-STM, Sealevel, Narwhal/Bullshark/Mysticeti, HotStuff-2, LMAX-style matching, Phoenix/OpenBook, Firedancer, Reth/Erigon storage, NOMT/QMDB) onto concrete Torus code. Keep only techniques that fit the current architecture as a node-local experiment and attack a measured bottleneck; give path:line targets and the measured cost each would cut.' },
)

const VLENSES = [
  { key: 'gain', text: `LENS: premise and gain. Verify every factual claim against the code on main ${A.base_sha} and the measured data. Recompute the gain yourself from measured numbers, not from the candidate's estimate: which cost is removed, on which thread, is that thread on the critical path in this regime, and what that does to matched/s, blk/s, block_ms, p99 and CPU-s/1M. Refute (severity fatal) if the premise is false on current main, if the removed cost is much smaller than claimed, or if the corrected gain is below bench noise with no measurable CPU-s/1M, p99 or RSS benefit. If you cannot confirm the gain from data, keep=false.` },
  { key: 'safety', text: 'LENS: determinism and safety. Every node must produce byte-identical state and state hash for the same blocks whatever the flag value and thread timing; with the flag unset behavior must equal main; consensus safety and liveness (timeouts, view changes, leader behavior) must not regress; crash/restart, fail-stop and fault paths must stay correct; concurrency must be sound (ordering, locks, Send/Sync, panics in workers). Refute (fatal) only if it cannot be made safe; otherwise keep and list the exact constraints the implementation must meet.' },
  { key: 'novelty', text: 'LENS: novelty and feasibility. Compare against the brief, memory (search_knowledge, several phrasings), git history (git log --all --oneline -S or -G on the key symbol, recent branches) and docs/perf plus docs/plans. Refute (fatal) if it repeats or substantially overlaps 18c s123 experiments or rejected candidates, any measured no-gain or refuted item, the queued TORUS_PARALLEL_ENGINE A/B, or in-flight work (checkpoint slicing, item 7, Phase 5): name the prior item and its source. Also judge whether it is implementable as a default-off node-local env-flag experiment of reasonable size with tests, and note file conflicts with likely sibling experiments.' },
]
VLENSES.push({ key: 'measure', text: 'LENS: measurability and fit. Does the matched-bench workload exercise this path at 10m and/or 300m (action mix, load shape, tail depth, cell length)? Predict the effect on each metric (matched/s, native blk/s, exec block_ms, block p99, CPU-s/1M, RSS) with an uncertainty range and compare it with the noise of an n=4 ABBA campaign. Check that it can be built as an isolated default-off experiment (env flag, or a build profile or harness option where that is the nature of the change) with tests, and that it does not collide with in-flight branches. Refute (fatal) if the bench cannot see it and it has no other measurable value, or if it cannot be isolated as an experiment.' })

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

// ---------------- XL: schemas and prompts for picks, specs, builds, audits, integration ----------------
const RANK_SCHEMA = S({
  ranking: arr(S({ id: str, score: num, rationale: str }, ['id', 'score', 'rationale'])),
  knob_arms: arr(S({ name: str, env: str, rationale: str }, ['name', 'env', 'rationale'])),
  rationale: str,
}, ['ranking', 'knob_arms', 'rationale'])
const RECON_SCHEMA = S({
  picks: arr(S({ id: str, slug: str, title: str, kind: en(['env-flag', 'build-profile', 'harness']), why: str, conflicts: str }, ['id', 'slug', 'title', 'kind', 'why'])),
  knob_arms: arr(S({ name: str, env: str, rationale: str }, ['name', 'env', 'rationale'])),
  not_picked: arr(S({ id: str, reason: str }, ['id', 'reason'])),
  rationale: str,
}, ['picks', 'knob_arms', 'not_picked', 'rationale'])
const SPEC_SCHEMA = S({
  flag: str, files: arr(str), spec: str, tests_first: str, acceptance: str, microbench: str,
  expected_gain: str, bench_measure: str, constraints: str,
}, ['flag', 'files', 'spec', 'tests_first', 'acceptance', 'expected_gain', 'bench_measure', 'constraints'])
const SPECREV_SCHEMA = S({
  verdict: en(['go', 'go-with-fixes', 'stop']),
  issues: arr(S({ issue: str, where: str, fix: str }, ['issue', 'where', 'fix'])),
  summary: str,
}, ['verdict', 'issues', 'summary'])

const pending = []
const lateBacklog = []
const verifyJobs = []

function plannerPromptXL(round) {
  const pend = pending.map(e => `- ${e.slug}: ${e.title}`).join('\n')
  return `${CTX}

TASK: Plan the next round of a dynamic perf-idea search (read-only; no edits, no cargo). Round ${round} research and triage just finished; verification runs in the background, so some verdicts below are still pending.
First save one memory summarizing round ${round}: its lenses, the candidates sent to verification, and the ones dropped with the reason (tags: project:torus-hyperbft,machine:ozarchy,experiment-${A.ns},type:decision,area:backend).
Then decide whether another round is likely to find NEW, verifiable wins of meaningful size. If yes, propose up to ${A.max_new_lenses} lenses: unexplored angles (subsystems, cross-cutting effects, workload regimes, measurement blind spots, the existing-knob space), deeper dives where candidates cluster (the next-largest measured cost in the same phase), or reframings of rejected ideas that fix the reason they failed. Never repeat a rejected idea or an AVOID item; skip territory already well covered by pending or verified candidates unless you target a specific gap. Each lens: key (new kebab-case), title, focus (3-6 sentences: what to examine, which files, which measured cost to attack, what to avoid). If returns are diminishing, set continue=false.

PRIOR-WORK BRIEF:
${BRIEF}

LENSES USED SO FAR:
${lensesUsed.join('\n')}

SURVIVORS SO FAR:
${survText() || '(none yet)'}

REJECTED OR DROPPED SO FAR:
${rejText() || '(none yet)'}

VERIFICATION PENDING:
${pend || '(none)'}

TRIAGED BUT NOT YET SENT TO VERIFICATION (carried into the next triage):
${backText() || '(none)'}`
}

function judge4(vs) {
  if (vs.length < 3) return { ok: false, unverified: true, reason: `only ${vs.length} verifiers returned` }
  const fatal = vs.find(v => !v.keep && v.severity === 'fatal')
  if (fatal) return { ok: false, reason: `${fatal.lens}: ${fatal.summary}` }
  const keeps = vs.filter(v => v.keep).length
  if (keeps < Math.ceil(vs.length * 0.6)) return { ok: false, reason: vs.filter(v => !v.keep).map(v => `${v.lens}: ${v.summary}`).join(' | ') }
  return { ok: true }
}

function verifyOne(x, round) {
  const c = x.cand
  const entry = { id: c.id, slug: c.slug, title: c.title, status: 'verifying' }
  seen.push(entry)
  pending.push(entry)
  return parallel(VLENSES.map(v => () =>
    agent(verifyPrompt(c, x.dupes, v), { ...R, label: `verify:${c.slug}:${v.key}`, phase: 'Verify', schema: VERDICT_SCHEMA })
      .then(r => (r ? { lens: v.key, ...r } : null))))
    .then(vs => {
      const verdicts = vs.filter(Boolean)
      const j = { ...x, verdicts }
      const v = judge4(verdicts)
      const i = pending.indexOf(entry)
      if (i >= 0) pending.splice(i, 1)
      if (v.ok) {
        survivors.push(j)
        entry.status = 'survived'
      } else if (v.unverified) {
        lateBacklog.push(j)
        entry.status = 'unverified'
      } else {
        rejected.push({ id: c.id, slug: c.slug, title: c.title, round, stage: 'verify', reason: v.reason })
        entry.status = 'rejected'
        entry.reason = v.reason
      }
      log(`verify ${c.slug}: ${entry.status} (${verdicts.map(d => d.lens + '=' + (d.keep ? 'keep' : d.severity)).join(', ')}); ${survivors.length} survivors so far`)
      return j
    })
}

// ---------------- research rounds; verification of round N overlaps research of round N+1 ----------------
let lenses = ROUND1
try {
for (let round = 1; round <= A.max_rounds && lenses.length > 0; round++) {
  phase('Research')
  lensesUsed.push(...lenses.map(l => `- r${round} ${l.key}: ${l.title}`))
  const res = await parallel(lenses.map(l => () =>
    agent(researchPrompt(l, round), { ...R, label: `research:r${round}:${l.key}`, phase: 'Research', schema: CAND_SCHEMA })))
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
    const tri = await agent(triagePrompt(round, pool), { ...R, label: `triage:r${round}`, phase: 'Triage', schema: TRIAGE_SCHEMA })
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
        log(`Triage r${round} did not place ${missed.length} candidates; they are ranked last: ${missed.map(c => c.slug).join(', ')}`)
        missed.forEach(c => fresh.push({ cand: { ...c, triage_score: 0, triage_note: 'not placed by triage' }, dupes: carriedDupes[c.id] || [] }))
      }
      fresh.sort((a, b) => (b.cand.triage_score || 0) - (a.cand.triage_score || 0))
    } else {
      log(`Triage r${round} failed; verifying without dedup`)
      fresh = pool.map(c => ({ cand: c, dupes: carriedDupes[c.id] || [] }))
    }
  }
  const freshNew = fresh.filter(x => !x.cand.carried).length
  const cap = round === 1 ? A.verify_cap_r1 : A.verify_cap_later
  const toVerify = fresh.slice(0, cap)
  const over = fresh.slice(cap)
  if (over.length) {
    backlog = backlog.concat(over)
    log(`Round ${round}: ${over.length} triaged candidates over the verify cap (${cap}) carried forward: ${over.map(x => x.cand.slug).join(', ')}`)
  }
  toVerify.forEach(x => verifyJobs.push(verifyOne(x, round).catch(err => { log(`verify job error for ${x.cand.slug}: ${err && err.message || err}`); return null })))
  rounds.push({ round, lenses: lenses.map(l => l.key), new_candidates: found.length, fresh_new: freshNew, sent_to_verify: toVerify.length })
  log(`Round ${round}: ${freshNew} fresh new candidates, ${toVerify.length} sent to verification (runs in the background); ${survivors.length} survivors and ${rejected.length} rejected or dropped so far`)
  if (round >= A.max_rounds) break
  if (round >= 2 && freshNew < A.min_fresh) { log(`Round ${round}: only ${freshNew} fresh new candidates; research is dry, stopping`); break }
  phase('Plan')
  const plan = await agent(plannerPromptXL(round), { ...R, label: `plan:r${round}`, phase: 'Plan', schema: PLAN_SCHEMA })
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
} catch (err) {
  log(`research loop error: ${err && err.message || err}; continuing with what was found`)
}
phase('Verify')
await Promise.all(verifyJobs)
try {
  const leftovers = backlog.concat(lateBacklog.splice(0))
  backlog = []
  if (leftovers.length) {
    leftovers.sort((a, b) => (b.cand.triage_score || 0) - (a.cand.triage_score || 0))
    const fin = leftovers.slice(0, A.verify_cap_final)
    const rest = leftovers.slice(A.verify_cap_final)
    log(`Final verification pass: ${fin.length} carried candidates${rest.length ? '; left unverified (reported only): ' + rest.map(x => x.cand.slug).join(', ') : ''}`)
    await Promise.all(fin.map(x => verifyOne(x, 'final').catch(err => { log(`verify job error for ${x.cand.slug}: ${err && err.message || err}`); return null })))
    backlog = rest.concat(lateBacklog.splice(0))
  }
} catch (err) {
  log(`final verification pass error: ${err && err.message || err}`)
}
log(`Research done: ${survivors.length} survivors, ${rejected.length} rejected or dropped, ${backlog.length} unverified`)

const allCandsText = () => [
  'SURVIVED:', survText() || '(none)',
  'REJECTED OR DROPPED:', rejText() || '(none)',
  'TRIAGED, NOT VERIFIED:', backText() || '(none)',
].join('\n')

const baseResult = () => ({
  rounds,
  survivors: survivors.map(s => ({ slug: s.cand.slug, title: s.cand.title, est_gain: short(s.cand.est_gain, 200), corrected: s.verdicts.map(v => `${v.lens}: ${short(v.corrected_gain, 160)}`) })),
  rejected: rejected.map(r => ({ slug: r.slug, title: r.title, stage: r.stage, reason: short(r.reason, 240) })),
  unverified: backlog.map(x => ({ slug: x.cand.slug, title: x.cand.title })),
})

// ---------------- picks: two independent rankings, then a reconciler ----------------
phase('Pick')
if (!survivors.length) {
  log('No candidate survived verification; nothing to build.')
  return { ...baseResult(), picks: [], note: 'no survivors' }
}
const survJson = JSON.stringify(survivors.map(s => ({ id: s.cand.id, candidate: s.cand, merged_duplicates: s.dupes.map(d => ({ slug: d.slug, evidence: d.evidence })), verdicts: s.verdicts })))
const ANGLES = {
  gain: 'Rank by the size of the expected improvement to matched/s and native blk/s first, then exec block_ms, block p99 and CPU-s/1M, using the verifiers\' corrected gains, not the researchers\' estimates. Ignore effort unless the change is impractical.',
  confidence: 'Rank by the probability that the experiment produces a measurable, safe win in an n=4 ABBA campaign, per unit of implementation effort and risk. Prefer simple, well-evidenced changes whose verifiers agreed.',
}
const rankPrompt = (angle) => `${CTX}

TASK: Rank the verified perf candidates (read-only; no edits, no cargo). ${ANGLES[angle]}
Score every survivor 0-100 with a one-line rationale. Also list zero-code bench arms (existing env knobs or config values worth benching at the current baseline, never TORUS_PARALLEL_ENGINE, which is already queued).

VERIFIED SURVIVORS (JSON, with verdicts and constraints):
${survJson}`
const ranks = await parallel([
  () => agent(rankPrompt('gain'), { ...R, label: 'pick:rank-gain', phase: 'Pick', schema: RANK_SCHEMA }),
  () => agent(rankPrompt('confidence'), { ...R, label: 'pick:rank-confidence', phase: 'Pick', schema: RANK_SCHEMA }),
])
const recPrompt = `${CTX}

TASK: Choose the experiments to implement now (read-only; read code as needed; no edits, no cargo). Two independent rankings are below: one by expected gain, one by confidence per effort. Each pick is built in its own worktree on branch exp/archy-${A.ns}-<slug> off ${A.base_branch} (= main ${A.base_sha}), behind its own default-off switch (kind env-flag: node-local env var; build-profile: a new cargo profile or feature; harness: an opt-in harness or generator option), then all are merged onto ${A.base_branch} and benched against a control with every switch off.
Rules: up to ${A.max_picks} picks, ordered best first; weigh both rankings and the verifier constraints; avoid pairs that rewrite the same functions (or say how to sequence them in conflicts); nothing that redoes 18c s123 work or other AVOID items; do not pick a change the sibling run has already picked or built (check git -C ${A.repo} branch --list '${A.sibling_glob}' and its memories first). Merge both rankings' knob arms into knob_arms (drop duplicates and TORUS_PARALLEL_ENGINE). Slugs kebab-case, unique, at most 40 chars. Every survivor not picked goes in not_picked with the reason.
Afterwards save one memory listing the picks, knob arms and not-picked survivors with reasons (tags: project:torus-hyperbft,machine:ozarchy,experiment-${A.ns},type:decision,area:backend).

SIBLING RUN: ${A.sibling || '(none)'}

RANKING BY GAIN (JSON):
${JSON.stringify(ranks[0])}

RANKING BY CONFIDENCE PER EFFORT (JSON):
${JSON.stringify(ranks[1])}

VERIFIED SURVIVORS (JSON, with verdicts and constraints):
${survJson}`
const rec = await agent(recPrompt, { ...R, label: 'pick:reconcile', phase: 'Pick', schema: RECON_SCHEMA })
if (!rec || !rec.picks || !rec.picks.length) {
  log('Reconciler returned no picks.')
  return { ...baseResult(), picks: [], knob_arms: rec ? rec.knob_arms : [], not_picked: rec ? rec.not_picked : [], note: rec ? short(rec.rationale, 600) : 'reconciler failed' }
}
if (rec.picks.length > A.max_picks) log(`Reconciler returned ${rec.picks.length} picks; building the first ${A.max_picks}, not building: ${rec.picks.slice(A.max_picks).map(p => p.slug).join(', ')}`)
const survById = {}
survivors.forEach(s => { survById[s.cand.id] = s })
const usedSlugs = new Set()
const picks = rec.picks.slice(0, A.max_picks).map((p, i) => {
  const s = slugify(p.slug || p.id)
  let k = s
  let n = 2
  while (usedSlugs.has(k)) { k = `${s.slice(0, 37)}-${n++}` }
  usedSlugs.add(k)
  const sv = survById[p.id] || survivors.find(x => x.cand.slug === p.slug) || null
  if (!sv) log(`pick ${k}: no survivor record matches id ${p.id}; the spec writer works from the title`)
  return { ...p, slug: k, index: i, slot: i % A.targets.length, survivor: sv }
})
log(`Picks: ${picks.map(p => p.slug + ' (' + p.kind + ')').join(', ')}; knob arms: ${(rec.knob_arms || []).map(k => k.name).join(', ') || 'none'}`)

// ---------------- per pick: spec -> spec critic -> build -> 2-lens audit -> fixes ----------------


function specPrompt(p) {
  const others = picks.filter(q => q.slug !== p.slug).map(q => q.slug + ': ' + q.title).join(' | ')
  return `${CTX}

TASK: Write the implementation spec for one picked perf experiment (read-only; read the code as needed; no edits, no cargo). A builder (opus, xhigh effort) will follow it in worktree ${A.wt_root}/exp-${A.ns}-${p.slug} on branch exp/archy-${A.ns}-${p.slug} off ${A.base_branch} (= main ${A.base_sha}); afterwards all experiments are merged onto ${A.base_branch}.
Experiment ${p.slug}: ${p.title} (kind ${p.kind}). Why picked: ${p.why}
${p.conflicts ? 'Possible conflicts: ' + p.conflicts : ''}
Sibling picks (avoid editing the same functions; if unavoidable, say how to keep the merges clean): ${others || '(none)'}

Write a spec a builder can follow without re-researching: exact files and functions (path:line on ${A.base_sha}); the change step by step; the switch: for env-flag a TORUS_<NAME> env var not already used (check with grep), read once and cached, default off; for build-profile or harness the profile, feature or option name with the default unchanged; tests to write first (what fails on main, or what equivalence they pin, including flag-on vs flag-off identical results); acceptance criteria; an optional cheap microbench; the expected gain and how the bench should see it (bench_measure: metric, regime, size); every constraint from the verifiers.

VERIFIED CANDIDATE (JSON, with verdicts and constraints):
${JSON.stringify(p.survivor || { note: 'survivor record not found; research the idea from the title and why' })}`
}

function specReviewPrompt(p, spec) {
  return `${CTX}

TASK: Adversarially review an implementation spec before a builder spends hours on it (read-only; no edits, no cargo). Check: every path:line and symbol exists on ${A.base_sha} and says what the spec claims; the change attacks the measured cost the candidate targets; the flag or profile name is unused; flag-off identity and determinism hold by design; the tests would catch a wrong implementation; the plan fits a focused experiment; nothing redoes 18c s123 work or in-flight work. verdict: go (fine), go-with-fixes (list the concrete fixes the builder must apply), stop (fatal flaw: premise false on current code, cannot be made deterministic, a redo of prior work, or a duplicate of an experiment the sibling run already picked or built (git -C ${A.repo} branch --list '${A.sibling_glob}'); explain).
Experiment ${p.slug}: ${p.title} (kind ${p.kind})
SPEC (JSON):
${JSON.stringify(spec)}
CANDIDATE AND VERDICTS (JSON):
${JSON.stringify(p.survivor || {})}`
}

function buildPromptXL(p) {
  const wt = `${A.wt_root}/exp-${A.ns}-${p.slug}`
  const br = `exp/archy-${A.ns}-${p.slug}`
  const rev = p.specReview
  return `${CTX}

TASK: Implement one perf experiment, test-first, and commit it on its own branch.

EXPERIMENT ${p.slug}: ${p.title}
Kind: ${p.kind} (env-flag: node-local env var read once and cached, default off, flag-off identical to main; build-profile: a new cargo profile or feature, default build unchanged, bench arms select it at build time; harness: an opt-in harness or generator option, default behavior unchanged)
Switch: ${p.flag}
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
How the bench should see it: ${p.bench_measure || ''}
${p.conflicts ? 'Possible conflicts with sibling experiments: ' + p.conflicts : ''}
SPEC REVIEW (${rev ? rev.verdict : 'none'}): apply every fix listed; if you disagree with one, explain with evidence in notes.
${rev ? rev.summary + '\n' + JSON.stringify(rev.issues || []) : ''}

SETUP
- Worktree: git -C ${A.repo} worktree add ${wt} -b ${br} ${A.base_branch}. If it already exists from an earlier attempt, reuse it. If git reports a lock, wait a few seconds and retry. Work only in ${wt}; never touch the primary checkout, other worktrees or other branches.
${setupRulesXL(wt, p.slot)}
${preexistRules()}

STEPS
1. Read the spec's code on ${A.base_sha}. If the premise is false or it cannot be made deterministic, stop and return status failed with the reason.
2. Write the tests first and run them: they must fail for the right reason, or (equivalence tests) pin current behavior.
3. Implement. Switch off: same code path and results as main. Switch on: byte-identical state results across nodes and runs (add a flag-on vs flag-off equivalence test where the spec asks).
4. Prove: targeted tests pass; cargo clippy on the changed crates with 0 new warnings; cargo fmt --check clean. Optional microbench: indicative only, through your slot lock; report numbers.
5. Add docs/plans/exp-archy-${A.ns}-${p.slug}.md (what changed, why with evidence, switch, tests, expected gain, how to bench it, risks). Commit on ${br} with subject "perf(exp): <what> (${p.flag}, default off)".
6. Codex quick review of the whole branch (background; if it has not finished after 25 minutes, record that Codex timed out and continue): cd ${wt} && mkdir -p /tmp/claude-$(id -u) && torus-codex review --base ${A.base_branch} > /tmp/claude-$(id -u)/codex-${A.ns}-${p.slug}.md 2>/dev/null. Read the file. Fix every P0/P1 and each valid P2 test-first, re-run the affected tests, commit the fixes. Record each finding and its disposition (if Codex is unavailable, say so). Codex remarks about failures listed in the baseline are not yours: note them and move on.
Return the structured result; head = final commit sha.`
}

const ALENSES = [
  { key: 'safety', text: 'Lens safety: with the switch off, behavior identical to main (no changed path; at most a cached flag read on the hot path); with it on, deterministic state across nodes and runs (iteration order, float math, thread timing, error, fault and fail-stop paths, worker panics, partial failure); consensus safety and liveness unaffected or explicitly bounded.' },
  { key: 'correctness', text: 'Lens correctness: the change implements the spec and attacks the measured cost; the tests would fail on a wrong implementation (flag-on vs flag-off equivalence where relevant); Codex findings handled correctly; no dead or debug code; the notes file is accurate.' },
]

function auditPromptXL(p, b, n, lens) {
  const wt = `${A.wt_root}/exp-${A.ns}-${p.slug}`
  return `${CTX}

TASK: Adversarially audit one implemented perf experiment before it is merged onto ${A.base_branch} (audit round ${n}). Read-only: no edits and no cargo (builders own the cargo slots); git and reading are fine.
${lens.text}
${auditPreexist()}
Worktree ${wt}, branch exp/archy-${A.ns}-${p.slug}. Diff: git -C ${wt} diff ${A.base_branch}...HEAD
Experiment: ${p.title}; kind ${p.kind}; switch ${p.flag}
Spec:
${p.spec}
Constraints:
${p.constraints}
Builder report (JSON):
${JSON.stringify(b)}
Give path:line evidence. ok=false only with concrete blocking issues (where, why, fix); style nits are nonblocking.`
}

function fixPromptXL(p, blocking, n) {
  const wt = `${A.wt_root}/exp-${A.ns}-${p.slug}`
  return `${CTX}

TASK: Fix blocking audit findings on perf experiment ${p.slug} (fix round ${n}) in the existing worktree ${wt} (branch exp/archy-${A.ns}-${p.slug}). Work only there.
${setupRulesXL(wt, p.slot)}
${preexistRules()}

Blocking findings (JSON):
${JSON.stringify(blocking)}
Fix each test-first (a test that fails before the fix where possible), re-run the affected tests, keep clippy and fmt clean, update docs/plans/exp-archy-${A.ns}-${p.slug}.md if behavior changed, and commit. If a finding is wrong, say why with evidence instead of changing code.
Spec for reference:
${p.spec}
Return the structured result; head = final commit sha.`
}

async function auditBoth(p, b, n) {
  const as = await parallel(ALENSES.map(l => () =>
    agent(auditPromptXL(p, b, n, l), { ...R, label: `audit:${p.slug}:${l.key}:${n}`, phase: 'Audit', schema: AUDIT_SCHEMA })))
  const got = as.map((a, i) => (a ? { lens: ALENSES[i].key, ...a } : null)).filter(Boolean)
  return {
    ok: got.length === ALENSES.length && got.every(a => a.ok),
    blocking: got.flatMap(a => (a.ok ? [] : (a.blocking || []).map(x => ({ lens: a.lens, ...x })))),
    summary: got.map(a => `${a.lens}: ${a.ok ? 'ok' : 'blocking'}; ${short(a.summary, 300)}`).join(' | ') || 'no auditor returned',
    complete: got.length === ALENSES.length,
  }
}

async function runPick(p) {
  const spec = await agent(specPrompt(p), { ...R, label: `spec:${p.slug}`, phase: 'Spec', schema: SPEC_SCHEMA })
  if (!spec) { log(`spec ${p.slug}: no result`); return { pick: p, ok: false, stage: 'spec', reason: 'spec writer returned nothing' } }
  const rev = await agent(specReviewPrompt(p, spec), { ...R, label: `spec-review:${p.slug}`, phase: 'Spec', schema: SPECREV_SCHEMA })
  if (rev && rev.verdict === 'stop') {
    log(`spec ${p.slug}: stopped by the critic: ${short(rev.summary, 200)}`)
    return { pick: { ...p, ...spec }, ok: false, stage: 'spec-review', reason: rev.summary }
  }
  const full = { ...p, ...spec, specReview: rev }
  let b = await agent(buildPromptXL(full), { ...B, label: `build:${p.slug}`, phase: 'Build', schema: BUILD_SCHEMA })
  if (!b) { log(`build ${p.slug}: no result`); return { pick: full, ok: false, stage: 'build', reason: 'builder returned nothing' } }
  if (b.status === 'failed') { log(`build ${p.slug}: failed: ${short(b.notes, 200)}`); return { pick: full, build: b, ok: false, stage: 'build', reason: b.notes } }
  let audit = null
  let fixRounds = 0
  for (let n = 1; n <= 3; n++) {
    audit = await auditBoth(full, b, n)
    if (audit.ok || n === 3) break
    if (!audit.blocking.length) continue
    log(`audit ${p.slug} #${n}: ${audit.blocking.length} blocking; fixing`)
    const f = await agent(fixPromptXL(full, audit.blocking, n), { ...B, label: `fix:${p.slug}:${n}`, phase: 'Build', schema: BUILD_SCHEMA })
    if (!f) break
    fixRounds = n
    b = { ...b, ...f }
    if (f.status === 'failed') break
  }
  const ok = !!(audit && audit.ok && b.status !== 'failed')
  log(`experiment ${p.slug}: ${ok ? 'ready' : 'NOT ready'} (build ${b.status}, audit ${audit ? (audit.ok ? 'ok' : 'blocking') : 'none'}, fix rounds ${fixRounds})`)
  return { pick: full, build: b, audit, ok, fixRounds, stage: ok ? 'ready' : 'audit', reason: ok ? '' : (audit ? audit.summary : 'no audit') }
}

const baseline = await baselineP
BASE_TXT = baseline ? fmtBaseline(baseline) : `(baseline run failed: treat a failing test as pre-existing if it also fails on main in the baseline worktree ${BASELINE_WT})`
log(`Baseline: ${baseline ? baseline.status + ', ' + (baseline.failing || []).length + ' tests failing on main' : 'no result'}`)
phase('Spec')
const built = await parallel(picks.map(p => () => runPick(p)))

// ---------------- integrate, then audit the combination ----------------
function integratePromptXL(done, failed, knobArms, allCands) {
  const wt = `${A.wt_root}/exp-${A.ns}-perf-search`
  return `${CTX}

TASK: Integrate the finished perf experiments onto ${A.base_branch} and make it bench-ready.
SETUP: git -C ${A.repo} worktree add ${wt} ${A.base_branch} (reuse if it exists). Work only there.
${setupRulesAll(wt)}
${preexistRules()}
Integration rules: pre-existing failures never block integration. For a NEW failure after a merge, find the experiment that causes it (re-test merge by merge), then fix it test-first or revert that merge and report the experiment as skipped. Always finish with a committed branch, the doc and the report; status failed only if nothing could be merged.

1. Merge each experiment below in the listed order: git merge --no-ff exp/archy-${A.ns}-<slug> -m "Merge exp/archy-${A.ns}-<slug>: <title> (<switch>, default off)". Resolve conflicts so each experiment keeps switch-off identity and its own semantics; if two cannot coexist, keep the earlier one and report the other as skipped.
2. Prove per TESTING.md "Before every commit": cargo nextest run --workspace with the output flags and cargo test --doc; plus cargo clippy --workspace --all-targets (0 new warnings vs main ${A.base_sha}; compare against main only if warnings appear) and cargo fmt --check. Fix integration breakage test-first. Report counts.
3. Release build: cargo build --release -p torus-node to prove the bench binary builds; also build every build-profile arm the experiments define; report binary paths and md5s.
4. Write docs/plans/exp-archy-${A.ns}-perf-search.md: (a) table of merged experiments (switch, kind, what, expected gain, files, tests, Codex and audit status); (b) skipped or failed experiments and why; (c) the zero-code knob arms; (d) every candidate of the search with its verdict and reason (list below); (e) a bench plan for the bench agents: control = every switch off vs each switch alone, then all-on; 300m and 10m; ABBA n>=4; 600 s cells; metrics matched/s, native blk/s, exec block_ms, block p99, CPU-s/1M, RSS, consensus timeouts; AGREE every cell; per arm, which metric should move and by how much; the knob arms as their own cells. Commit it.
5. Save one memory: branch head, merged experiments with switches, suite results, doc path (tags: project:torus-hyperbft,machine:ozarchy,experiment-${A.ns},type:decision,area:backend).

EXPERIMENTS TO MERGE (in order, JSON):
${JSON.stringify(done)}

NOT MERGEABLE (JSON):
${JSON.stringify(failed)}

ZERO-CODE KNOB ARMS (JSON):
${JSON.stringify(knobArms)}

ALL CANDIDATES AND VERDICTS:
${allCands}`
}

function interactionPrompt(integ, done) {
  const wt = `${A.wt_root}/exp-${A.ns}-perf-search`
  return `${CTX}

TASK: Adversarially audit the integrated experimental branch ${A.base_branch} (read-only; no edits, no cargo). Worktree ${wt}; diff vs main: git -C ${wt} diff ${A.base_sha}...HEAD
${auditPreexist()}
Check: (1) every experiment's change survived the merges intact (compare with each exp/archy-${A.ns}-<slug> branch); (2) with every switch off the code paths equal main; (3) switches turned on together do not interfere (shared state, ordering, locks, double-applied work, conflicting assumptions); (4) the integrator's suite results are real (commands and counts) and cover the changed crates; (5) the bench plan in docs/plans/exp-archy-${A.ns}-perf-search.md names the right env vars or build profiles per arm, has a clean control, and states the expected metric movement per arm.
Integrator report (JSON):
${JSON.stringify(integ)}
Merged experiments (JSON):
${JSON.stringify(done)}
ok=false only with concrete blocking issues (where, why, fix).`
}

function integFixPrompt(blocking) {
  const wt = `${A.wt_root}/exp-${A.ns}-perf-search`
  return `${CTX}

TASK: Fix blocking interaction-audit findings on the integrated branch ${A.base_branch} in worktree ${wt}. Work only there.
${setupRulesAll(wt)}
${preexistRules()}

Findings (JSON):
${JSON.stringify(blocking)}
Fix each test-first where possible, re-run the affected tests (and the full nextest workspace run if shared code changed), update the doc if needed, commit. If a finding is wrong, explain with evidence. Return the structured result with the same fields as the integration report.`
}

const okBuilt = built.filter(x => x && x.ok).sort((a, b) => a.pick.index - b.pick.index)
const notOk = built.filter(x => x && !x.ok)
let integ = null
let interaction = null
try {
if (okBuilt.length) {
  phase('Integrate')
  const done = okBuilt.map(x => ({ slug: x.pick.slug, title: x.pick.title, kind: x.pick.kind, flag: x.pick.flag, branch: x.build.branch, head: x.build.head, expected_gain: x.pick.expected_gain, bench_measure: x.pick.bench_measure || '', tests: x.build.tests, codex: x.build.codex, audit: x.audit ? x.audit.summary : '' }))
  const failed = notOk.map(x => ({ slug: x.pick.slug, title: x.pick.title, kind: x.pick.kind, flag: x.pick.flag || '', stage: x.stage, reason: short(x.reason, 600) }))
  integ = await agent(integratePromptXL(done, failed, rec.knob_arms || [], allCandsText()), { ...B, label: 'integrate', phase: 'Integrate', schema: INTEG_SCHEMA })
  if (integ && integ.status !== 'failed') {
    interaction = await agent(interactionPrompt(integ, done), { ...R, label: 'interaction-audit:1', phase: 'Integrate', schema: AUDIT_SCHEMA })
    if (interaction && !interaction.ok && (interaction.blocking || []).length) {
      log(`interaction audit: ${interaction.blocking.length} blocking; fixing`)
      const fx = await agent(integFixPrompt(interaction.blocking), { ...B, label: 'integrate-fix', phase: 'Integrate', schema: INTEG_SCHEMA })
      if (fx) integ = { ...integ, ...fx }
      interaction = await agent(interactionPrompt(integ, done), { ...R, label: 'interaction-audit:2', phase: 'Integrate', schema: AUDIT_SCHEMA })
    }
  }
} else {
  log('No experiment passed build and audit; skipping integration.')
}
} catch (err) {
  log(`integration stage error: ${err && err.message || err}`)
}

return {
  ...baseResult(),
  picks: picks.map(p => ({ slug: p.slug, title: p.title, kind: p.kind, why: short(p.why, 200) })),
  knob_arms: rec.knob_arms || [],
  not_picked: rec.not_picked || [],
  builds: built.map(x => (x ? ({
    slug: x.pick.slug, kind: x.pick.kind, flag: x.pick.flag || '', ok: x.ok, stage: x.stage,
    status: x.build ? x.build.status : 'not built',
    branch: x.build ? x.build.branch : '', head: x.build ? x.build.head : '',
    tests: x.build ? short(x.build.tests, 400) : '',
    codex: x.build ? short(x.build.codex, 400) : '',
    audit: x.audit ? short(x.audit.summary, 400) : '',
    fix_rounds: x.fixRounds || 0,
    reason: short(x.reason, 300),
  }) : null)),
  integration: integ,
  interaction: interaction ? { ok: interaction.ok, summary: short(interaction.summary, 600), blocking: interaction.blocking } : null,
}
