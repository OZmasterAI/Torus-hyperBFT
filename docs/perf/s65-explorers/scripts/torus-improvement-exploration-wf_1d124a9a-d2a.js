export const meta = {
  name: 'torus-improvement-exploration',
  description: '20 read-only explorers (10 Fable 5.1, 10 Opus 5.5) propose unexplored Torus-hyperBFT improvements with estimated gains; one synthesizer ranks them',
  phases: [
    { title: 'Explore', detail: '20 angles, alternating fable/opus, read-only' },
    { title: 'Synthesize', detail: 'dedupe, check against past work, rank by expected gain' },
  ],
}

const CONTEXT = `
You are exploring the Torus-hyperBFT codebase to find performance or robustness improvements that have NOT been explored yet.

PROJECT
- Rust workspace. READ THE CODE AT /home/18c/projects/wt/s63-body-fetch (git branch main, commit 9ddece6). Do NOT use /home/18c/projects/Torus-hyperBFT (older checkout).
- Crates: hotstuff_rs (forked HotStuff BFT: pacemaker, block_tree, block_sync), torus-consensus (app.rs: produce_block / validate_block / on_committed_block, CTE = consensus-then-execute, exec_pipeline.rs flush worker), torus-bridge (native_executor.rs), torus-core (order_book.rs matching engine), torus-mempool (rate_limit.rs block caps), torus-network (libp2p gossipsub + unicast, block-body DA push/fetch, CompactBlock), torus-state (RocksDB, native trie / state root), torus-rpc, torus-node, torus-types, tools/bench-throughput (load generator), tools/matched-bench (harness: run-cell.sh, summarize.py).
- A perp/spot DEX chain: native order-book actions (place/cancel/cancel-all, PlaceOrderBatch) go into blocks; blocks are agreed by HotStuff, then executed by the matching engine.

CURRENT STATE (measured on a 3-validator devnet, all on one shared 18-core host, cap 200 actions/block, 300 s, 76k orders/s offered, 10 markets)
- Record: 61.6k mean matched fills/s (4 cells, best 65.7k); s64 confirmation cell 64.7k. Exec pipeline + batched cancel-all now default ON.
- Native block rate only ~1.1-1.2 blocks/s; 107-167 view timeouts per node per 300 s; commit interval p50 58 ms but p95 3.1 s; exec thread busy 0.78-0.82 (execution is no longer the limit).
- Bottleneck is CONSENSUS: block interval is 71-96% HotStuff-thread blocked time; load-window attribution says block building (produce_block) is the critical path. Gossipsub "Send Queue full" warnings appear under load.
- s64 crash test: killing 1 of 3 validators froze the chain 56-62 s. With 3 equal validators every vote is needed; view timeouts double per empty view (2,4,8,16,32 s); the restarted node took ~25 s before 'chain configuration loaded', finished replay at +30 s, reached the right view at +31.9 s, validated the proposal but did not vote, so everyone waited out a 32 s timeout.
- Known: hotstuff_rs progress_and_validator_set_update_test fails (pre-existing).

PAST WORK (check before suggesting — say explicitly if an idea was already tried and how it went)
- docs/perf/*.md in the repo (e.g. s63-builds-and-body-fetch-2026-09-23.md, cap100-gap-attribution-2026-09-10.md, consensus-thread-attribution-2026-08-21.md, blown-cell-diagnosis-2026-09-17.md, throughput-campaign-2026-09-18.md, load-improvements-2026-09-19.md, block-latency-campaign-2026-08-20.md, design-exec-pipeline-2026-08-20.md, stall-584-diagnosis-2026-09-18.md) and docs/plans/.
- Benchmark cells with summary.json/logs: /home/18c/bench-results-matched/ (e.g. s64-confirm-main-r1, s64-crash-combo-r1, s63-combo-*).
- Memory: mcp toolshed run_tool("memory","search_knowledge",{"query": "...", "project": "torus-hyperbft"}). Code index: run_tool("indexer","code_search",{"project":"Torus-hyperBFT","query":"..."}) (index may lag the branch — verify by reading files).

HARD RULES
- READ ONLY. Do not edit, create or delete any file. No git commands that change anything. Do NOT write to memory (no remember_this / record_*).
- Do NOT run cargo (build/test/check/bench), nodes, benchmarks, or any CPU-heavy command — a measured benchmark may be running on this host. Reading files, grep, small python/jq over existing result files are fine.
- Ground every claim in evidence you actually read (file:line, doc section, or cell result). Mark estimates as estimates and show the arithmetic behind each gain estimate (e.g. "produce_block is ~X ms of a Y ms interval, removing Z% of it => ...").
- Prefer fewer, well-argued suggestions (2-5) over a long list. Novel angles matter more than restating known backlog items.
`

