# P0.0: activation-height gating for state-transition changes

Status (s131): design draft from a read-only code study (main `8a5d4aeb`); nothing built. Roadmap item P0.0.
Paths are relative to the repo root.

## Current state
- The genesis field `consensus.state_hash_activation_height` (`crates/torus-genesis/src/lib.rs:126`) is copied into
  `ChainConfig` (`lib.rs:743`; `crates/torus-types/src/lib.rs:1712`, which rejects 0 at :1744). At boot,
  `configure_activation` (`crates/torus-consensus/src/app.rs:6229`) writes it to a node-local META key. That key is not
  part of the state hash (`crates/torus-state/src/running_hash.rs:262-300`, `cf.rs:229`). Changing the value throws away
  the stored hash chain.
- Every boot re-reads the genesis file (`crates/torus-node/src/main.rs:638-650`). It is applied to the DB only on a
  fresh DB (`node_dbs.rs:137-144`). No hash ties the file to the chain, so editing it later changes the config silently.
- A block header's `state_root` is not part of the block hash (`torus-types/src/lib.rs:546-559`, `:607-610`). The
  running hash is the only cross-node check of execution results.
- Two existing patterns this design copies:
  - Header EVM values change only at epoch boundaries, through a plan written by execution; consensus waits for it
    (`EpochPlanNotReady`, `app.rs:52-55`, `:7401-7435`; `crates/torus-economics/src/epoch_plan.rs:137`).
  - The genesis parent header must match on every node or block 1 fails to link (`crates/torus-bridge/src/proposer.rs:222-229`).
- Crash replay re-executes each stored committed block with its stored header, in height order (`app.rs:7029-7079`).

## 1. Mechanism
**Registry.** `enum Feature` in torus-types with fixed, append-only numeric ids, plus `KNOWN_FEATURES: u128` (features
this binary knows). New header field `TorusBlockHeader.features: u128`, the bitmap of features active at that height,
included in `canonical_header_bytes` (`lib.rs:600`).

**Checking a feature.** `active(f, header) = header.features & bit(f) != 0`. Execution reads the bits from the header of
the block being executed, never wall clock or node config.
- The executor context gets `features` next to `block_height` (`crates/torus-bridge/src/native_executor.rs:5517`,
  constructors from :5828).
- Vote rules use the proposal's header.
- The EVM `SpecId` (`crates/torus-evm/src/executor.rs:75`) becomes a function of the bits.
- Mempool / RPC admission use the expected bits for tip+1 (best effort, not consensus).

**Header rule.** In `check_parent_link` (`app.rs:2433`), which runs before a vote and in `validate_block`
(`app.rs:8303`), require `header.features == schedule.active_at(header.height)`. The proposer sets the bits from the
same function (`proposer.rs:80`).

**Where the schedule comes from:**
- Built into the binary, keyed by chain id (`eip712::set_network_chain_id`, `main.rs:657`): fastest; the release that
  carries a fix carries its height. Moving a height needs a release.
- Node config / genesis edits only: nothing binds them across nodes. Rejected as the only source.
- Governance proposal: on-chain agreement, height changeable without a rebuild; but voting + timelock is ~8 days
  (`governance.rs:62`, `:507`) and heights must snap to epoch boundaries (the `EpochPlanNotReady` pattern).

**Recommendation:** schedule = genesis `activations` ∪ the built-in table for this chain id; the node refuses to start if
both name a feature with different heights. Add governance `ScheduleActivation` before mainnet, feeding the same header
rule through `EpochRotationPlan`.

**Replay, sync, crash recovery.** Certified headers carry the bits, so execution always applies the rule each height was
agreed under, even if a later binary moves a future height. Snapshot restore (`crates/torus-state/src/snapshot.rs:165`)
and block sync use the same path.

**Safe stop for an old binary:**
- A header bit outside `KNOWN_FEATURES`: refuse to vote and propose (`refused_proposal`, `app.rs:60`).
- Before executing that height, latch the execution fail-stop (`exec_failed`), flush nothing, log
  `UPGRADE REQUIRED: feature N at H`.
