Item 6 lens (owner s116). Apply ONLY when the diff touches the hot path:
crates/torus-consensus, crates/torus-bridge (native_executor), crates/torus-core,
crates/torus-state, crates/torus-mempool, crates/torus-evm, crates/hotstuff_rs, or
torus-node startup/allocator. Light version (latency only) for crates/torus-network.
Skip for torus-rpc, torus-explorer, tools/, bench/, docs/, scripts.

Goal: item 6 (market scaling, in-memory execution) Gate 3 = matched/s at 300 markets
>= 1.4x the pre-item-6 main, HL parity as the end goal. The exec thread is the
bottleneck (~0.96 busy), so cost per fill and per block on it is what counts.

After the normal findings, add a separate section "Item 6 notes". In it, list only:
new work on the exec or consensus path (say per fill / per action / per block),
extra RocksDB reads, writes or flushes on that path, RAM that grows with markets,
traders or orders, locks or channels the exec thread waits on, and anything that
conflicts with the Phase 3-5 design (checkpoint layer and merge-wait, per-trader
records, typed in-memory state; docs/plans/item6-*). Each note needs evidence: the
path:line plus how often it runs, or a measurement. Item 6 notes are not findings: they
never block a merge on their own; real ones go to an A/B bench.
