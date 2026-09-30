# Implementation Plan: Oracle aggregation into block execution (item 2, option A — time-based, rev. 3)

Design: `docs/plans/oracle-aggregation.md`. Binding decisions (user, s517 + rev. 2):

1. Option A only: on-chain aggregation each block, mark = aggregated oracle price; hook at the
   START of the block, before `execute_batch(pre_evm)`, in `execute_committed_block_with`.
2. **Time-based (HL-like):** a validator's LATEST submission counts iff its block timestamp is
   ≤ 10 s older than the current block's timestamp; the aggregate is stale 60 s after the block
   timestamp of the last FRESH aggregate. Stake-weighted median as today, min reporters 3; with
   fewer, the last price is kept (it ages toward stale).
3. R9 (exact 3×MAD) stays in A. C1 (run the native phase while submission rows exist) and C2
   (one `run_native` flag) stay.
4. **Epoch skip first**: T0 is its own task and its **own commit**, before any oracle work.
5. **Block-timestamp validation** (rev. 3): T0b, its own commit after T0 and before the oracle:
   reject a proposal whose timestamp is < its parent's or > local clock + 5 s; never on sync,
   execution or replay; the proposer uses `max(local clock, parent timestamp)`. Oracle ages
   clamp at 0 (tested). The precompile time rule lands in the same commit as T2 (no commit
   with a unit mismatch).

Branch `feat/oracle-aggregation`, stacked on `fix/parity-audit-bugs` @ `3e9365e`.
Consensus-visible (lockstep upgrade; fresh genesis is already required by the stack).
Tasks are TDD: failing test first (a missing API = RED by compile error), implement, run
`validate`. Only the lead runs cargo (serialized, `-j6`). Line numbers verified at `3e9365e`.

## Verified anchors

| What | Where |
|------|-------|
| Proposer sets `timestamp = SystemTime::now().as_secs()` (**seconds**, proposer wall clock) | `torus-consensus/src/app.rs:5010-5013`; header built :5061-5080 |
| `timestamp` is hashed into the block identity | `torus-types/src/lib.rs:439` (`canonical_header_bytes`) |
| Validation: data hash, EVM decode, native sigs (session expiry vs `header.timestamp`), proposer attestation, `parent_hash` — **no timestamp check** | app.rs:4789-4990 (`validate_datum*`, `validate_body_checks_core`, `finish_validate`) |
| Execution reads the committed header: `NativeExecContext::new_with_mode / new_env(…, torus_block.header.timestamp, …)` | app.rs:2156-2181 |
| Crash replay loads the persisted header (`CF_BLOCK_HEADERS`, hash‖json) | app.rs:1225-1240 (`load_replay_header`); `replay_committed` :4005 |
| EVM block env uses `header.timestamp` (BlockValidator, RPC `eth_call`) | `torus-bridge/src/validator.rs:146-152, 255, 417`; `torus-rpc/src/eth.rs:154-161` |
| `NativeExecContext.timestamp` (pub) | `torus-bridge/src/native_executor.rs:1500` (NE) |
| Submission rows ALREADY store the block timestamp: `submit_price(sender, m, price, ctx.block_height, ctx.timestamp)`; the action's own `timestamp` is ignored | NE:6512-6548; `OracleSubmission` `oracle.rs:30-70` |
| Existing pin: trade rows carry `header.timestamp` | app.rs:10710 |
| `OracleConfig` / defaults (100 blocks, 3, window 10 blocks) | `torus-core/src/oracle.rs:19-22, 115-131` |
| keys `"sub"‖market(8)‖validator(20)‖block(8)`, `"agg"‖market(8)` | `oracle.rs:137-161` |
| `StoredAggregatedPrice` = price(16)‖block(8)‖reporters(4) = 28 B | `oracle.rs:81-113` |
| `aggregate_price` / `get_price` / `collect_submissions` (prunes ≤ 100, ignores errors) / `get_last_valid_price` | `oracle.rs:200-271 / 274-295 / 299-333 / 338-355` |
| panicking `FixedPoint` operators in the aggregation | `oracle.rs:379-451`; `torus-types/src/lib.rs:113-131` |
| `AccountReader` (`height`) / `mark` | NE:441-469 |
| `exec_submit_oracle_prices` (Active check only) | NE:6512-6549 |
| `core_writer_due` | NE:6747-6749 |
| `aggregate_oracle_prices` (no caller) | NE:6809-6826 |
| `process_epoch_boundary` (early `None` unless boundary; rewards, inflation, native-side rotation) | NE:6931-6990 |
| `build_current_validator_set` (wei → whole-token power) | NE:7028-7052 |
| No per-action rollback | NE:3291-3299 |
| `GovernanceManager::market_exists` / `next_market_id` (private) | `torus-economics/src/governance.rs:1457-1478` |
| `StakingManager::all_validators` / `put_validator` / `get_pending_rewards` (CF_STAKING_REWARDS) | `staking.rs:914, 797, 851` |
| `register_validator` creates status **Candidate** | `staking.rs:42-75` |
| inflation credits `credit_rewards` | `torus-economics/src/rewards.rs:166-260` |
| `NativeStateOverlay::iterate_cf` merges DB + parent layer + own pending | `torus-state/src/backend.rs:1166-1189` |
| `pipelined` excludes epoch boundaries (serial barrier) | app.rs:1630-1636 |
| native gate `has_native \|\| fee > 0 \|\| core_writer_due`; parent/overlay built inside | app.rs:1956-1977 |
| hook point / fatal check / tail incl. `process_epoch_boundary` | app.rs:2197-2199 / 2204-2218 / 2233-2236 |
| standalone marker + `advance_untouched` when `!(has_native \|\| fee > 0)` | app.rs:2579-2612 |
| consensus-side rotation (`epoch_validator_set_updates`, validate path; independent of the native phase) | app.rs:4350-4420 |
| RPC `get_header_with_hash`, `store_header` (test), `get_position` UPnL, `get_mark_price` | `torus-rpc/src/eth.rs:102-118`; `lib.rs:641`; `torus.rs:795-802, 1604-1643` |
| precompile dispatch / `order_book_reader` / `read_position` / 0x0802 readers / `get_oracle_price_fp` | `torus-core/src/precompiles.rs:348-350, 365, 460-497, 609-679, 689-701` |
| `TorusPrecompiles` (`current_block`) built from `block_cfg.number` | `torus-evm/src/precompile_provider.rs:24-60, 117-141`; `executor.rs:215, 284, 427` |
| `execute_precompile*` call sites: `precompile_tests.rs` 27, `cross_vm_read.rs` 7, `cross_vm_write.rs` 1, `book_read_modes_tests.rs` 1, `claim_unbonded_tests.rs` 1, `precompile_provider.rs` 2 | |
| `set_mark` helpers `aggregate_price(m, ctx.block_height, &stakes)`; staleness via `ctx.block_height += 1_000` | `account_margin_tests.rs:165-180`, `market_order_margin_tests.rs:467-480` (+ :560), `modify_order_tests.rs:413-426`, `engine_parallel_tests.rs:569-582` |
| app.rs test helpers (`mod crash_recovery_tests`, :6994-EOF): `make_test_config_and_db` :6998, `make_block` :7030 (ts = 1000 + height), `make_exec_ctx` :10185, `link_blocks` :12440, `dispatch_and_execute` :12472, `dump_all_cfs` / `assert_dumps_equal` :12481-12492, `persist_committed_block_durably` :968, WorkerGate pattern :12680-12712 | |
| chaos incremental-vs-full root test | `torus-integration-tests/tests/chaos.rs:321-378` |

## Timestamp finding (T1 pins it)

* **Deterministic for execution: yes.** The proposer chooses it; it is hashed into the block
  identity and committed with the block; every execution path reads the committed header —
  live dispatch and `execute_committed_block` (both via `execute_committed_block_with`,
  app.rs:2156-2181) and crash replay (`load_replay_header`). It already reaches
  `NativeExecContext.timestamp`, and oracle submission rows already store it (NE:6543). The EVM
  gets it via `BlockEnvCfg.timestamp`, but precompiles do not receive it yet (T8 threads it).
* **Units: seconds** (`.as_secs()`); ~15 blocks share a second at today's ~65 ms cadence.
* **NOT validated — safety flag (no rule invented).** Validators do not check the timestamp:
  not monotonic vs the parent, not bounded vs the local clock. A faulty or byzantine proposer
  can set any `u64`. Effects here: a far-future timestamp makes every submission "old" for that
  block and stamps a fresh aggregate in the future, which then stays "fresh" (ages use
  `saturating_sub`) until real time passes it + 60 s; a back-dated one can re-admit a
  validator's last (≤ 1) row or make an aggregate look stale. Honest clock skew between
  proposers makes consecutive timestamps non-monotonic. Session expiry has the same exposure
  today. **Decided (rev. 3): T0b** adds `parent.ts ≤ ts ≤ local_now + 5 s` on the proposal/body
  path and `max(now, parent.ts)` in the proposer — but see T0b's flag: header-first voting
  means the rule runs after the vote.

## Block time vs the time windows

No configured block time (HotStuff rounds as fast as they complete; `timeout_base_ms = 1200`
bounds a failed view, `torus-node/src/main.rs:315`). Measured S432 (memory, not re-measured):
~65 ms/block idle on the 3-of-3 WAN testnet. With time-based windows the semantics no longer
depend on it: 10 s ≈ 150 blocks, 60 s ≈ 900 blocks at idle; a 3 s feeder (B) fits the 10 s
window with margin. Cost side: every block within 10 s of any submission runs the native phase
(C1) — with a feeder that is every block.

## Semantics (exact)

* `now` = the current block's header timestamp (s). Every age is `now.saturating_sub(ts)`.
* Submission rows: ONE per (market, validator) — key `"sub"‖market‖validator` (31 B); the value
  is unchanged (`OracleSubmission`: validator, market, price, block_number, timestamp = the
  submitting block's header timestamp). A new submission overwrites, so the row IS the
  validator's latest submission.
* A row counts at `now` iff `now − row.timestamp ≤ 10`, its price is in `(0, 10^12 units]` and
  the validator is Active (whole-token stake weight). The step runs before the block's actions,
  so a submission of block h first counts in the next executed block.
* ≥ 3 reporters after the 3×MAD cut ⇒ **fresh** aggregate written:
  `{price, block_number = h, num_reporters, timestamp = now}`. < 3 ⇒ nothing written; the last
  aggregate keeps its timestamp and ages.
* **usable** (the one mark rule for every consumer) iff the aggregate exists,
  `now − agg.timestamp ≤ 60` and `price > 0`.
* Pruning at block start: delete every submission row with `now − ts > 10` (and undecodable
  rows). Rows ≤ (validators) × (listed markets) at all times — the per-block work is bounded
  by that, no cap needed.

## Success criteria (each is a named test below)

0. **Every epoch-boundary block runs epoch processing** — today an empty boundary block skips
   `process_epoch_boundary` (rewards, inflation, native-side rotation) (T0, own commit).
1. **Timestamp pinned** — the submission row timestamp equals the committed header timestamp on
   the live, `execute_committed_block` and crash-replay paths (T1).
2. **One mark per block, time-based timing** — the step runs first; a submission of block h
   (ts T) counts while `now ≤ T + 10` and is deleted once `now > T + 10`; the aggregate is
   usable while `now ≤ T_agg + 60`; same-block submissions never move the mark (T5, T6).
3. **End-to-end whole block** — 3 Active validators via signed `SubmitOraclePrices`; after the
   next block the mark is the stake-weighted median; with 2 reporters the last price persists
   and is unusable 61 s after the last fresh aggregate; non-validators rejected (T6).
4. **Submission hardening** — unlisted market, price ∉ (0, 10^12 units], 0 or > 256 entries,
   duplicate market ⇒ whole action rejected, nothing written (T4).
5. **Pruning bounded and complete** (T2, T5, T6).
6. **Errors never abort the block** — per-market results; bounded inputs; exact 3×MAD (R9) (T2, T5).
7. **Consumers** — `AccountReader::mark` / `mark_price`, RPC `getMarkPrice` / `getPosition`,
   precompiles 0x0802 (stale flag) and 0x0800 (UPnL) use the time-based usable rule; ABIs
   unchanged (T2, T8, T9).
8. **Determinism** — serial = pipelined (parked worker) = crash replay; incremental root =
   full scan (T7).
9. **Nothing else moves** — existing suites green after the planned edits.

## Where determinism could break, and how this plan keeps it

| Hazard | Guard |
|--------|-------|
| Wall-clock input | Execution reads only the COMMITTED header timestamp (never `SystemTime`); every path reads the same header (T1 pins it). Its validity is unchecked (S1) but identical on every node. |
| Timestamp backwards / in the future | All ages `saturating_sub` — defined and deterministic; no validity rule invented. |
| Whether the native phase runs (T0 epoch boundary, C1 oracle) | Epoch boundary from height only. `oracle_due` reads the block's overlay (DB + pipelined parent layer), never `self.state_db` alone (block h−1's rows may not be durable yet); a read error fail-stops (never "assume due"). |
| Market list from off-root `CF_NATIVE_MARKETS` | Same source and risk as governance id assignment and genesis seeding; 8-byte keys ascending. |
| Stake set / order | `all_validators()` key order; `Active` only; `whole_token_power` shared with `build_current_validator_set`. |
| Arithmetic panics | Price range at submission AND filtered at aggregation; power ≤ u64 tokens; exact integer 3×MAD. |
| Pruning | One pass over ≤ V × M rows, keyed deletes, errors propagate. |
| In-block visibility | Only `begin_block_oracle` writes `"agg"`, before any action; listings / stake changes of block h act from the next block. |
| Crash replay / pipelining | Same function, same header, layered reads (T7). |
| EVM precompile staleness | `block_cfg.timestamp` = header timestamp on proposer, validator and `eth_call` paths (T8). |

---

## Tasks

### Commit boundaries