- A schedule mismatch between nodes stalls block H (no quorum) instead of splitting the chain.
- A certified header whose known bits differ from the node's own schedule: execute by the header, log ERROR.

## 2. Data model and agreement
- Genesis JSON: `consensus.activations: { "<feature_name>": height }`.
  - Unknown name: refuse to start (`load_genesis`, `main.rs:569`).
  - Height 0 or 1: active from block 1; those bits go into the genesis parent header, so a different genesis set fails
    block 1's parent link.
  - Governance-scheduled heights: epoch boundaries >= 2L (the first boundary has no plan, `epoch_plan.rs:15-16`).
- No state rows for the genesis / built-in path; the header hash chain is the commitment.
- Governance later writes rows under a new hashed META prefix `activation:` added to `META_CONSENSUS_PREFIXES`
  (`running_hash.rs:83`).
- Agreement: the bits are in every block hash and checked against each voter's own schedule.

## 3. Migrating `state_hash_activation_height`
- `Feature::RunningStateHash` = id 0.
- The old genesis field stays as an alias for `activations.running_state_hash`; refuse to start if both are set and differ.
- `ChainConfig.state_hash_activation_height` becomes derived from the schedule.
- `configure_activation` keeps its node-local copy and reset logic; add a check that from the start height the header
  bit matches.

## 4. Test plan
1. Crossing: 4-node chain, test feature X at H with a state-visible rule: old rule below H, new rule from H, identical
   running hashes (hash on).
2. Mixed binaries: X-unaware next to X-aware nodes. Below H all agree. At H with 3 of 4 upgraded, the chain continues
   and the old node halts with UPGRADE REQUIRED at an unchanged applied height. With 2 of 4, the chain stalls, no fork.
3. Crash across H: kill between H-1 and H+k (applied below H, committed above), restart, replay matches an uncrashed node.
4. Replay from genesis with a newer binary whose built-in table moves a future height: history unchanged.
5. Unit: header bits != schedule -> Invalid; unknown bit -> fail-stop; genesis alias conflict -> refuse to start;
   genesis bits bound into block 1.

## 5. Steps (days, estimate)
1. Feature registry, genesis `activations`, built-in table, merge + validation: 1.5
2. Header field, canonical bytes, genesis parent header, proposer, `check_parent_link` rule (34 header literals): 2
3. Executor and EVM read bits from the header; unknown-bit fail-stop and vote refusal: 1
4. Migrate the running-hash field: 0.5-1
5. Integration tests 1-4: 2-3
6. Ops: features gauge, RPC listing the schedule, upgrade runbook: 0.5-1
7. (Before mainnet) governance `ScheduleActivation` through the epoch plan: 3-4

Steps 1-6: ~8-9 days. Step 2 changes the block hash, so it must land before the testnet genesis.

**Pending changes:**
- All of `fix/pretestnet-p0-p1` ships directly in the genesis (each commit says "genesis-bound"); P0.0 doesn't exist
  yet and its header field is itself genesis-bound.
- `perf/t53-manifest-consensus-db`: node-local DB layout, no gating.
- L3.1: node-local, refuses to run with any activation height; no gating.
- L6.1: consensus-changing (`docs/perf/8k-explore-ideas.md:57`); first real adopter after launch.
- Every later P1-P5 item gets gated.

## 6. Open questions for the owner
1. Running hash in the testnet genesis? Without it nothing detects divergent execution; only header bits are cross-checked.
2. Built-in schedule now, governance later — or governance from day one (voting period / timelock for activations)?
3. With 3 validators (f = 0) every upgrade needs all of them; is a stall at H acceptable on the trial testnet?
4. Old binaries keep voting past H (liveness) or stop as proposed (safe when vote rules change)?
5. u128 bitmap enough, or numbered fork bundles?
6. Refuse to start when the on-disk genesis file differs from the applied genesis (store a genesis digest)?
