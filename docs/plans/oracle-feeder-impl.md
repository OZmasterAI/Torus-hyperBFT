# Implementation Plan: Validator price feeder (item 2, option B) — rev. 2

Design: `docs/plans/oracle-feeder.md` (rev. 2). Tasks: `PRPs/oracle-feeder.tasks.json`.
Branch `feat/oracle-feeder` @ `d4fe69c` (stacked on `feat/oracle-aggregation`).
Worktree `/home/crab/projects/Torus-hyperBFT-wt-feeder`. Line numbers verified at `d4fe69c`.

Binding decisions (user, s517, rev. 2):

* **Signer.** Each validator record gets an HL-style hot **oracle signer address**.
  * A new action, signed with the validator's EVM key, sets or rotates it.
  * The signer may only submit `SubmitOraclePrices` for that validator. It has no expiry.
  * This is a consensus change (lockstep, fresh genesis) and lands as its own commit.
  * Session keys and session renewal are dropped.
* **Mempool order.** Cancels, then oracle, then the rest.
  * Priority applies only to Active validators and their signers.
  * At most 4 pending per validator.
* **Feeder rules.**
  * USDT/USDC count as USD. Kraken USDT/USD conversion is optional.
  * A market needs >= 3 sources AND >= 50 % of its configured weight.
  * MATIC uses the POL tickers.
  * Cadence is 3 s.
* **Networks.** Devnet is unchanged.

**TDD.** Every task writes the failing test first, then the implementation, then runs
`validate`. A missing API counts as RED by compile error.

**Cargo.** Every cargo command uses the serial wrapper with the worktree's own target dir:

```
cd /home/crab/projects/Torus-hyperBFT-wt-feeder && CARGO_TARGET_DIR=/home/crab/projects/Torus-hyperBFT-wt-feeder/target /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh <args>
```

`CARGO⟨args⟩` below abbreviates exactly that line. The tasks JSON spells it out in full.

## Verified anchors

| What | Where |
|------|-------|
| `NativeAction` enum (append-only), canonical bytes (tags ≤ 26) | `torus-types/src/lib.rs:530-635, 681-903` |
| `SessionScope::Full` exclusion list | lib.rs:225-236 |
| EIP-712 dispatch (`ClaimUnbonded` arm :242), helper pattern (`hash_claim_unbonded` :438), `requires_eip712` :757-769 | `torus-types/src/eip712.rs` |
| `ValidatorState` + custom borsh | `torus-economics/src/types.rs:77-134` |
| `ValidatorState { … }` literals (10 in 6 files): types.rs, staking.rs, torus-genesis/src/lib.rs, torus-consensus/src/app.rs, torus-bridge/tests/oracle_block_tests.rs, torus-integration-tests/tests/chaos.rs | grep |
| `StakingManager::get_validator` / `put_validator` / `all_validators` (decodes every value) | `staking.rs:784, 797, 913-923` |
| CF names + `native_nonce_key` pattern | `torus-state/src/cf.rs:61-110` |
| Native-root CF buckets (`CF_NATIVE_ORACLE` 3, `CF_STAKING_VALIDATORS` 5) | `torus-state/src/native_trie.rs:48-53` |
| Oracle prefixes `"sub"` / `"agg"` | `torus-core/src/oracle.rs:181-200` |
| Exec dispatch (oracle arm :3383), `exec_submit_oracle_prices` :6512-6560, `classify_action` :7161-7190, no per-action rollback :3291 | `torus-bridge/src/native_executor.rs` |
| Oracle exec test helpers (`oracle_db`, `put_validator`, `list_market`, `exec`, `sub_rows`, `assert_rejected`, `ctx_at`, `run_block`) | `torus-bridge/tests/oracle_block_tests.rs:17-175` |
| Whole-block oracle helpers (`oracle_blocks`, `oracle_key`, `make_block`, `link_blocks`); `dump_all_cfs` / `assert_dumps_equal` | `torus-consensus/src/app.rs:14380-14420, 12481-12492` |
| Pool `SortKey`, entry, insert/evict, cancels-only walk, `is_cancel`; tests `make_action` / `sig()`; `assert_index_consistent` (cfg(test)) :505; `remove_committed` :481 | `torus-mempool/src/native_pool.rs:13-35, 38-58, 174-232, 398-440, 540-570` |
| `native_admission_backlogged` :233; `add_native_action_presigned` :457; `admit_gossip` :579-608; `submit_native_action[_inner]` :643-680; cancels wrapper :1047-1068; tests `setup()` :1265 | `torus-mempool/src/lib.rs` |
| RPC: `validate_known_markets` :257-281, batch screen :435-480, single screen :1121-1131, `get_validators` :1040-1075 | `torus-rpc/src/torus.rs` |
| `RpcValidatorInfo` :414-422; RPC tests: backlog :1418, pool-full :1503, `store_market` :2307 | `torus-rpc/src/types.rs`; `torus-rpc/src/lib.rs` |
| Pacing tiers + tests | `torus-consensus/src/app.rs:288-333, 5690-5732, 6440-6535` |
| Wallet `Cli`/`Command`, `submit_native_action`, `parse_address`, validator cmds, keystore | `tools/wallet/src/main.rs:22-60, 284, 487`; `sign.rs:132-162`; `parse.rs:43`; `commands/validator.rs`; `keystore.rs:32-130` |
| Faucet TCP HTTP server | `tools/faucet/src/main.rs:289-330, 533-580` |

## Success criteria (each one is a named test)

1. **Signer authorization (S1, S2).**
   * Only a validator that is not Tombstoned can set a signer, and only with its EVM key (a session key is rejected).
   * Rotation replaces the signer; clear removes it.
   * One validator per signer. The signer cannot be a validator or the sender itself.
2. **Signer reporting (S3, S4).**
   * A signer's submission is written under the validator's `(market, validator)` row and weighted by the validator's stake.
   * The old signer is rejected from the block after rotation. Same-block rotation is pinned.
   * A jailed validator's signer is rejected.
3. **No other authority (S3).** Every non-oracle action from the signer leaves the validator's native state byte-identical.
4. **Mempool (M1-M4).**
   * Order is cancels, then oracle, then the rest.
   * Priority and admission apply only to Active validators and their signers.
   * At most 4 pending per validator (its own address plus its signer).
   * The backlog / pool-full bypass and the priority-only pacing tier include oracle submissions.
5. **RPC (R1, R2).** `getValidators` reports `oracleSigner`. Ingress applies the exec oracle rules.
6. **Wallet (W1).** The wallet becomes lib + bin. `validator set-oracle-signer` builds the right action.
7. **Feeder (F1-F9).**
   * Parsing, median, freshness, min sources and weight, quote modes, the 7 parsers, backoff.
   * Submission validity (property test), nonce, cycle failure modes, health.
8. **End to end (F10).**
   * Real `RpcServer` + `Mempool`, an Active validator and its signer: a feeder cycle is admitted with the signer as sender, including under backlog.
   * A wrong signer fails the startup check.
9. **Determinism (S4).** Serial execution equals crash replay for a block carrying `SetOracleSigner` and signer submissions. Existing suites stay green.

