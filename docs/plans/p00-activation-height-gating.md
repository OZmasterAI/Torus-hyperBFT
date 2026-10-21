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

## Owner decisions (s131)
- Schedule: built-in table per chain id now (plus genesis `activations`); governance `ScheduleActivation` later,
  before mainnet.
- Old binaries past an activation height: stop (refuse to vote, fail-stop with UPGRADE REQUIRED), as proposed.
- Genesis check: a node refuses to start when the on-disk genesis file differs from the applied genesis (store a
  genesis digest at first boot).
- Running hash in the testnet genesis: undecided; L6.1 is being built so Classic keeps L3.1's gain with it on.

## Owner decisions (s134)
- A node that lost its running-hash chain rows (phase 3: it keeps running, hash-unverified, with the stored
  activation or 1): options (a) keep running and log it, (b) keep running and voting, a loud ERROR at boot and
  periodically, the `torus_state_hash_unverified` gauge, and no state-hash attestations until resynced /
  restored, (c) refuse to start. **Chosen: (b).** Built s134 on `feat/p00-phase4`:
  `state_hash::chain_health` at boot (after the replay, `TorusApp::new`): `Unverified` (the sticky marker),
  `RowsMissing` (the header at the applied height carries the bit, or the configured activation covers it,
  but no stored chain ends there) or `Verified` / `Off`. `StateHashMonitor::report_at_boot` sets the gauge
  and logs the ERROR; `after_block` repeats the ERROR at most once per 60 s while unverified; the attest
  path (`submit_attestations`) returns without submitting while unverified, and a node found `RowsMissing`
  at boot does not attest before its next flush marks it (a pipelined node's first `after_block` can run
  before that flush) unless a stored chain ends at the durable height again. Voting and execution are
  unchanged (no fail-stop). Operator text: `docs/node-operator-guide.md`, "Running state hash: a
  hash-unverified node".

## 6. Open questions for the owner
1. Running hash in the testnet genesis? Without it nothing detects divergent execution; only header bits are cross-checked.
2. Built-in schedule now, governance later — or governance from day one (voting period / timelock for activations)?
3. With 3 validators (f = 0) every upgrade needs all of them; is a stall at H acceptable on the trial testnet?
4. Old binaries keep voting past H (liveness) or stop as proposed (safe when vote rules change)?
5. u128 bitmap enough, or numbered fork bundles?
6. Refuse to start when the on-disk genesis file differs from the applied genesis (store a genesis digest)?

## Build phases (s134)
Each phase lands on its own branch with RED-first tests and full gates; each one is mergeable alone.
1. **Header bits and the rule** (~3.5 d; doc steps 1-2, plus the vote/propose refusal and the execution
   fail-stop of step 3): `Feature` registry + `KNOWN_FEATURES`, `ActivationSchedule` (genesis `activations` +
   the old `state_hash_activation_height` alias + built-in table per chain id, conflicts refuse to start),
   `TorusBlockHeader.features` in the canonical bytes, genesis parent header bits, proposer bits,
   `check_parent_link` rule (vote and insertion), unknown bit -> no vote / no proposal, execution and crash
   replay: unknown bit -> fail-stop `UPGRADE REQUIRED`, known bits != own schedule -> execute by the header + ERROR.
   Built s134 on `feat/p00-activation-gating` (RED 7f2b4be2, GREEN f08b7388, 924c7646). Choices: `features` is
   appended to `canonical_header_bytes` only when non-zero (a header without bits keeps its pre-P0.0 hash; the
   item 7 option 0 golden stays valid); `ChainConfig.state_hash_activation_height` is derived from the schedule
   (the old field 0 is refused); the built-in table is empty for every chain id; an unknown bit is `Invalid` at
   insertion too, so block sync of an old binary stops at H. The item 7 option 0 golden
   (`crates/torus-consensus/tests/fixtures/item7_opt0_golden.json`) was regenerated for the stored header JSON,
   which now carries `"features"`: only the dump digests and the final `cf_block_headers` digest changed (with
   zero bits left out of the JSON the old golden passed).
