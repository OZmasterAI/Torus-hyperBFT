export const meta = {
  name: 'torus-explore-r2-part',
  description: 'Read-only explorers (one model, a slice of 30 angles) find NEW Torus-hyperBFT improvements beyond the s64 report',
  phases: [{ title: 'Explore', detail: 'one agent per angle in the slice, read-only' }],
}

const CONTEXT = `
You are exploring the Torus-hyperBFT codebase to find improvements that have NOT been tried and are NOT already identified.

PROJECT
- Rust workspace. READ THE CODE AT /home/18c/projects/wt/s63-body-fetch (git branch main, HEAD f05b20e or later). Do NOT use /home/18c/projects/Torus-hyperBFT (older checkout). Files there may be mid-edit by another session (kv_store.rs, telemetry); read, never modify.
- Crates: hotstuff_rs (forked HotStuff/MonadBFT-style BFT: algorithm.rs main loop, hotstuff/implementation.rs, pacemaker/, block_tree/, block_sync/), torus-consensus (app.rs produce_block / validate_block / on_committed_block, CTE = consensus-then-execute, exec_pipeline.rs), torus-bridge (native_executor.rs), torus-core (order_book.rs matching engine), torus-economics, torus-mempool, torus-network (libp2p gossipsub + direct, native DA), torus-state (RocksDB, native trie), torus-rpc, torus-node (main.rs wiring), torus-evm, torus-types, torus-telemetry, tools/bench-throughput (load generator), tools/matched-bench (harness).
- A DEX chain: native order-book actions (place/cancel/cancel-all, PlaceOrderBatch of 400 orders) go into blocks (cap 200 actions); HotStuff agrees on blocks, then the matching engine executes them.

MEASURED STATE (3-validator devnet, one shared 18-core host, cap 200, 300 s cells)
- ~60-65k matched fills/s; native blocks ~1.1/s; view ~620-660 ms in the load window; ~1.5 views per committed block; 120-170 view timeouts per node per 300 s; commit interval p95 ~3.4 s.
- The HotStuff thread (hotstuff-algo) is ~70% of the time asleep in futex_wait_queue (user-space lock or channel wait), ~28-30% running/runnable, <0.5% in disk wait (10 Hz /proc wchan sampling, s65). Storage is NOT the blocker.
- Offered load exceeds capacity: the generator sends ~343 actions/s, the chain includes ~225/s, ~35% expire unused after 60 s; at a lower offered load throughput rises ~8%.
- Exec thread busy ~0.8 on average but the last ~2 minutes of each cell are exec-bound as resting orders grow.
- Load-window per-view timers (val0, ms): view_duration ~620, qc_collect ~260-300, proposal_arrival ~380, propose_delay ~380, block_build ~185, insert_persist ~160-180, vote_delay ~46-50, on_committed_block ~120, mempool_remove_committed ~30.

ALREADY IDENTIFIED — READ FIRST, DO NOT REPEAT
- docs/perf/s64-exploration-2026-09-24.md in the repo: a ranked list of ~78 ideas from a previous 20-agent round (sections 1-5), including: deferring the leader's commit feed past its header broadcast; rejoin view-sync; WAL cap; commit-side hash/restash dedup; skipping produce-side re-mirror; offered-load / admission shedding; parallel cancel-all per market; body push; blocked-time attribution; follower pool-first body reconstruction; allocator; release profile/LTO; renice; trie in RAM; hash-only push; UDP buffers; 4th validator; leader reputation; etc.
- Planned right now (do not propose these): (A) make the vote-state write fast, (B) node-side admission limit with per-sender fairness, (C) move the leader's commit work after its proposal plus commit-side hash dedup.
- Also see docs/perf/*.md (past campaigns) and memory: mcp toolshed run_tool("memory","search_knowledge",{"query":"...","project":"torus-hyperbft"}). Code index: run_tool("indexer","code_search",{"project":"Torus-hyperBFT","query":"..."}) (may lag; verify by reading files).
- Benchmark evidence: /home/18c/bench-results-matched/ (e.g. s65-votefix-*, s65-zerocode-* cells: summary.json, sampler.csv, metrics-after-val*.txt, val*.log.gz; host samplers in s65-*-20260924/<label>-host/ incl. wchan.log, threads.log).

HARD RULES
- READ ONLY. Do not edit, create or delete any file. No git commands that change anything. Do NOT write to memory (no remember_this / record_*).
- Do NOT run cargo (build/test/check/bench/clippy), do not start nodes or benchmarks, do not run tests, do not write new code. Many explorers share this host: avoid CPU-heavy commands (no recursive greps over target/ dirs or ~/, no decompressing many large logs; one or two zcat | head/grep on a single log is fine).
- Only report ideas that are NEW or substantially different from the s64 report and the planned items. If you find something already listed, skip it (or mention it briefly in notes only if you have new evidence that changes its estimate).
- Ground every claim in evidence you actually read (file:line, doc section, or cell result). Show the arithmetic behind each % estimate. Be honest about confidence.
- Prefer 2-5 well-argued ideas over a long list.
`