## Determinism / consensus notes

* **Commit 1 is consensus-visible and lockstep (fresh genesis).** It adds an action variant, a borsh field, and root writes in `CF_STAKING_VALIDATORS` and `CF_NATIVE_ORACLE`.
  * Writes go through the exec overlay.
  * All validation runs before any write.
* **Order within a block.** `SetOracleSigner` (`Other`) runs after `SubmitOraclePrices` (`Oracle`), per the post-EVM order.
* **Resolution is deterministic.** The validator record is checked first, then the signer index with a cross-check against the record.
* **Commits 2-3 are node-local** (admission, selection, ingress). Validators never re-derive selection (app.rs:5713/5727).

## Tasks

### Commit boundaries

| Commit | Tasks | Content |
|--------|-------|---------|
| **1 — oracle signer (consensus, own commit)** | S1-S4 | `SetOracleSigner`, `ValidatorState.oracle_signer`, reverse index, reporter resolution |
| **2 — mempool priority (own commit)** | M1-M4 | cancels, then oracle, then the rest; Active/signer gate; 4 per validator; RPC screens; pacing tier |
| 3 — rpc | R1, R2 | `getValidators.oracleSigner`; ingress oracle checks |
| 4 — wallet | W1 | lib + bin split; `validator set-oracle-signer` |
| **5 — feeder core (own commit)** | F1-F7 | `tools/price-feeder` lib |
| **6 — feeder binary (own commit)** | F8-F10 | health/metrics, CLI, e2e |
| 7 — docs | D1 | runbook, example config, design status |

---

### S1 — types: `SetOracleSigner` + `ValidatorState.oracle_signer` + index key

`══ COMMIT 1 (oracle signer, consensus) ══` `feat(oracle): validator hot oracle signer (SetOracleSigner), HL-style; lockstep`.

**RED tests**

* `torus-types` (eip712.rs tests):
  * `set_oracle_signer_requires_eip712_and_is_outside_every_session_scope`: `requires_eip712` holds, and none of `Trading`, `TransfersOnly`, `Full` allows the action.
  * `set_oracle_signer_hash_binds_signer_and_nonce`: the hash changes with the signer and with the nonce.
  * Signing with an EIP-712 key and calling `recover_sender` returns that key's address.
* `torus-types` (lib.rs tests): canonical bytes are tag `27` followed by the 20-byte signer; serde JSON round-trips.
* `torus-economics`: `validator_state_borsh_roundtrip_with_signer` covers both `Some(signer)` and `None`.
* `torus-state` (cf): `oracle_signer_key(a)` equals `b"sgn" ‖ a` (23 bytes), and the prefix differs from `"sub"` and `"agg"`.

**Implementation**

* lib.rs:
  * Append `SetOracleSigner { signer: Address }` after `ClaimUnbonded` (:634). `Address::ZERO` clears the signer.
  * Canonical encoding: `buf.push(27); buf.extend_from_slice(signer.as_slice());`.
  * Add the variant to the `Full` scope exclusion list (:225-236).
* eip712.rs:
  * Add `hash_set_oracle_signer` with type string `"SetOracleSigner(address signer,uint64 nonce)"`.
  * Add the dispatch arm at :242.
  * Add the variant to `requires_eip712` (:757-769).
* types.rs: add `pub oracle_signer: Option<Address>`. Serialize it last (an Option tag, then 20 bytes); deserialize to match.
* Add `oracle_signer: None` to the 10 existing struct literals.
* cf.rs: add `pub fn oracle_signer_key(signer: &Address) -> [u8; 23]`; the key lives in `CF_NATIVE_ORACLE`.
* Exhaustive matches on `NativeAction`: the compiler lists them. `classify_action` already falls through to `_ => Other`. The exec dispatch gets the S2 arm.

**validate:** `CARGO⟨test -j6 -p torus-types⟩ && CARGO⟨test -j6 -p torus-economics --lib⟩ && CARGO⟨test -j6 -p torus-state --lib oracle_signer_key⟩ && CARGO⟨check -j6 --workspace --all-targets⟩` · depends_on: []

### S2 — exec: `exec_set_oracle_signer`

**RED tests**

New file `torus-bridge/tests/oracle_signer_tests.rs`:

* Copy the helpers from `oracle_block_tests.rs:17-96`. `put_validator` gains `oracle_signer: None`.
* Test addresses: `S = addr(50)`, `S2 = addr(51)`.

```rust
fn set(sender: u8, signer: Address) -> (Address, NativeAction) {
    (addr(sender), NativeAction::SetOracleSigner { signer })
}
fn signer_of(db: &StateDb, v: u8) -> Option<Address> {
    StakingManager::new(db.clone()).get_validator(&addr(v)).unwrap().unwrap().oracle_signer
}
fn index(db: &StateDb, s: Address) -> Option<Vec<u8>> {
    db.get_cf_raw(CF_NATIVE_ORACLE, &torus_state::cf::oracle_signer_key(&s)).unwrap()
}

#[test]
fn validator_sets_and_rotates_its_signer() {
    let (_d, db) = oracle_db();
    assert!(exec(&db, 1, set(V1, S)).success);
    assert_eq!(signer_of(&db, V1), Some(S));
    assert_eq!(index(&db, S).as_deref(), Some(addr(V1).as_slice()));
    assert!(exec(&db, 2, set(V1, S)).success, "re-setting the same signer is a no-op");
    assert!(exec(&db, 3, set(V1, S2)).success);
    assert_eq!(signer_of(&db, V1), Some(S2));
    assert_eq!(index(&db, S), None, "rotation deletes the old index entry");
    assert!(exec(&db, 4, set(V1, Address::ZERO)).success);
    assert_eq!(signer_of(&db, V1), None);
    assert_eq!(index(&db, S2), None);
}

#[test]
fn non_validator_cannot_set_a_signer() {
    let (_d, db) = oracle_db();
    assert_rejected(&exec(&db, 1, set(77, S)), "not a registered validator");
    assert_eq!(index(&db, S), None);
}

#[test]
fn a_signer_serves_one_validator_and_is_not_a_validator() {
    let (_d, db) = oracle_db();
    assert!(exec(&db, 1, set(V1, S)).success);
    assert_rejected(&exec(&db, 2, set(V2, S)), "already serves");
    assert_eq!(signer_of(&db, V2), None);
    assert_rejected(&exec(&db, 3, set(V2, addr(V3))), "is a validator");
    assert_rejected(&exec(&db, 4, set(V2, addr(V2))), "is a validator"); // self
}

#[test]
fn tombstoned_validator_cannot_set_but_candidate_can() {
    // put_validator(.., Tombstoned) -> rejected "tombstoned"; Candidate -> ok
}
```

**Implementation**

Add the handler near NE :6512 and its dispatch arm near :3383. The handler validates everything first and only then writes:

```rust
    fn exec_set_oracle_signer<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        signer: Address,
    ) -> NativeActionResult {
        use torus_economics::types::ValidatorStatus;
        let err = |m: String| NativeActionResult::err("set_oracle_signer", m);
        let mut v = match ctx.staking.get_validator(sender) {
            Ok(Some(v)) if v.status != ValidatorStatus::Tombstoned => v,
            Ok(Some(_)) => return err(format!("validator {sender} is tombstoned")),
            Ok(None) => return err(format!("{sender} is not a registered validator")),
            Err(e) => return err(e.to_string()),
        };
        if v.oracle_signer == Some(signer) {
            return NativeActionResult::ok("set_oracle_signer", 1000);
        }
        if signer != Address::ZERO {
            match ctx.staking.get_validator(&signer) {
                Ok(None) => {}
                Ok(Some(_)) => return err(format!("signer {signer} is a validator")),
                Err(e) => return err(e.to_string()),
            }
            match ctx.state.get_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&signer)) {
                Ok(None) => {}
                Ok(Some(owner)) => {
                    return err(format!("signer {signer} already serves validator 0x{}", hex::encode(owner)))
                }
                Err(e) => return err(e.to_string()),
            }
        }
        // writes (validation complete)
        if let Some(old) = v.oracle_signer {
            if let Err(e) = ctx.state.delete_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&old)) {
                return err(e.to_string());
            }
        }
        v.oracle_signer = (signer != Address::ZERO).then_some(signer);
        if let Some(s) = v.oracle_signer {
            if let Err(e) = ctx.state.put_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&s), sender.as_slice()) {
                return err(e.to_string());
            }
        }
        match ctx.staking.put_validator(sender, &v) {
            Ok(()) => NativeActionResult::ok("set_oracle_signer", 2000),
            Err(e) => err(e.to_string()),
        }
    }
```

At implementation time, confirm which field name of `NativeExecContext` holds the `StateBackend` handle; the oracle and staking managers write through it. The writes must stay inside the exec overlay.

**validate:** `CARGO⟨test -j6 -p torus-bridge --test oracle_signer_tests⟩` · depends_on: [S1]

### S3 — exec: the signer reports for its validator and has no other authority

**RED tests** (same file)

```rust
fn submit_from(sender: Address, prices: &[(MarketId, FixedPoint)]) -> (Address, NativeAction) {
    (sender, NativeAction::SubmitOraclePrices(OracleSubmission { prices: prices.to_vec(), timestamp: 0 }))
}

#[test]
fn signer_submission_is_keyed_and_weighted_by_its_validator() {
    let (_d, db) = oracle_db();
    assert!(exec(&db, 1, set(V3, S)).success);
    assert!(exec(&db, 2, submit_from(S, &[(1, fp(100))])).success);
    let rows = db.iterate_cf(CF_NATIVE_ORACLE, Some(b"sub")).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(&rows[0].0[11..31], addr(V3).as_slice(), "row key is \"sub\"‖market(8)‖VALIDATOR");
    // Weighting: V1=90 and V2=110 submit directly, V3 (3x stake) submits 100 via its signer;
    // run_block as in oracle_block_tests.rs:151-200 -> stake-weighted mark == 100.
}

#[test]
fn unknown_signer_and_jailed_validators_signer_are_rejected() {
    let (_d, db) = oracle_db();
    assert_rejected(&exec(&db, 1, submit_from(S, &[(1, fp(100))])), "not a registered validator or oracle signer");
    assert!(exec(&db, 2, set(V1, S)).success);
    put_validator_status(&db, V1, ValidatorStatus::Jailed); // keeps oracle_signer
    assert_rejected(&exec(&db, 3, submit_from(S, &[(1, fp(100))])), "not active");
    assert_eq!(sub_rows(&db), 0);
}

#[test]
fn old_signer_is_rejected_after_rotation() {
    let (_d, db) = oracle_db();
    assert!(exec(&db, 1, set(V1, S)).success);
    assert!(exec(&db, 2, set(V1, S2)).success);
    assert_rejected(&exec(&db, 3, submit_from(S, &[(1, fp(100))])), "oracle signer");
    assert!(exec(&db, 4, submit_from(S2, &[(1, fp(100))])).success);
}

/// D-S1: the signer has NO authority over the validator's account.
#[test]
fn signer_cannot_act_for_its_validator() {
    let (_d, db) = oracle_db();
    // V1 has a balance and one resting order (fund + place as in oracle_block_tests.rs:167-175).
    assert!(exec(&db, 1, set(V1, S)).success);
    let before = dump_validator_state(&db, V1);
    for action in [
        place_order(1, fp(100)),
        NativeAction::CancelOrder { order_id: v1_order },
        NativeAction::CancelAllOrders { market_id: None },
        NativeAction::Withdraw { amount: U256::from(1u8), to: S },
        NativeAction::TransferToSpot { amount: U256::from(1u8) },
        NativeAction::Delegate { validator: addr(V1), amount: U256::from(1u8) },
        NativeAction::ClaimRewards,
        NativeAction::UnjailSelf,
        NativeAction::SetOracleSigner { signer: S2 }, // a signer cannot rotate itself
        NativeAction::UpdateCommission { new_rate: 1 },
    ] {
        let _ = NativeExecutor::execute(&mut ctx_at(db.clone(), 2), &S, &action);
    }
    assert_eq!(dump_validator_state(&db, V1), before, "validator state byte-identical");
    assert_eq!(signer_of(&db, V1), Some(S));
}
```

`dump_validator_state` collects V1's rows from `CF_NATIVE_BALANCES`, `CF_NATIVE_POSITIONS` and `CF_NATIVE_ORDERS`, plus V1's value in `CF_STAKING_VALIDATORS`. Check the key layouts at implementation time. A full CF dump that excludes the signer's own rows works too.

**Implementation**

In NE :6512-6535, replace the Active check on `sender`:

```rust
        let reporter = match Self::resolve_oracle_reporter(ctx, sender) {
            Ok(r) => r,
            Err(m) => return NativeActionResult::err("submit_oracle_prices", m),
        };
        // ... Active check on `reporter` (text unchanged: "validator {reporter} is not active") ...
        // rows: ctx.oracle.submit_price(&reporter, …)
```

```rust
    /// Validator record first (direct submission), else the hot signer index with
    /// the record cross-check (stale index entries never resolve).
    fn resolve_oracle_reporter<T: StateBackend>(ctx: &NativeExecContext<T>, sender: &Address) -> Result<Address, String> {
        if ctx.staking.get_validator(sender).map_err(|e| e.to_string())?.is_some() {
            return Ok(*sender);
        }
        let raw = ctx.state.get_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(sender)).map_err(|e| e.to_string())?;
        if let Some(v) = raw.filter(|b| b.len() == 20).map(|b| Address::from_slice(&b)) {
            if let Some(rec) = ctx.staking.get_validator(&v).map_err(|e| e.to_string())? {
                if rec.oracle_signer == Some(*sender) {
                    return Ok(v);
                }
            }
        }
        Err(format!("{sender} is not a registered validator or oracle signer"))
    }
```

The existing test `only_active_validators_may_submit` (oracle_block_tests.rs:133) must stay green. Its needle "not a registered validator" is a substring of the new error text.