| Commit | Tasks | Content |
|--------|-------|---------|
| **1 — epoch** | T0 | epoch processing on every boundary block + single `run_native` flag (C2) |
| **2 — timestamp** | T0b + T1 | proposal timestamp rule, proposer `max(now, parent)`, pin that execution reads the committed header |
| **3 — oracle core** | T2 + T8 | time-based oracle, row layouts, `usable()`, R9, bounds, clamps, compile ripple (AccountReader, set_mark, RPC) AND the precompile 0x0802/0x0800 time rule with the threaded EVM timestamp — one commit, so no commit mixes units |
| 4 | T3 | listed markets (economics) |
| 5 | T4 | submission hardening |
| 6 | T5 | `begin_block_oracle` / `oracle_due` |
| 7 | T6 | wiring into block execution (C1) |
| 8 | T7 | determinism gates |
| 9 | T9 | RPC staleness tests |
| 10 | T10 | docs |

### T0 — consensus: epoch processing on EVERY boundary block (**own commit, first**)

`══ COMMIT 1 (epoch) ══`

Commit on its own, before any oracle change, e.g.
`fix(consensus): run epoch processing on empty epoch-boundary blocks`.

**Test first** — append to `mod crash_recovery_tests` (app.rs):

```rust
    /// T0: epoch processing (permanent-stake rewards, validator inflation,
    /// native-side rotation) lives in the native phase, which is skipped for a
    /// block with no native action, fee or due CoreWriter row. HL runs epochs by
    /// round count: an EMPTY boundary block must still run it.
    fn epoch_fixture(boundary_action: bool) -> bool {
        let (mut config, db) = make_test_config_and_db();
        config.epoch_length = 4;
        let v = Address::new([0x5A; 20]);
        StakingManager::new(db.clone())
            .put_validator(&v, &torus_economics::ValidatorState {
                address: v,
                pubkey: [0x5A; 32],
                commission_bps: 500,
                self_stake: torus_economics::MIN_SELF_DELEGATION * U256::from(100u8),
                total_delegated: U256::ZERO,
                status: torus_economics::ValidatorStatus::Active,
                jailed_until: None,
                last_commission_change_block: None,
            })
            .unwrap();
        let ctx = make_exec_ctx(&config, &db);
        for h in 1..=3u64 {
            ctx.execute_committed_block(&make_block(h, vec![]), vec![]);
        }
        let actions = if boundary_action { vec![sign_claim_rewards(4)] } else { vec![] };
        ctx.execute_committed_block(&make_block(4, actions), vec![]); // 4 = epoch boundary
        assert!(!ctx.exec_failed.load(Ordering::SeqCst));
        assert_eq!(read_native_applied_height(&db), Some(4));
        StakingManager::new(db.clone())
            .get_pending_rewards(&v)
            .unwrap()
            .is_some_and(|r| r.amount > U256::ZERO)
    }

    /// Control (GREEN today): a NON-empty boundary block credits inflation.
    #[test]
    fn nonempty_epoch_boundary_block_runs_epoch_processing() {
        assert!(epoch_fixture(true), "control: inflation must be non-zero for this fixture");
    }

    #[test]
    fn empty_epoch_boundary_block_runs_epoch_processing() {
        assert!(epoch_fixture(false), "empty boundary block 4 must distribute validator inflation");
    }
```

(`make_block(4, …)` links to the empty-chain parent in both variants, see the `make_block` doc
:7031-7040.)

Procedure: on `3e9365e` the control must be GREEN (if the inflation rounds to 0, raise the
stake multiple / `epoch_length` until it is non-zero). **If `empty_…` is also GREEN**, the
empty boundary block does NOT skip: record that (plan notes + memory), keep the two tests as a
pin only if the lead wants, and DROP the fix. Otherwise:

**Implementation** (app.rs:1956-1957, 2579):

```rust
        let core_writer_due = NativeExecutor::core_writer_due(&self.state_db, height);
        // T0: epochs run by height (HL: by round count), whatever the block carries.
        let epoch_boundary = EpochManager::is_epoch_boundary(height, self.epoch_length);
        let run_native = has_native || computed_fee_revenue > 0 || core_writer_due || epoch_boundary;
        if run_native {
```

and :2579 `if !(has_native || computed_fee_revenue > 0) {` → `if !run_native {`. This is C2: a
block that ran the native phase must not also take the standalone-marker / `advance_untouched`
branch (today it does for CoreWriter-only blocks: marker written twice, resident books drained;
it would for empty boundary blocks too). `EpochManager` is imported (app.rs:37); boundaries are
already serial barriers (:1636).

The pipeline fixture has empty boundary blocks 8 and 12 (`epoch_length` 4):
`exec_pipeline_state_identical_over_sequence` compares ON vs OFF — both change alike; if its
`received_on` list or marker expectations move, update them with a one-line justification.

**validate:** `cargo test -j6 -p torus-consensus --lib epoch_boundary_block && cargo test -j6 -p torus-consensus --lib exec_pipeline && cargo test -j6 -p torus-consensus --lib crash` · depends_on: []

**Correction s517** (landed): (1) the fixture uses 4 Active validators (BFT minimum; with 1 the
consensus-side rotation aborts in `check_minimum_set`, so a set-diff check would be vacuous) and
returns an `EpochOutcome` (rewarded, validator status/stake, `epoch_validator_set_updates` at
boundaries 4 and 8 from a `TorusApp` over the same DB); the empty variant must equal the control
(no spurious validator-set diff, memory d8e0cf6b). Blocks 1-3 assert empty non-boundary blocks
still skip the native phase (marker only, native root untouched, no rewards); blocks 5-7 link to
the persisted block 4 header. (2) The `exec_chain_seconds` metrics gate (:2653, documented as
"the SAME predicate as the native section") also moves to `run_native`. RED on `3e9365e`: control
ok, empty FAILED "empty boundary block 4 must distribute validator inflation".

### T0b — consensus: block-timestamp validation + proposer never regresses (**own commit, second**)

> **Superseded placement (rebase s87 onto main, owner option A).** The `finish_validate` /
> `TsRule` / `validate_block_with` placement below was the s517 design. On main a replica votes
> once it holds the body, BEFORE `validate_block` (973cc74), and every rule that can reject a
> block runs before the vote (e89eaaa), so a certified block always inserts. As landed:
> * `ts >= parent.ts` is in `check_parent_link` (a pure header comparison): before the vote and
>   in `validate_block` (insertion) alike.
> * `ts <= local clock + MAX_BLOCK_TIMESTAMP_DRIFT_SECS` (5 s) is pre-vote only:
>   `check_timestamp_drift` in `check_proposal_data`, reached from hotstuff's
>   `App::check_block_data`. Hotstuff now runs that check before EVERY vote for a proposal
>   (header-first body, a block already in the tree, a full proposal right after its
>   insertion); a refused block is still inserted. Never applied at insertion, on block sync,
>   in execution or in crash replay.
> * No `TsRule`, `validate_block_with`, `effective_ts_rule` or `validate_datum_with`; tests
>   inject the clock via `TorusApp::test_now_secs`. Proposer: `max(now, parent.ts)` as planned.
> * Tests: `far_future_proposal_is_refused_before_the_vote`,
>   `far_future_block_is_accepted_at_insertion_sync_execution_and_replay` (no wedge),
>   `regressing_timestamp_is_refused_before_the_vote_and_by_the_parent_link`, plus hotstuff
>   `full_proposal_failing_the_data_check_is_inserted_but_not_voted` and
>   `block_in_the_tree_failing_the_data_check_is_not_voted`.

`══ COMMIT 2 (timestamp) ══` — after T0, before any oracle work, e.g.
`feat(consensus): reject proposals whose timestamp regresses or runs ahead of the local clock`.
Includes T1's pin test (the oracle clock reads the committed header) in the same commit.

**Where validation hooks in (verified — read this first):**

* hotstuff calls the app on three paths: `app.validate_block` for a full proposal
  (`hotstuff_rs/src/hotstuff/implementation.rs:1395`) and for a header-first BODY insert
  (`try_insert_body` :2670-2700, reached from the header path :2414, data responses :2554,
  deferred bodies :2848/:2889, retries :3112 — this also covers by-hash recovery of ancestors);
  `app.validate_block_for_sync` from block sync (`block_sync/client.rs:494`).
* Torus: `validate_block` (app.rs:5191) → `validate_datum_after_fail_stop` (:4806; async-verdict
  hit or `decode_proposal_and_ensure_durable` + `validate_body_checks_core`) → `finish_validate`
  (:4959, parent-hash ancestry + `pending_proposals`). `validate_block_for_sync` (:5229) today
  just delegates to `validate_block`.
* Execution of committed blocks (`execute_committed_block_with`), crash replay
  (`replay_committed`) and the commit feed never call `validate_*` — nothing to exclude there;
  the plan adds no check to them and pins that with tests.
* **FLAG — the vote is NOT gated by the app.** Every proposal is broadcast header-first
  (`broadcast_proposal_as_header`, :964, used at :745/:762/:910/:1953) and replicas phase-vote on
  the `ProposalHeader` (:2340-2385) BEFORE `app.validate_block` runs on the body ("vote-before-DA",
  T1.3). `ProposalHeader` (`messages.rs:412-422`) has no Torus timestamp (it lives in the datum).
  So a rule in `validate_block` — like every existing app check (signatures, attestation) —
  runs AFTER this replica's vote: it keeps the block out of THIS replica's tree (and counts
  `header_vote_invalid_count`), it does not withhold the vote. A true vote-side rule needs a
  hotstuff change (carry the app timestamp in `ProposalHeader`, bound by the block hash, and an
  app hook before the vote), or disabling vote-before-DA. **Needs a decision; not planned here.**
  Consequence today: a byzantine leader's out-of-range timestamp can still be certified by
  header votes, after which honest replicas reject its body — the same liveness exposure as any
  app-invalid body (T1.3). Committed history can therefore still contain a regressing
  timestamp, which is why sync / execution must never apply the rule (below) and why the oracle
  keeps its `saturating_sub` clamps.

**Rule** (pure, node-local, only on the proposal/body path):

```rust
/// Max seconds a proposal's header timestamp may run ahead of this node's clock.
pub(crate) const MAX_BLOCK_TIMESTAMP_DRIFT_SECS: u64 = 5;

/// T0b: reject a proposal whose timestamp regresses below its parent's, or runs
/// more than MAX_BLOCK_TIMESTAMP_DRIFT_SECS ahead of `local_now` (seconds).
/// `parent_ts = None` (parent header not resolvable) skips the regression half.
fn check_proposal_timestamp(ts: u64, parent_ts: Option<u64>, local_now: u64) -> Result<(), String> {
    if let Some(p) = parent_ts {
        if ts < p {
            return Err(format!("timestamp {ts} < parent timestamp {p}"));
        }
    }
    if ts > local_now.saturating_add(MAX_BLOCK_TIMESTAMP_DRIFT_SECS) {
        return Err(format!("timestamp {ts} > local clock {local_now} + {MAX_BLOCK_TIMESTAMP_DRIFT_SECS}s"));
    }
    Ok(())
}
```

* Parent timestamp: from the block tree the request already exposes —
  `request.block_tree().block(&proposed.justify.block)` → decode its datum header (the exact
  decode `produce_block` uses at app.rs:5160-5176; extract it into one helper
  `decode_datum_header(bytes) -> Option<TorusBlockHeader>`, used by both). Genesis justify
  (`justify.is_genesis_pc()`) ⇒ parent = `genesis_parent_header()` whose `timestamp` is 0
  (`torus-bridge/src/proposer.rs:332`) — the first block only has the drift bound.
* Plumbing: `validate_datum_after_fail_stop(datum, hash, ts_rule: Option<TsRule>)` with
  `struct TsRule { parent_ts: Option<u64>, local_now: u64 }`; the check runs in
  `finish_validate` (both the async-hit and the sync decode paths reach it) right after the
  ancestry check, through `effective_ts_rule(height, ts_rule)`, which returns `None` (skip) when
  `ts_rule` is `None` or the block is history (`height <= self.last_header.height` — covers
  by-hash recovery of already-committed ancestors on the body path). `validate_block` passes `Some(TsRule { parent_ts, local_now:
  SystemTime::now().as_secs() })`; `validate_block_for_sync` passes `None` (refactor: both call
  a private `validate_block_with(request, ts_rule)`); the test-facing `validate_datum` gets a
  sibling `validate_datum_with(datum, hash, ts_rule)` so tests inject `local_now`.
* Proposer (`build_proposal`, app.rs:5010-5013): `let timestamp = now_secs.max(parent_header.timestamp);`
  — never below the parent. (Its parent header falls back to `self.last_header` when the parent
  datum cannot be decoded (:5177-5183); then it may still be below the real parent — flagged,
  liveness only.)

**Test first** — `mod crash_recovery_tests` (app.rs):