const SCHEMA = {
  type: 'object',
  properties: {
    ideas: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          title: { type: 'string' },
          area: { type: 'string' },
          what_needs_to_be_done: { type: 'string', description: 'Concrete change: files/functions, steps' },
          reasoning: { type: 'string', description: 'Why it helps, with the arithmetic behind the estimate' },
          evidence: { type: 'array', items: { type: 'string' } },
          metric: { type: 'string', enum: ['matched_s', 'blk_s', 'latency_ms', 'recovery_s', 'liveness', 'safety', 'cpu', 'memory', 'other'] },
          expected_improvement: { type: 'string', description: 'e.g. "+3-6% matched/s"' },
          improvement_low_pct: { type: 'number' },
          improvement_high_pct: { type: 'number' },
          confidence: { type: 'string', enum: ['low', 'medium', 'high'] },
          effort: { type: 'string', enum: ['S', 'M', 'L'] },
          risk: { type: 'string' },
          why_new: { type: 'string', description: 'Why this is not already in the s64 report or planned items' },
        },
        required: ['title', 'area', 'what_needs_to_be_done', 'reasoning', 'evidence', 'metric', 'expected_improvement', 'confidence', 'effort', 'risk', 'why_new'],
      },
    },
    notes: { type: 'string', description: 'Surprises, possible bugs, or evidence that changes an existing estimate' },
  },
  required: ['ideas', 'notes'],
}