**validate:** `CARGO⟨test -j6 -p torus-bridge --test oracle_signer_tests⟩ && CARGO⟨test -j6 -p torus-bridge --test oracle_block_tests⟩ && CARGO⟨test -j6 -p torus-bridge⟩` · depends_on: [S2]

### S4 — consensus: whole-block and determinism pins (`app.rs` tests)

**RED tests** in `mod crash_recovery_tests`, next to the oracle helpers at :14380:

* `signer_signed_submissions_aggregate_through_whole_blocks`:
  * Block 1: every validator sends an EIP-712 `SetOracleSigner`, signed with `oracle_key(seed)`.
  * Blocks 2+: `SubmitOraclePrices` signed with the signer keys (`signer_key(seed)`).
  * After the next block, `OracleManager::get_price` equals the stake-weighted median.
  * This exercises the full path: batch signature verification, then sender = signer, then resolution to the validator.
* `rotation_in_block_counts_old_signer_once_then_rejects_it`:
  * One block carries both `SetOracleSigner(new)` and a submission from the old signer. The row is written, because the oracle category runs first.
  * In the next block, a submission from the old signer adds no row.
* `oracle_signer_blocks_replay_identically`: the same chain, run through `execute_committed_block` and through crash replay, gives equal `dump_all_cfs`, including the `"sgn"` rows and the validator records.

**validate:** `CARGO⟨test -j6 -p torus-consensus --lib oracle_signer⟩ && CARGO⟨test -j6 -p torus-consensus --lib oracle⟩ && CARGO⟨test -j6 -p torus-integration-tests --test chaos⟩` · depends_on: [S3]

`══ end COMMIT 1 ══`

---

### M1 — pool: classes cancels(0), then oracle(1), then rest(2); pending count per validator

`══ COMMIT 2 (mempool priority) ══` `feat(mempool): oracle submissions get priority after cancels, gated to Active validators/signers`.

**RED tests** (native_pool.rs tests; `oracle(n)` builds a one-entry `SubmitOraclePrices`):

* `selection_order_is_cancels_then_oracle_then_rest`: insert a low-address ClaimRewards, a mid-address cancel and a high-address oracle submission. `select_for_block` returns `[cancel, oracle, ClaimRewards]`.
* `full_pool_oracle_submission_evicts_a_normal_entry`: capacity 2, filled with ClaimRewards. Inserting an oracle submission evicts one of them; `assert_index_consistent` holds.
* `full_pool_of_priority_entries_rejects_oracle`: capacity 1 holding a cancel. Inserting an oracle submission fails with `NativePoolFull`.
* `oracle_pending_counts_across_validator_and_signer`: insert 2 entries from V and 1 from S.
  * `oracle_pending(&[V, S]) == 3` and `oracle_pending(&[V]) == 2`.
  * After `remove_committed` of one V entry, `oracle_pending(&[V, S]) == 2`.
* `priority_only_selection_takes_cancels_and_oracle`: `select_cancels_for_block_with_senders_excluding` returns 2 of 3 entries, without the ClaimRewards.
* `selection_order_identical_to_reference_stable_sort` (:871) needs no change. Cancels stay at class 0, and that test's data contains no oracle submissions.

**Implementation**

* Add `PRIO_CANCEL = 0`, `PRIO_ORACLE = 1`, `PRIO_NORMAL = 2` and update the `SortKey` doc (:13-35).
* Pool entry: replace `is_cancel: bool` with `priority: u8` (:43). Compute it with a new `priority_class(action)`.
* Key becomes `(priority, sender, nonce, seq)` (:220).
* Eviction (:200-215): an entry with `priority != PRIO_NORMAL` may evict the last entry, but only if that last entry has `priority == PRIO_NORMAL`.
* The cancels-only walk stops at the first `entry.priority == PRIO_NORMAL` (:414). Update its doc; keep its name.
* New method:

```rust
    /// Pooled oracle submissions whose sender is any of `accounts` (a validator
    /// and its hot signer). The M2 per-validator cap reads this under the same lock.
    pub fn oracle_pending(&self, accounts: &[Address]) -> usize {
        accounts.iter().map(|a| {
            self.entries.range((PRIO_ORACLE, *a, 0, 0)..=(PRIO_ORACLE, *a, u64::MAX, u64::MAX)).count()
        }).sum()
    }
```

* Next to `is_cancel` (:540-547), add `pub fn is_oracle_submission` and `pub fn is_priority` (cancel or oracle). Re-export both in `lib.rs:34`.
* `rate_limit.rs`: add `pub const ORACLE_PENDING_PER_VALIDATOR: usize = 4;`.

**validate:** `CARGO⟨test -j6 -p torus-mempool --lib native_pool⟩` · depends_on: [S1]

### M2 — mempool: gate and cap in `submit_native_action_inner`

**RED tests** (mempool lib.rs tests)

Helpers:

* `put_validator(state, addr, status, signer: Option<Address>)`. When a signer is given, it also writes the `"sgn"` index entry via `oracle_signer_key`.
* `oracle_from(key, nonce)`.

`oracle_admission_requires_active_validator_or_its_signer`:

* Admitted:
  * the Active validator V's own submission;
  * its registered signer S's submission, through the presigned, gossip-recover and gossip-trusted paths.
* Rejected, each with `"oracle submission from {s}: not an active validator or its signer"`:
  * a jailed validator;
  * the signer of a jailed validator;
  * a stranger;
  * a signer whose index entry points to V while `V.oracle_signer != Some(S)` (stale index).

`oracle_cap_is_four_per_validator_across_its_signer`:

* 2 submissions from V and 2 from S are admitted.
* A 5th, from either, is rejected with `"oracle pending cap"`.
* Other validators are unaffected, and a ClaimRewards from V is still admitted.

**Implementation**

* `torus-mempool/Cargo.toml`: add `torus-economics = { workspace = true }`. Economics depends only on types and state, so this creates no cycle.
* In `submit_native_action_inner` (:657):

```rust
        let oracle_accounts = if crate::native_pool::is_oracle_submission(&action.action) {
            match self.oracle_reporter(&sender) {
                Some(accts) => Some(accts),
                None => return Err(MempoolError::NativeValidationFailed(format!(
                    "oracle submission from {sender}: not an active validator or its signer"))),
            }
        } else { None };
        {
            let mut pool = self.native.write().unwrap();
            if let Some(accts) = &oracle_accounts {
                if pool.oracle_pending(accts) >= crate::rate_limit::ORACLE_PENDING_PER_VALIDATOR {
                    return Err(MempoolError::NativeValidationFailed(format!(
                        "oracle pending cap {} reached for validator {}",
                        crate::rate_limit::ORACLE_PENDING_PER_VALIDATOR, accts[0])));
                }
            }
            let result = pool.insert_with_restash_key(sender, action, cache_key);
            // ... unchanged ...
        }
```

* `oracle_reporter(&sender) -> Option<Vec<Address>>`:
  * for an Active validator sender, returns `[validator, signer?]`;
  * for a valid signer, returns `[V, sender]`;
  * does at most 2 point reads (`StakingManager::get_validator` and `get_cf_raw(CF_NATIVE_ORACLE, oracle_signer_key)`) with the same cross-check as S3.
