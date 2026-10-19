# Bench queue step 8k: explore workflow (archy, after 8j; owner s131)

Run this file's prompt as one multi-agent workflow on archy (the owner asked for a workflow, so the opt-in is given).
Pass everything between the two `PROMPT` lines verbatim.

---- PROMPT START ----

project: torus-hyperbft. You run on archy (ozarchy), the bench host. Repo: your Torus-hyperBFT clone, `git pull` main
first. The owner wants new, unexplored improvement ideas with potentially high gains, including out-of-the-box and
theoretical ones, then the best one or two built and benched on their own branch. Quality over speed: verify, then
assert.

## Goal

Find, rank, build and A/B the most promising improvement to the chain's throughput or latency that has NOT already
been explored. Deliver: an idea report, a prototype on its own branch, a bench verdict, a results-doc section.

## Baseline (main, code defaults, standard load: hsdb-300m shape, 300 markets, 3 validators, one host)

- matched/s ~257-265k; native blocks/s ~11-16 (bistable: small- vs big-block regime); block p50 ~14-20 ms, p99
  ~650-860 ms; exec-thread on-CPU ~2.7-2.8 ms per 1k fills; CPU-s per 1M fills ~23 (sections 94, 95).
- Deep book (1M resting orders): matched/s -14%, blocks/s 13 -> 3, `save_books` ~5x per fill (section 93).
- On the real network the chain was network-bound (gossip + DA), not engine-bound (`testnet/p3-testnet/RESULTS.md`).
- Reference point: Hyperliquid publishes ~0.2 s median, ~0.9 s p99 end-to-end latency.

## Phase 1: survey what is already explored (one agent; output: a list, not ideas)

Read and list every idea already tried, built, rejected or parked, with its verdict and the doc it lives in:
- `docs/Torus-hyperBFT-roadmap.md` (tiers, pre-testnet decisions, "not chosen" notes);
- `docs/perf/bench-queue.md` (queue, Items, "Done / dropped", Later);
- `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` (results sections; headlines of every section, and the
  profiles in sections 81, 90, 92, 93);
- every file name and status line in `docs/plans/` and `docs/perf/` (designs, experiments `exp-*`, verdicts);
- memory: `search_knowledge` with project torus-hyperbft, if the toolshed is available.
- explored on 18c, code pushed s131 as reference only (treat as explored; the docs on main describe them): propose-defer on
  the exec queue (`exp/s123-propose-defer-on-exec-queue-v2`, `TORUS_PROPOSE_EXEC_WATERMARK`, campaign B, dropped s126;
  bench-queue "Done / dropped"); exec-aware mempool admission (`perf/exec-aware-admission`, parked WIP, uncompiled,
  unbenched; results doc and `docs/configuration-reference.md`); the chunked level-hash / book-digest line
  (`perf/book-digest-3c` + `wip/book-digest-3c-stash`, hard fork judged not justified; `docs/design-level-rows-authority.md`,
  `docs/mission-handoff-layer3.md`). The docs on main are the authority; memory, where available, is a hint.
Also profile-ground the baseline: what are the top exec-thread, consensus-thread and network costs today (cite
sections). The list is the "already explored" filter for Phase 2.

## Phase 2: ideation (fan-out, 5-6 agents, one lens each; each returns 3-5 ideas)

Lenses: (1) consensus protocol and block cadence (pipelining, leader / view mechanics, vote aggregation, optimistic
paths); (2) execution engine and matching (parallelism, data layout, batching, branch-free hot loops, SIMD);
(3) state and storage (what is written, when, in which format; RocksDB, LSM vs other structures, write
amplification, the deep-book `save_books` cost); (4) networking and data availability (gossip vs direct, erasure
coding, compression, bandwidth per block, WAN); (5) cryptography and verification (batch signature verification,
aggregation, caching, precomputation); (6) wildcard: ideas from other high-throughput systems (Hyperliquid, Solana,
Sui / Mysticeti, Aptos Block-STM, Narwhal / Bullshark, Firedancer, kdb-style systems, HFT exchange engines) or from
theory, that don't fit the other lenses.
Each idea: the mechanism; why it could pay off here (cite the profile row it attacks and the size of that row); the
expected gain range with the arithmetic; node-local (byte-identical results, no consensus change) or consensus /
genesis-changing; rough size (lines, days); the cheapest experiment that would falsify it. Drop anything the Phase 1
list already covers, unless the idea is a materially different mechanism (say why).

## Phase 3: adversarial review and ranking (one critic agent per surviving idea, then one ranker)

