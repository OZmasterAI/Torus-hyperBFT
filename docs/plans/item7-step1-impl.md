# Implementation Plan: item 7 step 1, EVM lanes (A + B)

Design: `docs/plans/item7-step1-design.md`, option A with "Decisions (owner s123)". Step 1 decisions (s122):
`item7-evm-lanes.md` lines 22-38. Code facts from four read-only scouts on main `46428bfa` (s123). Line numbers are
from that commit; re-check them before each edit.

Branch `feat/item7-step1-evm-lanes`, worktree `/home/18c/projects/wt/item7-step1` (off main), own target dir. The header
and the epoch plan change format, so the chain needs a fresh genesis (free before testnet; no activation height).

```bash
git -C /home/18c/projects/Torus-hyperBFT worktree add /home/18c/projects/wt/item7-step1 -b feat/item7-step1-evm-lanes main
cd /home/18c/projects/wt/item7-step1
export CARGO_TARGET_DIR=/home/18c/.cargo-target-item7   # never share a target dir with another worktree
L="flock -w 60 -o $HOME/projects/wt/.cargo-heavy.lock"  # gate 26; exit 1 after the wait = lock busy, report it
P="-p torus-types -p torus-genesis -p torus-bridge -p torus-consensus -p torus-economics -p torus-mempool \
   -p torus-rpc -p torus-core -p torus-evm -p hotstuff_rs -p torus-node -p torus-state -p torus-integration-tests"
# one consistent -p set for every cargo call (other subsets re-unify features and rebuild librocksdb-sys)
```

## Design Decision

- **Header:** new field `evm_block_interval: u64` (N), in the canonical bytes. `evm_gas_limit` is the per-EVM-block budget.
- **Genesis:** `evm.gas_limit` = 3M and `evm.block_interval` = 5 build the genesis parent header (today hardcoded 30M).
  Built once in `TorusApp::try_new`, stored on `TorusApp` and `ExecutionContext`.
- **Rule:** off an epoch boundary both values equal the parent's. At a boundary they equal the epoch plan's `PlanEvm`
  (`None` = the parent's). Execution of boundary H copies the governance rows into plan(H + L). The rule runs pre-vote
  and at insertion: hotstuff votes after `check_block_data` alone, so a vote-only gap would let a quorum certify a
  block that insertion rejects.
- **Cadence and caps:** pre-vote only, in `evm_body_fits_header`: EVM txs only at `height % N == 0`, count <=
  `evm_gas_limit / 21_000`, raw bytes <= 256 KiB.
- **Proposer:** at a boundary it uses the plan's values; it drains EVM only on cadence (otherwise txs stay pooled).
- **Governance:** keys `evm_block_gas_limit` (1M..=10M) and `evm_block_interval` (2..=100), range-checked at
  submission and execution. A change lands at boundary + L, 100-199 blocks after it executes.
- **Mempool:** admission caps tx gas at the committed header's limit and tx size at 256 KiB.
- **Lost txs:** EVM txs drained into our own proposal go back to the pool when another block commits at that height.
- **Routing:** RPC forwards EVM txs to the leader of the next EVM height, not the current-view leader.
- **RPC:** `eth_call` / `eth_estimateGas` default and cap gas at the header limit (today 30M, which revm rejects above
  a 3M block).
- **Precompile:** the `getOrderBook` selector at 0x0800 is removed (`getPosition` stays).
- **CI:** the Uniswap e2e genesis gets a 10M budget (Router deploy ~4.2M) until step 2's big blocks.

## Owner decisions on the planning scouts' points (s123)

1. **Tombstone compaction kept.** Decision 6's condition ("delete it if nothing else range-scans the column family")
   fails: RPC `torus_getOrderBook` row layouts scan every row of the market (`torus-rpc/src/scan.rs:745-768`). E14 only
   fixes its comments.
2. **256 KB = 256 KiB** (262,144 bytes).
3. **`eth_call` gas above the header limit is clamped** to the limit (revm would reject it).

## Success Criteria

1. A fresh 3M / N = 5 genesis yields headers with 3M / 5. An honest proposer puts EVM txs only into blocks at
   `height % 5 == 0`, and validators refuse before voting any block that carries EVM txs off cadence, more than
   `limit / 21,000` of them, or more than 256 KiB of them.
2. A ParameterChange of either key inside its range reaches the header at the next boundary + L. Out-of-range values are
   refused at submission. A boundary header that differs from the plan is Invalid before the vote and at insertion. A
   boundary whose plan is not ready is Missing.
3. Admission refuses a tx above the committed header's gas limit or above 256 KiB.
4. EVM txs from our proposal that lost at its height return to the pool, minus those the committed block carries.
5. EVM txs are forwarded to the leader of the next EVM height.
6. `eth_call` without gas and `eth_estimateGas` work under a 3M header.
7. `getOrderBook` reverts for exactly the precompile base gas; `getPosition` is unchanged.
8. Every touched crate's tests pass; `RUSTFLAGS="-D warnings" cargo build $P --all-targets` passes (CI uses it, so
   anything left unused must go); fmt diffs and clippy warnings do not grow vs main (main is not fmt-clean: 3,687
   `cargo fmt --check` diffs and ~72 clippy warnings, so never run `cargo fmt --all`); the CI Uniswap e2e still deploys.

## Tasks

### E1: shared constants and the governance value parser (torus-types)
- **Test first** (`crates/torus-types/src/lib.rs` `mod tests`):
  ```rust
  #[test]
  fn gov_u64_in_parses_in_range_else_none() {
      let r = 2..=100u64;
      assert_eq!(gov_u64_in(None, &r), None);
      assert_eq!(gov_u64_in(Some(b"abc"), &r), None);
      assert_eq!(gov_u64_in(Some(b"1"), &r), None);
      assert_eq!(gov_u64_in(Some(b"101"), &r), None);
      assert_eq!(gov_u64_in(Some(b"5"), &r), Some(5));
  }
  ```