* On the gossip paths, DA mirroring runs before this gate, so rejected bodies stay reconstructable.

**validate:** `CARGO⟨test -j6 -p torus-mempool --lib oracle⟩ && CARGO⟨test -j6 -p torus-mempool⟩` · depends_on: [M1]

### M3 — RPC screens let priority actions through

**RED tests** (`torus-rpc/src/lib.rs`, harness at :1418-1446)

`admission_backlog_lets_validator_and_signer_oracle_through`:

* Setup: backlog on; `store_market(1)`; Active validator V (key `ac09…ff80`) with signer S (key `[21; 32]`).
* Submit the batch `[V oracle, S oracle, stranger oracle]`:
  * V and S get hashes;
  * the stranger gets an error containing "active validator or its signer", not "busy".
* The single-submit endpoint also admits S.
* `native_pool_size() == 3`.

`pool_full_lets_validator_oracle_through` (pattern of :1503): the pool is full with one ClaimRewards; the oracle submission evicts it.

**Implementation**

* `torus.rs:456` and `:1122-1126`: replace `is_cancel` with `torus_mempool::is_priority`.
* Update the comments at :436-441 and :1119-1120.

**validate:** `CARGO⟨test -j6 -p torus-rpc --lib admission_backlog⟩ && CARGO⟨test -j6 -p torus-rpc --lib pool_full⟩` · depends_on: [M2]

### M4 — pin the priority-only pacing tier

**RED test** (mempool lib.rs): `priority_only_tier_selects_cancels_then_oracle`. `Mempool::select_native_cancels_for_block_with_senders_excluding` returns `[cancel, V oracle]`, and the ClaimRewards stays pooled.

**Implementation:** wording only, in app.rs. The `CancelsOnly` doc (:296-300) and the warn text (:5721-5726) become "CANCELS + ORACLE ONLY". The pacing tests stay unchanged.

**validate:** `CARGO⟨test -j6 -p torus-mempool --lib priority_only⟩ && CARGO⟨test -j6 -p torus-consensus --lib pacing⟩ && CARGO⟨test -j6 -p torus-consensus --lib deferred_exec_counts_toward_pacing_backlog⟩` · depends_on: [M1, M2]

`══ end COMMIT 2 ══`

---

### R1 — rpc: `getValidators.oracleSigner`

`══ COMMIT 3 (rpc) ══` `feat(rpc): expose oracle signer; ingress checks for oracle submissions`.

**RED test:** `get_validators_reports_oracle_signer`. A validator with a signer reports `"oracleSigner": "0x…"`. A validator without one omits the field.

**Implementation**

* `RpcValidatorInfo` (types.rs:416): add `#[serde(skip_serializing_if = "Option::is_none")] pub oracle_signer: Option<String>`.
* Fill it from `all_states` (torus.rs:1052-1070) with `hex_address`.

**validate:** `CARGO⟨test -j6 -p torus-rpc --lib get_validators⟩ && CARGO⟨test -j6 -p torus-rpc --test torus_staking_gov_rpc_tests⟩` · depends_on: [M3]

### R2 — rpc: ingress oracle checks (`validate_known_markets`)

**RED test** (torus.rs tests, near :1925). With market 1 listed, `verify_one_action` on a signed `SubmitOraclePrices`:

* returns `Err` containing:
  * "unknown market_id 2";
  * "duplicate market 1";
  * "invalid oracle price" (for 0 and for `MAX_ORACLE_PRICE_RAW + 1`);
  * "1..=256" (for an empty list and for 257 entries);
* returns `Ok` for a valid submission.

**Implementation:** add a new arm (:268-280) that mirrors the exec checks at NE :6531-6548:

```rust
        torus_types::NativeAction::SubmitOraclePrices(sub) => {
            use torus_core::oracle::{valid_oracle_price, MAX_ORACLE_PRICES_PER_SUBMISSION as CAP};
            if sub.prices.is_empty() || sub.prices.len() > CAP {
                return Err(format!("oracle submission carries 1..={CAP} prices, got {}", sub.prices.len()));
            }
            let mut seen = std::collections::BTreeSet::new();
            for &(mid, price) in &sub.prices {
                if !seen.insert(mid) { return Err(format!("duplicate market {mid} in oracle submission")); }
                check(mid)?;
                if !valid_oracle_price(price) { return Err(format!("invalid oracle price {price} for market {mid}")); }
            }
            Ok(())
        }
```

**validate:** `CARGO⟨test -j6 -p torus-rpc --lib oracle⟩ && CARGO⟨test -j6 -p torus-rpc⟩` · depends_on: [R1]

`══ end COMMIT 3 ══`

### W1 — wallet: lib + bin; `validator set-oracle-signer`

`══ COMMIT 4 ══` `feat(wallet): set-oracle-signer; expose keystore as a library`.

**RED tests** (commands/validator.rs tests, `set_oracle_signer_action`):

* `build_set_oracle_signer(Some(addr))` returns `SetOracleSigner{signer: addr}`.
* `build_set_oracle_signer(None)` returns `SetOracleSigner{signer: Address::ZERO}`.
* JSON round-trips.
* Signing with EIP-712 using `test_signing_key()`, then `recover_sender`, returns that key's address.
* `requires_eip712` holds.

**Implementation**

* `Cargo.toml`: add `[lib] path = "src/lib.rs"`. `lib.rs` contains `pub mod keystore;`.
* `main.rs`: use `torus_wallet::keystore` and drop `mod keystore;`. The other modules stay private to the binary.
* `SetOracleSigner { #[arg(long)] signer: Option<String>, #[arg(long)] clear: bool }`. Exactly one of the two must be given.
* `cmd_set_oracle_signer`: `parse_address`, then `submit_native_action` (EIP-712 via `--keystore`; `--dry-run` and `--json` work as before).

**validate:** `CARGO⟨test -j6 -p torus-wallet⟩` · depends_on: [S1]

---

### F1 — feeder crate + config

`══ COMMIT 5 (feeder core) ══` `feat(price-feeder): HL-style validator oracle feeder (lib)`.

**Crate setup:**

* Workspace member `"tools/price-feeder"` (Cargo.toml:20-23). Package `torus-price-feeder`: lib + bin `price-feeder`.
* Deps: `torus-types`, `torus-core` (light: `valid_oracle_price` and the cap), `torus-wallet` (keystore), `alloy-primitives`, `k256`, `serde`, `serde_json`, `toml`, `tokio`, `reqwest`, `clap`, `hex`, `rand`, `tracing`, `tracing-subscriber`.
* Dev-deps: `tempfile`. For F10 also `torus-rpc`, `torus-mempool`, `torus-state`, `torus-economics`, `torus-evm`, `jsonrpsee`, `borsh`.

**`config.rs`:**

* `Exchange` covers the 7 venues:
  * default weights 3/2/2/1/1/1/1;
  * default quote USD for Kraken, USDT for the others;
  * a default base URL per venue.
* `Config` fields:
  * `rpc_url` and `validator_address`;
  * signer key source: either `signer_keystore` + `passphrase_file`, or `signer_key_file` (exactly one of the two);
  * timing and thresholds: `interval_ms`, `fetch_timeout_ms`, `max_source_age_ms`, `min_sources`, `min_weight_bps`;
  * `quote_mode`: `par` | `kraken_usdt`;
  * `health_listen`, `exchanges` overrides, `markets`.