2. **Execution reads the bits** (~1 d): executor context `features`, EVM `SpecId` from the bits,
   mempool / RPC admission with the expected bits for tip+1.
   Built s134 on `feat/p00-phase2` (from phase 1 75cab960). torus-evm: `EvmSpecRule { base, upgrades:
   &[(Feature, SpecId)] }` (the highest active upgrade wins), `BUILTIN_EVM_SPEC_RULE` = Cancun with no
   upgrade (a release moving the spec adds a feature and a row), `BlockEnvCfg.features`, every
   executor entry runs at `spec_rule.spec(block_cfg.features)`, `intrinsic_gas(tx, spec)` /
   `intrinsic_gas_for` (EIP-7623 floor from Prague). The validator (strict, catch-up, overlay) takes
   the bits from the executed header and the proposer from the bits it stamps;
   `NativeExecContext.features` (+ `feature_active`) is set by the block pipeline from the
   committed header (live and crash replay), not from the node's schedule. Mempool:
   `set_activations` (from `ChainConfig::activations` at `TorusApp` boot), admission at
   `active_at(committed tip + 1)` (`admission_spec`); RPC eth_call / eth_estimateGas run at
   the bits expected at eth head + 1 (state reads only exist at the head). Choices: the spec
   rule is binary code, not node config (a test rule only through `EvmExecutor::with_spec_rule` /
   `Mempool::set_evm_spec_rule`); no feature changes execution yet, so the tests use a rule
   moving to Prague with bit 0 as a stand-in. Also on this branch, the Codex review P2 of phase 1:
   an unknown bit at insertion is `ValidateBlockResponse::UnsupportedUpgrade` (not inserted, block
   sync logs UPGRADE REQUIRED and ends the session without blacklisting the peer).
   Codex review of phase 2 (P2), fixed on `feat/p00-phase3`: `UnsupportedUpgrade` only for a datum whose bytes
   hash to the block's `data_hash` (`insertion_link_response_for_datum`; tampered data with an unknown bit is
   `Invalid`, so the peer is blacklisted), and block sync ends such a session without counting it as futile (the
   global trigger backoff does not grow); the peer cools down for `blacklist_expiry_time` (not picked,
   advertisements ignored) and another peer is tried after the short no-server retry.
3. **Running-hash migration** (~0.5-1 d): `configure_activation` checks the header bit from the start height;
   `ChainConfig.state_hash_activation_height` comes only from the schedule.
   Built s134 on `feat/p00-phase3` (from phase 2 8a5632a6). The running hash follows the committed headers,
   not the node's schedule: before block h executes (live and crash replay),
   `ExecutionContext::follow_running_hash_bit` makes META's configured activation (what every flush's chain
   step reads) `running_hash::header_activation`: `None` while the `RunningStateHash` bit is off, else the
   first height whose header carries it (parent bit first, then a binary search over the stored headers, which
   are never pruned). Steady state reads nothing (cached per execution context); a change (once per chain, at
   H; or the first block after a boot whose stored value disagrees) runs `pipeline_barrier` (every earlier
   height durable, the checkpoint window closed, the flush worker reloads its chain memo) and then
   `configure_activation` (its reset logic kept: a stored chain of another definition is discarded). The boot
   no longer writes the activation from the chain config. `ChainConfig.state_hash_activation_height` is now a
   method over `activations` (0 read as 1); the field is gone (the genesis alias stays). L3.1 on main:
   `configure_book_coalesce` is blocked by the schedule's height or an activation the replayed headers already
   set, and a header turning the hash on while L3.1 is on fail-stops (it cannot happen under the phase 1 header
   rule, which refuses such headers before the vote and at insertion). Test fixtures: `make_test_config_and_db`
   schedules the running hash at 1 and `make_block` / `default_genesis_header` carry the bit, so the module's
   running-hash coverage stays on.
   **L6.1 switch (not done here):** `perf/l61-window-books` keys its L3.1 gate on
   `ChainConfig::book_window_activation()` through `l31_hash_blocker`. Once it is a `Feature` (window books),
   `l31_hash_blocker` should take the header-derived running-hash activation (`read_configured_activation`
   after the replay, as `configure_book_coalesce` does here) plus the schedule, and the window-book rule itself
   should read the executed header's bit (`NativeExecContext::feature_active`) instead of a height compare, so
   replay and nodes with another schedule run it at the height the headers agreed.