Critic: try to kill the idea. Is the attacked cost real at the stated size (re-read the profile)? Does Amdahl cap the
gain? Does it break determinism, safety, crash recovery or replay? Did a past doc already measure something that
refutes it? Verdict: survives / weakened (new estimate) / dead (why).
Ranker: score survivors by expected gain x confidence / cost; prefer node-local, byte-identical changes (they can ship
behind a flag and validators can mix). Pick the top 1 (at most 2) to build. A consensus-changing idea may be picked
only if its gain is clearly larger; then the prototype is research-only and the report says so.
Write the idea report BEFORE building: `docs/perf/8k-explore-ideas.md`, one table with a row per idea (every idea
from Phase 2, dead ones included): id, idea, lens, mechanism (one line), attacked cost (profile row, its size, section
cited), expected gain range (with the arithmetic in a footnote), confidence (low / medium / high), cost (lines, days),
node-local or consensus-changing, critic verdict, score, status (picked / backlog / dead). Below the table: the pick
and why, and the falsifying experiment for each backlog idea. Unbuilt survivors stay in it as the idea backlog.
Keep the table's status column current through Phases 4-6.

From Phase 1 onward, keep an APPEND-ONLY progress log `docs/perf/8k-explore-log.md`: one dated entry per event
(phase started / done; idea picked, dropped or demoted and why; microbench result; build or test result; bench cell
valid / void; verdict), each naming the commit or file it refers to. Never edit or delete an earlier entry; a
correction is a new entry that names the one it corrects. Commit the log with each phase.

## Phase 4: build (one builder per pick, sequential)

- Branch `exp/8k-<short-name>` from main, own worktree, own `CARGO_TARGET_DIR`; behind an env flag, default off.
- Tests first: a failing test that defines correct behaviour, then the code. For a node-local change: a test that the
  results are byte-identical with the flag off and on (state, `h_n`, receipts) over a realistic block stream; the
  workspace nextest set stays green.
- A microbench of the attacked cost first, where one is possible: if the microbench shows less than half of the
  expected gain, stop, write it up, and go to the next pick instead of benching.
- Push the branch to origin. Never merge to main, never push to main.

## Phase 5: bench (A/B on archy's usual rig)

A = main at the branch point, B = the branch with the flag on; one binary per arm built the same way (md5 checked on
every node). Standard load, 300 markets, 600 s cells, warm-up then A B B A A B (n = 3). Judge by the exec-path merge
rule in `bench-queue.md` (exec-thread on-CPU per 1k fills, native blocks/s, block p50 / p99, timeouts per 1,000 blocks;
plus matched/s and CPU-s per 1M). Add the shape the idea targets if the standard load can't show it (e.g. the 8f deep
book with `PRELOAD=1000000`, or EVM load). Every cell valid (ACCEPT / AGREE / PASS) or the arm is void. Report CIs.

## Phase 6: write-up

Results-doc section (next number) in `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` + a Done line under 8k in
`bench-queue.md`, on a `docs/perf-section-<n>-8k` branch with origin/main merged in, plus `docs/perf/8k-explore-ideas.md` (final
statuses) and `docs/perf/8k-explore-log.md`. Push the docs branch after each phase, not only at the end, so 18c can
follow.
One short report to 18c / the owner: the ideas (top 5 with scores), what was built, the verdict, the recommendation
(merge candidate / more work / drop), and the next best idea.

## Rules

- Do NOT touch code that 18c is changing: item 7 option 0 (`feat/item7-option0`: the EVM section and the block
  overlay in `torus-consensus/src/app.rs`, the flush worker in `exec_pipeline.rs`, `torus-state` backend / overlay
  absorb), RH4's DA GC. Do not re-open item 6 Phase 5 (parked, section 91) or checkpoint slicing (parked).
- Quiet host for every bench (no cargo, rustc or indexer); never bench while building. Build lock and stop rules as
  in the bench queue. `perf_event_paranoid` stays as it is unless the owner says otherwise.
- Budget: Phases 1-3 at most ~12 agents in total; at most 2 builds; bench time at most ~4 h. If the top pick dies at
  the microbench, the second pick gets the budget.
- If 8j's SHAs are not posted when 8i ends, run 8k in full (all phases). When they appear, pause 8k at the next safe
  point (after the current bench cell or build), run 8j, then resume 8k. Never build or bench 8k while 8j runs.
- Never edit results. A failed idea is a result: write it up as carefully as a win.

---- PROMPT END ----