* `MarketCfg { market_id, base_asset, symbols: BTreeMap<Exchange, SymbolCfg> }`, where `SymbolCfg` is untagged: `Plain(String) | Full { symbol, quote }`.
* `deny_unknown_fields`.

**RED tests:**

* `example_config_parses_and_validates` loads `include_str!("../feeder.example.toml")`. That file is created in this task and includes the MATIC → POL mapping.
* `defaults_are_hl`: 3000 / 2000 / 5000 / 3 / 5000, weights 3,2,2,1,1,1,1, quote mode `par`.
* `validation_rejects` table:
  * interval < 1000;
  * fetch timeout >= interval;
  * max source age < fetch timeout;
  * min sources = 0;
  * bps > 10000;
  * no markets;
  * duplicate market id;
  * empty symbol;
  * fewer enabled symbols than `min_sources`;
  * weight 0;
  * both key sources set, or neither;
  * unknown key.

**validate:** `CARGO⟨test -j6 -p torus-price-feeder --lib config⟩` · depends_on: []

### F2 — price math (`price.rs`)

**RED tests:**

* `parse_price`:
  * "65012.34" → raw 6_501_234_000_000;
  * "0.000012345678901" → raw 1234 (truncated, not rounded);
  * "1" → 10^8;
  * rejects "", ".", "-1", "1e5", "1.2.3", " 1", "NaN", and a 40-digit overflow.
* `mid`:
  * (100, 102) → 101;
  * (100, 101) → 100.5 (raw);
  * crossed book → None;
  * bid 0 → None.
* `weighted_median`:
  * HL example B=100(3), O=101(2), Y=102(2), K=103, Ku=104, G=105, M=106 (weight 1 each), W=11 → **102**;
  * [(100,1),(200,1)] → 100;
  * a single point → itself;
  * empty or all-zero weight → None;
  * shuffled input gives the same result.
* `aggregate` (configured weight 11, min 3 sources, 5000 bps):
  * B+O+Y (weight 7) → Price;
  * K+Ku+G (3 < 6) → `TooLittleWeight{3,6}`;
  * 2 sources → `TooFewSources`;
  * a 5001 ms old sample is excluded; a 5000 ms old one is included;
  * `kraken_usdt` scales USDT mids, and drops them if the rate is missing;
  * `par` leaves them unchanged.

**Implementation** (integers only):

```rust
pub fn parse_price(s: &str) -> Option<FixedPoint> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty() && frac.is_empty() { return None; }
    if !whole.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) { return None; }
    let w: i128 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let f: i128 = format!("{:0<8}", &frac[..frac.len().min(8)]).parse().ok()?;
    Some(FixedPoint::from_raw(w.checked_mul(FixedPoint::SCALE)?.checked_add(f)?))
}
pub fn mid(bid: FixedPoint, ask: FixedPoint) -> Option<FixedPoint> {
    (bid.raw() > 0 && bid.raw() <= ask.raw()).then(|| FixedPoint::from_raw(bid.raw() + (ask.raw() - bid.raw()) / 2))
}
/// Lower weighted median: first price (ascending) where 2·cum >= total.
pub fn weighted_median(points: &mut [(FixedPoint, u32)]) -> Option<FixedPoint> {
    points.sort_by_key(|p| p.0.raw());
    let total: u64 = points.iter().map(|p| u64::from(p.1)).sum();
    let mut cum = 0u64;
    for &(p, w) in points.iter() {
        cum += u64::from(w);
        if total > 0 && 2 * cum >= total { return Some(p); }
    }
    None
}
```

`aggregate(samples, configured_weights, usdt_usd, now_ms, &Rules) -> Outcome`:

* the required weight is `need_w = ceil(total · bps / 10_000)`;
* USD conversion uses `checked_mul`; a sample whose conversion overflows is dropped.

**validate:** `CARGO⟨test -j6 -p torus-price-feeder --lib price⟩` · depends_on: [F1]

### F3 — venue parsers + `HttpGet` (`exchange.rs`)

**RED tests:**

* Each venue parses its fixture `tools/price-feeder/tests/fixtures/<venue>.json` (loaded with `include_str!`).
  * Fixtures are captured once at implementation time with a manual curl and trimmed to 2-3 symbols.
  * **No network access in tests.**
* Error envelopes → `Err`:
  * OKX `code != "0"`;
  * Bybit `retCode != 0`;
  * Kraken non-empty `error`;
  * KuCoin `code != "200000"`.
* A bad price on one symbol drops only that symbol. Malformed JSON → `Err`.
* `url_for`:
  * Binance URL-encodes `symbols=[...]`;
  * Kraken uses `pair=…`, plus `USDTZUSD` in `kraken_usdt` mode;
  * the `base_url` override is honoured.

**Implementation:**

* `Quotes = HashMap<String, (FixedPoint, FixedPoint)>`.
* `url_for` and `parse_quotes` (`serde_json::Value` + `parse_price`).
* `trait HttpGet { fn get(&self, url: String, timeout: Duration) -> impl Future<Output = Result<String, String>> + Send; }`. Rust 1.93 supports this directly, so no `async-trait`.
* `ReqwestGet` checks the HTTP status and caps the body at 4 MiB.

**validate:** `CARGO⟨test -j6 -p torus-price-feeder --lib exchange⟩` · depends_on: [F2]

### F4 — fetch + backoff (`fetch.rs`)

**RED tests** use `FakeHttp` (routes by `base_url` prefix, e.g. `fake://binance`, with a call log) and `FakeClock`:

* `backoff_doubles_to_60s_and_resets`;
* `fetch_all_skips_backed_off_venues`: a backed-off venue is not called at +500 ms and is called again at +1000 ms;
* `failed_venue_keeps_last_quotes_which_age_out`;
* `timeout_counts_as_failure`.

**Implementation:**

* `Backoff { failures, next_ms }`: the delay is `min(1000 << min(k-1, 6), 60_000)`.
* `fetch_all` runs one `JoinSet` task per venue that is not backed off, and stamps `fetched_at` and latency on each result.
* `Clock` trait: `SystemClock` and `FakeClock(Arc<AtomicU64>)`.

**validate:** `CARGO⟨test -j6 -p torus-price-feeder --lib fetch⟩` · depends_on: [F3]

### F5 — submissions + signing (`submit.rs`)

**RED tests:**

* `builds_only_listed_valid_markets`: prices {1: 65000, 2: 0, 3: 10^12+1, 4: 100} with listed {1, 3, 4} → [1, 4].
* `nothing_to_send_is_empty`.
* `chunks_at_the_cap`: 600 markets → 256 + 256 + 88.
* **Property** `every_built_submission_passes_exec_rules`: 500 seeded `StdRng` cases. Each submission has 1..=256 entries, no duplicates, only listed markets, and only prices that pass `valid_oracle_price`.
* `nonce_is_strictly_monotonic`.
* `signed_by_the_signer_key`: `recover_sender` returns the signer address.
* `classify_submit_error`:
  * "busy" or "overloaded" → Retryable;
  * "not a registered validator or oracle signer", "not an active validator", "active validator or its signer" → NotAuthorized;
  * "oracle pending cap" → Retryable;
  * anything else → Other.