4. **Genesis digest** (~0.5 d, owner decision): store a digest of the applied genesis at first boot, refuse to
   start when the on-disk file differs.
   Built s134 on `feat/p00-phase4` (from phase 3 8e269d77). `torus_genesis::GenesisDigest`: the file is parsed
   as a JSON value (before the typed parse) and written in a canonical form (compact, object keys sorted by
   their UTF-8 bytes, arrays in file order, strings and numbers as `serde_json` writes them);
   `digest = keccak256("torus-genesis-file-v1\0" || canonical bytes)`. Whitespace, indentation and key order
   do not change it; any value change does, including keys the typed parse ignores and an explicit value equal
   to a default. The typed struct is not digested, so a newer binary with more (defaulted) genesis fields
   computes the same digest for the same file. Field digests (one per top-level key; a top-level object with
   at most 32 keys gets one per key of it, e.g. `consensus.timeout_base_ms`; `evm_alloc` stays one field) name
   what changed. Stored in the state DB's `cf_consensus_meta` under `genesis_file_digest` (node-local, not in
   the running hash). Boot step 4 (`node_dbs::init_genesis`): fresh data dir -> genesis applied, digest
   stored; initialized dir with a stored digest -> the file must match, else
   `refusing to start: genesis file <path> differs from the genesis data dir <dir> was initialized with
   (stored digest .., file digest ..); changed: <fields>` (nothing written); initialized dir without a stored
   digest (every data dir from before this build) -> the current file's digest is stored and a warning logged
   (it cannot be checked against the genesis the dir was initialized with); a bound dir started without
   `--genesis` refuses (the default chain config would replace the genesis one). No override flag: to run a
   new genesis, start from a fresh data dir. Bench harness: none affected (run-cell.sh starts every cell with
   `launch-3val.sh CLEAN=1`, fresh data dirs; the genesis edits (`LIQ_THIN`, cache fetch) happen before the
   launch; crash-kill restarts reuse the same file).
   Codex review of phase 4 (3 x P2), fixed on `feat/p00-phase5`: (1) the genesis binding is checked
   (read-only, `check_genesis_binding`) before any boot-time write: the history-retention marker and the
   genesis writes come after it (`bind_genesis_and_retention`), so a refused start leaves the dir as it
   was; (2) a fresh boot stores the digest (WAL-synced) before it seeds the genesis, and a dir with a
   digest but no genesis is still bound (a crash in between no longer leaves a dir a later boot takes as
   Legacy); (3) numbers are digested without precision loss: the file is parsed over `RawValue`, a number
   is written as `serde_json` writes the u64 / i64 / f64 holding it exactly (unchanged digests), else as an
   exact decimal (an integer beyond 64 bits: the literal).
5. **Integration tests 1-4** (~2-3 d): needs a test-only feature with a state-visible rule.
   Built s134 on `feat/p00-phase5` (from phase 4 a7803f82): `crates/torus-consensus/src/p00_cluster_tests.rs`,
   four validators in one process (hotstuff_rs replicas over `ChannelNetwork`, each a real `TorusApp` on
   a state DB + consensus DB, stopped / restarted / wiped like a process). Test feature: torus-types
   cargo feature `test-features` (enabled from torus-consensus's dev-dependencies only; no node build has
   it) adds `Feature::TestStateRule` = id 127; its rule (test builds) runs the native phase of a block with
   the bit and bumps a hashed `cf_treasury` row (count, height), so the bit's height is in every `h_n`.
   An "old binary" is `AppOptions::known_features` without bit 127 plus a schedule without the feature
   (`TorusApp::try_new_with_options`; the node passes `KNOWN_FEATURES`, the rule functions take the set);
   `ExecHold` (test) holds a node's execution thread before a height and kills it there. Every node
   records its running-hash write sets (`running_hash::capture_*`); the tests recompute `h_n` from them
   and check the stored hash. Tests: (1) crossing at H: every node's headers carry the bit from exactly H,
   the rule row from H, identical write sets and `h_n` at every height; (2) 3 of 4 upgraded: the chain
   goes on, the old node refuses H (vote / insertion, `UPGRADE REQUIRED`), its applied and committed
   heights stop below H, no fail-stop, its history is a prefix of the others'; 2 of 4: every node stalls
   below H, no header at H anywhere, all headers equal (no fork); (3) crash with applied H-2, H-1, H+1 and
   committed past H: the boot replay crosses H by the stored headers and matches the uncrashed nodes;
   the same crash restarted on an old binary: the replay fail-stops before H (applied H-1, nothing of H
   flushed); (4) every node upgrades to a release that moves a future height (900 -> above the tip), one
   from an empty data dir (block sync from genesis): every `h_n` from before the upgrade is unchanged and
   the feature switches on at the new height on all four; a node restarted with another schedule
   replays its committed gap by the header bits (no fail-stop, same `h_n`) and then inserts no block
   whose bits differ from its schedule. Mutations checked: execution reading the node's schedule
   instead of the header bits, and the execution fail-stop ignoring the known set, each fail a test.
   nextest: `threads-required = 4` for these tests (view timers under a full parallel run).
6. **Ops** (~0.5-1 d): features gauge, RPC schedule listing, upgrade runbook.
7. **Governance `ScheduleActivation`** (before mainnet, 3-4 d) through `EpochRotationPlan`.