```rust
    // ---- T0b: block timestamp validation ----

    #[test]
    fn proposal_timestamp_rule_boundaries() {
        assert!(check_proposal_timestamp(100, Some(100), 1_000).is_ok(), "equal to parent");
        assert!(check_proposal_timestamp(99, Some(100), 1_000).is_err(), "regresses");
        assert!(check_proposal_timestamp(1_005, Some(100), 1_000).is_ok(), "drift 5");
        assert!(check_proposal_timestamp(1_006, Some(100), 1_000).is_err(), "drift 6");
        assert!(check_proposal_timestamp(0, Some(0), 0).is_ok(), "first block after genesis (ts 0)");
        assert!(check_proposal_timestamp(50, None, 1_000).is_ok(), "unknown parent: drift only");
        assert!(check_proposal_timestamp(u64::MAX, None, u64::MAX).is_ok(), "no overflow");
    }

    /// A TorusApp whose committed tip is `parent` (so the proposal at parent+1 has
    /// a resolvable parent via `last_header`) — helper for the validate tests.
    fn app_with_tip(parent: &TorusBlock) -> (TorusApp, StateDb) { /* TorusApp::new on a fresh
        make_test_config_and_db(); persist `parent` (execute_committed_block) and set
        app.last_header = parent.header.clone() — mirror async_validate_flag_off_parity :9134 */ }

    fn child_of(parent: &TorusBlock, ts: u64) -> TorusBlock {
        let mut b = make_block(parent.header.height + 1, vec![sign_claim_rewards(parent.header.height + 1)]);
        b.header.parent_hash = alloy_primitives::keccak256(parent.header.canonical_header_bytes());
        b.header.timestamp = ts;
        b
    }

    fn vote_rule(app: &TorusApp, parent_ts: u64, now: u64) -> Option<TsRule> {
        let _ = app;
        Some(TsRule { parent_ts: Some(parent_ts), local_now: now })
    }

    #[test]
    fn proposal_whose_timestamp_regresses_is_rejected() {
        let mut parent = make_block(4, vec![]);
        parent.header.timestamp = 2_000;
        let (mut app, _db) = app_with_tip(&parent);
        let datum = encode_proposal_datum(&child_of(&parent, 1_999), false);
        let r = app.validate_datum_with(&datum, &data_hash_of(&datum), vote_rule(&app, 2_000, 2_010));
        assert!(matches!(r, ValidateBlockResponse::Invalid));
    }

    #[test]
    fn proposal_equal_to_parent_or_within_drift_is_accepted() {
        let mut parent = make_block(4, vec![]);
        parent.header.timestamp = 2_000;
        for ts in [2_000u64, 2_015] {
            let (mut app, _db) = app_with_tip(&parent);
            let datum = encode_proposal_datum(&child_of(&parent, ts), false);
            let r = app.validate_datum_with(&datum, &data_hash_of(&datum), vote_rule(&app, 2_000, 2_010));
            assert!(matches!(r, ValidateBlockResponse::Valid { .. }), "ts {ts}");
        }
    }

    #[test]
    fn proposal_far_in_the_future_is_rejected() {
        let mut parent = make_block(4, vec![]);
        parent.header.timestamp = 2_000;
        let (mut app, _db) = app_with_tip(&parent);
        let datum = encode_proposal_datum(&child_of(&parent, 2_016), false);
        let r = app.validate_datum_with(&datum, &data_hash_of(&datum), vote_rule(&app, 2_000, 2_010));
        assert!(matches!(r, ValidateBlockResponse::Invalid));
    }

    /// Genesis / first block: parent = genesis_parent_header (ts 0).
    #[test]
    fn first_block_only_has_the_drift_bound() {
        let (config, db) = make_test_config_and_db();
        let mut app = TorusApp::new(db, &config, None, None, None);
        let mut b1 = make_block(1, vec![]);
        b1.header.timestamp = 1_700_000_000;
        let datum = encode_proposal_datum(&b1, false);
        let ok = Some(TsRule { parent_ts: Some(0), local_now: 1_700_000_000 });
        assert!(matches!(app.validate_datum_with(&datum, &data_hash_of(&datum), ok), ValidateBlockResponse::Valid { .. }));
    }

    /// The proposer never emits a timestamp below its parent's, even when the
    /// parent's is ahead of the local clock (within drift), and the block it
    /// produces passes the rule on a validator with the same clock.
    #[test]
    fn proposer_after_a_future_ish_parent_still_produces_a_valid_timestamp() {
        let (config, db) = make_test_config_and_db();
        let mut app = TorusApp::new(db, &config, None, None, None);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let mut parent = app.last_header.clone();
        parent.timestamp = now + 3;
        let _ = app.build_proposal(parent.clone());
        let produced = &app.pending_proposals[&(parent.height + 1)].block;
        assert!(produced.header.timestamp >= parent.timestamp, "never below the parent");
        assert!(check_proposal_timestamp(produced.header.timestamp, Some(parent.timestamp), now).is_ok());
    }

    /// Committed history is accepted as-is: the sync path skips the rule, and so
    /// do blocks at or below the local tip (recovered ancestors); execution and
    /// crash replay never check timestamps.
    #[test]
    fn sync_history_execution_and_replay_accept_any_committed_timestamp() {
        let mut parent = make_block(4, vec![]);
        parent.header.timestamp = 2_000;
        // sync path: no rule
        let (mut app, _db) = app_with_tip(&parent);
        let datum = encode_proposal_datum(&child_of(&parent, 1_000), false);
        assert!(matches!(app.validate_datum_with(&datum, &data_hash_of(&datum), None), ValidateBlockResponse::Valid { .. }));
        // history (height <= local tip, e.g. a recovered ancestor on the body path):
        // the rule is not applied even on the vote path
        assert!(app.effective_ts_rule(parent.header.height, vote_rule(&app, 0, 2_010)).is_none());
        assert!(app.effective_ts_rule(parent.header.height + 1, vote_rule(&app, 0, 2_010)).is_some());
        // execution + replay of a regressing committed chain
        for replay in [false, true] {
            let (config, db) = make_test_config_and_db();
            let ctx = make_exec_ctx(&config, &db);
            let mut blocks = vec![make_block(1, vec![sign_claim_rewards(1)]), make_block(2, vec![sign_claim_rewards(2)])];
            blocks[0].header.timestamp = 5_000;
            blocks[1].header.timestamp = 4_000; // regresses: committed anyway (header-first voting)
            link_blocks(&mut blocks);
            if replay {
                for b in &blocks { persist_committed_block_durably(&db, b); }
                assert_eq!(TorusApp::replay_committed(&db, &ctx).1, None);
            } else {
                for b in &blocks { ctx.execute_committed_block(b, vec![]); }
            }
            assert!(!ctx.exec_failed.load(Ordering::SeqCst));
            assert_eq!(read_native_applied_height(&db), Some(2), "replay={replay}");
        }
    }
```

(`app_with_tip` is a sketch to finish while writing: mirror the TorusApp setup of
`async_validate_flag_off_parity` (:9134-9150), execute `parent` as a committed block and set
`app.last_header = parent.header.clone()`. `effective_ts_rule(&self, height, rule) ->
Option<TsRule>` is the one gate `finish_validate` uses: `None` when `rule` is `None` or
`height <= self.last_header.height`.)

RED: `check_proposal_timestamp` / `TsRule` / `validate_datum_with` do not exist; the proposer
test fails whenever the parent is ahead of the clock (today `timestamp = now`).

**Correction s517 (as implemented on crab; superseded on rebase s87, see the note at the top of T0b):**
* `validate_block_with(request, proposal_path: bool)` (not `ts_rule`): the rule needs the
  request's block tree for the parent, so it is built inside — `validate_block` passes `true`,
  `validate_block_for_sync` `false`. Parent ts = `parent_timestamp(&request)` (genesis PC ⇒
  `genesis_parent_header().timestamp`, else `block_tree().block(&justify.block)` → header).
* `decode_datum_header` decodes only the header prefix (`bincode::deserialize::<TorusBlockHeader>`,
  trailing body ignored; both datum formats start with the header) — cheap on the vote path;
  `produce_block` uses it too. Pinned by `proposal_timestamp_datum_header_decodes_full_and_compact`.
* `unix_now_secs()` shared by proposer and validator; tests inject `local_now` via `TsRule`.
* `finish_validate(block, ts_rule)` — its 3 existing test callers pass `None`; `validate_datum`
  (test entry) = `validate_datum_with(.., None)`, so existing async-validate tests are unchanged.
* `app_with_tip` just sets `last_header` (no execution needed); `vote_rule(parent_ts, now)` drops
  the unused `app` param. Test names share the `proposal_timestamp` prefix; the committed-height
  skip is also tested end-to-end (`proposal_timestamp_rule_skips_committed_heights`, heights 3/4,
  regressing AND far-future ts accepted), and split from the sync/execution/replay test.
* validate: `cargo test -j6 -p torus-consensus --lib -- proposal_timestamp oracle_clock`.

**validate:** `cargo test -j6 -p torus-consensus --lib timestamp && cargo test -j6 -p torus-consensus --lib async_validate && cargo test -j6 -p torus-consensus --lib crash && cargo test -j6 -p torus-consensus --lib oracle_clock` · depends_on: [0]

### T1 — verify & pin: the committed header timestamp reaches execution on every path

**Test first** (expected GREEN — a pin). Append to `mod crash_recovery_tests` the oracle test
helpers (used again by T6/T7) and the pin:

```rust
    // ---- item 2: oracle helpers ----

    const ORACLE_MARKET: u64 = 1;
    /// (signing-key seed, stake multiple of MIN_SELF_DELEGATION).
    const ORACLE_VALIDATORS: [(u8, u64); 3] = [(61, 1), (62, 1), (63, 3)];

    fn oracle_key(seed: u8) -> k256::ecdsa::SigningKey {
        k256::ecdsa::SigningKey::from_slice(&[seed; 32]).unwrap()
    }

    fn oracle_addr(seed: u8) -> Address {
        torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, 0, &oracle_key(seed))
            .recover_sender()
            .unwrap()
    }

    fn oracle_put_validator(db: &StateDb, seed: u8, mult: u64, status: torus_economics::ValidatorStatus) {
        StakingManager::new(db.clone())
            .put_validator(
                &oracle_addr(seed),
                &torus_economics::ValidatorState {
                    address: oracle_addr(seed),
                    pubkey: [seed; 32],
                    commission_bps: 0,
                    self_stake: torus_economics::MIN_SELF_DELEGATION * U256::from(mult),
                    total_delegated: U256::ZERO,
                    status,
                    jailed_until: None,
                    last_commission_change_block: None,
                },
            )
            .unwrap();
    }

    /// 3 Active validators, market 1 listed, epoch length 1000 (no boundary).
    fn oracle_fixture_db() -> (ChainConfig, StateDb) {
        let (mut config, db) = make_test_config_and_db();
        config.epoch_length = 1_000;
        for (seed, mult) in ORACLE_VALIDATORS {
            oracle_put_validator(&db, seed, mult, torus_economics::ValidatorStatus::Active);
        }
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &ORACLE_MARKET.to_be_bytes(), b"listed")
            .unwrap();
        (config, db)
    }

    fn px(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    /// Heights 1..=rounds.len() (ts = 1000 + h: one second per block); round i =
    /// the (seed, price) submissions of block i+1; linked to actual parents.
    fn oracle_blocks(rounds: &[&[(u8, i64)]]) -> Vec<TorusBlock> {
        let mut blocks: Vec<TorusBlock> = rounds
            .iter()
            .enumerate()
            .map(|(i, subs)| {
                let h = i as u64 + 1;
                let actions = subs
                    .iter()
                    .map(|&(seed, price)| {
                        torus_types::eip712::sign_native_action(
                            NativeAction::SubmitOraclePrices(torus_types::OracleSubmission {
                                prices: vec![(ORACLE_MARKET, px(price))],
                                timestamp: 0,
                            }),
                            h * 1_000 + seed as u64,
                            &oracle_key(seed),
                        )
                    })
                    .collect();
                make_block(h, actions)
            })
            .collect();
        link_blocks(&mut blocks);
        blocks
    }

    fn oracle_sub_rows(db: &StateDb) -> Vec<(Vec<u8>, Vec<u8>)> {
        StateBackend::iterate_cf(db, torus_state::cf::CF_NATIVE_ORACLE, Some(b"sub")).unwrap()
    }

    /// T1: the oracle clock is the COMMITTED header timestamp. The submission
    /// row stores it (last 8 bytes of the value) — identical on the live
    /// dispatch path, execute_committed_block and crash replay.
    #[test]
    fn oracle_clock_is_the_committed_header_timestamp_on_every_path() {
        for path in ["serial", "dispatch", "replay"] {
            let (config, db) = oracle_fixture_db();
            let ctx = make_exec_ctx(&config, &db);
            let mut blocks = oracle_blocks(&[&[(61, 100)]]);
            blocks[0].header.timestamp = 777_777; // height 1: parent is the genesis header
            match path {
                "serial" => ctx.execute_committed_block(&blocks[0], vec![]),
                "dispatch" => dispatch_and_execute(&ctx, &db, &blocks[0]),
                _ => {
                    persist_committed_block_durably(&db, &blocks[0]);
                    let (_, parked) = TorusApp::replay_committed(&db, &ctx);
                    assert_eq!(parked, None, "{path}");
                }
            }
            assert_eq!(read_native_applied_height(&db), Some(1), "{path}");
            let rows = oracle_sub_rows(&db);
            assert_eq!(rows.len(), 1, "{path}");
            let v = &rows[0].1;
            assert_eq!(u64::from_be_bytes(v[v.len() - 8..].try_into().unwrap()), 777_777, "{path}");
        }
    }
```

No implementation. Record the *Timestamp finding* (incl. S1) in
`docs/plans/oracle-aggregation.md` → *Open Questions* (answered) in the same commit.

Lands in `══ COMMIT 2 (timestamp) ══` together with T0b.

**Correction s517:** the pin also asserts `!ctx.exec_failed` per path; otherwise as drafted
(GREEN on first run, as expected for a pin).

**validate:** `cargo test -j6 -p torus-consensus --lib oracle_clock` · depends_on: ["0b"]

### T2 — core: time-based oracle, bounds, one row per (market, validator), `usable()`, prune by time, exact 3×MAD (+ compile ripple)