**Implementation:**

```rust
pub fn build_submissions(prices: &BTreeMap<MarketId, FixedPoint>, listed: &BTreeSet<MarketId>,
                         sample_ts_ms: u64) -> Vec<OracleSubmission> {
    let ok: Vec<_> = prices.iter()
        .filter(|(m, p)| listed.contains(m) && torus_core::oracle::valid_oracle_price(**p))
        .map(|(m, p)| (*m, *p)).collect();
    ok.chunks(torus_core::oracle::MAX_ORACLE_PRICES_PER_SUBMISSION)
        .map(|c| OracleSubmission { prices: c.to_vec(), timestamp: sample_ts_ms }).collect()
}
pub struct NonceGen { last: u64 }
impl NonceGen { pub fn next(&mut self, now_ms: u64) -> u64 { self.last = now_ms.max(self.last + 1); self.last } }
pub fn sign(sub: OracleSubmission, nonce: u64, key: &k256::ecdsa::SigningKey) -> SignedNativeAction {
    torus_types::eip712::sign_native_action(NativeAction::SubmitOraclePrices(sub), nonce, key)
}
```

**validate:** `CARGO⟨test -j6 -p torus-price-feeder --lib submit⟩` · depends_on: [F1]

### F6 — node client + startup checks (`node.rs`)

**RED tests:**

* `parses_markets_page`, `pages_until_short_page` (500 then 3), `parses_validators_with_oracle_signer`.
* `startup_check`, as a table over `FakeNode`:
  * validator missing → Err;
  * `oracleSigner` absent or different from the feeder's address → Err, and the message contains `torus-wallet … validator set-oracle-signer --signer 0x…`;
  * validator not active → Ok(Idle);
  * listed market's `baseAsset` differs from the config → Err;
  * quote ≠ USD → Err;
  * configured market not listed → warn and skip;
  * key file with mode 0644 → Err.

**Implementation:**

* `NodeApi` trait: `listed_markets`, `validators`, `submit`.
* `RpcNode` follows the pattern in `tools/wallet/src/rpc.rs:24-50`, with a 2 s timeout. It submits via `torus_submitNativeAction` with hex-encoded JSON (wallet `rpc.rs:135-146`).
* Key loading: `torus_wallet::keystore::load_keystore`, or a hex key file with mode 0600 enforced.

**validate:** `CARGO⟨test -j6 -p torus-price-feeder --lib node⟩` · depends_on: [F5]

### F7 — the cycle (`feeder.rs`)

**RED tests** (`FakeHttp` + `FakeNode` + `FakeClock`):

* `happy_cycle_submits_weighted_medians_once`;
* `omits_only_the_starved_market`;
* `unlisted_or_all_omitted_sends_nothing`;
* `stale_cache_is_not_used`;
* `busy_is_retried_next_cycle_with_higher_nonce`;
* `not_authorized_marks_health_down`;
* `not_active_validator_idles`;
* `markets_refreshed_each_cycle`.

**Implementation:** `Feeder<H, N, C>::run_cycle` does, in order:

1. Every 60 s, re-check the validator and signer status; stay idle if the validator is not active or the signer is not registered.
2. Fetch `listed_markets`; on error, skip this cycle.
3. `fetch_all`.
4. Aggregate each configured market.
5. `build_submissions`.
6. Sign, submit, and classify the result.
7. Update health.

`run` drives cycles with `tokio::time::interval` (`MissedTickBehavior::Skip`) until ctrl-c.

**validate:** `CARGO⟨test -j6 -p torus-price-feeder --lib feeder⟩ && CARGO⟨test -j6 -p torus-price-feeder⟩` · depends_on: [F4, F6]

`══ end COMMIT 5 ══`

### F8 — health + metrics (`health.rs`)

`══ COMMIT 6 (feeder binary) ══` `feat(price-feeder): CLI, /health + /metrics, e2e against a real RPC server`.

**RED tests:**

* `status_ok_degraded_down`:
  * ok: a successful submission within 3 × interval and no market omitted;
  * degraded: a market omitted or a venue failing;
  * down (HTTP 503): no successful submission for more than 3 × interval, or NotAuthorized.
* `health_json_shape`.
* `metrics_text_contains_counters`.
* `http_routes`: `/health`, `/metrics`, and 404 for anything else.

**Implementation:**

* `Health` (serde) and `render_metrics`.
* `serve` is the faucet TCP loop (:533-580) without CORS. It binds `health_listen` (loopback).

**validate:** `CARGO⟨test -j6 -p torus-price-feeder --lib health⟩` · depends_on: [F7]

### F9 — CLI (`main.rs`)

**Commands:**

* `keygen --keystore <out> [--passphrase-file]`:
  * creates the keystore with `torus_wallet::keystore::generate_keystore`;
  * prints the signer address and the exact `torus-wallet --keystore <validator keystore> validator set-oracle-signer --signer 0x…` command.
* `address`: prints the signer address.
* `check --config`:
  * runs the startup check, then one fetch and aggregate;
  * prints a per-market table and the symbols missing per venue;
  * **never submits**.
* `run --config`.

**RED tests** (`keyfile.rs`):

* `hex_key_file_roundtrip_mode_0600`;
* `load_refuses_group_or_world_readable`;
* `load_rejects_bad_hex_or_length`;
* `keygen_refuses_to_overwrite`.

**validate:** `CARGO⟨test -j6 -p torus-price-feeder --lib keyfile⟩ && CARGO⟨build -j6 -p torus-price-feeder⟩` · depends_on: [F8, W1]

### F10 — end to end (`tools/price-feeder/tests/rpc_e2e.rs`)

**Setup** (harness from `torus-rpc/src/lib.rs:1418-1446`):

* A temp `StateDb` with markets 1 BTC/USD and 2 ETH/USD, using the `store_market` borsh layout (:2307-2320, copied).
* An Active validator V with `oracle_signer: Some(S)`: `put_validator(V, Active, oracle_signer: Some(S))` plus `put_cf_raw(CF_NATIVE_ORACLE, oracle_signer_key(S), V)`.
* `Mempool::new(state, MempoolConfig::default())`, `RpcServer::new(...)`, `start("127.0.0.1:0")`.

**Run:** a `Feeder` built from `RpcNode`, `FakeHttp` (fixtures for all 7 venues), `SystemClock` and signer key S. Call `run_cycle()` once.

**Assert:**

* `mempool.native_pool_size() == 1`;
* the pooled action has sender **S** and is a `SubmitOraclePrices` for markets [1, 2] with the expected medians.

**Variants:**

* backlogged mempool (`native_admission_horizon_ms: 1`, floor 0) → still admitted;
* signer not registered for V → `startup_check` fails and nothing is submitted.

Resolution from S to V at execution is covered by S3 and S4.