const SCHEMA = {
  type: 'object',
  properties: {
    suggestions: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          title: { type: 'string' },
          area: { type: 'string' },
          change: { type: 'string', description: 'What concretely to change' },
          reasoning: { type: 'string', description: 'Why it should help, including the arithmetic behind the estimate' },
          evidence: { type: 'array', items: { type: 'string' }, description: 'file:line, doc section, or cell result actually read' },
          metric: { type: 'string', enum: ['matched_s', 'blk_s', 'latency_ms', 'recovery_s', 'liveness', 'cpu', 'other'] },
          estimated_gain: { type: 'string', description: 'e.g. "+8-15% matched/s" or "rejoin freeze 60 s -> ~5 s"' },
          gain_low_pct: { type: 'number' },
          gain_high_pct: { type: 'number' },
          confidence: { type: 'string', enum: ['low', 'medium', 'high'] },
          effort: { type: 'string', enum: ['S', 'M', 'L'] },
          risk: { type: 'string' },
          already_tried: { type: 'string', description: '"no" or where/how it was tried and the outcome' },
          how_to_validate: { type: 'string' },
        },
        required: ['title', 'area', 'change', 'reasoning', 'evidence', 'metric', 'estimated_gain', 'confidence', 'effort', 'risk', 'already_tried', 'how_to_validate'],
      },
    },
    notes: { type: 'string', description: 'Anything surprising found while exploring, incl. possible bugs' },
  },
  required: ['suggestions', 'notes'],
}

const ANGLES = [
  ['fable', 'pacemaker-rejoin', 'Pacemaker and view-timeout policy, and the rejoin freeze: why a caught-up restarted node validated the view-614 proposal but did not vote (read the s64-crash-combo-r1 logs and hotstuff_rs pacemaker/implementation code), timeout doubling/backoff caps, view sync on rejoin. Also how the 107-167 view timeouts per 300 s arise in steady state.'],
  ['opus', 'consensus-protocol', 'Consensus protocol structure: phases per block, chained/pipelined HotStuff, proposing the next block before the previous QC completes, 2-chain vs 3-chain commit, optimistic responsiveness. What does this hotstuff_rs fork actually do per view, and where does latency per committed block go?'],
  ['fable', 'block-building', 'produce_block (block building) as the critical path: what it does per block (mempool selection, action hashing, pre-proposal push, CompactBlock), what could be precomputed, cached, or moved off the HotStuff thread.'],
  ['opus', 'validate-vote-path', 'validate_block and the vote path: work done before a replica can vote, CompactBlock fallback, async validate (TORUS_ASYNC_VALIDATE, commit 4076355), body availability waits. How much of the block interval is replicas waiting to vote?'],
  ['fable', 'data-availability', 'Native DA / block-body dissemination: body push vs fetch, "Send Queue full" gossipsub saturation, body_fetch paths, CompactBlock reconstruction, redundant bytes sent. Can bodies travel with or ahead of proposals more cheaply?'],
  ['opus', 'networking-transport', 'Networking layer: libp2p config (gossipsub params, mesh, queue sizes, yamux/tcp settings, message size limits), unicast vs gossip for consensus messages, compression, per-message overhead. Look for low-hanging config wins.'],
  ['fable', 'mempool-ingress', 'Mempool and ingress: admission, signature verification (batched ed25519, verified sender cache), nonce windows, eviction, selection cost per block, lock contention between RPC ingress and block building.'],
  ['opus', 'matching-engine', 'Matching engine (torus-core order_book.rs and the executor): data structures, per-fill cost (engine_ms_per_1k_fills ~7), allocation, cancel paths, book persistence. Estimate headroom once consensus stops being the limit.'],
  ['fable', 'storage-rocksdb', 'Storage: RocksDB configuration (write buffers, compaction, pipelined write, WAL, column families, block cache), write amplification per block, fsync policy, what the flush worker writes per block.'],
  ['opus', 'state-root', 'State root / commitment: native trie, incremental root, parallel bucket hash, root cache — cost per block and whether the root must be on the critical path of proposing or voting.'],
  ['fable', 'exec-parallelism', 'Execution parallelism: per-market sharding, parallel settle, what still runs serially on the exec thread, pipelining execution with consensus further (execute block N+1 while N flushes).'],
  ['opus', 'block-sizing', 'Block sizing and cadence: cap 200 vs higher caps (TORUS_NATIVE_TOTAL_BLOCK_CAP), bytes caps, adaptive caps under load, fixed minimum block time vs as-fast-as-possible. Use existing cap-100/200/300 cell results to model throughput = blocks/s x actions/block x fills/action.'],
  ['fable', 'encoding-hashing', 'Serialization and hashing costs: encoding formats on the wire and on disk, repeated (de)serialization of the same block/actions, hash functions used (sha256/keccak/blake3), redundant hashing across produce/validate/execute.'],
  ['opus', 'threads-locks', 'Threading, scheduling and locks: which threads contend with the HotStuff thread, mutex/RwLock hot spots, channel hops, tokio runtime configuration, CPU pinning experiments (HOTSTUFF_CPUS) and schedstat results already recorded.'],
  ['fable', 'startup-recovery', 'Node startup and crash recovery time: the ~25 s before "chain configuration loaded" on restart (genesis parsing? 29 MB weighted genesis, DB open, resident book rebuild?), replay, block sync catch-up. How to make a restarted validator useful in a few seconds.'],
  ['opus', 'harness-validity', 'Benchmark methodology validity: is 76k offered load / bench-throughput itself a limit, is the generator or RPC the bottleneck in some windows, noise between cells, whether matched/s is the right headline, and what the harness misses (e.g. liveness threshold 30 s hid a 29 s freeze).'],
  ['fable', 'crypto', 'Cryptography on the critical path: vote/QC signature verification, signature aggregation options, per-action signature checks, key types, caching of verified signatures across mempool, validate and execute.'],
  ['opus', 'memory-allocator', 'Memory and allocation: global allocator choice (jemalloc/mimalloc?), allocation hot spots per block and per fill, clone-heavy paths, cache locality of the order book and state structures, RSS growth over a run.'],
  ['fable', 'validator-topology', 'Validator set and leader policy: 3 equal validators give f=0; effect of 4 validators on throughput and fault tolerance, weighted stake, leader rotation vs stable leader (a stable-leader branch existed), leader reputation, proposer locality.'],
  ['opus', 'end-to-end-latency', 'End-to-end user latency and ingress: RPC submission path (torus_submitNativeActions, batching), time from submit to inclusion to execution to query, and whether latency-focused changes also raise throughput.'],
]