const ANGLES = [
  ['blocktree-io', 'hotstuff_rs block_tree accessors: how many KV reads/writes happen per view and per vote/insert/update; redundant reads, repeated deserialization of the same blocks/PCs, snapshot usage, anything cacheable in memory.'],
  ['steady-timeouts', 'Why 120-170 view timeouts per 300 s occur in HEALTHY runs (not the rejoin case): which views time out, who was leader, what the leader was doing, timeout base vs actual view durations. Read pacemaker code and logs of s65-votefix-ctl-r2 or r3.'],
  ['leader-rotation', 'Leader rotation and pipelining: does the leader change every view, and what does a leader change cost (vote routing to leader(v+1), body availability at the next leader, cold caches)? Could a different rotation schedule or view structure reduce views per block (~1.5 today)?'],
  ['qc-verify', 'Vote and QC handling: signature verification cost per vote/QC/TC, repeated verification of the same certificate (justify checked on header, proposal, sync), batch verification, caching of verified certificates.'],
  ['msg-encoding', 'Consensus message encoding and sizes: borsh encode/decode of proposals, headers, votes, CompactBlock; how many times each is (de)serialized per view across threads; per-message allocation.'],
  ['network-loop', 'The network event loop and the hand-off to the HotStuff thread: swarm poll structure, channel hops, message queues between libp2p and hotstuff-algo, per-message wakeups. Relate to the 70% futex wait.'],
  ['futex-waits', 'What exactly the hotstuff-algo thread waits on: mpsc recv/recv_timeout loops, polling intervals, sleeps, Mutex/RwLock acquisitions shared with other threads (mempool, block tree, app state, metrics). Find locks held across expensive work by other threads that could block consensus.'],
  ['main-loop', 'algorithm.rs and the consensus main loop structure: event ordering, how pacemaker ticks, block sync, and message handling are interleaved; any fixed sleeps or tick granularity that add latency per view.'],
  ['event-bus', 'Event bus, committed feed, and event publishing overhead: events published per view, subscribers, cloning of blocks into events, synchronous handlers on the consensus thread.'],
  ['logging-telemetry', 'Logging and metrics overhead on hot paths: INFO/diag logs per message (TORUS_BODY_FETCH_TRACE=1 is set in benchmarks), formatting cost, log volume per node, prometheus histogram updates, and whether benchmarks measure logging rather than consensus.'],
  ['rpc-ingress', 'RPC ingress path: HTTP server, request parsing (JSON vs binary), per-request allocation, verify thread pool sizing and queueing, how ingress threads compete with consensus for CPU on the shared host.'],
  ['gossip-reverify', 'Action gossip: every validator re-verifies and re-stores every action received by gossip. Is duplicated work avoidable (trusted-peer attestation, verify-once caching, leader-only verification with fraud proofs)? Quantify the CPU spent.'],
  ['mempool-structures', 'Mempool internals beyond selection order and admission: data structures, nonce tracking, eviction scans, lock granularity and hold times, per-block maintenance costs.'],
  ['matching-inner-loop', 'Matching engine inner loops in torus-core order_book.rs: price-level structures, iteration, per-fill allocations, hashing, branch-heavy code. Micro-optimizations with measurable per-fill gains (engine_ms_per_1k_fills ~7).'],
  ['risk-margin', 'Margin/risk and economics checks per order and per fill (torus-economics, executor): repeated computations, precision/integer math costs, caching of account risk state within a block.'],
  ['settlement-writes', 'Settlement and account/position state updates per fill: write amplification, per-fill state reads, batching of account updates within a block, resident caches.'],
  ['trie-hash', 'State commitment internals: native trie node layout, hash function choice and cost, batch update algorithm, parallelism, what fraction of exec time is root computation.'],
  ['rocksdb-options', 'RocksDB options not covered by the report (WAL cap and pipelined write are covered): bloom filters, block cache sizing, per-CF compression, memtable settings, compaction style, direct IO, number of CFs (44) and their cost.'],
  ['exec-pipeline', 'exec_pipeline.rs and the exec thread hand-off: queue depths, backpressure, flush worker batching, wait_idle points, what the exec thread waits on, and whether the pipeline can go one stage deeper safely.'],
  ['trade-history', 'Trade history, receipts, and query-side writers: what is written per block, background writer queues, low-priority IO throttling, whether any of it blocks exec or consensus.'],
  ['evm-overhead', 'EVM path overhead when blocks carry no EVM transactions: per-block EVM state handling, mirroring, receipts, roots — anything paid on every block regardless of content.'],
  ['bench-generator', 'The benchmark generator (tools/bench-throughput) and harness: is it efficient, fair, and representative (order mix, cancels, cross fraction, sender distribution, signing cost, connection handling)? Could generator behaviour be inflating or deflating results?'],
  ['memory-growth', 'Memory growth (RSS reportedly 124 MB -> 7.7 GB per node over ~330 s): what grows (caches, books, pools, logs, block store), whether it is bounded, and whether it slows the node over time (the late-window slowdown).'],
  ['build-config', 'Build and codegen configuration beyond LTO/allocator (already listed): target-cpu, opt-level per crate, panic strategy, debug assertions/overflow checks in release, feature flags compiled in, dependency versions with known perf fixes (e.g. libp2p, rocksdb, tokio, rayon).'],
  ['thread-oversubscription', 'Thread pools vs cores: 3 nodes share 18 cores; count tokio workers, rayon pools, rocksdb background threads, verify pools per node and the resulting oversubscription; which pools are sized by num_cpus and should not be.'],
  ['erasure-da-prod', 'Production-only paths disabled in the devnet (e.g. TORUS_SHARD_CUSTODY=0, erasure coding, DA sampling): what they would cost when enabled and cheaper designs, so benchmark numbers stay honest for production.'],
  ['fees-accounting', 'Per-action fee, nonce, and account bookkeeping: work done per action in validation and execution that could be batched per sender or per block.'],
  ['sync-catchup', 'Block sync and catch-up in normal operation: how often block sync or body fetch fallbacks trigger in healthy runs, their cost on the consensus thread, and cheaper paths.'],
  ['graph-hubs', 'Fresh eyes: read .torus/GRAPH_REPORT.md (or /home/18c/projects/Torus-hyperBFT/.torus/GRAPH_REPORT.md) for hubs and clusters, then look at hot code paths nobody has examined in docs/perf. Look for O(n) scans per block, clones of large structures, and repeated work.'],
  ['correctness-perf-bugs', 'Hunt for performance bugs: accidental quadratic behaviour, retries that never back off, timers set wrongly, caches that never hit, configs read but ignored (e.g. timeout_max_ms), metrics that mislead tuning.'],
]

const model = args.model
const slice = ANGLES.slice(args.start, args.end)
phase('Explore')
const results = await parallel(slice.map(([key, focus]) => () =>
  agent(`${CONTEXT}\n\nYOUR ANGLE (${key}): ${focus}\n\nReturn 2-5 NEW ideas for this angle in the schema, each with what needs to be done and the expected approximate % improvement.`,
    { label: `${model}:${key}`, phase: 'Explore', model, schema: SCHEMA })
    .then(r => r ? { angle: key, model, ...r } : null)))
const ok = results.filter(Boolean)
const dropped = slice.filter((a, i) => !results[i]).map(a => `${model}:${a[0]}`)
if (dropped.length) log(`no result: ${dropped.join(', ')}`)
log(`${ok.length}/${slice.length} ${model} explorers returned ${ok.reduce((n, r) => n + r.ideas.length, 0)} ideas`)
return { raw: ok, dropped }