`══ COMMIT 3 (oracle core) ══` — T2 and T8 are ONE commit (T8's precompile changes are part of it).

**Test first** — `crates/torus-core/tests/oracle_tests.rs`.

*Planned edits of the 10 existing tests* (new signatures `aggregate_price(market, block, now,
stakes)` and `get_price(market, now)`; config fields `max_age_secs`, `min_oracle_reporters`,
`window_secs`):
- every test submits with `ts = 0` → aggregate with `now = 0`
  (`aggregate_price(market, block, 0, &stakes)`, `get_price(market, 0)`); results unchanged.
- `staleness_detection`: config `{ max_age_secs: 50, min_oracle_reporters: 1, window_secs: 10 }`;
  `submit_price(&addr(1), market, fp(1000), 100, 1_000)`; `aggregate_price(market, 100, 1_000,
  &stakes)`; `get_price(market, 1_010)` not stale, price 1000; `get_price(market, 1_100)` stale,
  price still 1000.
- `normal_aggregation_5_validators`, `outlier_rejected`, `all_same_price`,
  `stake_weight_dominance`, `no_floating_point_in_aggregation`, `oracle_price_reports_num_reporters`,
  `single_validator_works`, `validator_overwrites_same_block`, `no_data_returns_error`: only the
  argument change (`get_price(999, 0)` in the last).

*New tests* — extend imports:

```rust
use torus_core::oracle::{valid_oracle_price, MAX_ORACLE_PRICE_RAW};
use torus_state::cf::CF_NATIVE_ORACLE;
use torus_state::StateBackend;

fn sub_rows(db: &StateDb) -> usize {
    StateBackend::iterate_cf(db, CF_NATIVE_ORACLE, Some(b"sub")).unwrap().len()
}

fn three_equal() -> Vec<(Address, FixedPoint)> {
    (1..=3u8).map(|v| (addr(v), fp(1))).collect()
}

fn mgr_on(db: &StateDb) -> OracleManager {
    OracleManager::new(db.clone(), OracleConfig::default())
}

/// One row per (market, validator): a later submission replaces the earlier.
#[test]
fn a_validator_has_one_row_per_market_its_latest() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    mgr.submit_price(&addr(1), 1, fp(100), 5, 1_005).unwrap();
    mgr.submit_price(&addr(1), 1, fp(200), 6, 1_006).unwrap();
    mgr.submit_price(&addr(1), 2, fp(7), 6, 1_006).unwrap();
    assert_eq!(sub_rows(&db), 2);
    for v in 2..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(200), 6, 1_006).unwrap();
    }
    assert_eq!(mgr.aggregate_price(1, 7, 1_007, &three_equal()).unwrap(), fp(200));
}

/// Window: a row counts iff now − ts <= 10; a fresh aggregate records (block, now).
#[test]
fn a_submission_counts_for_ten_seconds() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 5, 1_000).unwrap();
    }
    assert_eq!(mgr.aggregate_price(1, 6, 1_010, &three_equal()).unwrap(), fp(100), "age 10");
    let p = mgr.get_price(1, 1_010).unwrap();
    assert_eq!((p.block_number, p.timestamp), (6, 1_010));
    // age 11: no row counts -> the last price is returned, NOT re-stamped
    assert_eq!(mgr.aggregate_price(1, 7, 1_011, &three_equal()).unwrap(), fp(100));
    assert_eq!(mgr.get_price(1, 1_011).unwrap().timestamp, 1_010);
}

/// Staleness: usable while now − agg.ts <= 60.
#[test]
fn usable_is_the_time_based_mark_rule() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 10, 2_000).unwrap();
    }
    mgr.aggregate_price(1, 10, 2_000, &three_equal()).unwrap();
    assert_eq!(mgr.get_price(1, 2_060).unwrap().usable(), Some(fp(100)), "age 60");
    assert_eq!(mgr.get_price(1, 2_061).unwrap().usable(), None, "age 61");
    assert_eq!(mgr.get_price(1, 1_500).unwrap().usable(), Some(fp(100)), "clock behind: age 0");
}

/// Prune: every row with now − ts > 10 goes (all markets); undecodable rows go.
#[test]
fn prune_deletes_every_row_older_than_the_window() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for (v, ts) in [(1u8, 1_000u64), (2, 1_005), (3, 1_010)] {
        for m in [1u64, 7] {
            mgr.submit_price(&addr(v), m, fp(100), 1, ts).unwrap();
        }
    }
    db.put_cf_raw(CF_NATIVE_ORACLE, &[b"sub".as_slice(), &[9u8; 28]].concat(), &[1, 2])
        .unwrap();
    assert_eq!(mgr.prune_submissions(1_015).unwrap(), 3, "v1 x 2 markets (age 15) + corrupt row");
    assert_eq!(sub_rows(&db), 4);
    assert_eq!(mgr.prune_submissions(1_015).unwrap(), 0, "idempotent");
}

/// Aggregation never deletes (pruning is the block-start step).
#[test]
fn aggregate_price_is_read_only_on_submissions() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 1, 1_000).unwrap();
    }
    assert!(mgr.aggregate_price(1, 50, 1_050, &three_equal()).is_err(), "nothing in the window");
    assert_eq!(sub_rows(&db), 3);
}

/// Out-of-range rows (only writable by bypassing the handler) are ignored —
/// before: `mean_abs_deviation` overflowed and PANICKED.
#[test]
fn out_of_range_rows_are_ignored_not_a_panic() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 100, 0).unwrap();
    }
    mgr.submit_price(&addr(4), 1, FixedPoint::from_raw(i128::MAX), 100, 0).unwrap();
    mgr.submit_price(&addr(5), 1, fp(-5), 100, 0).unwrap();
    let stakes: Vec<_> = (1..=5u8).map(|v| (addr(v), fp(1))).collect();
    assert_eq!(mgr.aggregate_price(1, 100, 0, &stakes).unwrap(), fp(100));
    assert_eq!(mgr.get_price(1, 0).unwrap().num_reporters, 3);
}

/// Guard (GREEN): the extreme VALID inputs never overflow — 100 validators
/// alternating the smallest / largest price, all at u64::MAX power.
#[test]
fn extreme_valid_inputs_aggregate_without_panic() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    let max = FixedPoint::from_raw(MAX_ORACLE_PRICE_RAW);
    let whale = FixedPoint::from_raw(u64::MAX as i128 * FixedPoint::SCALE);
    for v in 1..=100u8 {
        let p = if v % 2 == 0 { max } else { FixedPoint::from_raw(1) };
        mgr.submit_price(&addr(v), 1, p, 100, 0).unwrap();
    }
    let stakes: Vec<_> = (1..=100u8).map(|v| (addr(v), whale)).collect();
    let _ = mgr.aggregate_price(1, 100, 0, &stakes);
    assert!(valid_oracle_price(max));
    assert!(!valid_oracle_price(FixedPoint::from_raw(MAX_ORACLE_PRICE_RAW + 1)));
    assert!(!valid_oracle_price(FixedPoint::ZERO));
    assert!(!valid_oracle_price(fp(-1)));
}

/// R9: 100 / 100 / 101 — 3 x MAD is exactly 1, so 101 must be KEPT. The
/// truncated FixedPoint MAD (0.33333333 x 3 = 0.99999999) cut it, leaving 2
/// reporters (< 3) and no price.
#[test]
fn two_equal_prices_and_one_other_keep_all_three() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for (v, p) in [(1u8, 100), (2, 100), (3, 101)] {
        mgr.submit_price(&addr(v), 1, fp(p), 100, 0).unwrap();
    }
    assert_eq!(mgr.aggregate_price(1, 100, 0, &three_equal()).unwrap(), fp(100));
    assert_eq!(mgr.get_price(1, 0).unwrap().num_reporters, 3);
}

/// Defence in depth (rev. 3): every age clamps at 0. Rows / an aggregate stamped
/// AFTER `now` (possible in committed history, see T0b's flag) have age 0: the
/// rows are not pruned and count, the aggregate is not stale. No panic.
#[test]
fn ages_clamp_at_zero_when_timestamps_run_backwards() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 5, 2_000).unwrap();
    }
    assert_eq!(mgr.prune_submissions(1_000).unwrap(), 0, "future rows: age 0, kept");
    assert_eq!(mgr.aggregate_price(1, 6, 1_000, &three_equal()).unwrap(), fp(100), "they count");
    let p = mgr.get_price(1, 500).unwrap();
    assert!(!p.stale, "aggregate stamped after now: age 0");
    assert_eq!(p.usable(), Some(fp(100)));
}
```

**Implementation** (`crates/torus-core/src/oracle.rs`):

1. Constants / config (:19-22, :115-131):

```rust
/// Oracle price max age in SECONDS of block (header) time: an aggregate is
/// usable while `now − its timestamp <= 60`. Single source for precompiles.rs.
pub(crate) const DEFAULT_MAX_ORACLE_AGE_SECS: u64 = 60;
/// A validator's latest submission counts while `now − its block ts <= 10`.
const DEFAULT_ORACLE_WINDOW_SECS: u64 = 10;
const DEFAULT_MIN_ORACLE_REPORTERS: usize = 3;

/// Most entries one `SubmitOraclePrices` may carry. A valid submission has one
/// entry per LISTED market (duplicates are rejected), so the cap only has to
/// exceed the listed-market count (HL lists ~200 perps); it bounds one action's
/// validation reads and row writes. A feeder with more markets splits them.
pub const MAX_ORACLE_PRICES_PER_SUBMISSION: usize = 256;

/// Largest accepted oracle price: 10^12 units (raw 10^20). Far above any asset
/// and small enough that every aggregation sum / product stays far inside i128
/// (its `FixedPoint` operators panic on overflow).
pub const MAX_ORACLE_PRICE_RAW: i128 = 1_000_000_000_000 * FixedPoint::SCALE;

/// Accepted range of one oracle price: `0 < price <= MAX_ORACLE_PRICE_RAW`.
pub fn valid_oracle_price(price: FixedPoint) -> bool {
    price > FixedPoint::ZERO && price.raw() <= MAX_ORACLE_PRICE_RAW
}

pub struct OracleConfig {
    /// Seconds after the last fresh aggregate's block timestamp before it is stale.
    pub max_age_secs: u64,
    pub min_oracle_reporters: usize,
    /// Seconds a submission counts after its block timestamp.
    pub window_secs: u64,
}
// Default: DEFAULT_MAX_ORACLE_AGE_SECS, DEFAULT_MIN_ORACLE_REPORTERS, DEFAULT_ORACLE_WINDOW_SECS
```

2. `OraclePrice` gains `pub timestamp: u64` (block timestamp of the last fresh aggregate) and

```rust
impl OraclePrice {
    /// The mark rule of every reader: fresh (age <= max age) and > 0.
    pub fn usable(&self) -> Option<FixedPoint> {
        (!self.stale && self.price > FixedPoint::ZERO).then_some(self.price)
    }
}
```

3. `StoredAggregatedPrice` gains `timestamp: u64`, serialized LAST (price 16 ‖ block 8 ‖
   reporters 4 ‖ timestamp 8 = 36 B; the precompile's offsets 0..24 stay valid).
4. `submission_key(market, validator)` = `"sub"‖market‖validator` (31 B, doc updated) — drop the
   block suffix; `submit_price` signature unchanged (block and ts stay in the value).
5. `aggregate_price(&self, market_id, current_block, now, validator_stakes)`: collect with `now`;
   ≥ min reporters → store `{price, block_number: current_block, num_reporters, timestamp: now}`;
   below min → `get_last_valid_price(market_id, now)` (no write). FIX 19's single-reporter
   branch stays untouched (dead with min 3; configs with min 1 still use it).
6. `get_price(&self, market_id, now)` and `get_last_valid_price(market_id, now)`:
   `stale = now.saturating_sub(stored.timestamp) > self.config.max_age_secs`; fill `timestamp`.
7. `collect_submissions(market_id, now)`: read-only; iterate `submission_market_prefix(market)`,
   keep decodable rows with `now.saturating_sub(sub.timestamp) <= window_secs &&
   valid_oracle_price(sub.price)`; return them in key order (one per validator — the
   latest-per-validator `BTreeMap` and the prune code (:308-330) are deleted).
8. After `get_price`:

```rust
    /// Block-start step: delete every submission row (all markets) whose block
    /// timestamp is more than the window older than `now`, and rows that do not
    /// decode. Rows are one per (market, validator), so the pass is bounded by
    /// validators × markets. Errors propagate. Returns the number deleted.
    pub fn prune_submissions(&self, now: u64) -> Result<usize, CoreError> {
        let mut pruned = 0;
        for (key, value) in self.state.iterate_cf(CF_NATIVE_ORACLE, Some(b"sub"))? {
            let old = match OracleSubmission::try_from_slice(&value) {
                Ok(sub) => now.saturating_sub(sub.timestamp) > self.config.window_secs,
                Err(_) => true,
            };
            if old {
                self.state.delete_cf_raw(CF_NATIVE_ORACLE, &key)?;
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    /// Whether any submission row exists (the block's oracle step is due).
    pub fn has_submissions(&self) -> Result<bool, CoreError> {
        Ok(!self.state.iterate_cf(CF_NATIVE_ORACLE, Some(b"sub"))?.is_empty())
    }
```

9. R9 — `reject_outliers` (:394-421) exact integer rule; delete `mean_abs_deviation` (:379-391):

```rust
/// Keep x iff |x − median| <= 3 × MAD, MAD = Σ|p − median| / n — compared
/// exactly in raw integers as n·|x − median| <= 3·Σ|p − median| (inputs are
/// bounded by MAX_ORACLE_PRICE_RAW: no overflow). The FixedPoint form truncated
/// MAD and cut the boundary case (two equal prices + one other).
fn reject_outliers(pairs: &[(FixedPoint, FixedPoint)]) -> Vec<(FixedPoint, FixedPoint)> {
    if pairs.len() <= 1 {
        return pairs.to_vec();
    }
    let prices: Vec<FixedPoint> = pairs.iter().map(|(p, _)| *p).collect();
    let med = simple_median(&prices).raw();
    let dev = |p: FixedPoint| (p.raw() - med).abs();
    let n = prices.len() as i128;
    let three_sum: i128 = 3 * prices.iter().map(|&p| dev(p)).sum::<i128>();
    pairs.iter().filter(|(p, _)| n * dev(*p) <= three_sum).cloned().collect()
}
```

   The unit tests `test_reject_outliers_*` / `single_*` (oracle.rs:492-536) and
   `outlier_rejected` keep their results.

*Compile ripple — same commit (semantics final):*
- NE `AccountReader` (:441-456): replace `height: u64` by `now: u64` (`ctx.timestamp`; verify
  `height` has no other use there); `mark` →
  `self.oracle.get_price(market_id, self.now).ok().and_then(|p| p.usable())`; doc: "time-based;
  only `begin_block_oracle` writes the row, before any action of the block".
- NE:6819 `aggregate_price(market_id, ctx.block_height, ctx.timestamp, validator_stakes)`.
- `set_mark` ×4 (`account_margin_tests.rs:175`, `market_order_margin_tests.rs:477`,
  `modify_order_tests.rs:423`, `engine_parallel_tests.rs:579`):
  `.aggregate_price(market_id, ctx.block_height, ctx.timestamp, &stakes)`.
- `market_order_margin_tests.rs:560`: `ctx.block_height += 1_000;` → `ctx.timestamp += 61;`
  and its doc: "61 s after the aggregate: stale".
- RPC `torus.rs:797-801` and `:1605-1612`: `now` = header timestamp of the latest committed
  height (`crate::eth::get_header_with_hash(<RpcState>, latest)` → `header.timestamp`; no header
  ⇒ no mark). `getPosition`: `oracle.get_price(mid, now).ok().and_then(|op| op.usable())
  .unwrap_or(p.entry_price)`. `getMarkPrice`: `usable` ⇒ `(price, price, op.block_number)` else
  `(0, 0, 0)` (tests in T9).
- Precompiles: T8's whole change (threaded EVM timestamp, 36-byte decode, time rule for
  0x0802 / 0x0800) is part of THIS commit — no intermediate state with block numbers compared
  against seconds.

**validate:** `cargo test -j6 -p torus-core --test oracle_tests && cargo test -j6 -p torus-core --lib oracle::tests && cargo test -j6 -p torus-bridge --test market_order_margin_tests && cargo test -j6 -p torus-bridge --test account_margin_tests && cargo test -j6 -p torus-bridge --test modify_order_tests && cargo check --workspace --all-targets` (+ T8's validate before committing) · depends_on: [1]

**Correction s517 (as implemented):**
* `aggregate_price`: an EMPTY counting set (no row within 10 s) and "no row from a staked
  validator" now also take the below-min path (`get_last_valid_price(market, now)`, no write)
  instead of `Err(NoOraclePrice)` — required by the plan's own
  `a_submission_counts_for_ten_seconds` (age 11 ⇒ last price); with no prior aggregate it is
  still `Err` (`aggregate_price_is_read_only_on_submissions`).
* `get_price` / `get_last_valid_price` share `stored_aggregate` + `is_stale` (one staleness rule).
* RPC: one helper `RpcState::usable_oracle_price(mid)` (torus.rs) used by `getPosition` and
  `getMarkPrice`; a missing OR unreadable latest header ⇒ no mark. The existing
  `get_mark_price_from_oracle` test (lib.rs) now writes the 36-byte row and stores the latest
  header (it wrote a 28-byte row and no header).
* RED: compile errors (new API); behavioural RED with the time-based API in place and the old
  outlier code: `two_equal_prices_and_one_other_keep_all_three` (R9) and
  `a_submission_counts_for_ten_seconds` (the gap above). oracle_tests adds 9 new tests (not 10).

### T3 — economics: listed markets (`governance.rs`)

**Test first** — append to `crates/torus-economics/tests/governance_tests.rs`:

```rust
/// Item 2: the oracle aggregates exactly the listed markets — the 8-byte keys
/// of CF_NATIVE_MARKETS, ascending; metadata rows (other key lengths) skipped.
#[test]
fn listed_market_ids_are_the_8_byte_keys_ascending() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let gov = GovernanceManager::new(db.clone());
    for id in [9u64, 1, 300] {
        db.put_cf_raw(CF_NATIVE_MARKETS, &id.to_be_bytes(), b"m").unwrap();
    }
    db.put_cf_raw(CF_NATIVE_MARKETS, b"__book_mode__", &[1]).unwrap();
    db.put_cf_raw(CF_NATIVE_MARKETS, b"__next_global_order_id__", &7u128.to_be_bytes())
        .unwrap();
    assert_eq!(gov.listed_market_ids().unwrap(), vec![1, 9, 300]);
    assert!(gov.market_exists(9).unwrap());
    assert!(!gov.market_exists(2).unwrap());
}
```

**Implementation** (`governance.rs:1456-1478`): `market_exists` → `pub`; add

```rust
    /// Listed market ids: the 8-byte big-endian keys of CF_NATIVE_MARKETS,
    /// ascending. Metadata rows (`__book_mode__`, `__next_global_order_id__`)
    /// have other key lengths and are skipped.
    pub fn listed_market_ids(&self) -> Result<Vec<u64>> {
        Ok(self
            .state
            .iterate_cf(CF_NATIVE_MARKETS, None)?
            .iter()
            .filter(|(k, _)| k.len() == 8)
            .map(|(k, _)| u64::from_be_bytes(k[..8].try_into().unwrap()))
            .collect())
    }

    fn next_market_id(&self) -> Result<u64> {
        let max = self.listed_market_ids()?.last().copied().unwrap_or(0);
        max.checked_add(1)
            .ok_or(EconomicsError::MarketIdInUse(u64::MAX))
    }
```

**validate:** `cargo test -j6 -p torus-economics --test governance_tests` · depends_on: []

### T4 — bridge: submission hardening (`exec_submit_oracle_prices`)

**Test first** — new file `crates/torus-bridge/tests/oracle_block_tests.rs`:

```rust
//! Item 2 (option A, time-based): oracle aggregation into block execution.
//! `docs/plans/oracle-aggregation.md` and `docs/plans/oracle-aggregation-impl.md`.

use alloy_primitives::{Address, U256};

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::oracle::{MAX_ORACLE_PRICES_PER_SUBMISSION, MAX_ORACLE_PRICE_RAW};
use torus_core::position::NativeBalance;
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_ORACLE};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OracleSubmission, OrderType, PlaceOrderParams,
    TimeInForce,
};

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// Context at block `height`, timestamp `1_000 + height` (one second per block,
/// so seconds and blocks line up in the assertions); epoch length 1000.
fn ctx_at<T: StateBackend>(state: T, height: u64) -> NativeExecContext<T> {
    NativeExecContext::new(state, height, 1_000 + height, 0, 1_000, 10, addr(99), addr(100), addr(101))
}

const V1: u8 = 1;
const V2: u8 = 2;
const V3: u8 = 3; // 3x stake

fn put_validator(db: &StateDb, n: u8, stake: U256, status: ValidatorStatus) {
    StakingManager::new(db.clone())
        .put_validator(
            &addr(n),
            &ValidatorState {
                address: addr(n),
                pubkey: [n; 32],
                commission_bps: 0,
                self_stake: stake,
                total_delegated: U256::ZERO,
                status,
                jailed_until: None,
                last_commission_change_block: None,
            },
        )
        .unwrap();
}

fn list_market(db: &StateDb, id: MarketId) {
    db.put_cf_raw(CF_NATIVE_MARKETS, &id.to_be_bytes(), b"listed").unwrap();
}

/// V1, V2 (1x MIN_SELF_DELEGATION), V3 (3x) Active; markets 1 and 2 listed.
fn oracle_db() -> (tempfile::TempDir, StateDb) {
    let (dir, db) = open_test_db();
    put_validator(&db, V1, MIN_SELF_DELEGATION, ValidatorStatus::Active);
    put_validator(&db, V2, MIN_SELF_DELEGATION, ValidatorStatus::Active);
    put_validator(&db, V3, MIN_SELF_DELEGATION * U256::from(3u8), ValidatorStatus::Active);
    list_market(&db, 1);
    list_market(&db, 2);
    (dir, db)
}

fn submit(sender: u8, prices: &[(MarketId, FixedPoint)]) -> (Address, NativeAction) {
    (
        addr(sender),
        NativeAction::SubmitOraclePrices(OracleSubmission { prices: prices.to_vec(), timestamp: 0 }),
    )
}

/// One action through the single-action path at block `height`.
fn exec(db: &StateDb, height: u64, (sender, action): (Address, NativeAction)) -> NativeActionResult {
    NativeExecutor::execute(&mut ctx_at(db.clone(), height), &sender, &action)
}

fn sub_rows<T: StateBackend>(s: &T) -> usize {
    s.iterate_cf(CF_NATIVE_ORACLE, Some(b"sub")).unwrap().len()
}

fn assert_rejected(r: &NativeActionResult, needle: &str) {
    assert!(!r.success, "expected a rejection containing {needle:?}");
    let e = r.error.as_deref().unwrap_or("");
    assert!(e.contains(needle), "error {e:?} lacks {needle:?}");
}

/// Every bad entry rejects the WHOLE action and nothing is written — not even
/// the valid entries before it (there is no per-action rollback, NE:3291).
#[test]
fn submission_hardening_rejects_the_whole_action_and_writes_nothing() {
    let (_d, db) = oracle_db();
    let over_max = FixedPoint::from_raw(MAX_ORACLE_PRICE_RAW + 1);
    let oversize: Vec<_> =
        (0..=MAX_ORACLE_PRICES_PER_SUBMISSION as u64).map(|m| (m, fp(1))).collect();
    let cases: Vec<(Vec<(MarketId, FixedPoint)>, &str)> = vec![
        (vec![(1, fp(100)), (9, fp(100))], "market 9 is not listed"),
        (vec![(1, fp(100)), (2, FixedPoint::ZERO)], "invalid oracle price"),
        (vec![(1, fp(-1))], "invalid oracle price"),
        (vec![(1, over_max)], "invalid oracle price"),
        (vec![(1, fp(100)), (1, fp(101))], "duplicate market 1"),
        (vec![], "1..=256 prices"),
        (oversize, "1..=256 prices"),
    ];
    for (prices, needle) in cases {
        assert_rejected(&exec(&db, 5, submit(V1, &prices)), needle);
        assert_eq!(sub_rows(&db), 0, "{needle}: no row written");
    }
}

#[test]
fn submission_at_the_cap_over_listed_markets_is_accepted() {
    let (_d, db) = oracle_db();
    for m in 3..=MAX_ORACLE_PRICES_PER_SUBMISSION as u64 {
        list_market(&db, m);
    }
    let prices: Vec<_> =
        (1..=MAX_ORACLE_PRICES_PER_SUBMISSION as u64).map(|m| (m, fp(m as i64))).collect();
    let r = exec(&db, 5, submit(V1, &prices));
    assert!(r.success, "{:?}", r.error);
    assert_eq!(sub_rows(&db), MAX_ORACLE_PRICES_PER_SUBMISSION);
}

/// Pin (GREEN today): only ACTIVE validators may submit.
#[test]
fn only_active_validators_may_submit() {
    let (_d, db) = oracle_db();
    put_validator(&db, 4, MIN_SELF_DELEGATION, ValidatorStatus::Candidate);
    put_validator(&db, 5, MIN_SELF_DELEGATION, ValidatorStatus::Jailed);
    for (who, needle) in [(4, "not active"), (5, "not active"), (6, "not a registered validator")] {
        assert_rejected(&exec(&db, 5, submit(who, &[(1, fp(100))])), needle);
    }
    assert_eq!(sub_rows(&db), 0);
}
```

**Implementation** (NE:6512-6549): keep the validator check; then, before the write loop:

```rust
        use torus_core::oracle::{valid_oracle_price, MAX_ORACLE_PRICES_PER_SUBMISSION as CAP};
        let err = |m: String| NativeActionResult::err("submit_oracle_prices", m);
        // Item 2: validate EVERY entry before writing any (no per-action rollback)
        // — the action is all-or-nothing.
        if prices.is_empty() || prices.len() > CAP {
            return err(format!("a submission carries 1..={CAP} prices, got {}", prices.len()));
        }
        let mut seen = BTreeSet::new();
        for &(market_id, price) in prices {
            if !seen.insert(market_id) {
                return err(format!("duplicate market {market_id} in submission"));
            }
            match ctx.governance.market_exists(market_id) {
                Ok(true) => {}
                Ok(false) => return err(format!("market {market_id} is not listed")),
                Err(e) => return err(e.to_string()),
            }
            if !valid_oracle_price(price) {
                return err(format!("invalid oracle price {price} for market {market_id}"));
            }
        }
        // … existing write loop unchanged (stores ctx.block_height, ctx.timestamp) …
```

(`BTreeSet` is imported at NE:9.)

**validate:** `cargo test -j6 -p torus-bridge --test oracle_block_tests submission && cargo test -j6 -p torus-bridge --test oracle_block_tests only_active` · depends_on: [2, 3]

**Correction s517:** the T4 test file's import list was trimmed to what T4's
tests use (dropped `NativeBalance`, `NativeStateOverlay`, `OrderType`,
`PlaceOrderParams`, `TimeInForce` — unused until T5+; re-add with those
tests). No existing test submitted prices for an unlisted market through the
executor: the consensus oracle fixture already lists market 1; the
`native_bridge_tests` / `chaos` submissions are classify/sort only or come
from a non-validator (already rejected).

### T5 — bridge: `begin_block_oracle` + `oracle_due` + `whole_token_power`

**Test first** — append to `oracle_block_tests.rs`:

```rust
/// The mark as AccountReader::mark sees it in `ctx` (+ the stamp block).
fn mark<T: StateBackend>(ctx: &NativeExecContext<T>, m: MarketId) -> Option<(FixedPoint, u64)> {
    let p = ctx.oracle.get_price(m, ctx.timestamp).ok()?;
    p.usable().map(|px| (px, p.block_number))
}

/// Block `height` on `db`: the oracle step, then `actions` (batch path).
fn run_block(
    db: &StateDb,
    height: u64,
    actions: &[(Address, NativeAction)],
) -> (Vec<NativeActionResult>, Vec<NativeActionResult>) {
    let mut ctx = ctx_at(db.clone(), height);
    let agg = NativeExecutor::begin_block_oracle(&mut ctx);
    let res = NativeExecutor::execute_batch(&mut ctx, actions).results;
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    (agg, res)
}

fn round(price_m1: i64) -> Vec<(Address, NativeAction)> {
    [V1, V2, V3].iter().map(|&v| submit(v, &[(1, fp(price_m1))])).collect()
}

fn fund(ctx: &NativeExecContext, who: Address, amount: i64) {
    ctx.positions
        .put_native_balance(&who, &NativeBalance { available: fp(amount), order_margin: FixedPoint::ZERO })
        .unwrap();
}

/// Stake weighting (V3 = 3x): 100 / 101 / 102 -> 102 (simple median: 101).
/// One result per LISTED market; an unlisted market's rows are never aggregated.
#[test]
fn block_start_aggregates_every_listed_market_from_earlier_blocks() {
    let (_d, db) = oracle_db();
    let (_, r) = run_block(&db, 5, &[
        submit(V1, &[(1, fp(100)), (2, fp(10))]),
        submit(V2, &[(1, fp(101)), (2, fp(10))]),
        submit(V3, &[(1, fp(102)), (2, fp(10))]),
    ]);
    assert!(r.iter().all(|x| x.success), "{r:?}");
    assert!(ctx_at(db.clone(), 5).oracle.get_price(1, 1_005).is_err(), "not aggregated in block 5");

    let mut ctx = ctx_at(db.clone(), 6);
    for v in [V1, V2, V3] {
        ctx.oracle.submit_price(&addr(v), 3, fp(7), 5, 1_005).unwrap(); // unlisted, planted
    }
    let agg = NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(agg.len(), 2);
    assert!(agg.iter().all(|x| x.success), "{agg:?}");
    assert_eq!(mark(&ctx, 1), Some((fp(102), 6)));
    assert_eq!(mark(&ctx, 2), Some((fp(10), 6)));
    assert_eq!(ctx.oracle.get_price(1, 1_006).unwrap().num_reporters, 3);
    assert!(ctx.oracle.get_price(3, 1_006).is_err(), "unlisted market 3 is never aggregated");
}

/// Timing: a submission of block 5 (ts 1005) counts through ts 1015 (age 10) and
/// is deleted at ts 1016; the aggregate (stamped 1015) is usable through ts 1075.
#[test]
fn a_submission_counts_for_ten_seconds_then_is_pruned() {
    let (_d, db) = oracle_db();
    run_block(&db, 5, &round(100));
    for h in 6..=15 {
        let (agg, _) = run_block(&db, h, &[]);
        assert!(agg[0].success, "block {h}: {:?}", agg[0].error);
        assert_eq!(mark(&ctx_at(db.clone(), h), 1), Some((fp(100), h)), "block {h}: fresh");
    }
    assert_eq!(sub_rows(&db), 3);
    run_block(&db, 16, &[]);
    assert_eq!(sub_rows(&db), 0, "age 11: deleted at block start");
    let p = ctx_at(db.clone(), 16).oracle.get_price(1, 1_016).unwrap();
    assert_eq!((p.block_number, p.timestamp), (15, 1_015), "not re-stamped without a quorum");
    assert_eq!(mark(&ctx_at(db.clone(), 75), 1), Some((fp(100), 15)), "age 60");
    assert_eq!(mark(&ctx_at(db.clone(), 76), 1), None, "age 61: stale");
}

/// Only the block-start step writes the aggregate: submissions EARLIER in the
/// same batch do not move the mark a placement reserves at.
#[test]
fn every_reader_in_a_block_sees_the_block_start_mark() {
    let (_d, db) = oracle_db();
    run_block(&db, 5, &round(100));
    let mut ctx = ctx_at(db.clone(), 6);
    NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(mark(&ctx, 1), Some((fp(100), 6)));
    let taker = addr(20);
    fund(&ctx, taker, 40);
    let market_buy = PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(200), // cap
        quantity: fp(10),
        order_type: OrderType::Market,
        time_in_force: TimeInForce::IOC,
        reduce_only: false,
        client_order_id: None,
    };
    let mut actions = round(300);
    actions.push((taker, NativeAction::PlaceOrder(market_buy)));
    let r = NativeExecutor::execute_batch(&mut ctx, &actions).results;
    assert!(r[..3].iter().all(|x| x.success), "{r:?}");
    // reserved at the block-start mark: 10 x 100 / 20 = 50 > 40 (at 300 it would be the cap: 100)
    assert_rejected(&r[3], "need 50.00000000");
    assert_eq!(mark(&ctx, 1), Some((fp(100), 6)), "same-block submissions never move the mark");
    let mut next = ctx_at(db.clone(), 7);
    NativeExecutor::begin_block_oracle(&mut next);
    assert_eq!(mark(&next, 1), Some((fp(300), 7)));
}

/// Market 2's stored aggregate is corrupt (its 2-reporter fallback read fails),
/// market 3 has no data: both are error RESULTS; market 1 aggregates and the
/// block goes on.
#[test]
fn aggregation_errors_are_per_market_and_never_abort_the_block() {
    let (_d, db) = oracle_db();
    list_market(&db, 3);
    run_block(&db, 5, &[
        submit(V1, &[(1, fp(100)), (2, fp(10))]),
        submit(V2, &[(1, fp(100)), (2, fp(10))]),
        submit(V3, &[(1, fp(100))]),
    ]);
    db.put_cf_raw(CF_NATIVE_ORACLE, &[b"agg".as_slice(), &2u64.to_be_bytes()].concat(), &[1, 2, 3])
        .unwrap();
    let mut ctx = ctx_at(db.clone(), 6);
    let agg = NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(agg.iter().map(|r| r.success).collect::<Vec<_>>(), vec![true, false, false]);
    assert!(ctx.fatal_error.is_none());
    assert_eq!(mark(&ctx, 1), Some((fp(100), 6)));
    let buyer = addr(21);
    fund(&ctx, buyer, 1_000);
    let bid = PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(90),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let r = NativeExecutor::execute_batch(&mut ctx, &[(buyer, NativeAction::PlaceOrder(bid))]).results;
    assert!(r[0].success, "{:?}", r[0].error);
}

/// Weights = whole-token stake of ACTIVE validators: a jailed validator's
/// (planted) row is ignored; a stake beyond u64::MAX tokens saturates.
#[test]
fn stakes_are_whole_token_power_of_active_validators() {
    let (_d, db) = oracle_db();
    put_validator(&db, 4, U256::MAX, ValidatorStatus::Active);
    put_validator(&db, 5, MIN_SELF_DELEGATION * U256::from(1_000u32), ValidatorStatus::Jailed);
    let seed = ctx_at(db.clone(), 5);
    for (v, p) in [(V1, 100), (V2, 101), (V3, 102), (4, 103), (5, 50_000)] {
        seed.oracle.submit_price(&addr(v), 1, fp(p), 5, 1_005).unwrap();
    }
    let mut ctx = ctx_at(db.clone(), 6);
    NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(mark(&ctx, 1), Some((fp(103), 6)), "saturated whale dominates; jailed row ignored");
    assert_eq!(ctx.oracle.get_price(1, 1_006).unwrap().num_reporters, 4);
}

/// Through the handler, 3 validators x 2 markets every block: one row per pair
/// (<= 6 rows ever), all gone 11 s after the last submission.
#[test]
fn rows_stay_one_per_validator_and_market_and_are_pruned() {
    let (_d, db) = oracle_db();
    for h in 1..=40u64 {
        let subs: Vec<_> =
            [V1, V2, V3].iter().map(|&v| submit(v, &[(1, fp(100)), (2, fp(10))])).collect();
        let (_, r) = run_block(&db, h, &subs);
        assert!(r.iter().all(|x| x.success));
        assert!(sub_rows(&db) <= 6, "block {h}");
    }
    for h in 41..=51 {
        run_block(&db, h, &[]);
    }
    assert_eq!(sub_rows(&db), 0);
}

/// Without submission rows the step writes NOTHING (existing roots unchanged).
#[test]
fn block_start_writes_nothing_without_submissions() {
    let (_d, db) = oracle_db();
    let overlay = NativeStateOverlay::new(db.clone());
    let mut ctx = ctx_at(overlay.clone(), 7);
    let agg = NativeExecutor::begin_block_oracle(&mut ctx);
    assert!(agg.iter().all(|r| !r.success));
    assert_eq!(overlay.pending_write_count(), 0);
}

/// The due-check must see a not-yet-durable parent layer (pipelined exec).
#[test]
fn oracle_due_reads_through_the_parent_layer() {
    let (_d, db) = oracle_db();
    assert!(!NativeExecutor::oracle_due(&db).unwrap());
    let o1 = NativeStateOverlay::new(db.clone());
    let (s, a) = submit(V1, &[(1, fp(100))]);
    assert!(NativeExecutor::execute(&mut ctx_at(o1.clone(), 1), &s, &a).success);
    let o2 = NativeStateOverlay::with_parent(db.clone(), Some(o1.freeze(1)));
    assert!(NativeExecutor::oracle_due(&o2).unwrap(), "parent-layer rows count");
    assert!(!NativeExecutor::oracle_due(&db).unwrap(), "the DB alone does not have them yet");
}
```

**Implementation** (`native_executor.rs`):

1. Extract the power conversion of `build_current_validator_set` (NE:7033-7042, second call site):

```rust
/// Whole-token voting / oracle power: floor(wei / 10^18), saturating at
/// u64::MAX (U256 wei would overflow FixedPoint).
fn whole_token_power(v: &ValidatorState) -> u64 {
    let wei = U256::from(10u64).pow(U256::from(18u64));
    (v.total_stake() / wei).try_into().unwrap_or(u64::MAX)
}
```

   `build_current_validator_set`: `power: whole_token_power(&v)` (import `ValidatorState` from
   `torus_economics` if needed, NE:28-30).
2. Next to `core_writer_due` (NE:6747):

```rust
    /// Item 2: whether this block must run the native phase for the oracle —
    /// any submission row in `state` (the block's overlay: DB + pipelined parent
    /// layer). Without rows the block-start step writes nothing, so "aggregate
    /// every block" == "run it whenever a row exists".
    pub fn oracle_due<T: StateBackend>(state: &T) -> Result<bool, CoreError> {
        OracleManager::new(state.clone(), OracleConfig::default()).has_submissions()
    }
```

3. Above `aggregate_oracle_prices` (doc of the latter → "called by `begin_block_oracle`"):

```rust
    /// Item 2 (option A) block-start step — runs FIRST in every executed native
    /// block, before any action: deletes submission rows older than the window
    /// (all markets), then aggregates every listed market from the rows of
    /// EARLIER blocks, weighted by the whole-token stake of Active validators,
    /// at the block timestamp. Nothing else writes the aggregate row, so the
    /// whole block reads one mark. Per-market errors are results; a storage
    /// error in the global reads (prune, market list, validator set) is a node
    /// fault → `fatal_error` (fail-stop, never a silently skipped aggregation).
    pub fn begin_block_oracle<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Vec<NativeActionResult> {
        match Self::oracle_inputs(ctx) {
            Ok((markets, stakes)) => Self::aggregate_oracle_prices(ctx, &markets, &stakes),
            Err(e) => {
                ctx.fatal_error = Some(format!("oracle block-start step: {e}"));
                Vec::new()
            }
        }
    }

    fn oracle_inputs<T: StateBackend>(
        ctx: &NativeExecContext<T>,
    ) -> Result<(Vec<MarketId>, Vec<(Address, FixedPoint)>), String> {
        ctx.oracle.prune_submissions(ctx.timestamp).map_err(|e| e.to_string())?;
        let markets = ctx.governance.listed_market_ids().map_err(|e| e.to_string())?;
        let stakes = ctx
            .staking
            .all_validators()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|v| v.status == ValidatorStatus::Active)
            .map(|v| {
                let power = i128::from(whole_token_power(&v));
                (v.address, FixedPoint::from_raw(power * FixedPoint::SCALE))
            })
            .collect();
        Ok((markets, stakes))
    }
```

**validate:** `cargo test -j6 -p torus-bridge --test oracle_block_tests && cargo test -j6 -p torus-bridge --test market_order_margin_tests && cargo test -j6 -p torus-bridge --test account_margin_tests` · depends_on: [2, 3, 4]

### T6 — consensus: wire the step (C1 added to T0's `run_native`)

**Test first** — `mod crash_recovery_tests` (helpers from T1), plus:

```rust
    /// The mark as AccountReader::mark sees it at block timestamp `now` (+ stamp block).
    fn mark_at(db: &StateDb, now: u64) -> Option<(FixedPoint, u64)> {
        let ctx = NativeExecContext::new(db.clone(), 0, now, 0, 1_000, 4, Address::ZERO, Address::ZERO, Address::ZERO);
        let p = ctx.oracle.get_price(ORACLE_MARKET, now).ok()?;
        p.usable().map(|m| (m, p.block_number))
    }

    /// Signed submissions through committed blocks (ts = 1000 + h). Block 2 has
    /// NO native action and still aggregates (rows exist -> the step is due).
    #[test]
    fn oracle_e2e_three_validators_set_the_mark_from_the_next_block() {
        let (config, db) = oracle_fixture_db();
        oracle_put_validator(&db, 65, 1, torus_economics::ValidatorStatus::Candidate);
        let ctx = make_exec_ctx(&config, &db);
        let blocks = oracle_blocks(&[
            &[(61, 100), (62, 101), (63, 102), (64, 999), (65, 999)], // 64 unregistered, 65 Candidate
            &[],
            &[(61, 200), (62, 200), (63, 200)],
            &[],
        ]);
        ctx.execute_committed_block(&blocks[0], vec![]);
        assert_eq!(oracle_sub_rows(&db).len(), 3, "only Active validators' rows are stored");
        assert_eq!(mark_at(&db, 1_001), None, "block 1's submissions are not aggregated in block 1");
        ctx.execute_committed_block(&blocks[1], vec![]);
        assert_eq!(mark_at(&db, 1_002), Some((px(102), 2)), "stake-weighted (simple median 101)");
        ctx.execute_committed_block(&blocks[2], vec![]);
        assert_eq!(mark_at(&db, 1_003), Some((px(102), 3)), "block 3's prices count from block 4");
        ctx.execute_committed_block(&blocks[3], vec![]);
        assert_eq!(mark_at(&db, 1_004), Some((px(200), 4)));
        assert!(!ctx.exec_failed.load(Ordering::SeqCst));
        assert_eq!(read_native_applied_height(&db), Some(4));
    }

    /// 1: V1..V3 @100; 2..=11 empty (fresh through ts 1011, age 10); 12: V1, V2
    /// @150 -> 2 reporters: last price kept, stamp stays (11, 1011); rows gone at
    /// 23 (block 12's rows, ts 1012, age 11); usable through ts 1071, stale at 1072.
    #[test]
    fn oracle_e2e_two_reporters_keep_the_last_price_until_stale() {
        let (config, db) = oracle_fixture_db();
        let ctx = make_exec_ctx(&config, &db);
        let mut rounds: Vec<&[(u8, i64)]> = vec![&[(61, 100), (62, 100), (63, 100)]];
        rounds.extend(std::iter::repeat(&[][..]).take(10)); // 2..=11
        rounds.push(&[(61, 150), (62, 150)]); // 12
        rounds.extend(std::iter::repeat(&[][..]).take(12)); // 13..=24
        let blocks = oracle_blocks(&rounds);
        for b in &blocks[..11] {
            ctx.execute_committed_block(b, vec![]);
        }
        assert_eq!(mark_at(&db, 1_011), Some((px(100), 11)));
        for b in &blocks[11..13] {
            ctx.execute_committed_block(b, vec![]);
        }
        assert_eq!(mark_at(&db, 1_013), Some((px(100), 11)), "2 reporters: last price, stamp kept");
        for b in &blocks[13..] {
            ctx.execute_committed_block(b, vec![]);
        }
        assert!(oracle_sub_rows(&db).is_empty());
        assert_eq!(read_native_applied_height(&db), Some(24));
        assert_eq!(mark_at(&db, 1_071), Some((px(100), 11)), "age 60: usable");
        assert_eq!(mark_at(&db, 1_072), None, "age 61: stale -> no mark");
    }

    /// A block that runs the native phase ONLY for the oracle keeps the resident
    /// books (T0's single flag: no second, untouched-block advance).
    #[test]
    fn oracle_e2e_oracle_only_block_keeps_the_resident_books() {
        let (config, db) = oracle_fixture_db();
        let mut ctx = make_exec_ctx(&config, &db);
        ctx.test_book_mode = Some(torus_bridge::native_executor::BookMode::Classic);
        let blocks = oracle_blocks(&[&[(61, 100), (62, 100), (63, 100)], &[]]);
        ctx.execute_committed_block(&blocks[0], vec![]);
        assert_eq!(ctx.resident_books.lock().unwrap().height(), Some(1));
        ctx.execute_committed_block(&blocks[1], vec![]);
        assert_eq!(mark_at(&db, 1_002), Some((px(100), 2)));
        assert_eq!(ctx.resident_books.lock().unwrap().height(), Some(2), "holder not drained");
    }
```

RED: block 2 has no native action → skipped → no aggregate.

**Implementation** (app.rs, on top of T0):

```rust
        let core_writer_due = NativeExecutor::core_writer_due(&self.state_db, height);
        let epoch_boundary = EpochManager::is_epoch_boundary(height, self.epoch_length);
        // Item 2 (C1): the overlay is built before the gate so the oracle due-check
        // reads DB + the pipelined parent layer (block h−1's rows may not be durable
        // yet) — never `self.state_db` alone, which would diverge.
        let parent = if pipelined { self.last_job.lock().unwrap().clone() } else { None };
        debug_assert!(/* unchanged, moved from :1972-1976 */);
        let overlay = NativeStateOverlay::with_parent(self.state_db.clone(), parent);
        let oracle_due = match NativeExecutor::oracle_due(&overlay) {
            Ok(due) => due,
            Err(e) => {
                tracing::error!(%e, height, "FATAL: oracle due-check read failed — fail-stop");
                self.exec_failed.store(true, Ordering::SeqCst);
                if fold_header {
                    persist_block_header(&self.state_db, torus_block);
                }
                return;
            }
        };
        let run_native = has_native || computed_fee_revenue > 0 || core_writer_due
            || epoch_boundary || oracle_due;
        if run_native {
            // (the old `let parent` / `debug_assert!` / `let overlay` lines inside are removed)
```

and between `engine_timer` and `execute_batch(pre_evm)` (:2197-2198):

```rust
            // Item 2: aggregate every listed market BEFORE any action, at the block
            // timestamp — the whole block (placements, modify, withdrawals,
            // CoreWriter) reads one mark. A storage fault sets fatal_error
            // (the fail-stop check after the batches catches it).
            let _ = NativeExecutor::begin_block_oracle(&mut ctx);
```

`fold_header` (:1941) and the header branch (:1920) keep using `has_native`.

**validate:** `cargo test -j6 -p torus-consensus --lib oracle_e2e && cargo test -j6 -p torus-consensus --lib exec_pipeline && cargo test -j6 -p torus-consensus --lib crash` · depends_on: [5]

### T7 — determinism gates (serial = pipelined parked = crash replay; incremental = full root)

**Test first** — (a) `mod crash_recovery_tests`:

```rust
    /// 1: V1..V3 @100; 2 empty; 3: V1..V3 @200; 4..=6 empty; 7: V1 @300, V2 @310;
    /// 8..=14 empty (ts = 1000 + h).
    fn oracle_determinism_blocks() -> Vec<TorusBlock> {
        let full = |p: i64| vec![(61u8, p), (62, p), (63, p)];
        let (r1, r3) = (full(100), full(200));
        // 300 / 310 keeps the fixture away from the 3 x MAD boundary.
        let r7: Vec<(u8, i64)> = vec![(61, 300), (62, 310)];
        let e: &[(u8, i64)] = &[];
        oracle_blocks(&[&r1[..], e, &r3[..], e, e, e, &r7[..], e, e, e, e, e, e, e])
    }

    enum OracleRun {
        Serial,
        /// Flush worker ON and PARKED inside job 1: block 2's due-check and
        /// aggregation see block 1's rows only through the parent layer.
        PipelinedParked,
        /// Blocks 1..=4 executed; 5..=14 committed durably, then boot replay.
        Replay,
    }

    fn run_oracle_fixture(mode: OracleRun) -> (Vec<CfDump>, torus_types::B256, StateDb) {
        let (config, db) = oracle_fixture_db();
        let blocks = oracle_determinism_blocks();
        match mode {
            OracleRun::Serial => {
                let ctx = make_exec_ctx(&config, &db);
                for b in &blocks {
                    dispatch_and_execute(&ctx, &db, b);
                }
                assert!(!ctx.exec_failed.load(Ordering::SeqCst));
            }
            OracleRun::PipelinedParked => {
                let gate = crate::exec_pipeline::WorkerGate::new();
                let mut ctx = make_exec_ctx(&config, &db);
                ctx.attach_flush_worker(Some(gate.clone()));
                gate.hold();
                dispatch_and_execute(&ctx, &db, &blocks[0]);
                assert!(gate.wait_received(1));
                let (db2, rest) = (db.clone(), blocks[1..].to_vec());
                let t = std::thread::spawn(move || {
                    for b in &rest {
                        dispatch_and_execute(&ctx, &db2, b);
                    }
                    ctx
                });
                std::thread::sleep(std::time::Duration::from_millis(200));
                gate.release();
                let ctx = t.join().unwrap();
                assert!(!ctx.exec_failed.load(Ordering::SeqCst));
                drop(ctx); // drains + joins W
            }
            OracleRun::Replay => {
                let ctx = make_exec_ctx(&config, &db);
                for b in &blocks[..4] {
                    dispatch_and_execute(&ctx, &db, b);
                }
                for b in &blocks[4..] {
                    persist_committed_block_durably(&db, b);
                }
                let (last, parked) = TorusApp::replay_committed(&db, &ctx);
                assert_eq!(parked, None);
                assert_eq!(last.height, 14);
            }
        }
        assert_eq!(read_native_applied_height(&db), Some(14));
        let root = torus_state::native_trie::persisted_native_root(&db).unwrap();
        (dump_all_cfs(&db), root, db)
    }

    #[test]
    fn oracle_determinism_serial_pipelined_and_replay_are_identical() {
        let (serial, root_s, db_s) = run_oracle_fixture(OracleRun::Serial);
        let (piped, root_p, _) = run_oracle_fixture(OracleRun::PipelinedParked);
        let (replay, root_r, _) = run_oracle_fixture(OracleRun::Replay);
        // Non-vacuous: 8..=13: V1 300, V2 310 (ts 1007), V3 200 (ts 1003, 3x) in the
        // window through ts 1013 -> stake-weighted 200, re-stamped; 14: V3's row
        // (age 11) pruned -> 2 reporters, stamp stays 13; block 7's 2 rows remain.
        assert_eq!(mark_at(&db_s, 1_014), Some((px(200), 13)));
        assert_eq!(oracle_sub_rows(&db_s).len(), 2);
        assert_dumps_equal(&serial, &piped, "oracle: serial vs pipelined (parked)");
        assert_dumps_equal(&serial, &replay, "oracle: serial vs crash replay");
        assert_eq!(root_s, root_p);
        assert_eq!(root_s, root_r);
    }
```

(b) `crates/torus-integration-tests/tests/chaos.rs` (mirrors the consensus native block like
`native_incremental_root_matches_full_scan_under_real_execution`):

```rust
/// Item 2: the block-start oracle step (agg rows rewritten, sub rows deleted)
/// keeps the incremental native root equal to the full scan.
#[test]
fn oracle_block_start_step_keeps_incremental_root_equal_to_full_scan() {
    use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
    use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_ORACLE};
    use torus_state::native_trie::{build_native_trie_to_cf, native_root_full, persisted_native_root};
    use torus_state::{NativeStateOverlay, StateBackend};

    let tmp = tempfile::TempDir::new().unwrap();
    let state_db = StateDb::open(tmp.path()).unwrap();
    let staking = StakingManager::new(state_db.clone());
    for (v, mult) in [(21u8, 1u64), (22, 1), (23, 3)] {
        staking
            .put_validator(&addr(v), &ValidatorState {
                address: addr(v),
                pubkey: [v; 32],
                commission_bps: 0,
                self_stake: MIN_SELF_DELEGATION * U256::from(mult),
                total_delegated: U256::ZERO,
                status: ValidatorStatus::Active,
                jailed_until: None,
                last_commission_change_block: None,
            })
            .unwrap();
    }
    for m in [1u64, 2] {
        state_db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    build_native_trie_to_cf(&state_db).unwrap();

    for block in 1..=30u64 {
        let overlay = NativeStateOverlay::new(state_db.clone());
        let mut ctx = NativeExecContext::new(
            overlay.clone(), block, 1_700_000_000 + block, 0, 1_000, 100,
            Address::ZERO, Address::ZERO, Address::ZERO,
        );
        NativeExecutor::begin_block_oracle(&mut ctx);
        let actions: Vec<_> = if block <= 12 {
            [21u8, 22, 23]
                .iter()
                .map(|&v| (addr(v), NativeAction::SubmitOraclePrices(OracleSubmission {
                    prices: vec![(1, fp(50_000 + block as i64 + v as i64)), (2, fp(10))],
                    timestamp: 0,
                })))
                .collect()
        } else {
            Vec::new()
        };
        let r = NativeExecutor::execute_batch(&mut ctx, &actions);
        assert!(r.results.iter().all(|x| x.success), "block {block}");
        assert!(ctx.fatal_error.is_none());
        overlay.flush_with_native_trie(&state_db).unwrap();
        assert_eq!(
            persisted_native_root(&state_db).unwrap(),
            native_root_full(&state_db).unwrap(),
            "block {block}: incremental native root != full scan (oracle step)"
        );
    }
    let agg1 = [b"agg".as_slice(), &1u64.to_be_bytes()].concat();
    assert!(state_db.get_cf_raw(CF_NATIVE_ORACLE, &agg1).unwrap().is_some());
    assert!(StateBackend::iterate_cf(&state_db, CF_NATIVE_ORACLE, Some(b"sub")).unwrap().is_empty());
}
```

No implementation expected (gates); a failure is a bug in T0-T6.

**validate:** `cargo test -j6 -p torus-consensus --lib oracle_determinism && cargo test -j6 -p torus-integration-tests --test chaos oracle` · depends_on: [6]

### T8 — precompiles 0x0802 / 0x0800 on the time-based rule (EVM timestamp threaded)

**Part of `══ COMMIT 3 (oracle core) ══`** — implemented together with T2, committed once.

**Test first** — `crates/torus-core/tests/precompile_tests.rs`:
- helper `write_oracle_price(db, market, price, block, ts)` writes the 36-byte row
  (price ‖ block ‖ `3u32` ‖ ts).
- every `execute_precompile*(…, block)` call gets the block timestamp as a new last argument
  (27 calls; non-oracle tests pass `0`).
- `oracle_reader_get_price`: row (block 95, ts 1_000), call at (block 100, ts 1_005) → price,
  `block_number` 95, stale false. `oracle_reader_stale_price`: row ts 1_000, call ts 1_061 →
  stale true (was blocks 10 vs 200).
- new:

```rust
/// 0x0802 getAllPrices: stale flags from timestamps (60 s).
#[test]
fn oracle_reader_get_all_prices_uses_timestamps() {
    let (_dir, db) = setup();
    write_oracle_price(&db, 1, fp(50_000), 5, 1_000);
    write_oracle_price(&db, 2, fp(3_000), 9, 1_050);
    let address = precompile_address(ADDR_ORACLE_READER);
    let input = build_input("getAllPrices()", &[]);
    let out = execute_precompile(&address, &input, &addr(0), &db, 10, 1_061).unwrap();
    // three dynamic arrays; the stale-flag array is the third — decode its 2 elements
    let off = u64::from_be_bytes(out[88..96].try_into().unwrap()) as usize; // 3rd head word
    assert_eq!(u64::from_be_bytes(out[off + 24..off + 32].try_into().unwrap()), 2, "len");
    assert_eq!(out[off + 63], 1, "market 1: age 61 -> stale");
    assert_eq!(out[off + 95], 0, "market 2: age 11 -> fresh");
}

/// 0x0800 getPosition UPnL uses the price only while usable (ABI unchanged).
#[test]
fn order_book_reader_get_position_ignores_a_stale_oracle_price() {
    let (_dir, db) = setup();
    let trader = addr(1);
    PositionManager::new(db.clone())
        .put_position(&Position {
            trader,
            market_id: 1,
            is_long: true,
            size: fp(5),
            entry_price: fp(50_000),
            realized_pnl: fp(100),
            isolated_margin: fp(2_500),
            margin_type: torus_core::position::MarginType::Isolated,
        })
        .unwrap();
    write_oracle_price(&db, 1, fp(51_000), 100, 1_000);
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = build_input("getPosition(address,bytes32)", &[encode_addr(&trader), encode_market_id(1)]);
    let upnl = |ts: u64| {
        let out = execute_precompile(&address, &input, &addr(0), &db, 200, ts).unwrap();
        i128::from_be_bytes(out[80..96].try_into().unwrap())
    };
    assert_eq!(upnl(1_060), fp(5_000).raw(), "age 60: 5 x (51,000 - 50,000)");
    assert_eq!(upnl(1_061), 0, "age 61: stale -> 0");
    assert_eq!(upnl(900), fp(5_000).raw(), "clock behind the row: age clamps at 0 (usable)");
}
```

  (Check the array offset decoding against `abi::encode_arrays_response` (`precompiles.rs:218`)
  and `abi_arrays_response_encoding` when writing.)
- other crates' call sites (`cross_vm_read.rs` ×7, `cross_vm_write.rs`,
  `book_read_modes_tests.rs`, `claim_unbonded_tests.rs`): add the argument (their block's
  timestamp, else `0`).

**Implementation:**
- `precompiles.rs`: `execute_precompile`, `execute_precompile_with_value`,
  `execute_precompile_read_only`, `execute_precompile_inner` gain `current_timestamp: u64`
  (last). Dispatch: `order_book_reader(input, state_db, current_timestamp)` →
  `read_position(…, current_timestamp)`; `oracle_reader(input, state_db, current_timestamp)`
  (the block number is no longer used by these readers). One decode helper:

```rust
/// Aggregate row: price(16) ‖ block(8) ‖ reporters(4) ‖ ts(8). Stale iff
/// now − ts > DEFAULT_MAX_ORACLE_AGE_SECS — the rule of `OraclePrice::usable`.
fn decode_agg(data: &[u8], now: u64) -> Option<(FixedPoint, u64, bool)> {
    if data.len() < 36 {
        return None;
    }
    let price = FixedPoint::from_raw(i128::from_be_bytes(data[..16].try_into().unwrap()));
    let block = u64::from_be_bytes(data[16..24].try_into().unwrap());
    let ts = u64::from_be_bytes(data[28..36].try_into().unwrap());
    Some((price, block, now.saturating_sub(ts) > DEFAULT_MAX_ORACLE_AGE_SECS))
}
```

  used by `read_oracle_price` / `read_all_oracle_prices` (outputs unchanged: price,
  block_number, stale) and `get_oracle_price_fp(state_db, market, now)` → `Ok(price)` only if
  `!stale && price > 0`, else `Err(StaleOraclePrice / NoOraclePrice)`.
- `torus-evm/src/precompile_provider.rs`: `TorusPrecompiles` gains `current_timestamp: u64`;
  `new` / `with_mode` take it; `run` passes it. `executor.rs:215, 284, 427`: pass
  `block_cfg.timestamp` (header timestamp on proposer/validator paths; `block_env_from_header`
  on RPC `eth_call`).

**validate:** `cargo test -j6 -p torus-core --test precompile_tests && cargo test -j6 -p torus-evm && cargo test -j6 -p torus-integration-tests --test cross_vm_read --test cross_vm_write && cargo check --workspace --all-targets` · depends_on: [2] (same commit as T2)

**Correction s517 (as implemented):**
* Extra test `torus-evm/tests/evm_tests.rs::precompile_oracle_reader_uses_the_block_timestamp`
  (0x0802 via `execute_tx`: age 60 fresh, 61 stale) — pins that `block_cfg.timestamp` reaches
  the precompiles; RED with the provider passing a placeholder `0`.
* `get_oracle_price_fp` returns `StaleOraclePrice` for a stale row, `NoOraclePrice` for
  absent / short / non-positive.
* `execute_precompile_inner` has 8 params: `#[allow(clippy::too_many_arguments)]`.
* Integration harness `set_oracle_price` (`torus-integration-tests/tests/common/mod.rs`) wrote
  price‖block‖**ts‖count** (36 B, fields swapped — undecodable by `OracleManager`); now the
  canonical price‖block‖count‖ts. `cross_vm_read` oracle tests pass the harness timestamps
  (`1_700_000_000 + h`); the stale test checks age 60 / 61. `lockbox_e2e` now sees a usable
  mark (still green). Other call sites pass `0`.

### T9 — RPC `getMarkPrice` / `getPosition` staleness tests (implementation landed in T2)

**Test first** — `crates/torus-rpc/src/lib.rs` tests, next to
`torus_get_mark_price_from_order_book` (:3040); set the latest block and its timestamp with the
existing `store_header(&state, &TorusBlockHeader { timestamp: X, ..test_header(h, 0, 0) })`
BEFORE starting the server (`find_latest_height` reads it at construction):

```rust
    /// Item 2: a stale aggregate reads as absent — markPrice = indexPrice = 0,
    /// timestamp 0 (ABI unchanged). Usable at age 60, stale at 61 (header time).
    #[tokio::test]
    async fn torus_get_mark_price_hides_a_stale_oracle_price() {
        use torus_core::oracle::{OracleConfig, OracleManager};
        for (latest_ts, want) in [(5_060u64, Some(fp(50_000))), (5_061, None)] {
            let (_dir, state, mempool, executor) = setup();
            let oracle = OracleManager::new(state.clone(), OracleConfig::default());
            let reps = [Address::from([0xA1; 20]), Address::from([0xA2; 20]), Address::from([0xA3; 20])];
            for v in &reps {
                oracle.submit_price(v, 1, fp(50_000), 10, 5_000).unwrap();
            }
            let stakes: Vec<_> = reps.iter().map(|v| (*v, fp(1))).collect();
            oracle.aggregate_price(1, 10, 5_000, &stakes).unwrap();
            store_header(&state, &TorusBlockHeader { timestamp: latest_ts, ..test_header(20, 0, 0) });
            let (handle, addr) = start_server(state, mempool, executor).await;
            use jsonrpsee::core::client::ClientT;
            let client = jsonrpsee::http_client::HttpClientBuilder::default()
                .build(format!("http://{addr}"))
                .unwrap();
            let mp: RpcMarkPrice =
                client.request("torus_getMarkPrice", jsonrpsee::rpc_params!["0x1"]).await.unwrap();
            let (px, ts) = match want { Some(p) => (p, 10), None => (FixedPoint::ZERO, 0) };
            assert_eq!((mp.mark_price, mp.index_price, mp.timestamp), (hex_fp(px), hex_fp(px), ts));
            handle.stop().unwrap();
        }
    }
```

- `torus_get_position_ignores_a_stale_oracle_price`: same loop; position long 5 @ 50,000 (the
  fixture of `torus_get_position_open`, :2581); aggregate 51,000 at ts 5,000; latest ts 5,060 ⇒
  `unrealized_pnl == hex_fp(fp(5_000))`; 5,061 ⇒ `hex_fp(FixedPoint::ZERO)`. Write it in full
  like the first.

(`store_header` / `test_header` live in the outer tests module (:641, :621); from the nested
module at :2298 reach them with `super::`.)

**Implementation:** none expected (landed in T2); fix only what the tests expose.

**validate:** `cargo test -j6 -p torus-rpc --lib mark_price && cargo test -j6 -p torus-rpc --lib get_position` · depends_on: [2]

### T10 — docs

- `docs/parity-audit-fixes-s515.md` → *Known deferred items*: the mark is now produced
  (time-based, one per block; staleness in every consumer); remaining: no feeder in production
  (B), header timestamp unvalidated (S1), 3-validator net needs all three submitting. Behaviour
  table rows: "Oracle submissions" (T4 rules), "Oracle aggregation" (*Semantics* above), "Epoch
  processing" (T0: every boundary block, if T0 landed). *Deployment requirements*: lockstep.
- `docs/plans/oracle-aggregation.md` → *Decisions*: rev. 2 (time-based, R9, C1/C2, T0).

**validate:** `true` · depends_on: [6, 8, 9]

### T11 — end-to-end verification (lead) · depends_on: [7, 8, 9, 10]

See below.

## Verification (end-to-end)

1. `cargo test -j6 -p torus-core --test oracle_tests && cargo test -j6 -p torus-core --test precompile_tests`
2. `cargo test -j6 -p torus-economics --test governance_tests`
3. `cargo test -j6 -p torus-bridge` (all files incl. `oracle_block_tests`, margin / modify /
   engine / native_bridge tests)
4. `cargo test -j6 -p torus-consensus --lib`
5. `cargo test -j6 -p torus-evm` and `cargo test -j6 -p torus-integration-tests --test chaos --test cross_vm_read --test cross_vm_write`
6. 11-crate run: `cargo test -j6 -p torus-types -p torus-state -p torus-evm -p torus-core -p torus-consensus -p torus-bridge -p torus-rpc -p torus-mempool -p torus-economics -p torus-genesis -p torus-telemetry`
   Baseline 1528 pass / 2 known torus-core fails. Expected **1528 + 43 = 1571 pass, same 2
   fails**: T0 2, T0b 7, T1 1, oracle_tests 10, governance 1, oracle_block_tests 11, app.rs
   oracle 4, precompile 2, rpc 2 (recount at T11; −2 if T0 is dropped without keeping its pin).
7. `cargo check --workspace --all-targets`.
8. Perf sanity: `cargo test -j6 -p torus-bridge --release --test engine_parallel_bench -- --ignored --nocapture`
   before/after (the step: 2 `"sub"` scans + 1 per listed market + 1 validator scan per native
   block over ≤ V × M rows).

## Rollback

Commits per *Commit boundaries*; commits 1 (epoch) and 2 (timestamp) are independent of the oracle and can stay if it is reverted.
Revert in reverse order (T10 → T1). Row layouts change (submission key 31 B, aggregate 36 B);
fresh genesis anyway. A rollback is again a lockstep upgrade.

## Deployment

Lockstep upgrade of every validator: epoch processing also on empty boundary blocks (T0); the proposal timestamp rule and the proposer's `max(now, parent)` (T0b — a mixed fleet disagrees on body validity; validators need NTP-synced clocks: skew > 5 s makes a node reject honest proposals); the
native phase runs while submission rows exist (C1); submission / aggregate row layouts change;
old nodes accept submissions this build rejects; 0x0800 / 0x0802 outputs become time-based
(EVM-visible).

## Risks / design corrections (flagged, not silently changed)

* **S1 — header timestamp** — validated from T0b on (parent ≤ ts ≤ local + 5 s) on the
  proposal/body path only. **Header-first voting (T1.3) means the check runs AFTER this
  replica's vote**, so a byzantine leader can still get an out-of-range timestamp certified;
  honest replicas then reject its body (liveness, as for any app-invalid body). True vote-side
  enforcement needs a hotstuff change (timestamp in `ProposalHeader` + a pre-vote app hook) or
  disabling vote-before-DA — **decision needed**. Oracle ages clamp at 0 regardless.
* **S3 — clock skew** — a validator whose clock lags > 5 s rejects honest proposals' bodies;
  a proposer whose clock runs > 5 s ahead gets its bodies rejected. NTP required (ops note).
* **S4 — proposer parent fallback** — `produce_block` falls back to `last_header` when the parent
  datum cannot be decoded; `max(now, fallback.ts)` could then be below the real parent (liveness
  only; the validators reject it).
* **S2 — 1 s granularity**: ≤ 10 / ≤ 60 are whole seconds of header time (≈ up to 10.99 s real).
* **Row layout** — one submission row per (market, validator) (key without the block):
  "latest" = last written, row count and prune work bounded by V × M without a cap. Chosen as
  the minimal approach; `block_number` stays in the value (informational).
* **C1** — with a feeder, every block runs the native phase (rows live ≥ 10 s ≫ block time).
* **C2** — single `run_native` flag (lands in T0; also fixes the CoreWriter-only double marker /
  resident-books drain).
* **T0 side effect** — empty boundary blocks now run rewards / inflation / native-side
  rotation as non-empty ones always did; the consensus-side rotation
  (`epoch_validator_set_updates`) is unchanged. If the RED test shows no skip, T0 is dropped
  (recorded).
* **R9 — exact 3×MAD** (kept in A).
* **C4 — storage faults fail-stop** (due-check, prune, market list, validator set); per-market
  aggregation errors are results.
* **RPC `timestamp`** of `getMarkPrice` stays the aggregate's block number (meaning unchanged);
  switching it to the aggregate's timestamp would be more HL-like — decide separately.
* **R2** — min 3 reporters on 3 validators: one silent validator ⇒ no fresh aggregate; mark
  unusable 60 s after the last full round.
* **R3 — cost**: per native block ~2 + M prefix scans over ≤ V × M rows + a validator scan.
* **R5 — ingress** does not apply the new submission rules (exec rejects deterministically).
* **R6** — `CF_NATIVE_MARKETS` is off-root but drives consensus behaviour (as governance does).