phase('Explore')
const results = await parallel(ANGLES.map(([model, key, focus], i) => () =>
  agent(`${CONTEXT}\n\nYOUR ANGLE (${key}): ${focus}\n\nReturn 2-5 suggestions for this angle in the schema, each with an estimated gain and the reasoning behind it.`,
    { label: `${model}:${key}`, phase: 'Explore', model, schema: SCHEMA })
    .then(r => r ? { angle: key, model, ...r } : null)))

const ok = results.filter(Boolean)
const dropped = ANGLES.filter((a, i) => !results[i]).map(a => a[1])
if (dropped.length) log(`explorers with no result: ${dropped.join(', ')}`)
log(`${ok.length}/20 explorers returned ${ok.reduce((n, r) => n + r.suggestions.length, 0)} suggestions`)

phase('Synthesize')
const report = await agent(`${CONTEXT}

You are the SYNTHESIZER. Below are suggestions from ${ok.length} independent explorers (JSON). Produce a report in Markdown:

1. "Top 10" ranked table: rank, title, area, metric, estimated gain, confidence, effort, and who proposed it (angle/model; note when both a fable and an opus explorer independently proposed the same idea — that is a strong signal).
2. For each top-10 item: 3-6 lines of why, the key evidence, the arithmetic behind the gain, how to validate it cheaply, and risk.
3. "Already tried / known" — ideas that past docs or memory show were tried; state the outcome briefly.
4. "Possible bugs found" — anything the explorers flagged as a likely defect (especially the rejoin non-vote), with evidence.
5. "Everything else" — remaining deduplicated ideas, one line each.
6. A short "Recommended next 3 experiments" section.

Deduplicate aggressively. Rank by expected gain x confidence / effort, and favour ideas that attack the consensus bottleneck. Spot-check the 3-5 most important claims yourself by reading the cited file/lines (read-only) and say which you verified. Be honest where estimates are weak.

EXPLORER OUTPUT:
${JSON.stringify(ok)}`, { label: 'opus:synthesis', phase: 'Synthesize', model: 'opus' })

return { report, raw: ok, dropped }