- **Implementation:** next to `NATIVE_ORDERS_PER_BATCH_CAP` (~665-671):
  ```rust
  /// Item 7 step 1: genesis default EVM gas budget per EVM block (header `evm_gas_limit`).
  pub const DEFAULT_EVM_GAS_LIMIT: u64 = 3_000_000;
  /// Item 7 step 1: genesis default N; EVM txs only in blocks with `height % N == 0`.
  pub const DEFAULT_EVM_BLOCK_INTERVAL: u64 = 5;
  /// Item 7 step 1: most raw EVM tx bytes per block (vote rule), so also per tx (admission).
  pub const MAX_EVM_BLOCK_TX_BYTES: usize = 256 * 1024;
  /// Cheapest tx's intrinsic gas: `evm_gas_limit / EVM_MIN_TX_GAS` caps a block's EVM tx count.
  pub const EVM_MIN_TX_GAS: u64 = 21_000;
  ```
  After `price_band_bps` (~689-705):
  ```rust
  /// Item 7 step 1: governance keys (`CF_FEE_CONFIG`, decimal, `ParameterChange`). Execution of
  /// boundary H copies them into the plan for H + L; absent or invalid = the chain keeps its value.
  pub const EVM_BLOCK_GAS_LIMIT_PARAM: &str = "evm_block_gas_limit";
  pub const EVM_BLOCK_GAS_LIMIT_RANGE: std::ops::RangeInclusive<u64> = 1_000_000..=10_000_000;
  pub const EVM_BLOCK_INTERVAL_PARAM: &str = "evm_block_interval";
  pub const EVM_BLOCK_INTERVAL_RANGE: std::ops::RangeInclusive<u64> = 2..=100;

  /// A stored governance value: the decimal when it is in `range`, else `None`.
  pub fn gov_u64_in(stored: Option<&[u8]>, range: &std::ops::RangeInclusive<u64>) -> Option<u64> {
      stored
          .and_then(|b| std::str::from_utf8(b).ok())
          .and_then(|s| s.parse::<u64>().ok())
          .filter(|v| range.contains(v))
  }
  ```
  Leave `price_band_bps` as it is (no refactor in this item).
- **Verify:** `$L cargo test $P gov_u64_in_parses_in_range_else_none`
- **Depends on:** none

### E2: header field `evm_block_interval`
- **Test first** (`torus-types/src/lib.rs` `mod tests`, built like `parent_hash_included_in_canonical_bytes`):
  ```rust
  #[test]
  fn evm_block_interval_in_canonical_bytes() {
      let a = /* the header literal used by parent_hash_included_in_canonical_bytes */;
      let mut b = a.clone();
      b.evm_block_interval += 1;
      assert_ne!(a.canonical_header_bytes(), b.canonical_header_bytes());
  }
  ```
  Plus `header_bincode_round_trip_keeps_evm_block_interval` (bincode 1 is the wire format, app.rs `datum_header`
  ~2033): serialize a header with N = 7, deserialize, assert 7. If torus-types has no bincode dev-dependency, put this
  test next to `datum_header` in app.rs.