**validate:** `CARGO⟨test -j6 -p torus-price-feeder --test rpc_e2e⟩` · depends_on: [F9, M3, R1, R2]

`══ end COMMIT 6 ══`

### D1 — docs

`══ COMMIT 7 ══` `docs(oracle): price feeder runbook + design status`.

* `tools/price-feeder/README.md` runbook:
  1. `price-feeder keygen --keystore signer.keystore`.
  2. `torus-wallet --keystore <validator EVM keystore> validator set-oracle-signer --signer <addr>`: run once; `--clear` removes the signer.
  3. Write the config from `feeder.example.toml` (with MATIC → POL).
  4. `price-feeder check`.
  5. Install the systemd unit (`Restart=always`, RPC on loopback).
  6. Watch `/health`.
  7. Rotation: new keystore, then `set-oracle-signer`, then restart the feeder. The old signer is rejected from the next block on.
* `docs/plans/oracle-feeder.md`: set Status to implemented, with commit hashes.
* `docs/parity-audit-fixes-s515.md`:
  * mark the feeder deferred item as done;
  * add behaviour rows for "Oracle signer" and "Oracle mempool priority";
  * note under deployment that commit 1 needs a lockstep upgrade.

**validate:** `true` · depends_on: [F10, S4]

### V — verification (lead) · depends_on: [D1]

## Verification

Run everything from the worktree with `CARGO_TARGET_DIR=/home/crab/projects/Torus-hyperBFT-wt-feeder/target`, through the wrapper:

1. Before S1, record the baseline pass/fail counts of the 11-crate run (step 7) at `d4fe69c`.
2. `CARGO⟨test -j6 -p torus-types⟩`, `CARGO⟨test -j6 -p torus-economics⟩`, `CARGO⟨test -j6 -p torus-state --lib⟩`.
3. `CARGO⟨test -j6 -p torus-bridge⟩`.
4. `CARGO⟨test -j6 -p torus-consensus --lib⟩`, `CARGO⟨test -j6 -p torus-integration-tests --test chaos⟩`.
5. `CARGO⟨test -j6 -p torus-mempool⟩`, `CARGO⟨test -j6 -p torus-rpc⟩`.
6. `CARGO⟨test -j6 -p torus-wallet⟩`, `CARGO⟨test -j6 -p torus-price-feeder⟩`.
7. The 11-crate run: `CARGO⟨test -j6 -p torus-types -p torus-state -p torus-evm -p torus-core -p torus-consensus -p torus-bridge -p torus-rpc -p torus-mempool -p torus-economics -p torus-genesis -p torus-telemetry⟩`. Expect the baseline plus the new tests, and the same known failures.
8. `CARGO⟨check --workspace --all-targets⟩` and `CARGO⟨clippy -p torus-price-feeder -p torus-mempool -p torus-wallet -- -D warnings⟩`.
9. Optional, manual, not in CI: run `price-feeder check` against a local devnet node with live venues.

## Rollback

* Revert commits in reverse order, 7 back to 1.
* Commit 1 is consensus-visible: reverting it is again a lockstep upgrade with a fresh genesis.
* Commits 2-3 are node-local or ingress-only, so they can be reverted per node.
* The feeder can simply be stopped. The mark then goes stale after 60 s.

## Risks / design corrections (flagged, not silently changed)

* **D-S1 — signer authority.** The signer has oracle authority for its validator only, and no authority over the validator's account.
  * Hard-blocking every non-oracle action sent from a signer address is not planned: it would add an index read per action on the hot exec path, across several dispatch paths.
  * See Q-S1.
* **One validator per signer**, and a signer cannot itself be a validator. Decided and tested in S2.
* **Same-block rotation.** A submission from the old signer still counts in the rotation block. Pinned in S4.
* **New action variant** affects every exhaustive match on `NativeAction`. Appending the variant keeps serde indices stable.
* **`ValidatorState` layout change.** It needs a fresh genesis, which the stack already requires.
* **Mempool priority is not consensus-visible.** It still ships alongside the lockstep commit.
* **R-DoS.** Under backlog, each oracle-shaped action costs one signature verification before the gate rejects it. That is the same exposure cancels have today. Pool residency is bounded by the gate plus the cap of 4 per validator.
* **R-venues.** Exchange symbols can drift, fixture shapes can go stale, bulk response bodies are capped at 4 MiB, and rate limits allow 1 request per venue per 3 s.
* **R-depeg.** With `par`, a stablecoin depeg goes straight into the price. Use `kraken_usdt` if that matters.

## Implementation corrections (s517)

Each item notes a place where the plan draft was changed during implementation.

**Commit 1 (oracle signer)**

* Correction s517 (S1). At `76ae0ba` there are 6 `ValidatorState { … }` literals
  (staking, genesis, app.rs ×2, chaos, oracle_block_tests), not 10.
* Correction s517 (S1). `execute_action` is an exhaustive match. The S1
  `check --workspace` therefore runs after S2, which adds the dispatch arm. The
  S2 RED was `E0004 non-exhaustive patterns`.
* Correction s517 (S1). `action_hash_scratch_tests.rs` keeps an independent
  frozen encoder and a tag-coverage pin. The frozen encoder gets the tag-27 arm
  and the pin becomes `0..=27`.
* Correction s517 (S2). The no-op check compares
  `Option` (`v.oracle_signer == new`). Clearing an unset signer is therefore a
  no-op success, not a write.
* Correction s517 (S3). `resolve_oracle_reporter` returns
  `(validator, status)`. The Active check then needs no second record read. The
  error texts are unchanged.
* Correction s517 (S3, D-S1 test).
  * `Delegate { validator: V1 }` is dropped from the signer's action list. A
    third party may delegate to V1; that changes `V1.total_delegated`
    legitimately and is not authority over V1.
  * `ModifyOrder` and `ClaimUnbonded` are added to the list.
  * The signer is funded, so its own order really rests.
  * The dump covers the rows of the account and staking CFs whose key carries
    V1, plus V1's in-memory resting orders.
* Q-S1 was decided by the user: the signer's own account is unrestricted. There is
  no per-action lookup.

**Commit 2 (mempool priority)**

* Correction s517 (M1). `PRIO_*` and `priority_class` are public next to
  `is_cancel`, and `is_oracle_submission` / `is_priority` are re-exported.
  Priority entries never evict each other: a cancel does not evict an oracle
  submission, and an oracle submission does not evict a cancel. This is pinned
  in `full_pool_of_priority_entries_rejects_oracle`.
* Correction s517 (M2). `oracle_reporter` fails closed: a state read error
  rejects the submission. The admission test drains the pool between insert
  paths, so that it stays under the cap of 4 per validator, which the five
  admitted submissions would otherwise exceed.
* Correction s517 (M3). The single-submit endpoint has only the backlog screen
  (no pool-full screen), as before. Only its predicate changes to `is_priority`.

## Open questions (user)

* **Q-S1:** read "nothing else" strictly and hard-block every non-oracle native action sent from a registered signer address? That costs one index read per action on the hot path. The default plan scopes authority instead (D-S1).
* **Q-S2:** may a validator set its signer while still a Candidate? The plan says yes for Candidate and no for Tombstoned.
