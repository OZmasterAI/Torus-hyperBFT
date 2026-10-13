export const meta = {
  name: 'torus-explore-r3-part',
  description: 'Read-only explorers on NEW angles (round 3) find further Torus-hyperBFT improvements; angles passed via args',
  phases: [{ title: 'Explore', detail: 'one agent per angle, read-only' }],
}

const CONTEXT = `
You are exploring the Torus-hyperBFT codebase to find improvements, risks, and defects that have NOT been identified yet.

PROJECT
- Rust workspace. READ THE CODE AT /home/18c/projects/wt/s63-body-fetch (git branch main, HEAD f05b20e or later). Do NOT use /home/18c/projects/Torus-hyperBFT (older checkout). Some files there are mid-edit by another session (uncommitted changes in torus-mempool, torus-rpc, hotstuff_rs implementation.rs, kv_store.rs, telemetry); read, never modify.
- Crates: hotstuff_rs (forked HotStuff/MonadBFT-style BFT: algorithm.rs main loop, hotstuff/implementation.rs, pacemaker/, block_tree/, block_sync/), torus-consensus (app.rs produce_block / validate_block / on_committed_block, CTE = consensus-then-execute, exec_pipeline.rs), torus-bridge (native_executor.rs), torus-core (order_book.rs matching engine, oracle, positions, liquidation), torus-economics, torus-mempool, torus-network (libp2p gossipsub + direct, native DA), torus-state (RocksDB, native trie), torus-rpc, torus-node (main.rs wiring), torus-evm, torus-types, torus-telemetry, tools/bench-throughput (load generator), tools/matched-bench (harness).
- A perp/spot DEX chain: native order-book actions (place/cancel/cancel-all, PlaceOrderBatch of 400 orders) go into blocks (cap 200 actions); HotStuff agrees on blocks, then the matching engine executes them.

MEASURED STATE (3-validator devnet, one shared 18-core host, cap 200, 300 s cells)
- ~60-65k matched fills/s; native blocks ~1.1/s; view ~620-660 ms in the load window; ~1.5 views per committed block; 120-170 view timeouts per node per 300 s.
- hotstuff-algo thread ~70% asleep in futex_wait_queue, ~30% running, <0.5% disk wait. Storage is not the blocker.
- Offered load exceeds capacity (~343 actions/s sent vs ~225/s included; ~35% expire).
- Killing 1 of 3 validators freezes the chain ~60 s (n=3 needs all votes; slow restart; no view-sync on rejoin).

ALREADY IDENTIFIED — DO NOT REPEAT
- docs/perf/s64-exploration-2026-09-24.md (78 ideas from an earlier round).
- In progress right now: (A) vote-state write timing, (B) FIFO selection + adaptive admission limit, (C) deferring the leader's commit feed past its header broadcast + inline self-vote + stored restash key.
- A parallel round of 60 explorers is covering these angles, so do NOT focus on them: block_tree IO, steady-state timeouts, leader rotation, QC verification, consensus message encoding, network loop, futex waits, main loop, event bus, logging/telemetry overhead, RPC ingress, gossip re-verification, mempool structures, matching inner loop, risk/margin, settlement writes, trie hashing, RocksDB options, exec pipeline, trade history, EVM overhead, bench generator, memory growth, build config, thread oversubscription, production-only DA paths, fee accounting, block sync, graph hubs, perf bugs.
- Past campaigns: docs/perf/*.md. Memory: mcp toolshed run_tool("memory","search_knowledge",{"query":"...","project":"torus-hyperbft"}). Code index: run_tool("indexer","code_search",{"project":"Torus-hyperBFT","query":"..."}) (may lag; verify by reading files).
- Evidence: /home/18c/bench-results-matched/ (s65-votefix-*, s65-zerocode-* cells: summary.json, sampler.csv, metrics-after-val*.txt, val*.log.gz).

HARD RULES
- READ ONLY. Do not edit, create or delete any file. No git commands that change anything. Do NOT write to memory.
- Do NOT run cargo (build/test/check/bench/clippy), nodes, benchmarks, or tests; do not write new code. Many explorers share this host: avoid CPU-heavy commands (no recursive greps over target/ dirs or ~/, at most one or two zcat | grep on a single log).
- Only report NEW findings. Ground every claim in evidence you actually read (file:line, doc, or cell result). Show the arithmetic behind each % estimate; for risks/bugs, describe the concrete failure scenario. Be honest about confidence.
- Prefer 2-5 well-argued findings over a long list.
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
          kind: { type: 'string', enum: ['improvement', 'bug', 'risk'] },
          what_needs_to_be_done: { type: 'string', description: 'Concrete change: files/functions, steps' },
          reasoning: { type: 'string', description: 'Why it helps / why it is a problem, with arithmetic or a failure scenario' },
          evidence: { type: 'array', items: { type: 'string' } },
          metric: { type: 'string', enum: ['matched_s', 'blk_s', 'latency_ms', 'recovery_s', 'liveness', 'safety', 'security', 'cpu', 'memory', 'other'] },
          expected_improvement: { type: 'string', description: 'e.g. "+3-6% matched/s" or "prevents X"' },
          improvement_low_pct: { type: 'number' },
          improvement_high_pct: { type: 'number' },
          confidence: { type: 'string', enum: ['low', 'medium', 'high'] },
          effort: { type: 'string', enum: ['S', 'M', 'L'] },
          risk: { type: 'string' },
          why_new: { type: 'string' },
        },
        required: ['title', 'area', 'kind', 'what_needs_to_be_done', 'reasoning', 'evidence', 'metric', 'expected_improvement', 'confidence', 'effort', 'risk', 'why_new'],
      },
    },
    notes: { type: 'string' },
  },
  required: ['ideas', 'notes'],
}

const model = args.model
const angles = args.angles
phase('Explore')
const results = await parallel(angles.map(([key, focus]) => () =>
  agent(`${CONTEXT}\n\nYOUR ANGLE (${key}): ${focus}\n\nReturn 2-5 NEW findings for this angle in the schema, each with what needs to be done and the expected approximate % improvement (or the concrete failure it prevents).`,
    { label: `${model}:${key}`, phase: 'Explore', model, schema: SCHEMA })
    .then(r => r ? { angle: key, model, ...r } : null)))
const ok = results.filter(Boolean)
const dropped = angles.filter((a, i) => !results[i]).map(a => `${model}:${a[0]}`)
if (dropped.length) log(`no result: ${dropped.join(', ')}`)
log(`${ok.length}/${angles.length} ${model} explorers returned ${ok.reduce((n, r) => n + r.ideas.length, 0)} findings`)
return { raw: ok, dropped }