- **Implementation** (`torus-types/src/lib.rs`):
  ```rust
      pub evm_gas_limit: u64,
      /// Item 7 step 1: N. EVM txs only in blocks with `height % N == 0`. Equals the parent's,
      /// except at an epoch boundary, where the epoch plan may change it.
      pub evm_block_interval: u64,
  ```
  In `canonical_header_bytes` (~606), right after `evm_gas_limit`:
  ```rust
          buf.extend_from_slice(&self.evm_block_interval.to_be_bytes());
  ```
  No serde attribute: the wire is bincode and header JSON is only read back from the node's own rows; a default of 0
  would only hide a bug. Update the JSON literal in `header_without_attestation_defaults_to_zero` (~1962) with
  `"evm_block_interval":5`.
  The 24 literal sites:
  - Production: `torus-bridge/src/proposer.rs:145` (`build_block`): `evm_block_interval: parent.evm_block_interval,`.
    `proposer.rs:190` (genesis): `evm_block_interval: torus_types::DEFAULT_EVM_BLOCK_INTERVAL,` until E4.
    `torus-consensus/src/app.rs:7157` (`build_proposal`): `evm_block_interval: parent_header.evm_block_interval,` until E8.
  - Tests, value `torus_types::DEFAULT_EVM_BLOCK_INTERVAL` (or the parent's where one exists): app.rs 8806, 9974;
    torus-types lib.rs 1938, 1970, 2234; `torus-types/tests/action_hash_scratch_tests.rs:297`;
    `torus-state/src/block_body.rs:194`; torus-rpc lib.rs 1011, 4135, 4292, 4389, 5900, 5966, 6032;
    `torus-rpc/src/scan_tests.rs:1822`; `torus-rpc/tests/eth_compliance_tests.rs` 90, 361;
    torus-bridge tests `bridge_tests.rs:484`, `state_root_once_tests.rs:89`, `exec_isolation_tests.rs:82`;
    `torus-integration-tests/.../native_da_pull_fallback.rs:101`.
    Find them with `/usr/bin/grep -rn "TorusBlockHeader {" crates` (the shell's `grep` wrapper skips ignored files).
- **Verify:** `$L cargo test $P evm_block_interval`
- **Depends on:** E1

### E3: genesis JSON, ChainConfig and genesis files
- **Test first** (`crates/torus-genesis/src/lib.rs` `mod tests`; use the sample JSON and parser the existing tests use):
  - `evm_block_interval_defaults_and_feeds_chain_config`: no field gives 5; `"block_interval": 7` gives 7 in
    `chain_config().evm_block_interval`; `0` gives `Err(GenesisError::InvalidEvm(_))`.
  - `shipped_genesis_files_use_the_item7_evm_values`, modelled on `shipped_genesis_files_use_1200ms_timeout_base`
    (~902): every tracked genesis file has `evm.gas_limit == 3_000_000` and `evm.block_interval == 5`.
- **Implementation:**
  ```rust
  // torus-genesis EvmConfig (~140-145)
      #[serde(default = "default_evm_block_interval")]
      pub block_interval: u64,
  fn default_evm_block_interval() -> u64 {
      torus_types::DEFAULT_EVM_BLOCK_INTERVAL
  }
  // GenesisError
      #[error("invalid evm section: {0}")]
      InvalidEvm(String),
  // from_json (~368-373), after check_fee_split
      if genesis.evm.block_interval == 0 {
          return Err(GenesisError::InvalidEvm("evm.block_interval must be >= 1".into()));
      }
  // chain_config() (~681-715), next to `evm_gas_limit: self.evm.gas_limit,`
      evm_block_interval: self.evm.block_interval,
  // torus-types ChainConfig (~1433); the serde default keeps the two legacy-JSON tests (~1633, ~1657) parsing
      #[serde(default = "default_evm_block_interval")]
      pub evm_block_interval: u64,
  fn default_evm_block_interval() -> u64 {
      DEFAULT_EVM_BLOCK_INTERVAL
  }
  ```
  Genesis may set N = 1; the 2..=100 range binds governance only.
  Other `ChainConfig {` literals get the field: `torus-node/src/main.rs:384` (`default_chain_config`, also
  `evm_gas_limit: torus_types::DEFAULT_EVM_GAS_LIMIT` at 387; without `--genesis` it would otherwise build a 30M
  genesis header and reject a 3M chain's block 1), torus-genesis lib.rs 681, `native_da_pull_fallback.rs:58`,
  app.rs 6105, 8643, 9914 (tests: the `DEFAULT_` constants).
  Genesis files (`"gas_limit": 3000000`, add `"block_interval": 5`): `devnet/genesis.json`,
  `devnet/ab-harness/devnet-genesis.json`, `testnet/genesis-weighted-base.json`,
  `testnet/genesis-weighted-base-setup2b.json`, `testnet/p3-testnet/devnet-ladder/genesis-10mkt.json`. The gitignored
  generated files (`testnet/genesis-weighted-full.json`, `devnet/wsl/genesis-3val.json`) are regenerated by their
  scripts, not edited.
  CI (`devnet/uniswap/make-ci-genesis.py`, after it copies `devnet/genesis.json`):
  ```python
      # Item 7 step 1: the Router deploy needs ~4.2M gas, above the 3M default, until step 2's big blocks.
      g["evm"]["gas_limit"] = 10_000_000
  ```
  and update the comment at `ci/github-ci.yml:40-43`.
- **Verify:** `$L cargo test $P -p torus-genesis evm_block_interval shipped_genesis_files` (run the two filters
  separately if cargo rejects two)
- **Depends on:** E1

### E4: genesis parent header from the genesis values
- **Test first:**
  - `torus-bridge/src/proposer.rs` `mod tests`, `genesis_parent_header_carries_the_genesis_evm_values`:
    `genesis_parent_header(3_000_000, 5)` has height 0, a zero parent hash, gas limit 3M, N 5 and base fee 1 gwei.
  - app.rs, `first_block_uses_the_configured_genesis_header`: with `config.evm_gas_limit = 3_000_000` and
    `config.evm_block_interval = 5`, `build_proposal(app.last_header.clone())` produces 3M / 5 in
    `pending_proposals[&1]`, and the pre-vote check under a genesis justify returns `Held`; the same block re-built on
    `genesis_parent_header(30_000_000, 5)` is `Invalid`.
  - app.rs, `parent_link_violation_uses_the_configured_genesis`: at height 1, a parent hash taken from a 30M genesis is
    a violation when the configured genesis is 3M.
- **Implementation:**
  ```rust
  // proposer.rs:189
  pub fn genesis_parent_header(evm_gas_limit: u64, evm_block_interval: u64) -> TorusBlockHeader {
      TorusBlockHeader {
          // ... unchanged fields ...
          evm_gas_limit,
          evm_block_interval,
          base_fee_per_gas: 1_000_000_000,
          // ...
      }
  }
  // app.rs try_new (before the ExecutionContext literal ~5589)
  let genesis_header =
      torus_bridge::genesis_parent_header(config.evm_gas_limit, config.evm_block_interval);
  // ExecutionContext literal (~5589): genesis_header: genesis_header.clone(),
  // TorusApp literal (~5777):         genesis_header,
  // both structs: pub(crate) genesis_header: TorusBlockHeader,
  ```
  Callers (all must use the same header, or block 1 fails to link):
  - `replay_gap` (~1851) gets a `genesis: TorusBlockHeader` parameter: `let mut last_good = genesis;`.
    `replay_committed` (~6144) passes `exec_ctx.genesis_header.clone()`.
  - `detect_parent_link_violation` (~1947) gets `genesis: &TorusBlockHeader`:
    `alloy_primitives::keccak256(genesis.canonical_header_bytes())`. Its caller (~2952) passes `&self.genesis_header`.
  - `check_parent_link` (~2071) gets `genesis: &TorusBlockHeader` after `justify`:
    `let parent = if justify.is_genesis_pc() { genesis.clone() } else { /* as today */ };`. Callers 7061 and 7315
    pass `&self.genesis_header`.
  - `parent_block_ms` (~2002) reads only the genesis timestamp: `torus_types::eip712::block_timestamp_ms(0)`.
  - The test helper `make_exec_ctx` (~13442, literal ~13454) sets `genesis_header` the same way.
  - Test calls: `replay_gap` (3), `detect_parent_link_violation` (10), `check_parent_link` (3);
    `genesis_parent_header()` at app.rs 9967, 10176, 14747, 20798, 23351 and `bridge_tests.rs` 154, 281 pass
    `(DEFAULT_EVM_GAS_LIMIT, DEFAULT_EVM_BLOCK_INTERVAL)`.
  - The remaining 30M defaults follow the genesis value: `make_block` (app.rs ~9984) and `stub_on` (~6108) use
    `torus_types::DEFAULT_EVM_GAS_LIMIT` and the new field; `torus-mempool/src/lib.rs:122` uses
    `DEFAULT_EVM_GAS_LIMIT`; `proposer.rs:83-87` becomes `let gas_limit = parent.evm_gas_limit;`.
- **Verify:** `$L cargo test $P genesis_parent_header first_block_uses_the_configured parent_link_violation`
  (separately if needed), then `$L cargo test $P -p torus-consensus`
- **Depends on:** E2, E3

### E5: the epoch plan carries the EVM values
- **Test first** (`crates/torus-economics/src/epoch_plan.rs` tests, L = 4 like
  `plan_is_unchanged_once_installed_and_nothing_moved` ~386):
  - `plan_carries_governance_evm_params`: with the genesis set recorded and `CF_FEE_CONFIG` rows `"5000000"` / `"10"`,
    `execute_planned_rotation(.., 4, 4, 4)` yields `plan(8).evm == PlanEvm { gas_limit: Some(5_000_000),
    block_interval: Some(10) }` and `!changed`.
  - `plan_without_or_with_invalid_evm_params_keeps_the_chain_values`: no rows, `"999999"`, `"10000001"`, `"1"`, `"101"`
    and `"oops"` each yield `PlanEvm::default()`.
- **Implementation** (`epoch_plan.rs`):
  ```rust
  /// Item 7 step 1: EVM header values from `boundary` on (`None` = keep the parent's).
  #[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
  pub struct PlanEvm {
      pub gas_limit: Option<u64>,
      pub block_interval: Option<u64>,
  }
  // EpochRotationPlan, new last field:
      pub evm: PlanEvm,
  // plan_rotation: the 3 literals (~195, ~207, ~224) add `evm: PlanEvm::default(),`
  // execute_planned_rotation, replacing ~289-295:
          let mut next = Self::plan_rotation(
              staking,
              max_validators,
              height.saturating_add(epoch_length),
              epoch_length,
          )?;
          let gov = |key: &str, range: &std::ops::RangeInclusive<u64>| -> Result<Option<u64>> {
              let stored = staking.state().get_cf_raw(CF_FEE_CONFIG, key.as_bytes())?;
              Ok(torus_types::gov_u64_in(stored.as_deref(), range))
          };
          next.evm = PlanEvm {
              gas_limit: gov(torus_types::EVM_BLOCK_GAS_LIMIT_PARAM, &torus_types::EVM_BLOCK_GAS_LIMIT_RANGE)?,
              block_interval: gov(torus_types::EVM_BLOCK_INTERVAL_PARAM, &torus_types::EVM_BLOCK_INTERVAL_RANGE)?,
          };
          staking.put_epoch_rotation_plan(&next)?;
  ```
  Import `torus_state::cf::{CF_CONSENSUS_META, CF_FEE_CONFIG}` and re-export `PlanEvm` beside the existing exports
  (`torus-economics/src/lib.rs:25`). A read fault returns `Err`, which reaches `latch_local_fault`
  (`native_executor.rs:12570`) and fail-stops, as for the validator set. One plan row keeps one write, one read and one
  prune per boundary; the row is hashed (`EPOCH_VSET_PREFIX`), and its new bytes are deterministic.
- **Verify:** `$L cargo test $P plan_carries_governance_evm_params plan_without_or_with_invalid_evm_params`
- **Depends on:** E1

### E6: governance accepts the two keys
- **Test first** (`crates/torus-economics/tests/governance_tests.rs`, copy `execute_price_band_parameter_change` ~648
  and the `pass_param` helper ~421): `evm_block_param_changes_are_range_checked_and_execute_into_fee_config`. Bad
  values (`"999999"`, `"10000001"`, `"1"`, `"101"`, `"x"`) give `InvalidParameterValue` at submission; `"3000000"` and
  `"5"` pass and store exactly those bytes in `CF_FEE_CONFIG`.
- **Implementation** (`governance.rs`, before `_ =>` at ~764):
  ```rust
              // Item 7 step 1: copied into the next epoch plan (torus_economics::epoch_plan).
              torus_types::EVM_BLOCK_GAS_LIMIT_PARAM | torus_types::EVM_BLOCK_INTERVAL_PARAM => {
                  let range = if key == torus_types::EVM_BLOCK_GAS_LIMIT_PARAM {
                      torus_types::EVM_BLOCK_GAS_LIMIT_RANGE
                  } else {
                      torus_types::EVM_BLOCK_INTERVAL_RANGE
                  };
                  let v: u64 = value.parse().map_err(|_| EconomicsError::InvalidParameterValue {
                      key: key.to_string(),
                      reason: "must be a valid u64".to_string(),
                  })?;
                  if !range.contains(&v) {
                      return Err(EconomicsError::InvalidParameterValue {
                          key: key.to_string(),
                          reason: format!("must be between {} and {}", range.start(), range.end()),
                      });
                  }
              }
  ```
  Storage needs no change: keys outside `GovernanceParams` are written as their own `CF_FEE_CONFIG` row by
  `stage_payload` (~1235-1240); the check runs at submission (~816) and execution (~1227).
- **Verify:** `$L cargo test $P evm_block_param_changes_are_range_checked`
- **Depends on:** E1

### E7: the boundary rule (vote and insertion)
- **Test first** (app.rs, beside `evm_gas_limit_and_base_fee_must_equal_the_parents` ~20762, helpers `tip_2000`
  ~20560 and `epoch_fixture` ~20425):
  - `boundary_header_must_equal_the_plans_evm_params`: `check_parent_link` with a boundary closure returning
    `Ok(Some(PlanEvm { Some(5_000_000), Some(10) }))`: a 5M / 10 header is `Held`; the parent's values and each
    one-field mix are `Invalid`; with only `gas_limit` set, N must equal the parent's.
  - `off_boundary_header_must_equal_the_parents_evm_params`: extends 20762 with N ± 1 (pre-vote and link check);
    `app.boundary_evm(5) == Ok(None)`.
  - `boundary_without_a_plan_keeps_the_parents_evm_params`: L = 4, `app.boundary_evm(4) == Ok(None)`; block 4 with the
    parent's values is `Held`, changed values `Invalid`.
  - `boundary_plan_not_ready_is_missing_before_the_vote_and_at_insertion`: a closure `Err` gives `Missing`; with
    applied height 3, the pre-vote of block 8 is `Missing`. Plus `r02_applied_height_read_fault_boundary_evm_not_ready`
    (copy of ~23779).
  - `pruned_boundary_plan_is_not_read_as_no_change`: L = 4, plan(8) absent while applied >= 12, so
    `boundary_evm(8)` returns `Err(EpochPlanNotReady)`, not `Ok(None)`.
- **Implementation** (app.rs):
  ```rust
  /// Bug (b) guard + read, moved verbatim out of `epoch_validator_set_updates` (~6526-6553).
  fn epoch_plan(&self, height: u64) -> Result<Option<EpochRotationPlan>, EpochPlanNotReady> {
      /* applied-height guard exactly as today */
      self.staking.epoch_rotation_plan(height).map_err(|e| {
          tracing::error!(%e, height, "failed to read the epoch rotation plan");
          EpochPlanNotReady
      })
  }

  /// Item 7 step 1: the plan's EVM values for block `height` (`None` off a boundary or at the
  /// first boundary, which has no plan).
  fn boundary_evm(&self, height: u64) -> Result<Option<PlanEvm>, EpochPlanNotReady> {
      if !EpochManager::is_epoch_boundary(height, self.epoch_length) {
          return Ok(None);
      }
      match self.epoch_plan(height)? {
          Some(plan) => Ok(Some(plan.evm)),
          // From 2L on every boundary has a plan (written by the previous boundary's execution) until
          // H + L executes and prunes it. None there means pruned: never read that as "no change".
          None if height >= self.epoch_length.saturating_mul(2) => Err(EpochPlanNotReady),
          None => Ok(None),
      }
  }
  // epoch_validator_set_updates: lines ~6526-6553 become `let plan = self.epoch_plan(height)?;`
  ```
  `check_parent_link` gets the boundary closure, and its EVM check moves after the link check, so a header with a
  bogus height is `Invalid` and never waits on a plan:
  ```rust
  fn check_parent_link(
      header: &TorusBlockHeader,
      justify: &PhaseCertificate,
      genesis: &TorusBlockHeader,
      parent: impl FnOnce(&CryptoHash) -> ParentHeader,
      boundary: impl FnOnce(u64) -> Result<Option<PlanEvm>, EpochPlanNotReady>,
  ) -> BlockDataCheck {
      // parent resolution, `expected`, timestamp rule: unchanged (~2070-2091); the old EVM block (~2092-2111) goes.
      if header.height != parent.height + 1 || header.parent_hash != expected {
          tracing::warn!(/* ~2115-2120 unchanged */);
          return BlockDataCheck::Invalid;
      }
      // Off a boundary: the parent's values. At a boundary: the plan's (unset or no plan: the parent's).
      let plan = match boundary(header.height) {
          Ok(plan) => plan.unwrap_or_default(),
          Err(EpochPlanNotReady) => return BlockDataCheck::Missing,
      };
      let gas_limit = plan.gas_limit.unwrap_or(parent.evm_gas_limit);
      let block_interval = plan.block_interval.unwrap_or(parent.evm_block_interval);
      if header.evm_gas_limit != gas_limit
          || header.evm_block_interval != block_interval
          || header.base_fee_per_gas != parent.base_fee_per_gas
      {
          tracing::warn!(
              height = header.height,
              evm_gas_limit = header.evm_gas_limit,
              gas_limit,
              evm_block_interval = header.evm_block_interval,
              block_interval,
              "REJECTED -- EVM gas limit, interval or base fee differs from the rule value"
          );
          return BlockDataCheck::Invalid;
      }
      BlockDataCheck::Held
  }
  ```
  Call sites: vote (~7061) and insertion (~7315; sync reaches it through `validate_block_for_sync` ~7361) pass
  `&self.genesis_header, parent, |h| self.boundary_evm(h)`; `Missing` already maps to `MissingData` at insertion
  (~7322). Tests at ~20606, ~20647, ~20785 pass `|_| Ok(None)`. `detect_parent_link_violation` compares only the
  parent hash and needs no change.
- **Verify:** `$L cargo test $P boundary_ off_boundary_header pruned_boundary_plan evm_gas_limit_and_base_fee`
  (separately if needed)
- **Depends on:** E4, E5

### E8: the proposer uses the plan's values and drains EVM only on cadence
- **Test first** (app.rs):
  - `off_cadence_proposal_has_no_evm_txs_and_leaves_them_pooled`: 2 txs pooled; building on `make_block(3).header`
    gives `pending_proposals[&4]` with 0 EVM txs, `evm_tx_count == 0` and `evm_pool_size() == 2`; building on
    `tip_2000().header` (height 4, child 5) then takes both and empties the pool.
  - `governance_evm_params_reach_the_header_at_the_next_boundary_plus_l` (L = 4; rows written before block 4
    executes): `boundary_evm(8)` carries them; `build_proposal(parent7)` puts 5M / 10 in the header, which passes the
    link check against parent 7; a 30M header at 8 is `Invalid`; block 9 must carry 5M / 10. Variant: rows written in
    block 5 give a default plan(8) and plan(12) carries them.
- **Implementation** (`build_proposal`, replacing ~7110-7113):
  ```rust
          // Item 7 step 1: the new header's EVM values: the parent's, at a boundary the plan's.
          let Ok(plan_evm) = self.boundary_evm(next_height) else {
              return refused_proposal();
          };
          let plan_evm = plan_evm.unwrap_or_default();
          let evm_gas_limit = plan_evm.gas_limit.unwrap_or(parent_header.evm_gas_limit);
          let evm_block_interval = plan_evm.block_interval.unwrap_or(parent_header.evm_block_interval);
          // Cadence: EVM txs only at height % N == 0. A 0 limit makes select_block_payload skip
          // the destructive drain, so off-cadence txs stay pooled (N = 0 carries none).
          let gas_limit = if next_height.checked_rem(evm_block_interval) == Some(0) {
              evm_gas_limit
          } else {
              0
          };
  ```
  Header literal (~7167): `evm_gas_limit, evm_block_interval,` from these values. In `select_block_payload` (~7716):
  ```rust
          // Item 7 step 1: 0 = off cadence (or a 0 header limit): no destructive drain.
          let evm = if gas_limit == 0 {
              Vec::new()
          } else {
              mempool.drain_evm(gas_limit, parent_state_root)
          };
  ```
  The signature stays, so the 8 tests that pass 30M (8699, 8710, 8757, 8762, 11949, 11959, 13142, 13150) keep working.
- **Verify:** `$L cargo test $P off_cadence_proposal governance_evm_params_reach_the_header`
- **Depends on:** E7

### E9: cadence, count and byte caps before the vote
- **Test first** (app.rs `mod crash_recovery_tests`, `pre_vote_as` with full and compact datums):
  - `evm_txs_off_the_cadence_are_refused_before_the_vote`: EVM at height 4 (child of `make_block(3)`) is `Invalid`,
    without EVM `Held`; at height 5 (child of `tip_2000`) `Held`; a header N of 0 gives `Invalid` without a panic;
    `validate_datum` of the off-cadence block is still `Valid` (insertion does not enforce the cadence).
  - `evm_tx_count_over_limit_per_21000_is_refused_before_the_vote`: at height 5, `limit / 21_000` copies of
    `vec![0xff; 8]` are `Held`, one more `Invalid`.
  - `evm_tx_bytes_over_256_kib_are_refused_before_the_vote`: one tx of `MAX_EVM_BLOCK_TX_BYTES` bytes is `Held`, one
    byte more `Invalid`, two halves plus one byte `Invalid`.
  - Update `invalid_actions_are_skipped_and_recorded_identically_on_three_replicas` (~13849): move its fixture to heights
    4 and 5 (`parent3 = make_block(3, ..)`, `block1 = make_block(4, ..)`, `block2 = make_block(5, ..)`, execute heights
    1-3 first, `action_status` heights 1 -> 4 and 2 -> 5, the parent-lookup closure resolves `hs0 -> parent3`).
  - Update `honest_proposal_with_evm_txs_passes_the_pre_vote_check` (~20874): four 1M-gas txs, build on `tip_2000()`,
    assert `produced.evm_transactions.len() as u64 == limit / 1_000_000` (3M fits three).
- **Implementation** (`evm_body_fits_header`, after the count check ~2143):
  ```rust
      if evm_txs.is_empty() {
          return true;
      }
      // Item 7 step 1: only on the header's cadence, at most limit / 21,000 txs (undecodable txs
      // declare no gas, so the gas rule alone does not bound them), at most 256 KiB of tx bytes.
      let max_txs = header.evm_gas_limit / torus_types::EVM_MIN_TX_GAS;
      let bytes: usize = evm_txs.iter().map(Vec::len).sum();
      if header.height.checked_rem(header.evm_block_interval) != Some(0)
          || evm_txs.len() as u64 > max_txs
          || bytes > torus_types::MAX_EVM_BLOCK_TX_BYTES
      {
          tracing::warn!(
              height = header.height,
              n = header.evm_block_interval,
              txs = evm_txs.len(),
              max_txs,
              bytes,
              "not voting -- EVM txs off the cadence or over the count/byte cap"
          );
          return false;
      }
  ```
  Pre-vote only, like step 0: a certified block had >= f + 1 honest votes. A dishonest header N cannot sneak EVM off
  cadence: `check_parent_link` (E7) runs before the same vote and pins N.
- **Verify:** `$L cargo test $P before_the_vote invalid_actions_are_skipped honest_proposal_with_evm_txs`
- **Depends on:** E2, E7

### E10: mempool admission follows the committed header and caps tx size
- **Test first:**
  - torus-mempool lib.rs (helpers `setup`, `intrinsic_case`, `eip1559_with`): `admission_caps_gas_at_the_committed_header_limit`
    (after `set_evm_gas_limit(3_000_000)`, gas 3M + 1 gives `GasLimitExceeded`, 3M is accepted) and
    `admission_rejects_tx_above_256_kib` (256 KiB of zero calldata at 1.2M gas gives `TxTooLarge`, 200 KiB passes).
  - app.rs `committed_header_sets_mempool_gas_cap`: dispatching a committed block whose header limit is X makes a tx at
    X + 1 fail admission.
- **Implementation** (torus-mempool):
  ```rust
  // Mempool fields
      current_evm_gas_limit: std::sync::atomic::AtomicU64,
  // init (~252)
      current_evm_gas_limit: std::sync::atomic::AtomicU64::new(config.block_gas_limit),

  /// Item 7 step 1: the committed header's EVM gas limit caps admission (it can change at an
  /// epoch boundary by governance).
  pub fn set_evm_gas_limit(&self, gas_limit: u64) {
      self.current_evm_gas_limit.store(gas_limit, std::sync::atomic::Ordering::Relaxed);
  }
  // add_evm_tx (~601-616), first lines
      if raw_rlp.len() > torus_types::MAX_EVM_BLOCK_TX_BYTES {
          return Err(MempoolError::TxTooLarge {
              size: raw_rlp.len(),
              max: torus_types::MAX_EVM_BLOCK_TX_BYTES,
          });
      }
      let max_tx_gas = self
          .current_evm_gas_limit
          .load(std::sync::atomic::Ordering::Relaxed)
          .min(self.config.evm_block_gas_budget);
  ```
  New `MempoolError::TxTooLarge { size: usize, max: usize }` with Display text styled like `InitCodeTooLarge`
  (error.rs ~71, ~150). In app.rs `dispatch_to_exec` (~8024-8026), beside `set_base_fee`:
  ```rust
          if let Some(ref mempool) = self.mempool {
              mempool.set_base_fee(torus_block.header.base_fee_per_gas);
              mempool.set_evm_gas_limit(torus_block.header.evm_gas_limit);
          }
  ```
  `add_evm_tx` is the only way into the EVM pool (RPC eth.rs:803, network forward main.rs:876, `reinsert_evm`), so both
  caps cover every path.
- **Verify:** `$L cargo test $P admission_caps_gas admission_rejects_tx_above committed_header_sets_mempool`
- **Depends on:** E1, E4

### E11: EVM txs of a lost proposal go back to the pool
- **Test first** (app.rs; recording exec channel as in `app_with_recording_exec` ~16502):
  - `lost_proposal_evm_txs_return_to_the_pool_when_another_block_commits`: 3 txs, build at height 5,
    `app.exec_next_height = Some(5)`, commit `committed_compact_block(&evm_child_of(&tip_2000(), vec![txs[0].clone()]))`;
    `mempool.drain_evm(u64::MAX, B256::ZERO)` returns exactly `{txs[1], txs[2]}` (today the pool stays empty).
  - `own_committed_proposal_reinserts_nothing`.
  - `lost_proposal_overwritten_by_another_validated_block_still_reinserts` (call `finish_validate` with another block
    at height 5 first: `pending_proposals` is overwritten there, so it cannot be the record).
- **Implementation** (app.rs):
  ```rust
  // TorusApp field (near ~4741), init `BTreeMap::new()` (~5793)
      /// Item 7 step 1: EVM txs drained into our own proposals, by height, until a block commits
      /// there (drain_evm is destructive and pending_proposals entries get overwritten).
      own_evm_proposed: std::collections::BTreeMap<u64, Vec<Vec<u8>>>,
  // build_proposal, before the block moves into PendingProposal (~7200)
          if !block.evm_transactions.is_empty() {
              self.own_evm_proposed
                  .entry(height)
                  .or_default()
                  .extend(block.evm_transactions.iter().cloned());
          }
  // on_committed_block, after `let height = header.height;` (~7444)
          let committed_evm: &[Vec<u8>] = match &source {
              ExecSource::Ready(b) => &b.evm_transactions,
              ExecSource::Compact(c) => &c.evm_transactions,
              ExecSource::Durable(_) => &[],
          };
          self.reinsert_lost_evm(height, committed_evm);

  /// One block commits per height: our proposals at `height` that lost go back to the pool, minus
  /// what the committed block carries. Lower heights are settled (the commit feed is ordered).
  fn reinsert_lost_evm(&mut self, height: u64, committed: &[Vec<u8>]) {
      let lost: Vec<Vec<u8>> = self
          .own_evm_proposed
          .remove(&height)
          .unwrap_or_default()
          .into_iter()
          .filter(|tx| !committed.contains(tx))
          .collect();
      self.own_evm_proposed.retain(|&h, _| h > height);
      if let (Some(mempool), false) = (self.mempool.as_ref(), lost.is_empty()) {
          tracing::info!(height, txs = lost.len(), "EVM txs of our lost proposal back to the pool");
          mempool.reinsert_evm(lost);
      }
  }
  ```
  `reinsert_evm` re-validates through `add_evm_tx` (executed nonces drop as `NonceTooLow`, resent txs as
  `DuplicateTx`), and each sender's queue is nonce-ordered, so order survives. Rare gap: a tx committed in a later,
  not yet executed block can be re-pooled and is then skipped at execution (same class as "prune uses executed
  nonces").
- **Verify:** `$L cargo test $P lost_proposal own_committed_proposal_reinserts_nothing`
- **Depends on:** E8

### E12: forward EVM txs to the leader of the next EVM height
- **Test first:**
  - app.rs `mod leader_state_tests` (~9793, `make_set(3)`, `plain_iwrr`), `next_evm_leader_is_the_leader_of_the_next_evm_height`
    with N = 5: `observe_start_view(40)` + `observe_proposal(40, 21)` gives `plain_iwrr(44)`; then
    `observe_start_view(43)` gives `plain_iwrr(46)`; `observe_proposal(50, 24)` gives `plain_iwrr(51)`;
    `observe_proposal(51, 25)` gives `plain_iwrr(56)`.
  - torus-rpc lib.rs (model: the forwarding test ~3334), `evm_tx_forwarded_to_the_next_evm_height_leader`:
    `set_leader_forwarding(.., || Some(leader), ..)` plus `set_evm_leader_fn(|| Some(evm_leader))`; the forwarded key
    is `evm_leader`. The 3 existing `set_leader_forwarding` tests stay unchanged (fallback).
- **Implementation:**
  - hotstuff_rs: `ReceiveProposalHeaderEvent` (`src/events.rs:215-220`) gets `pub height: BlockHeight,`; the emit site
    (`src/hotstuff/implementation.rs:2224`) sets `height: header.height,`.
  - `LeaderState` (app.rs ~866-941):
    ```rust
        tip: std::sync::RwLock<(u64, u64)>,               // (view, height) of the newest proposal
        evm_block_interval: std::sync::atomic::AtomicU64, // the committed header's N

        pub fn observe_proposal(&self, view: u64, height: u64) {
            let mut tip = self.tip.write().unwrap();
            if view >= tip.0 {
                *tip = (view, height);
            }
        }

        fn set_evm_block_interval(&self, n: u64) {
            self.evm_block_interval.store(n, std::sync::atomic::Ordering::Relaxed);
        }

        /// Leader of the next EVM height: max(current view, tip view + 1) proposes tip + 1, then
        /// one block per view (plain IWRR, like the fallback above; reputation weighting is off).
        pub fn next_evm_leader(&self) -> Option<VerifyingKey> {
            let n = self.evm_block_interval.load(std::sync::atomic::Ordering::Relaxed);
            if n == 0 {
                return self.current_leader();
            }
            let (tip_view, tip_height) = *self.tip.read().unwrap();
            let h_e = (tip_height / n + 1) * n;
            let view_e = self.current_view().max(tip_view + 1) + (h_e - (tip_height + 1));
            let vs = self.validators.read().unwrap();
            if vs.is_empty() {
                return None;
            }
            Some(hotstuff_rs::pacemaker::select_leader(ViewNumber::new(view_e), &vs))
        }
    ```
    `set_evm_block_interval(header.evm_block_interval)` at ~5775 (boot) and ~7489 (commit).
  - main.rs: `on_propose` calls `observe_proposal(ev.proposal.view.int(), ev.proposal.block.height.int())`;
    `on_receive_proposal_header` calls `observe_proposal(ev.view.int(), ev.height.int())`; then
    `rpc_server.set_evm_leader_fn(Arc::new(move || ls.next_evm_leader().map(|vk| vk.to_bytes())))`.
  - torus-rpc: field `evm_leader_vk_fn: Option<Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync>>` (init `None` near
    lib.rs:366), `pub fn set_evm_leader_fn(&mut self, f: Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync>)`, and in
    `forward_evm_to_leader` (torus.rs:946-947) match on
    `(self.evm_leader_vk_fn.as_ref().or(self.leader_vk_fn.as_ref()), self.own_vk.as_ref(), self.forward_evm_tx.as_ref())`.
  N comes from the committed header, so it is briefly stale after a boundary change (the tx then waits one EVM
  height longer). A validator RPC node already keeps its own copy (eth.rs:801-809), as today.
- **Verify:** `$L cargo test $P next_evm_leader evm_tx_forwarded_to_the_next_evm_height_leader set_leader_forwarding`
- **Depends on:** E2

### E13: RPC gas defaults follow the header limit (independent of E1-E12)
- **Test first:**
  - `torus-rpc/tests/eth_compliance_tests.rs`: give `store_head_with_base_fee` (~360, hardcodes 30M at ~371) a
    gas-limit parameter; `bare_call_and_estimate_work_under_a_3m_header` (3M header, 1 gwei: bare `eth_call` is `Ok`,
    `eth_estimateGas` returns `"0x5208"`) and `eth_call_gas_above_the_header_limit_is_capped` (`"gas": "0x1c9c380"` is
    `Ok`).
  - eth.rs `call_typing_tests`: `call_gas_defaults_and_caps_at_the_block_limit` (`.gas_limit == 3_000_000` with no gas
    and with 30M).
- **Implementation** (eth.rs):
  ```rust
  // ~319
  fn build_call_tx_env(call: &CallRequest, chain_id: u64, block_gas_limit: u64) -> Result<TxEnv, RpcError> {
  // ~337
      let gas_limit = match &call.gas {
          Some(g) => parse_u64(g)?.min(block_gas_limit),
          None => block_gas_limit,
      };
  // callers ~1074, ~1124, ~1155:  build_call_tx_env(&tx, self.chain_id, block_env.gas_limit)
  // ~1123:                        let mut hi: u64 = block_env.gas_limit;
  // test helper ~1429:            build_call_tx_env(&req(v), 7, 30_000_000)
  ```
  Before block 1 (no header) both paths keep `BlockEnvCfg::default()` (30M in both the call and the block env), which
  stays consistent.
- **Verify:** `$L cargo test $P bare_call_and_estimate_work eth_call_gas_above call_gas_defaults_and_caps`
- **Depends on:** none

### E14: remove the getOrderBook selector (independent of E1-E12)
- **Test first:**
  - `crates/torus-evm/tests/evm_tests.rs`, `removed_get_order_book_reverts_for_exactly_the_base` (same body as
    `removed_get_open_orders_reverts_for_exactly_the_base` ~618, with
    `keccak256("getOrderBook(bytes32)")[..4] ++ U256::from(1).to_be_bytes::<32>()`; asserts `!r.success` and
    `r.gas_used == intrinsic + GAS_PRECOMPILE_READ`). Today it succeeds (16,560 gas), so it fails first.
  - `precompile_tests.rs:178` becomes `order_book_reader_get_order_book_is_an_unknown_selector`
    (`matches!(result, Err(CoreError::UnknownSelector(s)) if s == sel)`, the fc1fb25a pattern).
- **Implementation** (`torus-core/src/precompiles.rs`):
  ```rust
  fn order_book_reader(input: &[u8], state_db: &impl StateBackend, now: u64) -> Result<Vec<u8>, CoreError> {
      let sel = abi::selector(input)?;
      if sel == selector_for("getPosition(address,bytes32)") {
          let trader = abi::decode_address(&abi::word(input, 0)?);
          let market_id = abi::decode_market_id(&abi::word(input, 1)?);
          read_position(state_db, &trader, market_id, now)
      } else {
          Err(CoreError::UnknownSelector(sel))
      }
  }
  // ~603
  ADDR_ORDER_BOOK_READER => read(order_book_reader(input, state_db, current_timestamp), meter),
  ```
  Delete `READER_MAX_LEVELS_PER_SIDE` (~54-57), `ReadMeter::require_words` (~138-144), `PriceQtyLevels` (~640-641),
  `read_order_book`, `classic_levels`, `book_levels` (~643-772). Keep `scan_prefix_metered`, `charge_words`,
  `encode_arrays_response`, `OrderBookSnapshot`, `PriceLevel`, `write_order_book_snapshot`, `CoreError::BookLayout` /
  `BookCorrupt`.
  Tests: `precompile_tests.rs` :1125 switches to `getPosition`; `book_read_modes_tests.rs` drops :496, :504, :525,
  `call_get_order_book` (:169), `decode_u128_arrays` (:157), `R02_CLASSIC_REVERT_TEXT` (:550), docs at :15 and :409;
  `precompile_work_bound_tests.rs` drops :279, :305, :367, :404, :623 (or ports it to getMarkets), :650, :698, :741,
  :788, :829, :890, edits :319 (drop `order_book` from `run`, :348-357) and :438 (:462-468, :485-486), drops helpers
  `order_book` (:116), `arrays` (:121), `deletes_skipped` (:138), `churn_levels` (:760), keeps `seed_book`, `seed_sides`,
  `place`, `levels`, `raw`, fixes docs :16-21; `r02_book_corrupt_tests.rs` drops :264, docs :5 and :260;
  `cross_vm_read.rs` drops :39, :256 and the `OrderBookSnapshot, PriceLevel` import (:11);
  `ubench_read_precompile_gas.rs` drops `scenario_book` (:927), the tomb helpers and `scenario_tombstones`
  (:1094-1290), `scenario_churn` (:1292), :1382-1658, `place`, `exec_ctx`, `fund`, `execute_all` (:851-925),
  `COLD_BOOK_MARKET` (:118-119), their calls at :1695-1734 and the related env knobs.
  **Tombstone compaction stays** (owner point 1): rewrite its comments (`cf.rs:94-106`, `db.rs:919-968`) to say it
  serves the RPC `torus_getOrderBook` row scans (`scan.rs:745-768`), not the precompile.
- **Verify:** `$L cargo test $P removed_get_order_book order_book_reader_ book_read_modes precompile_work_bound`
- **Depends on:** none

### E15: docs and the full run
- **Test first:** none new; this task is the full suite (below).
- **Implementation:** update `docs/api/read-limits.md`, `docs/perf/read-precompile-gas.md`,
  `docs/design-level-rows-authority.md` (getOrderBook removed), `docs/configuration-reference.md` (genesis
  `evm.gas_limit` 3M and `evm.block_interval`, the two governance keys with ranges, the 256 KiB cap), and the status line
  of `docs/plans/item7-evm-lanes.md`.
- **Verify:** the end-to-end block below.
- **Depends on:** E1-E14

## Verification (end-to-end)

```bash
$L cargo test $P                                        # all touched crates
CARGO_TARGET_DIR=/home/18c/.cargo-target-item7-dw RUSTFLAGS="-D warnings" $L cargo build $P --all-targets
cargo fmt --all -- --check | grep -c '^Diff in'          # must not exceed main's 3,687; never `cargo fmt --all`
$L cargo clippy $P --all-targets 2>&1 | grep -c '^warning' # must not exceed main's count; none on changed lines
ci/run-local.sh uniswap                                  # Router deploy on the 10M CI genesis
```
Then a 4-validator devnet on a fresh 3M / N = 5 genesis: an EVM transfer lands within about 5 blocks; no block at
`height % 5 != 0` carries EVM txs; `eth_estimateGas` answers; a governance change of `evm_block_interval` to 10 shows up
in the header at the expected boundary. Archy's D1 mixed-load bench then re-tunes N and the budget on this binary.

Commit boundaries (each compiles and passes): C1 = E1-E4, C2 = E5-E8, C3 = E9-E11, C4 = E12, C5 = E13, C6 = E14,
C7 = E15. E13 and E14 touch other files and can be built in parallel with E1-E12; everything else is sequential.

## Rollback

All work is on `feat/item7-step1-evm-lanes`; main is untouched until the merge. No migration exists (fresh genesis),
so a rollback is reverting the merge commit and reusing the previous genesis files. If one group misbehaves late, its
commit (C1-C7) reverts on its own: C5 and C6 are independent, C2-C4 depend on C1.
