# Implementation Plan: item 7 step 2, HL-style small and big EVM blocks

Design: `docs/plans/item7-step2-design.md` (f9ec4b2d): option A, "Same in every option" 1-7 and "Decisions (owner
s123)" 1-8. Builds on step 1 (`item7-step1-impl.md`, merged 853fed28). Code facts from a read-only pass on main
`f9ec4b2d` (s124). Line numbers are from that commit.

**Line refs will shift.** Three branches merge before this build starts (all at f9ec4b2d, work uncommitted in their
worktrees):
- `chore/evm-n11-bench-evm-load` (wt/evm-n11): `DEFAULT_EVM_BLOCK_INTERVAL` 5 -> 11, genesis files, the
  shipped-genesis test, a bench EVM load option, the deploy.sh fix. Textual conflict with B3 (same genesis files and
  test). No test in this plan relies on the default N: tests set N in the header or the config.
- `perf/epoch-boundary-fast-path` (wt/epoch-fast-path): app.rs `ExecutionContext` field (~1194), `pipeline_barrier`
  docs (~2496), `checkpoint_upkeep` `due` (~2752), the `pipelined` predicate (~3150), `try_new` (~5819, ~5911), ~260
  test lines (~27066). Every app.rs ref below past ~1194 moves by up to ~45 lines. Its interaction with F3 is in B9.
- `fix/driver-generator-watchers`: bench tooling only (the A/B below uses `make-driver.py`; no code conflict).

Re-check every line ref before each edit.

```bash
# once the three branches above are on main
git -C /home/18c/projects/Torus-hyperBFT worktree add /home/18c/projects/wt/item7-step2 -b feat/item7-step2-big-blocks main
cd /home/18c/projects/wt/item7-step2
export CARGO_TARGET_DIR=/home/18c/.cargo-target-item7s2   # never share a target dir with another worktree
L="flock -w 60 -o /home/18c/projects/wt/.cargo-heavy.lock"   # exit 1 after the wait = lock busy, report it
F="--cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar"
P="-p torus-types -p torus-genesis -p torus-bridge -p torus-consensus -p torus-economics -p torus-mempool \
   -p torus-rpc -p torus-core -p torus-evm -p hotstuff_rs -p torus-node -p torus-state -p torus-integration-tests \
   -p torus-wallet"
# one -p set for every cargo call here (other subsets re-unify features and rebuild librocksdb-sys)
```

The header, the epoch plan, `NativeAction`, the CoreWriter queue rule and the column families change: fresh genesis
(free before testnet), no migration.

## Design Decision

Owner decisions (s123): option A, M = 0 at genesis (1); big blocks start at 10-15M, governance ceiling 30M (2); bbo at
0x080e with our metering (3); CoreWriter D2, K = 10, drain cap 200 (4); fees F3 (5); `TORUS_EVM_BLOCK_GAS_BUDGET` loses
its 5M default, the small lane keeps 1M..=10M default 3M (6); routing fix: re-send, upcoming N, in-flight filter, both
lanes (7); big-block byte cap 768 KiB (8). Choices the design left open are marked **(plan)**; each is in the open
questions.

- **Header:** three new fields in the canonical bytes: `evm_big_block_interval` (M), `evm_big_gas_limit`,
  `big_base_fee_per_gas`. M > 0 and `height % M == 0` = a big EVM block. EVM txs ride a height when `height % N == 0`
  or it is a big height (a big height also drains the small lane, design item 2). Per height: gas cap = small limit
  (+ big limit at a big height), byte cap 256 KiB (768 KiB at a big height), tx count <= gas cap / 21,000.
- **Boundary rule:** step 1's rule for all six EVM values (small limit, N, M, big limit, both base fees): the
  parent's off a boundary, the plan's (unset: the parent's) at a boundary, before the vote and at insertion.
  `PlanEvm` gains four fields; `PlanEvm::resolve(parent)` serves vote, insertion and proposer.
- **Genesis and governance:** genesis `evm.big_block_interval` (absent = 0) and `evm.big_gas_limit` (absent = 10M,
  plan). Governance keys `evm_big_block_interval` (0 = off, or 30..=10,000, plan), `evm_big_block_gas_limit`
  (1M..=30M; the ceiling is decision 2), `core_writer_order_delay` (1..=100, default 10). M and the big limit go
  through the epoch plan like N; K is read when a CoreWriter row is queued (execution only, no vote rule).
- **Lanes (design item 1):** `NativeAction::EvmUserModify { using_big_blocks }`: bincode index 30, canonical tag 30,
  EIP-712 `EvmUserModify(bool usingBigBlocks,uint64 nonce)`, not a trading action (no session key signs it). Row
  `"lan" ‖ address` -> `[1]` in `CF_NATIVE_ORACLE` **(plan)**: hashed (running hash id 5, native root tag 3) and
  answered by the ingress read view, which serves only `INGRESS_CFS` (`read_view.rs:58`; `CF_CONSENSUS_META` is not
  one, so a row there would reach admission only after a checkpoint, up to 15 s). Same reason `SetOracleSigner`'s
  `"sgn"` row lives in this CF (`cf.rs:150`). Effective lane = big iff the row exists and the header's M > 0: in
  admission (last committed header) and in execution (the executing block's header). Lanes stay out of the vote.
- **Fees per lane (F3):** execution charges each tx its lane's base fee (per-tx `Context::modify_block`, present in
  the pinned revm-context 15.0.0) and splits gas used by lane **(plan: lane from the sender's row, not the tx's
  position in the block)**. The native phase adds the block's `(small, big)` gas to one hashed row,
  `CF_CONSENSUS_META` `evm_epoch_gas` (new `META_CONSENSUS_PREFIXES` entry). Execution of boundary H reads the row and
  the boundary's parent header (the values in force over `[H - L, H)`), computes each lane's next base fee with
  `calc_next_block_base_fee`, the epoch counted as one block whose gas limit is the lane's limit times its EVM heights
  in the window (small lane: every EVM height, big ones included; big lane: big heights), writes both into
  plan(H + L), and deletes the row before H's own gas is added (H's gas starts the next epoch). Floor
  `EVM_MIN_BASE_FEE` = 1 gwei **(plan)**. `calc_next_block_base_fee` moves to torus-types (torus-economics depends
  only on torus-types and torus-state); torus-evm re-exports it.
- **Pools (design items 1-3, decision 6):** one lock over two `EvmPool`s (`EvmLanes`). Admission reads the lane from
  the ingress view, refuses a tx while the sender has pooled txs in the other lane (`EvmLaneSwitchPending`), and caps
  per lane (small: header limit, `TORUS_EVM_BLOCK_GAS_BUDGET` if set, 256 KiB, small fee; big: big limit, 768 KiB,
  big fee). The budget has no default and never caps the big lane. A big height drains the small lane (small limit,
  256 KiB), then the big lane (big limit, 768 KiB minus the small bytes). M back to 0 moves the big pool to the small
  lane.
- **Routing (decision 7, design item 4):** `LeaderState::next_evm_targets(lane)`: the next EVM height for a small-lane
  tx (a big height counts), the next two big heights for a big-lane tx; heights from the next boundary on use that
  boundary's N and M from its epoch plan once this node can read it (upcoming N). The RPC forwards to those leaders,
  the mempool tracks `(hash, target)`. On each commit: the in-flight filter drops pooled EVM txs the committed block
  carries (by hash), refuses their copies for 256 blocks (`DuplicateTx`) and keeps the sender's next nonce
  (`pending_nonce`); then each tracked tx whose target has committed and that is still pooled is re-sent to the next
  predicted leader(s), at most 8 times.
- **CoreWriter (decision 4):** at enqueue, order kinds (`PlaceOrder`, `CancelOrder`, `CancelAll`, `CancelByCloid`)
  target `block_queued + K` (never before the previous order kind's target, B15), every other kind `+ 1`; a target
  block that already holds 200 rows sends the row on to the next block with room **(plan: the cap applied at
  enqueue)**. Same rule as the decision (at most 200 drained per
  block, excess in the next block, queue order), and rows are still written only by EVM blocks, which are serial
  barriers, so `core_writer_due`'s raw-DB read stays valid (memory 25f7770c) and the `(drain height, seq)` key of the
  v5 status record and of the RPC tx links stays exact. A drain-time cap would rewrite rows in pipelined blocks,
  where the raw-DB due-check cannot see them: stranded rows.
- **bbo (decisions 3, 8; design item 5):** new column family `cf_native_bbo` **(plan)**: key market id (8 BE), value
  best bid raw ‖ best ask raw (i128 BE, 0 = empty side), no row for an empty book. Not hashed, not native root, not
  resident, not ingress: one overlay point put per changed market, no read, no Borsh, no trie bucket. Derived from the
  hashed book rows (an EVM read of a wrong row would change the hashed EVM state). A node-local
  `OrderBook::saved_bbo` skips the put when the top of the book did not move; safe because the CF is unhashed (a cold
  node's extra put writes the same bytes). Written for every dirty market in `save_order_books` and
  `save_order_books_deferred`, every book mode. Precompile 0x080e `bbo(bytes32) -> (uint128 bid, uint128 ask)`:
  16,400 base (the point read included, like `getPrice`) + 2 x 20 (two words) = 16,440 gas (owner s125, open question 1).
- **RPC (design item 7):** `eth_call` / `eth_estimateGas` run in the caller's lane (limit and base fee);
  `eth_gasPrice` = small lane; `torus_bigBlockGasPrice`; `eth_feeHistory` optional 4th param `blockType`
  (`"small"` default, `"big"`); `torus_usingBigBlocks(address)`; `eth_getBlockByNumber` `gasLimit` = the height's cap.
- **Consensus message (decision 8):** the worst compact proposal at a big height is 786,432 (EVM bytes) + 15,240
  (bincode u64 length per tx: 1,904 txs at the 10M + 30M ceilings, plus the list) + 12,808 (400 x 32-byte action
  hashes + length) + 16,384 (header, justify QC, framing) = **830,864 B <= 1,048,576**: it fits, 212 KiB headroom.
  `o5_feed_gates.rs` is rewritten to size from the vote caps (B10). Limit: `TORUS_NATIVE_TOTAL_BLOCK_CAP` above 7,203
  would push a worst big height over 1 MiB (documented in B19).

## Success Criteria

1. A genesis without the new keys gives headers with M = 0, big limit 10M, both base fees 1 gwei; no height is big;
   the small lane behaves as in step 1.
2. With M > 0 (genesis or governance) an honest proposer puts EVM txs only at `h % N == 0` or `h % M == 0`, and at a
   big height drains the small lane, then the big lane. Validators refuse before the vote a block with EVM txs off
   both cadences, more than (small + big) / 21,000 txs at a big height, more than 768 KiB (256 KiB off a big height),
   or declared gas above the height's cap.
3. `evm_big_block_interval` (0 or 30..=10,000), `evm_big_block_gas_limit` (1M..=30M) and `core_writer_order_delay`
   (1..=100) are refused outside their ranges at submission and execution. M and the big limit reach the header at
   the next boundary + L. A boundary header that differs from the plan in any of the six EVM values is Invalid before
   the vote and at insertion; off a boundary, any difference from the parent is.
4. `EvmUserModify` sets or clears the lane row. While M > 0 the sender's EVM txs enter the big pool only (a lane
   switch waits until the other lane holds none of its txs), go to the leaders of the next two big heights, and pay
   the big base fee at execution.
5. Each boundary sets the next epoch's two base fees from the closing epoch's executed gas per lane (at most +/-12.5%
   per epoch, never below 1 gwei); they reach the headers at boundary + L; serial and pipelined execution leave
   identical state.
6. `TORUS_EVM_BLOCK_GAS_BUDGET` unset = no cap; set, it caps only the small lane (drain and admission).
7. A committed block's EVM txs leave every pool at commit; their copies are refused for 256 blocks; `pending_nonce`
   counts them. A forwarded tx whose target EVM height commits without it is re-sent to the next predicted leader(s),
   at most 8 times. The prediction uses the next boundary's N and M once its plan is readable.
8. CoreWriter order actions drain K blocks after their EVM block (default 10), other kinds one block after; no block
   drains more than 200 rows; the excess drains in the next block, in queue order, also with the pipeline and a
   checkpoint interval on.
9. After every block, `cf_native_bbo` holds (best bid, best ask) for every market whose book changed, in all four
   book modes and both save paths. `bbo(bytes32)` at 0x080e returns it for exactly 16,440 gas, also in `eth_call`.
10. `eth_call` / `eth_estimateGas` use the caller's lane limit and fee; `torus_bigBlockGasPrice`, `eth_feeHistory`
    with `blockType`, `torus_usingBigBlocks` answer; `gasLimit` is the height's cap.
11. The o5 test shows the worst compact proposal at a big height fits the 1 MiB consensus accept gate.
12. Gates: workspace nextest (flaky 0), doc tests, `-D warnings` build, fmt and clippy counts not above main's at the
    merge base, the CI Uniswap e2e; one full `cargo test` before merging.
13. The 4-validator devnet checks a-f and archy's native-only A/B (Verification) pass.

## Tasks

Test names are the RED tests each task adds; every one fails before its implementation (missing item = compile
error, or a wrong value), except a test marked a structural guard (B10's o5 test), which passes before and must stay
green after. Tests never rely on the default N or M.

### B1: torus-types constants, keys, height rules; EIP-1559 moves to torus-types
- **Test first** (`crates/torus-types/src/lib.rs` `mod tests`):
  ```rust
  #[test]
  fn evm_height_rules_cover_the_small_and_big_cadence() {
      assert!(is_evm_height(10, 5, 0) && !is_big_evm_height(10, 0), "M = 0: off");
      assert!(!is_evm_height(11, 5, 0));
      assert!(is_evm_height(7, 5, 7) && is_big_evm_height(7, 7), "a big height off the small cadence");
      assert!(is_evm_height(35, 5, 7) && is_big_evm_height(35, 7));
      assert!(!is_evm_height(8, 5, 7));
      assert!(!is_evm_height(10, 0, 0), "N = 0 carries nothing, no panic");
  }

  #[test]
  fn big_block_interval_is_zero_or_in_range() {
      for (m, ok) in [(0, true), (1, false), (29, false), (30, true), (190, true), (10_000, true), (10_001, false)] {
          assert_eq!(evm_big_block_interval_ok(m), ok, "{m}");
      }
      assert_eq!(MAX_EVM_BIG_BLOCK_TX_BYTES, 786_432);
  }
  ```
  The six `base_fee_*` tests of `torus-evm/src/eip1559.rs` move unchanged into the new `torus-types/src/eip1559.rs`.
- **Implementation:** after `EVM_MIN_TX_GAS` (lib.rs:695):
  ```rust
  /// Item 7 step 2 (owner decision 8): most raw EVM tx bytes at a big EVM height (vote rule), so also
  /// per big-lane tx (admission). Fits the 1 MiB consensus message (`o5_feed_gates.rs`).
  pub const MAX_EVM_BIG_BLOCK_TX_BYTES: usize = 768 * 1024;
  /// Item 7 step 2 (owner decision 1): genesis default M; 0 = big blocks off.
  pub const DEFAULT_EVM_BIG_BLOCK_INTERVAL: u64 = 0;
  /// Item 7 step 2 (owner decision 2): genesis default big-lane gas limit (10-15M at switch-on).
  pub const DEFAULT_EVM_BIG_GAS_LIMIT: u64 = 10_000_000;
  /// Item 7 step 2 (F3): neither lane's base fee drops below this (today's fixed 1 gwei).
  pub const EVM_MIN_BASE_FEE: u64 = 1_000_000_000;
  /// Item 7 step 2 (owner decision 4): CoreWriter order actions wait this many blocks when the
  /// governance key is absent or invalid; every other kind waits one.
  pub const DEFAULT_CORE_WRITER_ORDER_DELAY: u64 = 10;
  /// Item 7 step 2 (owner decision 4): most CoreWriter rows one block drains; a row whose target is
  /// full queues for the next block with room (`CoreWriterQueue::enqueue`).
  pub const CORE_WRITER_DRAIN_CAP: u64 = 200;
  ```
  After `EVM_BLOCK_INTERVAL_RANGE` (lib.rs:725):
  ```rust
  /// Item 7 step 2: governance keys of the big lane (copied into the epoch plan like the two above)
  /// and of the CoreWriter order delay (read when a row is queued).
  pub const EVM_BIG_BLOCK_INTERVAL_PARAM: &str = "evm_big_block_interval";
  /// A non-zero M must lie here; 0 switches big blocks off ([`evm_big_block_interval_ok`]).
  pub const EVM_BIG_BLOCK_INTERVAL_RANGE: std::ops::RangeInclusive<u64> = 30..=10_000;
  pub const EVM_BIG_BLOCK_GAS_LIMIT_PARAM: &str = "evm_big_block_gas_limit";
  /// Owner decision 2: up to HL's 30M; growing there takes a vote, not a release.
  pub const EVM_BIG_BLOCK_GAS_LIMIT_RANGE: std::ops::RangeInclusive<u64> = 1_000_000..=30_000_000;
  pub const CORE_WRITER_ORDER_DELAY_PARAM: &str = "core_writer_order_delay";
  pub const CORE_WRITER_ORDER_DELAY_RANGE: std::ops::RangeInclusive<u64> = 1..=100;

  /// Item 7 step 2: a valid M is 0 (off) or inside [`EVM_BIG_BLOCK_INTERVAL_RANGE`].
  pub fn evm_big_block_interval_ok(m: u64) -> bool {
      m == 0 || EVM_BIG_BLOCK_INTERVAL_RANGE.contains(&m)
  }

  /// Item 7 step 2: a big EVM height: M > 0 and `height % M == 0`.
  pub fn is_big_evm_height(height: u64, big_interval: u64) -> bool {
      big_interval > 0 && height % big_interval == 0
  }

  /// Item 7 step 2: EVM txs may ride `height`: the small cadence (`height % N == 0`) or a big
  /// height. N = 0 (never a valid header) carries none.
  pub fn is_evm_height(height: u64, interval: u64, big_interval: u64) -> bool {
      height.checked_rem(interval) == Some(0) || is_big_evm_height(height, big_interval)
  }
  ```
  New `crates/torus-types/src/eip1559.rs` with `calc_next_block_base_fee` and its tests, verbatim from
  `torus-evm/src/eip1559.rs`; `pub mod eip1559;` in torus-types; `torus-evm/src/eip1559.rs` becomes
  `pub use torus_types::eip1559::calc_next_block_base_fee;` so `torus_evm::calc_next_block_base_fee` callers
  (validator.rs:122, proposer.rs:74, bridge and evm tests) compile unchanged.
- **Verify:** `$L cargo nextest run $P $F -E 'test(evm_height_rules_cover_the_small_and_big_cadence) or test(big_block_interval_is_zero_or_in_range) or test(base_fee_)'`
- **Depends on:** none

### B2: header fields `evm_big_block_interval`, `evm_big_gas_limit`, `big_base_fee_per_gas`
- **Test first** (torus-types lib.rs `mod tests`, helper `test_header()`):
  ```rust
  #[test]
  fn big_lane_fields_in_canonical_bytes() {
      let a = test_header();
      type Change = fn(&mut TorusBlockHeader);
      let changes: [Change; 3] = [
          |h| h.evm_big_block_interval += 1,
          |h| h.evm_big_gas_limit += 1,
          |h| h.big_base_fee_per_gas += 1,
      ];
      for change in changes {
          let mut b = a.clone();
          change(&mut b);
          assert_ne!(a.canonical_header_bytes(), b.canonical_header_bytes());
      }
  }

  #[test]
  fn header_caps_add_the_big_lane_at_big_heights() {
      let mut h = test_header();
      h.evm_gas_limit = 3_000_000;
      h.evm_block_interval = 5;
      h.evm_big_block_interval = 50;
      h.evm_big_gas_limit = 10_000_000;
      h.height = 100;
      assert!(h.is_big_evm_height() && h.is_evm_height());
      assert_eq!(h.evm_block_gas_cap(), 13_000_000);
      assert_eq!(h.evm_block_byte_cap(), MAX_EVM_BIG_BLOCK_TX_BYTES);
      h.height = 105;
      assert!(!h.is_big_evm_height() && h.is_evm_height());
      assert_eq!(h.evm_block_gas_cap(), 3_000_000);
      assert_eq!(h.evm_block_byte_cap(), MAX_EVM_BLOCK_TX_BYTES);
      h.evm_big_block_interval = 0;
      h.height = 100;
      assert_eq!(h.evm_block_gas_cap(), 3_000_000, "M = 0: big blocks off");
  }
  ```
  Extend `header_bincode_round_trip_keeps_evm_block_interval` (lib.rs:2343) to the three fields (values 190,
  15,000,000, 2 gwei); add them to the JSON literal of `header_without_attestation_defaults_to_zero` (lib.rs:2012):
  `"evm_big_block_interval":0,"evm_big_gas_limit":10000000,"big_base_fee_per_gas":1000000000`.
- **Implementation** (torus-types lib.rs). After `evm_block_interval` (572):
  ```rust
      /// Item 7 step 2: M. A block with M > 0 and `height % M == 0` is a big EVM block; 0 = off.
      /// Equals the parent's, except at an epoch boundary (the epoch plan).
      pub evm_big_block_interval: u64,
      /// Item 7 step 2: the big lane's gas limit, added to `evm_gas_limit` at a big height.
      pub evm_big_gas_limit: u64,
  ```
  after `base_fee_per_gas` (578):
  ```rust
      /// Item 7 step 2 (F3): the big lane's base fee. Changes only at an epoch boundary.
      pub big_base_fee_per_gas: u64,
  ```
  `canonical_header_bytes`: after `evm_block_interval` (610) push `evm_big_block_interval` then `evm_big_gas_limit`
  (`to_be_bytes`); after `base_fee_per_gas` (613) push `big_base_fee_per_gas`. No serde default (as step 1: the wire
  is bincode, header JSON is only the node's own rows). After `canonical_header_bytes`:
  ```rust
      /// Item 7 step 2: this block is a big EVM block.
      pub fn is_big_evm_height(&self) -> bool {
          is_big_evm_height(self.height, self.evm_big_block_interval)
      }
      /// Item 7 step 2: EVM txs may ride this block (small cadence or a big height).
      pub fn is_evm_height(&self) -> bool {
          is_evm_height(self.height, self.evm_block_interval, self.evm_big_block_interval)
      }
      /// Item 7 step 2: the EVM gas this block may declare and execute: the small limit, plus the
      /// big limit at a big height.
      pub fn evm_block_gas_cap(&self) -> u64 {
          if self.is_big_evm_height() {
              self.evm_gas_limit.saturating_add(self.evm_big_gas_limit)
          } else {
              self.evm_gas_limit
          }
      }
      /// Item 7 step 2: most raw EVM tx bytes this block may carry (vote rule).
      pub fn evm_block_byte_cap(&self) -> usize {
          if self.is_big_evm_height() {
              MAX_EVM_BIG_BLOCK_TX_BYTES
          } else {
              MAX_EVM_BLOCK_TX_BYTES
          }
      }
  ```
  and next to the header:
  ```rust
  /// Item 7 step 2: the six EVM header values the boundary rule pins: the parent's, at an epoch
  /// boundary the plan's (`torus_economics::PlanEvm::resolve`).
  #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
  pub struct EvmHeaderValues {
      pub gas_limit: u64,
      pub block_interval: u64,
      pub big_block_interval: u64,
      pub big_gas_limit: u64,
      pub base_fee: u64,
      pub big_base_fee: u64,
  }

  impl EvmHeaderValues {
      pub fn of(h: &TorusBlockHeader) -> Self {
          Self {
              gas_limit: h.evm_gas_limit,
              block_interval: h.evm_block_interval,
              big_block_interval: h.evm_big_block_interval,
              big_gas_limit: h.evm_big_gas_limit,
              base_fee: h.base_fee_per_gas,
              big_base_fee: h.big_base_fee_per_gas,
          }
      }
  }
  ```
  The 28 `TorusBlockHeader {` literals (find with `/usr/bin/grep -rn "TorusBlockHeader {" crates`; the shell's grep
  wrapper skips ignored files): production `torus-bridge/src/proposer.rs` (3: `build_block` copies the parent's three
  values; `genesis_parent_header` sets `DEFAULT_EVM_BIG_BLOCK_INTERVAL`, `DEFAULT_EVM_BIG_GAS_LIMIT`,
  `1_000_000_000`), `torus-consensus/src/app.rs` `build_proposal` (7346: the parent's values until B6). Tests (the
  `DEFAULT_` constants, the site's own base fee for the big one): app.rs (3 more), torus-types lib.rs (4) and
  `action_hash_scratch_tests.rs` (1), `torus-state/src/block_body.rs` (1), torus-rpc `lib.rs` (8), `scan_tests.rs` (1),
  `tests/eth_compliance_tests.rs` (2), torus-bridge `bridge_tests.rs`, `exec_isolation_tests.rs`,
  `state_root_once_tests.rs` (1 each), `torus-integration-tests/tests/native_da_pull_fallback.rs` (1).
- **Verify:** `$L cargo nextest run $P $F -E 'test(big_lane_fields_in_canonical_bytes) or test(header_caps_add_the_big_lane_at_big_heights) or test(header_bincode_round_trip) or test(header_without_attestation_defaults_to_zero)'`
- **Depends on:** B1

### B3: genesis keys, ChainConfig, the genesis parent header from the config
- **Test first:**
  - `torus-genesis/src/lib.rs` `mod tests`:
    ```rust
    #[test]
    fn big_block_values_default_off_and_feed_chain_config() {
        let genesis = Genesis::from_json(&sample_genesis_json()).unwrap();
        assert_eq!((genesis.evm.big_block_interval, genesis.evm.big_gas_limit), (0, 10_000_000));
        let mut json: serde_json::Value = serde_json::from_str(&sample_genesis_json()).unwrap();
        json["evm"]["big_block_interval"] = 190.into();
        json["evm"]["big_gas_limit"] = 15_000_000u64.into();
        let config = Genesis::from_json(&json.to_string()).unwrap().chain_config();
        assert_eq!((config.evm_big_block_interval, config.evm_big_gas_limit), (190, 15_000_000));
    }
    ```
    Extend `shipped_genesis_files_use_the_item7_evm_values` (~956): each tracked file spells out
    `"big_block_interval": 0` and `"big_gas_limit": 10000000`.
  - `torus-bridge/src/proposer.rs` `mod tests`, `genesis_parent_header_for_takes_the_big_lane_from_the_config`: a
    `ChainConfig` deserialized from a minimal JSON (as torus-types' legacy-config tests do) with
    `"evm_big_block_interval": 190, "evm_big_gas_limit": 15000000` gives a header with M 190, big limit 15M, both base
    fees 1 gwei, and the config's small limit and N.
- **Implementation:**
  ```rust
  // torus-genesis EvmConfig (lib.rs:142-150)
      /// Item 7 step 2: M, big EVM blocks at `height % M == 0`; absent = 0 (off).
      #[serde(default)]
      pub big_block_interval: u64,
      /// Item 7 step 2: the big lane's gas limit; absent = 10M.
      #[serde(default = "default_evm_big_gas_limit")]
      pub big_gas_limit: u64,
  fn default_evm_big_gas_limit() -> u64 {
      torus_types::DEFAULT_EVM_BIG_GAS_LIMIT
  }
  // chain_config() (lib.rs:695-731), after `evm_block_interval`
      evm_big_block_interval: self.evm.big_block_interval,
      evm_big_gas_limit: self.evm.big_gas_limit,
  // torus-types ChainConfig (lib.rs:1461), after `evm_block_interval`
      /// Item 7 step 2: genesis M (`evm.big_block_interval`); absent = 0.
      #[serde(default)]
      pub evm_big_block_interval: u64,
      /// Item 7 step 2: genesis big-lane gas limit (`evm.big_gas_limit`); absent = 10M.
      #[serde(default = "default_evm_big_gas_limit")]
      pub evm_big_gas_limit: u64,
  fn default_evm_big_gas_limit() -> u64 {
      DEFAULT_EVM_BIG_GAS_LIMIT
  }
  ```
  `torus-bridge/src/proposer.rs`, after `genesis_parent_header` (210), and re-exported beside it from torus-bridge:
  ```rust
  /// Item 7 step 2: the genesis parent header of a chain config (small limit, N, M and the big limit
  /// from genesis; both base fees 1 gwei). One value for every user, or block 1 fails to link.
  pub fn genesis_parent_header_for(config: &torus_types::ChainConfig) -> TorusBlockHeader {
      let mut header = genesis_parent_header(config.evm_gas_limit, config.evm_block_interval);
      header.evm_big_block_interval = config.evm_big_block_interval;
      header.evm_big_gas_limit = config.evm_big_gas_limit;
      header
  }
  ```
  `genesis_parent_header(gas, n)` keeps its signature (12 callers). Its two config-driven callers switch:
  app.rs:5717-5718 `let genesis_header = torus_bridge::genesis_parent_header_for(config);` and `make_exec_ctx`
  (app.rs:13886) `genesis_header: torus_bridge::genesis_parent_header_for(config),`. Other `ChainConfig {` literals
  get the two fields (`DEFAULT_` constants): `torus-node/src/main.rs` (2, `default_chain_config`), torus-genesis (2),
  `native_da_pull_fallback.rs` (2), app.rs (3). Genesis files (`"big_block_interval": 0, "big_gas_limit": 10000000`
  after `"block_interval"`): `devnet/genesis.json`, `devnet/ab-harness/devnet-genesis.json`,
  `testnet/genesis-weighted-base.json`, `testnet/genesis-weighted-base-setup2b.json`,
  `testnet/p3-testnet/devnet-ladder/genesis-10mkt.json`. Gitignored generated files are regenerated, not edited. The CI
  Uniswap genesis keeps its 10M small limit (the Router deploy, ~4.2M, needs no big lane).
- **Verify:** `$L cargo nextest run $P $F -E 'test(big_block_values_default_off_and_feed_chain_config) or test(shipped_genesis_files) or test(genesis_parent_header)'`
- **Depends on:** B2

### B4: the epoch plan carries M and the big limit
- **Test first** (`crates/torus-economics/src/epoch_plan.rs` tests, next to `plan_carries_governance_evm_params`
  ~477, helper `put_big_params` modelled on `put_evm_params` ~455):
  - `plan_carries_governance_big_lane_params`: rows `"190"` / `"15000000"` give
    `plan(8).evm.big_block_interval == Some(190)` and `big_gas_limit == Some(15_000_000)`; `!changed`.
  - `plan_big_lane_values_outside_their_range_keep_the_chain_values`: M `"0"` -> `Some(0)` (switching off is a
    value); `"1"`, `"29"`, `"10001"`, `"x"` -> `None`; big limit `"999999"`, `"30000001"`, `"x"` -> `None`,
    `"30000000"` -> `Some(30_000_000)`.
  - `plan_resolve_takes_set_values_else_the_parents`: `PlanEvm { big_block_interval: Some(190), base_fee:
    Some(2_000_000_000), ..Default::default() }.resolve(&parent)` takes those two and the parent's other four.
- **Implementation** (epoch_plan.rs):
  ```rust
  // PlanEvm (43-48): appended fields (the plan row is Borsh; fresh genesis)
      /// Item 7 step 2: M and the big lane's gas limit (governance), and both base fees (F3,
      /// set by the previous boundary's execution, B9).
      pub big_block_interval: Option<u64>,
      pub big_gas_limit: Option<u64>,
      pub base_fee: Option<u64>,
      pub big_base_fee: Option<u64>,

  impl PlanEvm {
      /// The header values from this boundary on: each set value, else the parent's.
      pub fn resolve(&self, parent: &torus_types::TorusBlockHeader) -> torus_types::EvmHeaderValues {
          torus_types::EvmHeaderValues {
              gas_limit: self.gas_limit.unwrap_or(parent.evm_gas_limit),
              block_interval: self.block_interval.unwrap_or(parent.evm_block_interval),
              big_block_interval: self.big_block_interval.unwrap_or(parent.evm_big_block_interval),
              big_gas_limit: self.big_gas_limit.unwrap_or(parent.evm_big_gas_limit),
              base_fee: self.base_fee.unwrap_or(parent.base_fee_per_gas),
              big_base_fee: self.big_base_fee.unwrap_or(parent.big_base_fee_per_gas),
          }
      }
  }
  // execute_planned_rotation (314-323)
          next.evm = PlanEvm {
              gas_limit: gov(torus_types::EVM_BLOCK_GAS_LIMIT_PARAM, &torus_types::EVM_BLOCK_GAS_LIMIT_RANGE)?,
              block_interval: gov(torus_types::EVM_BLOCK_INTERVAL_PARAM, &torus_types::EVM_BLOCK_INTERVAL_RANGE)?,
              big_block_interval: gov(torus_types::EVM_BIG_BLOCK_INTERVAL_PARAM, &(0..=u64::MAX))?
                  .filter(|&m| torus_types::evm_big_block_interval_ok(m)),
              big_gas_limit: gov(
                  torus_types::EVM_BIG_BLOCK_GAS_LIMIT_PARAM,
                  &torus_types::EVM_BIG_BLOCK_GAS_LIMIT_RANGE,
              )?,
              base_fee: None,     // F3, B9
              big_base_fee: None, // F3, B9
          };
  ```
  `PlanEvm {` literals in app.rs tests (3) and epoch_plan.rs (2) add `..PlanEvm::default()`.
- **Verify:** `$L cargo nextest run $P $F -E 'test(plan_carries_governance_big_lane_params) or test(plan_big_lane_values_outside_their_range) or test(plan_resolve_takes_set_values) or test(plan_carries_governance_evm_params)'`
- **Depends on:** B1, B2

### B5: governance accepts the three keys
- **Test first** (`crates/torus-economics/tests/governance_tests.rs`, copy of
  `evm_block_param_changes_are_range_checked_and_execute_into_fee_config` ~673, helpers `setup` 42, `setup_validator`
  64, `setup_voter` 74, `pass_param` 421): `big_lane_and_corewriter_param_changes_are_range_checked_and_execute_into_fee_config`.
  Refused at submission with `InvalidParameterValue { key }`: M `"1"`, `"29"`, `"10001"`, `"x"`; big limit
  `"999999"`, `"30000001"`, `"x"`; K `"0"`, `"101"`, `"x"`. Passed and stored byte for byte in `CF_FEE_CONFIG`: M
  `"0"` (block 0) and `"190"` (200), big limit `"15000000"` (400), K `"10"` (600).
- **Implementation** (`governance.rs` ~766-786): the step 1 arm takes the big limit and K too, M gets its own arm:
  ```rust
              // Item 7 steps 1-2: copied into the next epoch plan (torus_economics::epoch_plan), or
              // read when a CoreWriter row is queued (the order delay).
              torus_types::EVM_BLOCK_GAS_LIMIT_PARAM
              | torus_types::EVM_BLOCK_INTERVAL_PARAM
              | torus_types::EVM_BIG_BLOCK_GAS_LIMIT_PARAM
              | torus_types::CORE_WRITER_ORDER_DELAY_PARAM => {
                  let range = match key {
                      torus_types::EVM_BLOCK_GAS_LIMIT_PARAM => torus_types::EVM_BLOCK_GAS_LIMIT_RANGE,
                      torus_types::EVM_BLOCK_INTERVAL_PARAM => torus_types::EVM_BLOCK_INTERVAL_RANGE,
                      torus_types::EVM_BIG_BLOCK_GAS_LIMIT_PARAM => torus_types::EVM_BIG_BLOCK_GAS_LIMIT_RANGE,
                      _ => torus_types::CORE_WRITER_ORDER_DELAY_RANGE,
                  };
                  /* parse + range check: unchanged from step 1 */
              }
              torus_types::EVM_BIG_BLOCK_INTERVAL_PARAM => {
                  let v: u64 = value.parse().map_err(|_| EconomicsError::InvalidParameterValue {
                      key: key.to_string(),
                      reason: "must be a valid u64".to_string(),
                  })?;
                  if !torus_types::evm_big_block_interval_ok(v) {
                      let r = torus_types::EVM_BIG_BLOCK_INTERVAL_RANGE;
                      return Err(EconomicsError::InvalidParameterValue {
                          key: key.to_string(),
                          reason: format!("must be 0 (big blocks off) or between {} and {}", r.start(), r.end()),
                      });
                  }
              }
  ```
  Storage unchanged (non-`GovernanceParams` keys are their own `CF_FEE_CONFIG` row; the check runs at submission and
  execution).
- **Verify:** `$L cargo nextest run $P $F -E 'test(big_lane_and_corewriter_param_changes_are_range_checked) or test(evm_block_param_changes_are_range_checked)'`
- **Depends on:** B1

### B6: the boundary rule covers all six EVM values; the proposer copies them
- **Test first** (app.rs `mod crash_recovery_tests`, beside `boundary_header_must_equal_the_plans_evm_params` ~21473,
  helpers `tip_2000` 20995, `ts_app` 20980, `child_of` 20987, `link_on` 21433, `epoch4_app` 21460, `pre_vote`):
  - `boundary_header_must_equal_the_plans_big_lane_and_fees`: plan `PlanEvm { big_block_interval: Some(190),
    big_gas_limit: Some(15_000_000), base_fee: Some(1_100_000_000), big_base_fee: Some(1_200_000_000),
    ..Default::default() }` through `link_on(.., move |_| Ok(Some(plan)))`: the child carrying exactly those four
    (and the parent's small limit and N) is `Held`; each variant with one field at the parent's value is `Invalid`.
  - `off_boundary_header_must_equal_the_parents_big_lane_and_fees`: `M`, big limit, big base fee each +1 and -1 on
    `child_of(&tip_2000(), 2_000)`: `Invalid` in `pre_vote` and in `link_on(.., |h| app.boundary_evm(h))`.
  - `governance_big_lane_params_reach_the_header_at_the_next_boundary_plus_l`: copy of
    `governance_evm_params_reach_the_header_at_the_next_boundary_plus_l` (21720) with rows
    `evm_big_block_interval = "190"`, `evm_big_block_gas_limit = "15000000"`: `boundary_evm(8)` carries them,
    `build_proposal(parent7)` puts them in block 8, `link_on` is `Held`, block 9 keeps them.
- **Implementation:** `check_parent_link` (app.rs:2183-2211):
  ```rust
      // Item 7 steps 1-2: the six EVM values (gas limit, N, M, big gas limit, both base fees) are the
      // rule values. Off a boundary: the parent's. At a boundary: the plan's (unset or no plan: the
      // parent's). After the link check, so a bogus height is Invalid and never waits on a plan.
      let rule = match boundary(header.height) {
          Ok(plan) => plan.unwrap_or_default().resolve(&parent),
          Err(EpochPlanNotReady) => return BlockDataCheck::Missing,
      };
      let got = torus_types::EvmHeaderValues::of(header);
      if got != rule {
          tracing::warn!(
              height = header.height,
              ?got,
              ?rule,
              "REJECTED -- EVM header values differ from the rule values"
          );
          return BlockDataCheck::Invalid;
      }
      BlockDataCheck::Held
  ```
  `build_proposal` (7275-7292, header literal 7357-7361):
  ```rust
          let Ok(plan_evm) = self.boundary_evm(next_height) else {
              return refused_proposal();
          };
          let evm = plan_evm.unwrap_or_default().resolve(&parent_header);
          // Cadence: step 1's h % N (B12 adds big heights).
          let gas_limit = if next_height.checked_rem(evm.block_interval) == Some(0) {
              evm.gas_limit
          } else {
              0
          };
  // header literal
                  evm_gas_limit: evm.gas_limit,
                  evm_block_interval: evm.block_interval,
                  evm_big_block_interval: evm.big_block_interval,
                  evm_big_gas_limit: evm.big_gas_limit,
                  base_fee_per_gas: evm.base_fee,
                  big_base_fee_per_gas: evm.big_base_fee,
  ```
  Update the doc comment above `check_parent_link` (~2140: "The base fee always equals the parent's" no longer holds).
- **Verify:** `$L cargo nextest run $P $F -E 'test(boundary_header_must_equal_the_plans_big_lane_and_fees) or test(off_boundary_header_must_equal_the_parents_big_lane_and_fees) or test(governance_big_lane_params_reach_the_header) or test(boundary_) or test(evm_gas_limit_and_base_fee)'`
- **Depends on:** B3, B4

### B7: `NativeAction::EvmUserModify` (tag 30) and its lane row
- **Test first:**
  - torus-types lib.rs, `evm_user_modify_is_canonical_tag_30_and_eip712_only`: `canonical_bytes()` of
    `EvmUserModify { using_big_blocks: true }` is `[30, 1]`, of `false` `[30, 0]`; `!is_trading()`,
    `eip712::requires_eip712`, no `SessionScope` allows it; bincode variant index 30; serde JSON round trip.
  - eip712.rs tests, `evm_user_modify_struct_hash_binds_the_flag_and_nonce`: hash = keccak(typehash ‖ bool ‖ nonce);
    `true` and `false`, and two nonces, differ. Add the variant to the all-variants lists (eip712.rs ~1449, ~1501,
    ~1827), `action_hash_scratch_tests.rs` (~118) and `torus-types/tests/eip712_vectors.rs` `all_vectors()`; regenerate
    its fixture with `$L cargo nextest run $P $F --run-ignored only -E 'test(write_eip712_fixture_file)'`.
  - torus-state cf.rs, `evm_lane_key_layout`: `"lan" ‖ address`, 23 bytes, no common prefix with `"sgn"`, `"sub"`,
    `"agg"`.
  - `crates/torus-bridge/tests/evm_user_modify_tests.rs` (new, modelled on `oracle_signer_tests.rs`),
    `evm_user_modify_sets_and_clears_the_lane_row`: `true` writes `[1]` under `evm_lane_key(sender)` in
    `CF_NATIVE_ORACLE`; `true` again succeeds and writes nothing (the overlay's pending set is empty); `false` deletes
    it; `false` again writes nothing.
  - torus-rpc `variant_index` (torus.rs:3275) gets `EvmUserModify { .. } => 30` and a `samples()` entry.
  - `tools/wallet`, `evm_user_modify_action`: the command builds the action; serde round trip.
- **Implementation:**
  ```rust
  // torus-types lib.rs, after CancelByCloid (874)
      // === EVM (appended: item 7 step 2) ===
      /// HL `evmUserModify`: the sender's EVM lane. `true` = big blocks: while they are on (header
      /// M > 0) its EVM txs go to the big pool and pay the big base fee. EIP-712 only (not a trading
      /// action, so no session key). Lanes are proposer routing and fees, never a vote rule.
      EvmUserModify {
          using_big_blocks: bool,
      },
  // canonical_bytes, after the SetOracleSigner arm (1167-1180)
              NativeAction::EvmUserModify { using_big_blocks } => {
                  buf.push(30);
                  buf.push(u8::from(*using_big_blocks));
              }
  // eip712.rs eip712_struct_hash (242-306)
          NativeAction::EvmUserModify { using_big_blocks } => {
              hash_evm_user_modify(*using_big_blocks, nonce)
          }
  // eip712.rs, after hash_set_oracle_signer
  // ---------- EVM ----------

  fn hash_evm_user_modify(using_big_blocks: bool, nonce: u64) -> B256 {
      let th = keccak256("EvmUserModify(bool usingBigBlocks,uint64 nonce)");
      let mut buf = Vec::with_capacity(3 * 32);
      buf.extend_from_slice(&th.0);
      buf.extend_from_slice(&encode_bool(using_big_blocks));
      buf.extend_from_slice(&encode_u64(nonce));
      keccak256(&buf)
  }
  // torus-state cf.rs, after oracle_signer_key (150-160)
  /// Item 7 step 2: the EVM lane row in [`CF_NATIVE_ORACLE`]: `"lan" ‖ address(20)` -> `[1]` while
  /// the address uses big blocks (`EvmUserModify`); no row = the small lane. In this CF (like the
  /// `"sgn"` index) because the ingress view answers it (`read_view::INGRESS_CFS`): admission reads the
  /// lane without waiting for a checkpoint. Read by execution (fees), admission, routing and RPC.
  pub fn evm_lane_key(address: &alloy_primitives::Address) -> [u8; 23] {
      let mut key = [0u8; 23];
      key[..3].copy_from_slice(b"lan");
      key[3..].copy_from_slice(address.as_slice());
      key
  }
  ```
  `torus-bridge/src/native_executor.rs`: dispatch arm after `SetOracleSigner` (7005-7007)
  `NativeAction::EvmUserModify { using_big_blocks } => Self::exec_evm_user_modify(ctx, sender, *using_big_blocks),`
  and, next to `exec_set_oracle_signer` (~11940):
  ```rust
      /// Item 7 step 2: `EvmUserModify` sets (`true`) or clears the sender's lane row. An unchanged
      /// lane writes nothing. A read or write fault fail-stops (R02 branch 6b, as SetOracleSigner).
      fn exec_evm_user_modify<T: StateBackend>(
          ctx: &mut NativeExecContext<T>,
          sender: &Address,
          using_big_blocks: bool,
      ) -> NativeActionResult {
          use torus_state::cf::{evm_lane_key, CF_NATIVE_ORACLE};
          let key = evm_lane_key(sender);
          let current = match ctx.state.get_cf_raw(CF_NATIVE_ORACLE, &key) {
              Ok(row) => row.is_some(),
              Err(e) => {
                  let m = Self::latch_state_fault(ctx, "evm user modify read", e);
                  return NativeActionResult::err("evm_user_modify", m);
              }
          };
          if current != using_big_blocks {
              let res = if using_big_blocks {
                  ctx.state.put_cf_raw(CF_NATIVE_ORACLE, &key, &[1])
              } else {
                  ctx.state.delete_cf_raw(CF_NATIVE_ORACLE, &key)
              };
              if let Err(e) = res {
                  let m = Self::latch_state_fault(ctx, "evm user modify write", e);
                  return NativeActionResult::err("evm_user_modify", m);
              }
          }
          NativeActionResult::ok("evm_user_modify", 1000)
      }
  ```
  `classify_action` needs no arm (`_ => Other`); the funded check needs none (an EVM user's spot balance counts,
  `funded.rs` path 2). Wallet: `Command::EvmUserModify { #[arg(long, action = clap::ArgAction::Set)]
  using_big_blocks: bool }` (`tools/wallet/src/main.rs` ~59-345, dispatch ~473-550) and `cmd_evm_user_modify` in
  `commands/transfer.rs` (one `submit_native_action` call, as `cmd_claim_unbonded`).
- **Verify:** `$L cargo nextest run $P $F -E 'test(evm_user_modify) or test(evm_lane_key_layout) or test(fixture_file_matches_current_implementation)'`
- **Depends on:** B1

### B8: execution charges each sender its lane's base fee; a big height executes small + big gas
- **Test first:**
  - `crates/torus-evm/tests/evm_tests.rs` (TxEnv construction as in `block_execution_multiple_txs` ~314):
    ```rust
    /// Item 7 step 2: a big-lane sender's tx runs at the big base fee, the other at the block's;
    /// the result splits the gas and sums the fee revenue per lane. RED before: one base fee.
    #[test]
    fn big_lane_sender_pays_the_big_base_fee() {
        // A and B funded; transfers at max fee 5 gwei, tip 1 wei; block base fee 1 gwei.
        let cfg = BlockEnvCfg { base_fee: 1_000_000_000, ..default_block_cfg() };
        let big = BigLane { base_fee: 3_000_000_000, senders: [addr_b].into_iter().collect() };
        let r = EvmExecutor::new(TORUS_CHAIN_ID)
            .execute_block_lanes(&db, &cfg, vec![transfer_from_a, transfer_from_b], false, &big)
            .unwrap();
        assert_eq!(r.receipts[0].effective_gas_price, 1_000_000_001);
        assert_eq!(r.receipts[1].effective_gas_price, 3_000_000_001);
        assert_eq!(r.big_gas_used, 21_000);
        assert_eq!(r.fee_revenue, 21_000 * 1_000_000_000 + 21_000 * 3_000_000_000);
    }
    ```
    and `big_lane_tx_below_the_big_base_fee_is_skipped` (B's max fee 2 gwei < 3 gwei: with `skip_invalid` B gets no
    receipt, A's tx runs).
  - `crates/torus-bridge/tests/evm_lanes_tests.rs` (new):
    - `catchup_validation_charges_each_sender_its_lane_while_big_blocks_are_on`: header M = 50, big fee 3 gwei, a
      lane row for B: `validate_block_for_catchup` gives `fee_revenue` = 21k x 1 gwei + 21k x 3 gwei and
      `big_gas_used` = 21k; the same block with M = 0 charges both 1 gwei.
    - `a_tx_above_the_small_limit_runs_only_at_a_big_height`: small 3M, big 10M, M = 50: a 5M-gas-limit tx at height
      100 has a receipt; at height 105 revm refuses it (above the block gas limit) and it is skipped.
- **Implementation:**
  ```rust
  // torus-evm executor.rs, after BlockEnvCfg (78-96)
  /// Item 7 step 2: a block's big lane: its base fee and the senders that pay it (their
  /// `EvmUserModify` rows, read by the caller while big blocks are on). Default = one lane.
  #[derive(Clone, Debug, Default)]
  pub struct BigLane {
      pub base_fee: u64,
      pub senders: std::collections::HashSet<Address>,
  }
  // BlockExecResult (115-139), new fields (execute_block_with_overlay fills them for one lane)
      /// Item 7 step 2: gas used by big-lane txs (the rest is the small lane's).
      pub big_gas_used: u64,
      /// The base-fee part of the fees, each tx at its lane's base fee:
      /// `sum(gas_used * min(effective_gas_price, lane base fee))`.
      pub fee_revenue: u128,
  // execute_block (275) keeps its signature
      pub fn execute_block(
          &self,
          state_db: &StateDb,
          block_cfg: &BlockEnvCfg,
          transactions: Vec<TxEnv>,
          skip_invalid: bool,
      ) -> Result<BlockExecResult, EvmError> {
          self.execute_block_lanes(state_db, block_cfg, transactions, skip_invalid, &BigLane::default())
      }

      /// [`Self::execute_block`] with a big lane: a tx from `big.senders` runs at `big.base_fee`,
      /// every other tx at `block_cfg.base_fee`.
      pub fn execute_block_lanes(
          &self,
          state_db: &StateDb,
          block_cfg: &BlockEnvCfg,
          transactions: Vec<TxEnv>,
          skip_invalid: bool,
          big: &BigLane,
      ) -> Result<BlockExecResult, EvmError> {
          // today's execute_block body (282-420), with, in the tx loop before inspect_tx_commit (328):
          let in_big_lane = big.senders.contains(&tx.caller);
          let lane_base_fee = if in_big_lane { big.base_fee } else { block_cfg.base_fee };
          evm.ctx.modify_block(|b| b.basefee = lane_base_fee);
          // after gas_used (338):
          if in_big_lane {
              big_gas_used += gas_used;
          }
          // the effective price (386-387) and the revenue:
          let effective_gas_price = calc_effective_gas_price(lane_base_fee, max_fee, priority_fee);
          fee_revenue += u128::from(gas_used) * u128::from(effective_gas_price.min(lane_base_fee));
      }
  ```
  `torus-bridge/src/validator.rs` `validate_block_inner` (148-157):
  ```rust
          // Item 7 step 2: while big blocks are on (M > 0) a sender whose `EvmUserModify` row says big
          // pays the big base fee. An EVM block runs after a barrier, so the DB is current. A read
          // fault is a local fault (fail-stop), never "small".
          let mut big_lane = torus_evm::BigLane {
              base_fee: block.header.big_base_fee_per_gas,
              ..Default::default()
          };
          if block.header.evm_big_block_interval > 0 {
              let callers: std::collections::HashSet<Address> =
                  decoded_txs.iter().map(|(_, d)| d.tx_env.caller).collect();
              for caller in callers {
                  let row = state_db
                      .get_cf_raw(torus_state::cf::CF_NATIVE_ORACLE, &torus_state::cf::evm_lane_key(&caller))
                      .map_err(|e| BridgeError::Evm(torus_evm::EvmError::LocalFault(format!("EVM lane read: {e}"))))?;
                  if row.is_some() {
                      big_lane.senders.insert(caller);
                  }
              }
          }
          let block_cfg = BlockEnvCfg {
              number: block.header.height,
              timestamp: block.header.timestamp,
              beneficiary: block.header.proposer,
              gas_limit: block.header.evm_block_gas_cap(),
              base_fee: block.header.base_fee_per_gas,
          };
          let mut exec_result = evm_executor.execute_block_lanes(
              state_db,
              &block_cfg,
              tx_envs,
              skip_state_root_check,
              &big_lane,
          )?;
  ```
  The strict path's revenue check (169-178) compares `exec_result.fee_revenue`. `ValidatedBlock` (20-42) gains
  `gas_used: u64`, `big_gas_used: u64`, `fee_revenue: u128`, filled in both constructors (214, 321). app.rs
  (3252-3256): `computed_fee_revenue = validated.fee_revenue;` (`compute_fee_revenue` stays for the proposer path).
- **Verify:** `$L cargo nextest run $P $F -E 'test(big_lane_sender_pays_the_big_base_fee) or test(big_lane_tx_below_the_big_base_fee_is_skipped) or test(charges_each_sender_its_lane) or test(runs_only_at_a_big_height)'`
- **Depends on:** B2, B7

### B9: F3, the epoch's executed gas sets the next base fees
- **Test first:**
  - epoch_plan.rs tests:
    ```rust
    /// Item 7 step 2 (F3): the epoch counts as one EIP-1559 block per lane: target = half the
    /// lane's gas over its EVM heights in the window, +/-12.5% at most, never below 1 gwei.
    #[test]
    fn next_base_fees_follow_the_epochs_gas_per_lane() {
        let w = torus_types::EvmHeaderValues {
            gas_limit: 3_000_000,
            block_interval: 5,
            big_block_interval: 50,
            big_gas_limit: 10_000_000,
            base_fee: 2_000_000_000,
            big_base_fee: 2_000_000_000,
        };
        // [100, 200): 20 small-cadence heights (100 and 150 are big too), 2 big heights.
        // Targets: 20 x 3M / 2 = 30M (small), 2 x 10M / 2 = 10M (big).
        assert_eq!(next_base_fees(&w, 100, 200, 30_000_000, 10_000_000), (2_000_000_000, 2_000_000_000));
        assert_eq!(next_base_fees(&w, 100, 200, 60_000_000, 20_000_000), (2_250_000_000, 2_250_000_000));
        assert_eq!(next_base_fees(&w, 100, 200, 0, 0), (1_750_000_000, 1_750_000_000));
        let low = torus_types::EvmHeaderValues { base_fee: 1_000_000_000, big_base_fee: 1_000_000_000, ..w };
        assert_eq!(next_base_fees(&low, 100, 200, 0, 0), (EVM_MIN_BASE_FEE, EVM_MIN_BASE_FEE), "floor");
        let off = torus_types::EvmHeaderValues { big_block_interval: 0, ..w };
        assert_eq!(next_base_fees(&off, 100, 200, 30_000_000, 5).1, 2_000_000_000, "no big height: unchanged");
        // M = 7, N = 5 over [1, 15): EVM heights 5, 7, 10, 14 (4), big 7, 14 (2).
        let mixed = torus_types::EvmHeaderValues { big_block_interval: 7, ..w };
        assert_eq!(next_base_fees(&mixed, 1, 15, 6_000_000, 10_000_000), (2_000_000_000, 2_000_000_000));
        // Gas over a lane's window limit (60M small, 20M big) counts as the limit: exactly +12.5%
        // (unclamped: 2,500,000,000 and 2,375,000,000).
        assert_eq!(next_base_fees(&w, 100, 200, 90_000_000, 25_000_000), (2_250_000_000, 2_250_000_000));
        assert_eq!(next_base_fees(&w, 100, 200, u64::MAX, u64::MAX), (2_250_000_000, 2_250_000_000));
        let top = torus_types::EvmHeaderValues { base_fee: u64::MAX - 1, big_base_fee: u64::MAX - 1, ..w };
        assert_eq!(next_base_fees(&top, 100, 200, 60_000_000, 20_000_000), (u64::MAX, u64::MAX), "saturates");
    }
    ```
    The last three asserts are the clamp (Codex plan check s125): lanes come from the sender's row and the vote
    does not check lanes, so one lane's executed gas can exceed its limit x heights (a big height's cap is small +
    big), and `calc_next_block_base_fee` has no clamp; `top` panics on overflow in a debug build today.
  - `crates/torus-types/src/eip1559.rs` tests: `base_fee_clamps_gas_used_at_the_limit`
    (`calc_next_block_base_fee(45_000_000, 30_000_000, 1_000_000_000) == 1_125_000_000`, and the same with
    `u64::MAX` used; unclamped: 1,250,000,000) and `base_fee_saturates_at_u64_max`
    (`calc_next_block_base_fee(30_000_000, 30_000_000, u64::MAX) == u64::MAX`, no panic).
  - epoch_plan.rs, `boundary_turns_the_epoch_gas_into_plan_fees_and_resets_the_row` (setup as
    `plan_carries_governance_evm_params`; `add_epoch_evm_gas(mgr.state(), 9_000_000, 0)`; `execute_planned_rotation(&mgr, 4, 4, 4, Some(window))` with N = 1,
    M = 0, 3M, 10M, 2 gwei both: `plan(8).evm.base_fee == Some(2_250_000_000)` (3 EVM heights, target 4.5M, used 2x),
    `big_base_fee == Some(2_000_000_000)`, `epoch_evm_gas == (0, 0)`; with `None`: fees `None`, row still reset).
  - app.rs, `f3_epoch_gas_sets_plan_fees_serial_and_pipelined`: config `epoch_length = 4`,
    `evm_block_interval = 1`; blocks 1-4 (`make_block`; on all four headers `evm_block_interval = 1` and
    `evm_big_block_interval = 0`, as B10's `lane_block` sets them, then `link_blocks`: `make_block` pins
    `TEST_EVM_BLOCK_INTERVAL` = 5 (app.rs:262, header ~10398), and with N = 5 the window [1, 4) has no EVM height,
    so the fee would stay 1 gwei for the wrong reason (Codex plan check s125)), an EVM transfer in block 2 and in
    block 4, the row seeded to `9_000_000` small gas before block 4 (`db.put_cf_raw`); for `on` in `[false, true]`
    (`pipeline_ctx(&db, on, None)`): `plan(8).evm.base_fee == Some(1_125_000_000)` (1 gwei, 9M + 21k over a 4.5M
    target; used is clamped at the 9M window limit, so exactly +12.5%; unclamped it would be 1,125,583,333), the row
    after block 4 is `(21_000, 0)` (block 4's own gas), dumps equal across `on`.
    After `perf/epoch-boundary-fast-path` merges: a third run with `ctx.epoch_fast_path = true` and an EVM-free block
    4 (pipelined boundary) gives the same plan fees.
- **Implementation:**
  `calc_next_block_base_fee` (`crates/torus-types/src/eip1559.rs`, C1): clamp and saturate.
  ```rust
  pub fn calc_next_block_base_fee(gas_used: u64, gas_limit: u64, base_fee: u64) -> u64 {
      // Item 7 step 2 (F3): gas above the limit counts as the limit (at most +12.5% per step): a
      // lane's executed gas can exceed its window limit (lanes are not part of the vote).
      let gas_used = gas_used.min(gas_limit);
      // .. unchanged, except the over-target return:
          base_fee.saturating_add(fee_delta.max(1))
  ```
  The bridge callers (proposer.rs:74, validator.rs:122) pass the parent header's `evm_gas_used`, which is at most
  its limit (0 in consensus headers): no change for them.
  ```rust
  // epoch_plan.rs
  /// Item 7 step 2 (F3): `CF_CONSENSUS_META` row of the gas the EVM blocks since the last boundary
  /// used: small lane ‖ big lane (u64 BE each). Hashed (`running_hash::META_CONSENSUS_PREFIXES`).
  pub const EVM_EPOCH_GAS_KEY: &[u8] = b"evm_epoch_gas";

  /// Item 7 step 2 (F3): the epoch's EVM gas so far, `(small, big)`; no row = `(0, 0)`.
  pub fn epoch_evm_gas<T: StateBackend>(state: &T) -> Result<(u64, u64)> {
      Ok(match state.get_cf_raw(CF_CONSENSUS_META, EVM_EPOCH_GAS_KEY)? {
          Some(v) if v.len() == 16 => (
              u64::from_be_bytes(v[..8].try_into().expect("8 bytes")),
              u64::from_be_bytes(v[8..].try_into().expect("8 bytes")),
          ),
          _ => (0, 0),
      })
  }

  /// Item 7 step 2 (F3): add one block's EVM gas to the epoch row.
  pub fn add_epoch_evm_gas<T: StateBackend>(state: &T, gas: u64, big_gas: u64) -> Result<()> {
      let (small, big) = epoch_evm_gas(state)?;
      let mut v = [0u8; 16];
      v[..8].copy_from_slice(&small.saturating_add(gas).to_be_bytes());
      v[8..].copy_from_slice(&big.saturating_add(big_gas).to_be_bytes());
      state.put_cf_raw(CF_CONSENSUS_META, EVM_EPOCH_GAS_KEY, &v)?;
      Ok(())
  }

  /// Item 7 step 2 (F3): the base fees after the epoch `[from, to)` ran with `window` (its header
  /// values) and used `(gas, big_gas)`: EIP-1559 with the epoch as one block. A lane's gas limit is
  /// its limit times its EVM heights in the window (small lane: every EVM height, big ones included;
  /// big lane: big heights), so the target is half of it; at most +/-12.5% per epoch, never below
  /// `EVM_MIN_BASE_FEE`.
  pub fn next_base_fees(
      window: &torus_types::EvmHeaderValues,
      from: u64,
      to: u64,
      gas: u64,
      big_gas: u64,
  ) -> (u64, u64) {
      let (mut evm_heights, mut big_heights) = (0u64, 0u64);
      for h in from.max(1)..to {
          if torus_types::is_evm_height(h, window.block_interval, window.big_block_interval) {
              evm_heights += 1;
          }
          if torus_types::is_big_evm_height(h, window.big_block_interval) {
              big_heights += 1;
          }
      }
      let next = |used: u64, heights: u64, limit: u64, fee: u64| {
          torus_types::eip1559::calc_next_block_base_fee(used, heights.saturating_mul(limit), fee)
              .max(torus_types::EVM_MIN_BASE_FEE)
      };
      (
          next(gas, evm_heights, window.gas_limit, window.base_fee),
          next(big_gas, big_heights, window.big_gas_limit, window.big_base_fee),
      )
  }
  // execute_planned_rotation (281): new last parameter `fee_window: Option<torus_types::EvmHeaderValues>`;
  // after `next.evm = PlanEvm { .. }` (B4):
          // Item 7 step 2 (F3): the epoch [height - L, height) sets plan(height + L)'s fees; its row
          // starts over (this block's own EVM gas is added after, into the next epoch).
          let (gas, big_gas) = epoch_evm_gas(staking.state())?;
          if let Some(window) = fee_window {
              let (fee, big_fee) =
                  next_base_fees(&window, height.saturating_sub(epoch_length), height, gas, big_gas);
              next.evm.base_fee = Some(fee);
              next.evm.big_base_fee = Some(big_fee);
          }
          if (gas, big_gas) != (0, 0) {
              staking.state().delete_cf_raw(CF_CONSENSUS_META, EVM_EPOCH_GAS_KEY)?;
          }
  ```
  Re-export the three functions and the key (`torus-economics/src/lib.rs:25`); the five test callers pass `None`.
  `running_hash.rs:77-78`: append `b"evm_epoch_gas"` to `META_CONSENSUS_PREFIXES`. torus-bridge
  `NativeExecContext` (beside `pub liq_value_sum`, native_executor.rs:4662; constructors set `None` / `(0, 0)`):
  ```rust
      /// Item 7 step 2 (F3): at an epoch boundary, the EVM header values of the closing epoch (the
      /// boundary's parent header); `None` off a boundary. Set by the block pipeline.
      pub evm_fee_window: Option<torus_types::EvmHeaderValues>,
      /// Item 7 step 2 (F3): this block's EVM gas, `(small lane, big lane)`.
      pub evm_gas: (u64, u64),
  ```
  `process_epoch_boundary` (12546) passes `ctx.evm_fee_window`; new
  ```rust
      /// Item 7 step 2 (F3): add this block's EVM gas to the epoch row, after `process_epoch_boundary`
      /// (a boundary's own gas starts the next epoch).
      pub fn add_block_evm_gas<T: StateBackend>(ctx: &mut NativeExecContext<T>) {
          let (gas, big_gas) = ctx.evm_gas;
          if (gas, big_gas) == (0, 0) {
              return;
          }
          if let Err(e) = torus_economics::add_epoch_evm_gas(&ctx.state, gas, big_gas) {
              tracing::error!(%e, height = ctx.block_height, "epoch EVM gas write failed");
              Self::latch_local_fault(ctx, "epoch EVM gas", &e);
          }
      }
  ```
  app.rs `execute_committed_block_with`: `let mut evm_gas = (0u64, 0u64);` beside `computed_fee_revenue` (3216), set
  from `validated` after B8's line (`(validated.gas_used - validated.big_gas_used, validated.big_gas_used)`);
  `run_native` (3443) gains `|| evm_gas != (0, 0)`; before the native context is built (~3762):
  ```rust
              // Item 7 step 2 (F3): at a boundary, the closing epoch's values (its parent header) set
              // the next base fees. A read fault fail-stops, as the nonce window's parent read.
              let fee_window = if EpochManager::is_epoch_boundary(height, self.epoch_length) {
                  let parent = if height <= 1 {
                      Ok(self.genesis_header.clone())
                  } else {
                      header_at(&overlay, height - 1)
                  };
                  match parent {
                      Ok(p) => Some(torus_types::EvmHeaderValues::of(&p)),
                      Err(e) => {
                          self.fail_stop_on_fault(torus_block, fold_header, "F3 parent header read", &e);
                          return;
                      }
                  }
              } else {
                  None
              };
  ```
  then `ctx.evm_fee_window = fee_window; ctx.evm_gas = evm_gas;` after `ctx.metrics = ..` (3808), and
  `NativeExecutor::add_block_evm_gas(&mut ctx);` right after `process_epoch_boundary` (3956), before the fatal check
  (3960). `header_at(state, height)` is `parent_block_ms`'s row read and JSON decode (2078-2090) split out;
  `parent_block_ms` maps it to the timestamp. Tests that pin a block's exact hashed write set and execute EVM gas
  (e.g. `running_hash_captures_evm_writer_precompile_side_effects_end_to_end`, app.rs:20573) add the
  `evm_epoch_gas` row to their expectation.
  Fast path: a pipelined boundary reads the row and the parent header through its overlay (parent layer, L, C), so
  the result equals the barrier path.
- **Verify:** `$L cargo nextest run $P $F -E 'test(next_base_fees_follow_the_epochs_gas_per_lane) or test(boundary_turns_the_epoch_gas_into_plan_fees) or test(f3_epoch_gas_sets_plan_fees) or test(running_hash_captures) or test(base_fee_)'`
- **Depends on:** B4, B6, B8

### B10: per-height caps before the vote; the consensus message size test
- **Test first** (app.rs `mod crash_recovery_tests`; helpers below; `pre_vote_as` 21181, `evm_tx_gas` 21166):
  ```rust
  /// Item 7 step 2: `make_block(height)` with N = 5 and M = `m` (header values, not the genesis
  /// default N, which the n11 branch moves to 11).
  fn lane_block(height: u64, m: u64) -> TorusBlock {
      let mut b = make_block(height, vec![]);
      b.header.evm_block_interval = 5;
      b.header.evm_big_block_interval = m;
      b
  }

  /// `evm_child_of(parent)` keeping the parent's six EVM header values (the off-boundary rule).
  fn lane_child_of(parent: &TorusBlock, evm: Vec<Vec<u8>>) -> TorusBlock {
      let mut b = evm_child_of(parent, evm);
      let p = &parent.header;
      b.header.evm_gas_limit = p.evm_gas_limit;
      b.header.evm_block_interval = p.evm_block_interval;
      b.header.evm_big_block_interval = p.evm_big_block_interval;
      b.header.evm_big_gas_limit = p.evm_big_gas_limit;
      b.header.base_fee_per_gas = p.base_fee_per_gas;
      b.header.big_base_fee_per_gas = p.big_base_fee_per_gas;
      b
  }
  ```
  - `evm_txs_at_a_big_height_off_the_small_cadence_are_held`: full and compact datums; `lane_child_of(&lane_block(6,
    7), [evm_tx_gas(0, 21_000)])` (height 7, big) is `Held`; the same at height 8 (parent `lane_block(7, 7)`) is
    `Invalid`.
  - `big_height_caps_count_bytes_and_gas_at_small_plus_big`: parent `lane_block(49, 50)` with big limit 10M (small
    3M), child at 50: 619 = 13M / 21,000 undecodable 8-byte txs `Held`, 620 `Invalid`; one 768 KiB blob `Held`, one
    byte more `Invalid`; `evm_tx_gas(0, 13_000_000)` `Held`, `13_000_001` `Invalid`; with M = 0 (parent
    `lane_block(49, 0)`), a 300 KiB blob at 50 is `Invalid`.
  - `crates/torus-integration-tests/tests/o5_feed_gates.rs`, `worst_case_compact_proposal_fits_consensus_accept_gate`
    (:23) rewritten from the vote caps. A structural guard, not a RED test (Codex plan check s125): its constants landed in
    C1 and it calls no vote code, so it passes before B10; it must stay green through B11, which turns the old
    version's `evm_block_gas_budget() / 16` (312.5 KB) into `u64::MAX / 16`:
    ```rust
    use torus_mempool::rate_limit::{native_total_block_cap, NATIVE_ORDERS_PER_BATCH_CAP};
    use torus_types::{
        EVM_BIG_BLOCK_GAS_LIMIT_RANGE, EVM_BLOCK_GAS_LIMIT_RANGE, EVM_MIN_TX_GAS, MAX_EVM_BIG_BLOCK_TX_BYTES,
        MAX_EVM_BLOCK_TX_BYTES,
    };

    /// The vote caps a block's EVM txs (item 7): at most 768 KiB of raw tx bytes at a big height
    /// (256 KiB otherwise) and at most (small + big limit) / 21,000 txs, each framed by a u64
    /// length in bincode. A compact proposal carries them INLINE (`CompactBlock.evm_transactions`):
    /// the worst legal proposal must fit the consensus accept gate, or every receiver drops it and
    /// penalizes the proposer while the next leader re-selects the same mempool (livelock).
    #[test]
    fn worst_case_compact_proposal_fits_consensus_accept_gate() {
        assert!(MAX_EVM_BIG_BLOCK_TX_BYTES >= MAX_EVM_BLOCK_TX_BYTES);
        let max_txs = ((*EVM_BLOCK_GAS_LIMIT_RANGE.end() + *EVM_BIG_BLOCK_GAS_LIMIT_RANGE.end())
            / EVM_MIN_TX_GAS) as usize; // 1,904
        let evm_worst = MAX_EVM_BIG_BLOCK_TX_BYTES + 8 * max_txs + 8; // 801,672
        let manifest = native_total_block_cap() * 32 + 8; // 12,808 at the compiled cap
        let header_slack = 16 * 1024; // header, justify QC signatures, Proposal framing
        let worst = evm_worst + manifest + header_slack; // 830,864
        let accept = NetworkConfig::default().max_consensus_message_size;
        assert!(
            worst <= accept,
            "worst-case compact proposal {worst}B exceeds consensus accept gate {accept}B \
             — receivers drop+penalize, proposer livelocks (O5)"
        );
    }
    ```
- **Implementation:** `evm_body_fits_header` (app.rs:2243-2273):
  ```rust
      // Item 7 steps 1-2: only on an EVM height (h % N == 0, or a big height), at most cap / 21,000
      // txs (undecodable txs declare no gas, so the gas rule alone does not bound them), at most the
      // height's byte cap (256 KiB, 768 KiB at a big height); the cap is the small limit, plus the big
      // limit at a big height.
      let gas_cap = header.evm_block_gas_cap();
      let max_txs = gas_cap / torus_types::EVM_MIN_TX_GAS;
      let byte_cap = header.evm_block_byte_cap();
      let bytes: usize = evm_txs.iter().map(Vec::len).sum();
      if !header.is_evm_height() || evm_txs.len() as u64 > max_txs || bytes > byte_cap {
          tracing::warn!(
              height = header.height,
              n = header.evm_block_interval,
              m = header.evm_big_block_interval,
              txs = evm_txs.len(),
              max_txs,
              bytes,
              byte_cap,
              "not voting -- EVM txs off the cadence or over the count/byte cap"
          );
          return false;
      }
      // declared-gas fold unchanged; compare with `gas_cap` instead of `header.evm_gas_limit`
  ```
  Pre-vote only, as step 1 (a certified block had f + 1 honest votes). `check_parent_link` pins N and M before the
  same vote.
- **Verify:** `$L cargo nextest run $P $F -E 'test(evm_txs_at_a_big_height_off_the_small_cadence) or test(big_height_caps_count_bytes_and_gas) or test(worst_case_compact_proposal) or test(before_the_vote)'`
- **Depends on:** B2, B6

### B11: two pools, one lane per sender; the node budget loses its default (decision 6)
- **Test first** (`crates/torus-mempool/src/lib.rs` tests; helpers `setup` 1832, `intrinsic_case` 4350,
  `create_eip1559_tx`, `eip1559_with` 4382; lane rows via `state.put_cf_raw(CF_NATIVE_ORACLE, &evm_lane_key(..),
  &[1])`):
  - `big_lane_sender_goes_to_the_big_pool_only_while_big_blocks_are_on`: with M = 0 the row is ignored
    (`evm_lane_of == Some(Small)`); after `set_big_lane(50, 10_000_000, 1_000_000_000)` the next nonce lands in the big
    pool, `drain_evm` skips it, `drain_evm_big` takes it.
  - `sender_switching_lanes_waits_until_its_other_lane_drains`: a pooled small-lane tx, then the row is set: the next
    tx is `EvmLaneSwitchPending`; after `drain_evm` empties the small pool it is admitted to the big pool.
  - `big_lane_admission_caps_gas_at_the_big_limit_and_bytes_at_768_kib`: big lane 10M admitted, 10M + 1
    `GasLimitExceeded`; a 600 KiB call admitted in the big lane, `TxTooLarge` in the small lane; above 768 KiB refused
    in both.
  - `big_lane_fee_floor_is_the_big_base_fee`: big fee 3 gwei: max fee 2 gwei `FeeTooLow` in the big lane, admitted in
    the small lane.
  - `node_budget_caps_only_the_small_lane`: `evm_block_gas_budget: 1_000_000`: a 1.5M small-lane tx refused, a 5M
    big-lane tx admitted, `drain_evm_big(10M, .., 1 gwei, ..)` takes it.
  - `rate_limit` tests, `evm_block_gas_budget_has_no_default`: `parse_evm_block_gas_budget(None) == u64::MAX`,
    `(Some("abc")) == u64::MAX`, `(Some(" 7000000 ")) == 7_000_000`.
  - `big_blocks_switched_off_move_the_big_pool_to_the_small_lane`: `set_big_lane(0, ..)` after M was 50: the big
    pool's txs are in the small pool (a 600 KiB one is dropped, logged).
  - `lower_big_limit_evicts_big_pool_txs_above_it`.
  - `pending_nonce_spans_both_lanes`.
- **Implementation:**
  - `rate_limit.rs` (14-42): delete `EVM_BLOCK_GAS_BUDGET`;
    ```rust
    /// Per-node gas budget of the SMALL EVM lane (D1, S392; item 7 step 2 decision 6): unset = no cap,
    /// the header limits alone size both lanes. `TORUS_EVM_BLOCK_GAS_BUDGET` stays as an optional
    /// per-node cap for A/B tests; it never caps the big lane. Read once.
    pub fn evm_block_gas_budget() -> u64 {
        static BUDGET: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *BUDGET.get_or_init(|| parse_evm_block_gas_budget(std::env::var("TORUS_EVM_BLOCK_GAS_BUDGET").ok()))
    }

    /// `TORUS_EVM_BLOCK_GAS_BUDGET`: a whole number of gas, else no cap (`u64::MAX`).
    pub fn parse_evm_block_gas_budget(raw: Option<String>) -> u64 {
        raw.and_then(|v| v.trim().parse().ok()).unwrap_or(u64::MAX)
    }
    ```
    and fix the `MempoolConfig::evm_block_gas_budget` doc (lib.rs:70-73).
  - `evm_pool.rs`:
    ```rust
    /// Item 7 step 2: the EVM lane of a pooled tx.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum EvmLane {
        Small,
        Big,
    }

    /// Item 7 step 2: both EVM pools (and, B13, the in-flight filter) under one lock.
    pub(crate) struct EvmLanes {
        pub small: EvmPool,
        pub big: EvmPool,
    }

    impl EvmLanes {
        pub fn new(base_fee: u64) -> Self {
            Self { small: EvmPool::new(base_fee), big: EvmPool::new(base_fee) }
        }
        pub fn pool(&self, lane: EvmLane) -> &EvmPool {
            match lane {
                EvmLane::Small => &self.small,
                EvmLane::Big => &self.big,
            }
        }
        pub fn pool_mut(&mut self, lane: EvmLane) -> &mut EvmPool {
            match lane {
                EvmLane::Small => &mut self.small,
                EvmLane::Big => &mut self.big,
            }
        }
        pub fn lane_of(&self, hash: &B256) -> Option<EvmLane> {
            if self.small.contains(hash) {
                Some(EvmLane::Small)
            } else {
                self.big.contains(hash).then_some(EvmLane::Big)
            }
        }
        pub fn size(&self) -> usize {
            self.small.size() + self.big.size()
        }
    }
    // EvmPool: `pub fn has_sender(&self, s: &Address) -> bool` (by_sender key) and
    // `pub fn take_all(&mut self) -> Vec<Vec<u8>>` (every raw tx, indices emptied).
    ```
  - `Mempool` (lib.rs:157-275): `evm: RwLock<evm_pool::EvmLanes>`; new atomics `current_big_interval` (0),
    `current_big_gas_limit` (0), `current_big_base_fee` (`initial_base_fee`); `pub use evm_pool::EvmLane;`.
    ```rust
        /// Item 7 step 2: a sender's lane for admission: big iff big blocks are on (the last committed
        /// header's M > 0) and its `EvmUserModify` row says so, read from the ingress view. A read error
        /// reads as small (admission is node-local; execution charges the lane its row says).
        fn evm_lane(&self, sender: &Address) -> EvmLane {
            if self.current_big_interval.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                return EvmLane::Small;
            }
            match self.ingress_state().get_cf_raw(
                torus_state::cf::CF_NATIVE_ORACLE,
                &torus_state::cf::evm_lane_key(sender),
            ) {
                Ok(Some(_)) => EvmLane::Big,
                _ => EvmLane::Small,
            }
        }

        /// Item 7 step 2: a lane's admission caps: (tx gas, tx bytes, fee floor).
        fn lane_caps(&self, lane: EvmLane) -> (u64, usize, u64) {
            use std::sync::atomic::Ordering::Relaxed;
            match lane {
                EvmLane::Small => (
                    self.max_evm_tx_gas(),
                    torus_types::MAX_EVM_BLOCK_TX_BYTES,
                    self.current_base_fee.load(Relaxed),
                ),
                EvmLane::Big => (
                    self.current_big_gas_limit.load(Relaxed),
                    torus_types::MAX_EVM_BIG_BLOCK_TX_BYTES,
                    self.current_big_base_fee.load(Relaxed),
                ),
            }
        }
    ```
    `admit_evm_tx` (631-699): first refuse `> MAX_EVM_BIG_BLOCK_TX_BYTES` (`TxTooLarge`); call `validate_evm_tx` with
    the loosest caps (`small.0.max(big.0)`, `small.2.min(big.2)`), the sender is known only after recovery; then
    `let lane = self.evm_lane(&entry.sender)` and refuse per lane: bytes over the lane's cap (`TxTooLarge`), gas over it
    (`GasLimitExceeded`), `max_fee_per_gas` below the lane's fee (`FeeTooLow`). Under the write lock: `lane_of(&hash)`
    -> `DuplicateTx`; `pool(other lane).has_sender(sender)` -> new `MempoolError::EvmLaneSwitchPending { sender }`
    ("sender {sender} still has pooled EVM txs in its other lane; resend after they are included"); re-check the
    lane's gas cap; insert / reinsert into `pool_mut(lane)`.
    ```rust
        /// Item 7 step 2: the pool lane of a pooled tx (routing).
        pub fn evm_lane_of(&self, hash: &B256) -> Option<EvmLane> {
            self.evm.read().unwrap().lane_of(hash)
        }

        /// Item 7 step 2: the big lane's part of a big height: up to the header's big limit (the node
        /// budget never caps it) and `byte_budget` (768 KiB minus the small lane's bytes), with the
        /// per-sender share of the big limit, at the proposed header's big base fee (B12).
        pub fn drain_evm_big(
            &self,
            gas_limit: u64,
            byte_budget: usize,
            big_base_fee: u64,
            parent_hash: B256,
        ) -> Vec<Vec<u8>> {
            let sender_gas_cap = match self.config.evm_sender_share_pct {
                0 => None,
                pct => Some(gas_limit.saturating_mul(pct as u64) / 100),
            };
            let min_base_fee = big_base_fee as u128;
            let drained = self.evm.write().unwrap().big.drain(
                gas_limit,
                byte_budget,
                sender_gas_cap,
                &parent_hash,
                min_base_fee,
            );
            let bytes: usize = drained.iter().map(Vec::len).sum();
            if bytes > 0 {
                self.memory_used.fetch_sub(bytes, std::sync::atomic::Ordering::Relaxed);
            }
            drained
        }

        /// Item 7 step 2: the committed header's big lane. M back to 0 moves the big pool to the small
        /// lane (its rules apply; a tx they refuse is dropped with a warning). A lower big limit evicts
        /// the big-pool txs above it, as `set_evm_gas_limit` does for the small lane.
        pub fn set_big_lane(&self, interval: u64, gas_limit: u64, base_fee: u64) {
            use std::sync::atomic::Ordering::Relaxed;
            let moved = {
                let mut lanes = self.evm.write().unwrap();
                let old_interval = self.current_big_interval.swap(interval, Relaxed);
                let old_limit = self.current_big_gas_limit.swap(gas_limit, Relaxed);
                self.current_big_base_fee.store(base_fee, Relaxed);
                lanes.big.set_base_fee(base_fee);
                if gas_limit < old_limit {
                    let (evicted, freed) = lanes.big.evict_gas_above(gas_limit);
                    if evicted > 0 {
                        self.memory_used.fetch_sub(freed, Relaxed);
                        tracing::info!(evicted, gas_limit, "evicted big-lane EVM txs above the lower big limit");
                    }
                }
                if interval == 0 && old_interval > 0 { lanes.big.take_all() } else { Vec::new() }
            };
            if !moved.is_empty() {
                self.memory_used.fetch_sub(moved.iter().map(Vec::len).sum(), Relaxed);
                self.reinsert_evm(moved);
            }
        }

        /// Item 7 step 2: every EVM value of a committed header the pools follow.
        pub fn set_evm_header(&self, header: &torus_types::TorusBlockHeader) {
            self.set_base_fee(header.base_fee_per_gas);
            self.set_evm_gas_limit(header.evm_gas_limit);
            self.set_big_lane(header.evm_big_block_interval, header.evm_big_gas_limit, header.big_base_fee_per_gas);
        }
    ```
    `drain_evm`, `set_base_fee`, `set_evm_gas_limit` work on `.small`; `pending_nonce` walks the small pool, then the
    big pool from where it stopped; `notify_block_committed`, `prune_committed_txs` and `evm_pool_size` cover both.
- **Verify:** `$L cargo nextest run $P $F -E 'test(big_lane_sender_goes_to_the_big_pool) or test(switching_lanes_waits) or test(big_lane_admission_caps) or test(big_lane_fee_floor) or test(node_budget_caps_only_the_small_lane) or test(gas_budget_has_no_default) or test(big_blocks_switched_off_move) or test(lower_big_limit_evicts) or test(pending_nonce_spans_both_lanes) or test(admission_)'`
- **Depends on:** B2, B7

### B12: the proposer drains both lanes at a big height; the pools follow every committed header
- **Test first** (app.rs; helpers `funded_evm_signer` 21652, `funded_evm_mempool` 21674, `signed_eip1559_gas`,
  B10's `lane_block`):
  - `big_height_drains_the_small_lane_then_the_big_lane`: config with `evm_big_gas_limit = 10_000_000`; a small-lane
    1M tx (key 71) and a big-lane 6M tx (key 72, lane row set, `mempool.set_big_lane(10, 10_000_000, 1 gwei)`).
    `build_proposal` on `lane_block(4, 10)` (child 5: small only): the small tx only, the big tx stays pooled. A second
    small tx, `build_proposal` on `lane_block(9, 10)` with big limit 10M (child 10: big): `[small, big]` in that order,
    pool empty, and `pre_vote` of the produced block is `Held`.
  - `committed_header_sets_the_mempool_big_lane` (copy of `committed_header_sets_mempool_gas_cap` 21957): dispatching
    a committed block whose header has M = 50 and big limit 10M routes a lane-row sender's next tx to the big pool.
  - `boundary_drain_uses_the_planned_fee` (Codex plan check s125: `drain_evm` floors at the committed
    `current_base_fee`, mempool lib.rs:737-740, and B11's `drain_evm_big` did the same with the big fee, while the
    header carries `PlanEvm::resolve(parent)`; the two differ at a boundary from C4 on): `epoch4_app`, blocks 1-4
    with N = 1 and M = 0 in their headers and the F3 row seeded to 9M small gas before block 4 (as B9's f3 test), so
    `plan(8).evm.base_fee == Some(1_125_000_000)`; `funded_evm_mempool` holds a `signed_eip1559_gas` tx (max fee 1
    gwei: admitted at the committed 1 gwei, any max fee in [1, 1.125) gwei shows it). `build_proposal` on
    `make_block(7)` with N = 1 (base fee 1 gwei): block 8's header base fee is 1,125,000,000, it carries no EVM tx,
    `evm_pool_size() == 1`. Control: `select_block_payload(.., 1_000_000_000, ..)` drains it.
  - The 8 `select_block_payload` test calls (app.rs 8952, 8963, 9010, 9015, 12367, 12377, 13560, 13568) pass a `0`
    big limit and both fees `1_000_000_000`; the `drain_evm` test calls (mempool lib.rs, 19; app.rs ~22056-22164, 3;
    `rate_limit_mev.rs:282`) pass `mempool.base_fee()` (or the 1 gwei they were admitted at).
- **Implementation:** `build_proposal` (after B6's `evm`):
  ```rust
          // Item 7 steps 1-2: the small lane at h % N == 0 and at a big height, the big lane at a big
          // height. 0 = no destructive drain (the txs stay pooled).
          let big = torus_types::is_big_evm_height(next_height, evm.big_block_interval);
          let gas_limit = if big || next_height.checked_rem(evm.block_interval) == Some(0) {
              evm.gas_limit
          } else {
              0
          };
          let big_gas_limit = if big { evm.big_gas_limit } else { 0 };
  ```
  and `self.select_block_payload(parent_header.height, gas_limit, big_gas_limit, evm.base_fee, evm.big_base_fee,
  parent_header.state_root)`: the fees of the header being proposed (the resolved plan), not the committed ones.
  `select_block_payload` (7855; 7906 on main 7615bb89) gets `big_gas_limit: u64, base_fee: u64, big_base_fee: u64`;
  `drain_evm` becomes `drain_evm(gas_limit, base_fee, parent_hash)` (`min_base_fee = base_fee as u128` replaces the
  `current_base_fee` load, lib.rs:737-740; admission keeps the committed floor); its drain (7961-7966):
  ```rust
          // Item 7 steps 1-2: the small lane first (its limit, 256 KiB), then at a big height the big
          // lane in the bytes left of 768 KiB, each at the proposed header's base fee. 0 = no
          // destructive drain.
          let mut evm = if gas_limit == 0 {
              Vec::new()
          } else {
              mempool.drain_evm(gas_limit, base_fee, parent_state_root)
          };
          if big_gas_limit > 0 {
              let used: usize = evm.iter().map(Vec::len).sum();
              evm.extend(mempool.drain_evm_big(
                  big_gas_limit,
                  torus_types::MAX_EVM_BIG_BLOCK_TX_BYTES.saturating_sub(used),
                  big_base_fee,
                  parent_state_root,
              ));
          }
  ```
  `dispatch_to_exec` (8275-8278): `mempool.set_evm_header(&torus_block.header);`. `try_new`, next to
  `leader_state.set_evm_block_interval(last_header.evm_block_interval)` (5907): the mempool follows `last_header`
  at boot too (`set_evm_header(&last_header)` on the mempool `try_new` holds), so a restarted node admits by the
  current lanes before its first commit.
- **Verify:** `$L cargo nextest run $P $F -E 'test(big_height_drains_the_small_lane_then_the_big_lane) or test(committed_header_sets_the_mempool) or test(boundary_drain_uses_the_planned_fee) or test(select_block_payload) or test(catch_up_gate) or test(deferred_exec_counts_toward_pacing)'`
- **Depends on:** B6, B9 (the boundary fee test), B10, B11

### B13: the in-flight filter (decision 7)
- **Test first:**
  - mempool tests:
    - `committed_block_drops_its_pooled_evm_txs_before_execution`: sender S pools nonces 0-2, T one tx;
      `remove_committed_evm(10, &[raw0, raw1])` returns 2; S's nonce 2 and T's tx stay; `pending_nonce(S) == 3` with
      state nonce 0; every pooled entry's hash equals `keccak256(raw)`.
    - `committed_evm_tx_is_not_readmitted_until_expiry`: `add_evm_tx(raw0)` is `DuplicateTx` until
      `remove_committed_evm(10 + 257, &[])`, then admitted.
    - `lower_nonce_than_a_committed_one_is_refused`: a replacement of nonce 1 (higher fee, new hash) is
      `NonceTooLow { minimum: 2 }`.
    - `inflight_entry_drops_once_the_state_nonce_catches_up`: the account's state nonce set to 2,
      `prune_committed_txs(9)` (below the entry's height, so the nonce rule alone): the entry is gone
      (`pending_nonce` = state nonce + pooled).
    - `own_drained_txs_reserve_their_nonces_at_commit` (Codex plan check s125: the proposer's own txs left the pool
      at drain, lib.rs:741-748, and `own_evm_proposed`, app.rs:7857-7865, keeps only raw bytes, so the
      `remove_by_hash` hit never fires for them): S pools nonces 0 and 1, `drain_evm` takes both;
      `pending_nonce(S) == 0` (no reservation at drain); `remove_committed_evm(10, &[raw0, raw1])` returns 0 (none
      pooled); `pending_nonce(S) == 2` with state nonce 0 (starts at `committed_next` = 1 + 1, nothing pooled from
      2); a replacement of nonce 1 is `NonceTooLow { minimum: 2 }`. A second case drains 0 and 1, then
      `reinsert_evm([raw0, raw1])` (lost proposal): both pooled again, `drained` empty, a commit of neither leaves
      `pending_nonce(S) == 2` from the pool.
    - `skipped_committed_tx_releases_its_nonce_once_its_height_executes` (Codex plan check s125: a committed tx
      that execution skips, executor.rs:328-332, never moves the state nonce, so the entry held the sender ~80 s
      (256 blocks)): S pools nonce 0; `remove_committed_evm(10, &[raw0])`; state nonce stays 0 (fee too low at
      execution); a re-signed nonce-0 tx (higher fee, new hash) is `NonceTooLow { minimum: 1 }` after
      `prune_committed_txs(9)` and admitted after `prune_committed_txs(10)`.
  - app.rs, `commit_prunes_forwarded_copies_from_a_non_proposer_pool`: a node that holds a forwarded tx (not its own
    proposal) runs `on_committed_block` for a compact block carrying it: `evm_pool_size() == 0` before any execution.
- **Implementation:** `EvmLanes` (B11) gains
  ```rust
      /// Decision 7: EVM txs a committed block carried, with that height; admission refuses them
      /// (`DuplicateTx`) for `EVM_INFLIGHT_KEEP_BLOCKS`.
      pub committed: HashMap<B256, u64>,
      /// Decision 7: per sender, the nonce after its highest pooled or drained tx a committed block carried, with
      /// that height: admission refuses lower nonces and `pending_nonce` starts there, until the state
      /// nonce reaches it or the entry expires.
      pub committed_next: HashMap<Address, (u64, u64)>,
      /// Decision 7: this node's drained txs (its own proposals), hash -> (sender, nonce), until a
      /// committed block carries them or they are reinserted. No reservation at drain: a lost
      /// proposal's txs would be refused `NonceTooLow` by `reinsert_evm`.
      pub drained: HashMap<B256, (Address, u64)>,
  ```
  and `EvmLanes::remove_by_hash` (either pool). `EvmPool::drain` gains `drained: &mut HashMap<B256, (Address, u64)>`
  and inserts each selected tx (`drain_evm` / `drain_evm_big` pass `&mut lanes.drained`, a field borrow beside
  `lanes.small` / `lanes.big`); `reinsert_evm` removes each tx's hash from it, also when re-admission refuses the tx;
  `prune_committed_txs` drops `drained` entries whose nonce is below the sender's state nonce (a leak bound: each
  drained tx normally settles at its height through `remove_committed_evm` or `reinsert_lost_evm`). Mempool:
  ```rust
  /// Decision 7: how long (blocks) a committed EVM tx is remembered: above the exec channel (64) plus
  /// parked blocks, so a late copy is refused until execution has confirmed (or skipped) it.
  pub const EVM_INFLIGHT_KEEP_BLOCKS: u64 = 256;

      /// Item 7 step 2 (decision 7, in-flight filter): the EVM txs committed block `height` carries
      /// leave every pool now, not when execution confirms their nonces (the exec lag is ~14 s at
      /// load), so a re-sent or late-forwarded copy is not proposed twice. Returns the txs removed.
      pub fn remove_committed_evm(&self, height: u64, committed: &[Vec<u8>]) -> usize {
          let hashes: Vec<B256> = committed.iter().map(|raw| alloy_primitives::keccak256(raw)).collect();
          let keep_from = height.saturating_sub(EVM_INFLIGHT_KEEP_BLOCKS);
          let (mut removed, mut freed) = (0usize, 0usize);
          {
              let mut lanes = self.evm.write().unwrap();
              lanes.committed.retain(|_, h| *h >= keep_from);
              lanes.committed_next.retain(|_, (_, h)| *h >= keep_from);
              for hash in hashes {
                  lanes.committed.insert(hash, height);
                  let hit = match lanes.remove_by_hash(&hash) {
                      Some(e) => {
                          removed += 1;
                          freed += e.raw_rlp.len();
                          Some((e.sender, e.nonce))
                      }
                      // Our own proposal's txs left the pool at drain.
                      None => lanes.drained.remove(&hash),
                  };
                  if let Some((sender, nonce)) = hit {
                      let next = lanes.committed_next.entry(sender).or_insert((0, height));
                      *next = (next.0.max(nonce + 1), height);
                  }
              }
          }
          if freed > 0 {
              self.memory_used.fetch_sub(freed, std::sync::atomic::Ordering::Relaxed);
          }
          removed
      }
  ```
  `admit_evm_tx`, under the lock after the duplicate check: `lanes.committed.contains_key(&hash)` -> `DuplicateTx`;
  `nonce < committed_next[sender].0` -> `NonceTooLow { sender, have, minimum }` (`reinsert_evm` already treats both as
  done). `pending_nonce` starts at `max(state_nonce, committed_next[sender].0)`. `prune_committed_txs` (1727) becomes
  `prune_committed_txs(executed_height: u64)` and also drops `committed_next` entries whose state nonce reached them
  or whose height is `<= executed_height` (the state nonce is authoritative there: a tx skipped at execution
  releases its nonce then, not after 256 blocks). Its caller (torus-node main.rs:1109) passes the raw DB's applied
  marker (`CF_CONSENSUS_META` `META_NATIVE_APPLIED_HEIGHT`, cf.rs:200, read as app.rs:1229 `native_applied_height`
  does; absent = 0), the same raw DB `prune_committed_txs` reads the state nonces from. Not the handler's `height`:
  that is `find_latest_height`, the newest `CF_BLOCK_HEADERS` row, which `persist_committed_block_durably` writes at
  commit, before execution (app.rs:1593). With checkpoints on, the raw marker lags up to one interval (15 s), still
  far below 256 blocks. Owner decision s125, option (a): a node that never pooled or drained a committed tx does
  NOT ecrecover its sender on the consensus thread; its `pending_nonce` for that sender lags until execution (~14 s
  at load); worst case a different tx with a used nonce is pooled, then skipped at execution and pruned; the
  `committed` hash filter (`DuplicateTx`) still blocks the same tx. Reason: no hot-path cost. app.rs
  `on_committed_block` (7653-7660), before `reinsert_lost_evm`:
  ```rust
          // Item 7 step 2 (decision 7): the in-flight filter (every commit, also to expire entries).
          if let Some(ref mempool) = self.mempool {
              mempool.remove_committed_evm(height, committed_evm);
          }
  ```
  Cost on the consensus thread: one keccak over each committed EVM tx (~1.5 ms for a full 768 KiB big height).
- **Verify:** `$L cargo nextest run $P $F -E 'test(committed_block_drops_its_pooled_evm_txs) or test(committed_evm_tx_is_not_readmitted_until_expiry) or test(lower_nonce_than_a_committed_one) or test(inflight_entry_drops_once_the_state_nonce) or test(own_drained_txs_reserve_their_nonces) or test(skipped_committed_tx_releases_its_nonce) or test(commit_prunes_forwarded_copies) or test(lost_proposal)'`
- **Depends on:** B11

### B14: routing per lane: upcoming N/M, two big leaders, re-send (decision 7)
- **Test first:**
  - app.rs `mod leader_state_tests` (helpers `make_set` 10052, `plain_iwrr` 10077):
    ```rust
    /// Item 7 step 2 (decision 7): a small-lane tx targets the next EVM height (a big height counts),
    /// with the next boundary's N / M from that boundary on. RED before: no lanes, no upcoming N.
    #[test]
    fn evm_targets_follow_the_upcoming_interval_and_big_heights() {
        let ls = LeaderState::new();
        let (set, _) = make_set(3);
        ls.sync_validators(&set);
        ls.observe_start_view(40, plain_iwrr(&ls, 40));
        ls.set_evm_block_interval(5);
        // Tip Torus 98 (hotstuff 97) in view 40; boundary 100 switches N to 10.
        ls.observe_hotstuff_proposal(40, 97);
        ls.set_upcoming(100, 10, 0);
        assert_eq!(ls.next_evm_targets(EvmLane::Small), (100, vec![plain_iwrr(&ls, 42)]));
        // Tip 101 in view 41: N = 10 from 100 on, so 110 (not 105), 8 views after 102's.
        ls.observe_hotstuff_proposal(41, 100);
        assert_eq!(ls.next_evm_targets(EvmLane::Small), (110, vec![plain_iwrr(&ls, 50)]));
        // M = 7 before the boundary: the big height 7 is the small lane's next EVM height too.
        let ls2 = LeaderState::new();
        ls2.sync_validators(&set);
        ls2.observe_start_view(40, plain_iwrr(&ls2, 40));
        ls2.set_evm_block_interval(5);
        ls2.set_evm_big_block_interval(7);
        ls2.observe_hotstuff_proposal(40, 4); // tip 5
        assert_eq!(ls2.next_evm_targets(EvmLane::Small).0, 7);
    }

    /// A big-lane tx goes to the leaders of the next two big heights.
    #[test]
    fn big_lane_targets_the_next_two_big_heights() {
        // N = 5, M = 50, tip 60 in view 40: 100 in view 80, 150 in view 130.
        assert_eq!(ls.next_evm_targets(EvmLane::Big), (150, vec![plain_iwrr(&ls, 80), plain_iwrr(&ls, 130)]));
    }
    ```
    Step 1's `next_evm_leader_*` and `evm_routing_tip_ignores_views_beyond_the_next_one` tests stay green
    (`next_evm_leader` = the small lane's first leader).
  - mempool, `due_resends_are_the_tracked_txs_still_pooled_past_their_target`: two pooled txs tracked at target 10;
    `remove_committed_evm(10, [one])`; `evm_due_resends(9)` is empty, `evm_due_resends(10)` returns the other with its
    bytes and lane and drops it from the tracker; tracked again with `sends = EVM_RESEND_MAX + 1` it never returns.
  - torus-rpc lib.rs (model: step 1's `evm_tx_forwarded_to_the_next_evm_height_leader`),
    `evm_tx_forwarded_to_the_leaders_of_its_lane_and_tracked`: `set_evm_targets_fn` returning `(25, [a, b])` for the
    big lane: a big-lane tx goes to `a` and `b` (not to the node's own key), and the mempool tracks it at 25.
  - app.rs: `target_height_that_commits_without_the_tx_resends_it` (recording `evm_forward` closure, validators synced
    into `app.leader_state`): a tracked pooled tx with target 5, `on_committed_block` of block 5 without it: one
    forward to the predicted leader, re-tracked at the next target; `lost_proposal_txs_are_tracked_for_resend`.
- **Implementation:** `LeaderState` (app.rs:866-1006):
  ```rust
      /// Item 7 step 2: the committed header's M (0 = big blocks off).
      evm_big_block_interval: AtomicU64,
      /// Item 7 step 2 (decision 7): `(boundary, N, M)` of the next boundary from its epoch plan, once
      /// this node can read it; heights from the boundary on are predicted with them.
      upcoming: RwLock<Option<(u64, u64, u64)>>,

      fn set_evm_big_block_interval(&self, m: u64) {
          self.evm_big_block_interval.store(m, Ordering::Relaxed);
      }
      fn set_upcoming(&self, boundary: u64, n: u64, m: u64) {
          *self.upcoming.write().unwrap() = Some((boundary, n, m));
      }
      fn upcoming_boundary(&self) -> Option<u64> {
          self.upcoming.read().unwrap().map(|(b, _, _)| b)
      }
      /// N and M in force at `height`: the upcoming boundary's from it on, else the committed ones.
      fn intervals_at(&self, height: u64) -> (u64, u64) {
          match *self.upcoming.read().unwrap() {
              Some((boundary, n, m)) if height >= boundary => (n, m),
              _ => (
                  self.evm_block_interval.load(Ordering::Relaxed),
                  self.evm_big_block_interval.load(Ordering::Relaxed),
              ),
          }
      }

      /// Item 7 step 2 (decision 7): where an EVM tx of `lane` goes: the next EVM height for a
      /// small-lane tx (a big height drains the small lane too), the next two big heights for a
      /// big-lane tx (one view timeout must not cost a whole big interval). Returns the last target
      /// (the re-send deadline) and one leader per target (plain IWRR, as step 1). No target within
      /// the scan, no validator set, or overflowing tip values: the current leader.
      pub fn next_evm_targets(&self, lane: torus_mempool::EvmLane) -> (u64, Vec<VerifyingKey>) {
          const SCAN: u64 = 1 << 16;
          let (tip_view, tip_height) = *self.tip.read().unwrap();
          let want = if lane == torus_mempool::EvmLane::Big { 2 } else { 1 };
          let mut targets = Vec::with_capacity(want);
          let mut h = tip_height;
          while targets.len() < want && h < tip_height.saturating_add(SCAN) {
              h += 1;
              let (n, m) = self.intervals_at(h);
              let hit = match lane {
                  torus_mempool::EvmLane::Big => torus_types::is_big_evm_height(h, m),
                  torus_mempool::EvmLane::Small => torus_types::is_evm_height(h, n, m),
              };
              if hit {
                  targets.push(h);
              }
          }
          let fallback = || (tip_height.saturating_add(1), self.current_leader().into_iter().collect());
          let Some(&last) = targets.last() else {
              return fallback();
          };
          let Some(first_view) = tip_view.checked_add(1).map(|v| self.current_view().max(v)) else {
              return fallback();
          };
          let leaders: Option<Vec<VerifyingKey>> = {
              let vs = self.validators.read().unwrap();
              (!vs.is_empty()).then(|| {
                  targets
                      .iter()
                      .filter_map(|&h_e| first_view.checked_add(h_e - (tip_height + 1)))
                      .map(|view| hotstuff_rs::pacemaker::select_leader(ViewNumber::new(view), &vs))
                      .collect()
              })
          };
          match leaders {
              Some(l) if !l.is_empty() => (last, l),
              _ => fallback(),
          }
      }

      /// Step 1's API: the leader of the next small-lane EVM height.
      pub fn next_evm_leader(&self) -> Option<VerifyingKey> {
          self.next_evm_targets(torus_mempool::EvmLane::Small).1.first().copied()
      }
  ```
  `TorusApp`: field `evm_forward: Option<Arc<dyn Fn([u8; 32], Vec<u8>) + Send + Sync>>` (init `None` beside
  `own_evm_proposed` 5927) and `pub fn set_evm_forward(..)`. `on_committed_block`, in the `height > last_header.height`
  branch (7704-7712): `set_evm_big_block_interval(header.evm_big_block_interval)` and `self.refresh_upcoming_evm(&header)`;
  after the in-flight filter (B13) and `reinsert_lost_evm`:
  ```rust
          // Item 7 step 2 (decision 7): txs whose target EVM height has committed without them (they
          // are still pooled) go to the next predicted leader(s).
          if let (Some(mempool), Some(forward)) = (self.mempool.as_ref(), self.evm_forward.as_ref()) {
              for r in mempool.evm_due_resends(height) {
                  let (target, leaders) = self.leader_state.next_evm_targets(r.lane);
                  for vk in leaders {
                      forward(vk.to_bytes(), r.raw.clone());
                  }
                  mempool.track_evm_forward(r.hash, target, r.sends + 1);
              }
          }

      /// Item 7 step 2 (decision 7): hand the routing prediction the next boundary's N and M once its
      /// plan is readable (quietly; until then the committed values predict).
      fn refresh_upcoming_evm(&self, header: &TorusBlockHeader) {
          if self.epoch_length == 0 {
              return;
          }
          let boundary = (header.height / self.epoch_length + 1).saturating_mul(self.epoch_length);
          if self.leader_state.upcoming_boundary() == Some(boundary) {
              return;
          }
          let ready = matches!(self.applied_height(), Ok(Some(a)) if a >= boundary.saturating_sub(self.epoch_length));
          if !ready {
              return;
          }
          if let Ok(plan) = self.staking.epoch_rotation_plan(boundary) {
              let v = plan.map(|p| p.evm).unwrap_or_default().resolve(header);
              self.leader_state.set_upcoming(boundary, v.block_interval, v.big_block_interval);
          }
      }
  ```
  `reinsert_lost_evm` (7806): hash the lost txs before `reinsert_evm`, then track each one still pooled at
  `next_evm_targets(lane).0`. Mempool tracker:
  ```rust
  /// Decision 7: a forwarded tx is re-sent at most this many times; then it waits in the pool for this
  /// node's own EVM turn.
  pub const EVM_RESEND_MAX: u8 = 8;

  /// Decision 7: one tracked EVM tx due for a re-send.
  pub struct EvmResend {
      pub hash: B256,
      pub raw: Vec<u8>,
      pub lane: EvmLane,
      pub sends: u8,
  }
      // Mempool field: evm_resend: std::sync::Mutex<std::collections::BTreeMap<u64, Vec<(B256, u8)>>>,

      /// Decision 7: remember a forwarded tx and the EVM height it targets.
      pub fn track_evm_forward(&self, hash: B256, target: u64, sends: u8) {
          if sends <= EVM_RESEND_MAX {
              self.evm_resend.lock().unwrap().entry(target).or_default().push((hash, sends));
          }
      }

      /// Decision 7: the tracked txs whose target is at or below the committed `height` and that are
      /// still pooled (the in-flight filter removed the committed ones), with their bytes and current
      /// lane. They leave the tracker; the caller re-sends and re-tracks them.
      pub fn evm_due_resends(&self, height: u64) -> Vec<EvmResend> {
          let due: Vec<(B256, u8)> = {
              let mut tracked = self.evm_resend.lock().unwrap();
              let later = tracked.split_off(&height.saturating_add(1));
              std::mem::replace(&mut *tracked, later).into_values().flatten().collect()
          };
          let lanes = self.evm.read().unwrap();
          due.into_iter()
              .filter_map(|(hash, sends)| {
                  let lane = lanes.lane_of(&hash)?;
                  let raw = lanes.pool(lane).raw_of(&hash)?;
                  Some(EvmResend { hash, raw, lane, sends })
              })
              .collect()
      }
      // EvmPool::raw_of(&self, hash) -> Option<Vec<u8>>: by_hash -> by_sender -> raw_rlp.clone()
  ```
  torus-rpc: `evm_leader_vk_fn` (lib.rs:276) becomes
  `evm_targets_fn: Option<Arc<dyn Fn(torus_mempool::EvmLane) -> (u64, Vec<[u8; 32]>) + Send + Sync>>` with
  `set_evm_targets_fn` (replacing `set_evm_leader_fn`, 431); `forward_evm_to_leader(raw, hash)` (torus.rs:948):
  ```rust
      pub(crate) fn forward_evm_to_leader(&self, raw_rlp: Vec<u8>, hash: B256) {
          let (Some(own_vk), Some(fwd_tx)) = (self.own_vk.as_ref(), self.forward_evm_tx.as_ref()) else {
              return;
          };
          match self.evm_targets_fn.as_ref() {
              // Item 7 step 2 (decision 7): the leaders of the tx's lane's next EVM height(s); the
              // mempool tracks the target for a re-send.
              Some(targets) => {
                  let lane = self.mempool.evm_lane_of(&hash).unwrap_or(torus_mempool::EvmLane::Small);
                  let (target, leaders) = targets(lane);
                  for vk in leaders.into_iter().filter(|vk| vk != own_vk) {
                      let _ = fwd_tx.send((vk, raw_rlp.clone()));
                  }
                  self.mempool.track_evm_forward(hash, target, 0);
              }
              // No consensus attached: the current leader, as before item 7.
              None => {
                  if let Some(vk) = self.leader_vk_fn.as_ref().and_then(|f| f()) {
                      if vk != *own_vk {
                          let _ = fwd_tx.send((vk, raw_rlp));
                      }
                  }
              }
          }
      }
  ```
  eth.rs `send_raw_transaction` (798-817): `self.forward_evm_to_leader(raw_for_forward, hash);`. main.rs: move
  `let (evm_fwd_tx, mut evm_fwd_rx) = tokio::sync::mpsc::unbounded_channel();` (1211) above
  `ReplicaSpec::builder()` (1045), which consumes `app`; beside `app.set_fill_sink` (1006):
  ```rust
      // Item 7 step 2 (decision 7): commit-path re-sends ride the RPC's EVM forward channel.
      let evm_fwd_for_app = evm_fwd_tx.clone();
      app.set_evm_forward(Arc::new(move |vk: [u8; 32], raw: Vec<u8>| {
          if vk != own_vk {
              let _ = evm_fwd_for_app.send((vk, raw));
          }
      }));
  ```
  and (1223-1228) `rpc_server.set_evm_targets_fn(Arc::new(move |lane| { let (target, vks) =
  leader_state_for_evm.next_evm_targets(lane); (target, vks.iter().map(|vk| vk.to_bytes()).collect()) }));`.
  Prediction stays plain IWRR (reputation weighting off), as step 1.
- **Verify:** `$L cargo nextest run $P $F -E 'test(evm_targets_follow_the_upcoming_interval) or test(big_lane_targets_the_next_two_big_heights) or test(due_resends_are_the_tracked_txs_still_pooled) or test(forwarded_to_the_leaders_of_its_lane) or test(commits_without_the_tx_resends_it) or test(lost_proposal_txs_are_tracked) or test(next_evm_leader) or test(evm_routing_tip)'`
- **Depends on:** B12, B13

### B15: CoreWriter order delay K and the 200-row cap (decision 4)
- **Test first:**
  - `crates/torus-core/tests/precompile_tests.rs` (helpers `setup`, `addr`, `fp`, `drain_ok` 750):
    ```rust
    /// Item 7 step 2 (owner decision 4): order actions wait K blocks (default 10, governance
    /// `core_writer_order_delay`), other kinds one; an invalid stored K keeps 10. RED before: all +1.
    #[test]
    fn order_actions_wait_k_blocks_others_one() {
        let (_dir, db) = setup();
        let queue = |kind| {
            CoreWriterQueue::enqueue(&db, &QueuedAction { trader: addr(1), kind, block_queued: 100 }).unwrap()
        };
        queue(QueuedActionKind::PlaceOrder {
            market_id: 1, side: 0, order_type: 0, price: fp(50_000), quantity: fp(5), time_in_force: 0, cloid: None,
        });
        queue(QueuedActionKind::CancelOrder { order_id: 1 });
        queue(QueuedActionKind::CancelAll { market_id: 1 });
        queue(QueuedActionKind::CancelByCloid { market_id: 1, cloid: 7 });
        queue(QueuedActionKind::ClaimRewards);
        queue(QueuedActionKind::LockboxDeposit { amount: fp(1) });
        assert_eq!(CoreWriterQueue::pending_count(&db, 101).unwrap(), 2);
        assert_eq!(CoreWriterQueue::pending_count(&db, 110).unwrap(), 4);
        for (stored, target) in [("3", 103), ("0", 110), ("101", 110), ("x", 110)] {
            let (_dir, db) = setup();
            db.put_cf_raw(CF_FEE_CONFIG, torus_types::CORE_WRITER_ORDER_DELAY_PARAM.as_bytes(), stored.as_bytes())
                .unwrap();
            CoreWriterQueue::enqueue(
                &db,
                &QueuedAction { trader: addr(1), kind: QueuedActionKind::CancelOrder { order_id: 1 }, block_queued: 100 },
            )
            .unwrap();
            assert_eq!(CoreWriterQueue::pending_count(&db, target).unwrap(), 1, "{stored}");
        }
    }
    ```
    and `a_full_target_block_sends_later_rows_to_the_next_block`: 201 `CancelOrder` rows queued at block 100 give
    200 at 110 (seqs 0-199) and 1 at 111 (seq 0); a `ClaimRewards` queued at 109 (target 110, full) gets 111 seq 1;
    `drain_ok(&db, 111)` is `[CancelOrder { order_id: 200 }, ClaimRewards]`.
  - `lowering_k_never_lets_a_cancel_overtake_an_earlier_order` (Codex plan check s125: K is read at enqueue and the
    drain is strictly by target prefix, precompiles.rs:1341-1342, so K 10 -> 1 would drain a cancel queued at 102 at
    103, before a place queued at 100 drains at 110, against owner answer 3): `PlaceOrder` queued at 100 with K = 10
    (target 110); the K row set to `"1"`; `CancelOrder` queued at 102 (target max(103, 110) = 110):
    `pending_count(&db, 103) == 0` and `drain_ok(&db, 110)` is `[PlaceOrder { .. }, CancelOrder { .. }]` in that
    order. Then a `CancelOrder` queued at 115 targets 116 (the delay is back to K once the last target is passed),
    and a `ClaimRewards` queued at 102 still targets 103 (other kinds ignore the row).
  - app.rs, `corewriter_order_drains_k_blocks_later_with_the_pipeline_and_interval` (fixture modelled on
    `cw_due_blocks` 23745, configs of `corewriter_evm_row_drains_in_empty_next_block_with_interval` 23881: pipeline off,
    interval 0, 15000, never): block 2 carries a lockbox deposit and a CoreWriter (0x0810) `cancelAll(bytes32)` call;
    blocks 3-12 are empty. After 2: queue rows `(3, 0)` and `(12, 0)` in the raw DB. After 3: the deposit is credited,
    `(12, 0)` remains. After 12: the queue CF is empty and block 12's action status has one CoreWriter result of kind
    `CancelAll`. Dumps equal across the four configs.
  - Existing CoreWriter tests whose subject is not the delay pin K = 1 with one line in their fixture
    (`put_cf_raw(CF_FEE_CONFIG, b"core_writer_order_delay", b"1")`): `precompile_tests.rs` (the queue and
    `core_writer_*` tests, ~486-960), `torus-bridge/tests/corewriter_cloid_tests.rs`, `corewriter_results_tests.rs`,
    `torus-integration-tests/tests/cross_vm_write.rs`, the app.rs CoreWriter v4/v5 status tests (~24195-24400; their
    seq expectations assume one target), and `torus-evm/tests/evm_tests.rs` CoreWriter calls (~986-1030, ~1826) where
    they read the target. The lockbox tests (`corewriter_evm_row_drains_in_empty_next_block*`) stay as they are.
- **Implementation** (`torus-core/src/precompiles.rs`):
  ```rust
  impl QueuedActionKind {
      /// Item 7 step 2 (owner decision 4): order actions (the placement and every cancel, so a
      /// contract's cancel never overtakes its order) wait K blocks; staking and lockbox kinds one.
      pub fn is_order(&self) -> bool {
          matches!(
              self,
              Self::PlaceOrder { .. } | Self::CancelOrder { .. } | Self::CancelAll { .. } | Self::CancelByCloid { .. }
          )
      }
  }

  /// Queue key: target block (8 BE) ‖ seq (8 BE).
  fn queue_key(target_block: u64, seq: u64) -> [u8; 16] {
      let mut key = [0u8; 16];
      key[..8].copy_from_slice(&target_block.to_be_bytes());
      key[8..].copy_from_slice(&seq.to_be_bytes());
      key
  }

      /// Enqueue a CoreWriter action. Item 7 step 2 (owner decision 4): an order action targets
      /// `block_queued + K` (governance `core_writer_order_delay`, default 10), any other kind
      /// `block_queued + 1`; a target that already holds `CORE_WRITER_DRAIN_CAP` rows sends the row on
      /// to the next block with room, so no block drains more than the cap, in queue order. Rows are
      /// queued only by EVM blocks (serial barriers), so each is in the raw DB before its target's
      /// due-check (`NativeExecutor::core_writer_due`). Returns the row's seq.
      pub fn enqueue(state_db: &impl StateBackend, action: &QueuedAction) -> Result<u64, CoreError> {
          let delay = if action.kind.is_order() {
              let stored = state_db
                  .get_cf_raw(CF_FEE_CONFIG, torus_types::CORE_WRITER_ORDER_DELAY_PARAM.as_bytes())?;
              torus_types::gov_u64_in(stored.as_deref(), &torus_types::CORE_WRITER_ORDER_DELAY_RANGE)
                  .unwrap_or(torus_types::DEFAULT_CORE_WRITER_ORDER_DELAY)
          } else {
              1
          };
          let mut target_block = action.block_queued.saturating_add(delay);
          // Owner answer 3: an order kind never targets a block before the previous order kind's
          // target, so lowering K cannot let a cancel overtake its order.
          let order = action.kind.is_order();
          if order {
              let last = state_db.get_cf_raw(CF_FEE_CONFIG, LAST_ORDER_TARGET_KEY)?;
              let last = last.as_deref().and_then(|v| <[u8; 8]>::try_from(v).ok()).map_or(0, u64::from_be_bytes);
              target_block = target_block.max(last);
          }
          // A target's seqs are dense from 0 (rows leave only when the whole target drains), so it is
          // full iff its last slot is taken: one point read per full block skipped.
          while state_db
              .get_cf_len(CF_CORE_WRITER_QUEUE, &queue_key(target_block, torus_types::CORE_WRITER_DRAIN_CAP - 1))?
              .is_some()
          {
              target_block += 1;
          }
          let seq = Self::next_sequence(state_db, target_block)?;
          let data = borsh::to_vec(action).map_err(|e| CoreError::Borsh(e.to_string()))?;
          state_db.put_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(target_block, seq), &data)?;
          if order {
              // The target after the cap: a later order kind never lands before this row.
              state_db.put_cf_raw(CF_FEE_CONFIG, LAST_ORDER_TARGET_KEY, &target_block.to_be_bytes())?;
          }
          Ok(seq)
      }

  /// Item 7 step 2 (owner answer 3): `CF_FEE_CONFIG` row of the last order-kind target (u64 BE).
  /// Not a governance key (not in the allowlist, no proposal sets it), and not in
  /// `CF_CORE_WRITER_QUEUE`, whose drain takes every row under the target prefix.
  const LAST_ORDER_TARGET_KEY: &[u8] = b"core_writer_last_order_target";
  ```
  Order-kind target = max(`block_queued + K`, last order-kind target), then the 200 cap on top (see the test
  above). Deterministic: the row is written at execution into hashed state (`CF_FEE_CONFIG`,
  running hash id 14, running_hash.rs:49), the same on every node, and only by EVM blocks (serial barriers), like
  the queue rows. Cost: one extra point read and one put per order-kind CoreWriter action (not per fill, not per
  native block). A lowered K takes full effect once the old last target has passed (at most the old K blocks; the
  rollback lever `core_writer_order_delay` 1 included). Tests that dump `CF_FEE_CONFIG` or pin a
  CoreWriter block's hashed write set add the row.
  `next_sequence` scans at most 200 rows now (today a 30M block could put ~1,500 rows under one target and every
  enqueue rescans them). Docs to fix: `CoreWriterOrigin::target_block` (torus-evm executor.rs:144, "this block + 1"),
  `NativeExecutor::core_writer_due` (12219-12226) and `drain_core_writer` (12238).
- **Verify:** `$L cargo nextest run $P $F -E 'test(order_actions_wait_k_blocks_others_one) or test(full_target_block_sends_later_rows) or test(lowering_k_never_lets_a_cancel_overtake) or test(corewriter_order_drains_k_blocks_later) or test(corewriter_evm_row_drains) or test(queue_) or test(core_writer_)'`
- **Depends on:** B1, B5

### B16: bbo rows per dirty market in both save paths
- **Test first:**
  - `crates/torus-bridge/tests/bbo_rows_tests.rs` (new; book fixtures as in `resident_books_tests.rs`):
    - `dirty_market_bbo_row_follows_the_book_in_every_mode`: for `Classic`, `OrderRows`, `LevelAuthority`,
      `LevelAuthorityChunked`: bid 99 and ask 101 placed, saved -> row(1) = (99, 101) raw; the ask cancelled, saved ->
      (99, 0); the bid cancelled, saved -> no row. Modes 2/3 also through `save_order_books_deferred`.
    - `unchanged_top_of_book_writes_no_bbo_row`: a bid below the best, saved: no `cf_native_bbo` put (the counting
      backend of `tests/common/counting_backend.rs`).
    - `bbo_cf_is_neither_hashed_nor_in_the_native_root`: `hashed_cf_id(CF_NATIVE_BBO) == None`,
      `native_trie::cf_tag(CF_NATIVE_BBO) == None`, `ALL_CF_NAMES` contains it.
  - The C5 differential (`warm_equals_cold_every_block_{serial,pipelined,pipelined_level_authority_chunked}`, app.rs
    25737-25754) compares warm, restarted and cache-dropped replicas' full CF dumps, so it covers the bbo rows (cold
    `saved_bbo`, all paths). Add a non-vacuous assertion: the warm dump holds at least one `cf_native_bbo` row.
- **Implementation:**
  ```rust
  // torus-state cf.rs, appended to ALL_CF_NAMES too (intern_cf ids are positional: append only)
  /// Item 7 step 2 (owner decision 3): the best bid / ask per market the 0x080e precompile reads.
  /// Key: market id (8 BE). Value: best bid raw (i128 BE) ‖ best ask raw (i128 BE), 0 for an empty
  /// side; no row while the book is empty. Written at block end for every market whose book changed.
  /// Derived from the book rows: neither hashed nor in the native root (an EVM read of a wrong row
  /// would change the hashed EVM state, so a divergence is still caught).
  pub const CF_NATIVE_BBO: &str = "cf_native_bbo";

  // torus-core order_book.rs: field (in-RAM only, `None` in both literals, 1400 and 4671)
      /// Item 7 step 2: the bbo row last written for this book. Never serialized; `None` after a load
      /// (the next save writes the row again; the CF is unhashed, so a repeated put changes nothing).
      saved_bbo: Option<[u8; 32]>,

      /// Item 7 step 2: this book's bbo row: best bid raw ‖ best ask raw (i128 BE, 0 = empty side).
      pub fn bbo_row(&self) -> [u8; 32] {
          let mut row = [0u8; 32];
          row[..16].copy_from_slice(&self.best_bid().map_or(0, |p| p.raw()).to_be_bytes());
          row[16..].copy_from_slice(&self.best_ask().map_or(0, |p| p.raw()).to_be_bytes());
          row
      }
      pub fn saved_bbo(&self) -> Option<[u8; 32]> {
          self.saved_bbo
      }
      pub fn set_saved_bbo(&mut self, row: [u8; 32]) {
          self.saved_bbo = Some(row);
      }

  // torus-bridge native_executor.rs
      /// Item 7 step 2 (owner decision 3): the bbo row of every dirty market, written only when the top
      /// of its book moved since this book's last write; an emptied book deletes it. One overlay point
      /// put, no read, no Borsh.
      fn write_bbo_rows(&mut self) {
          use torus_state::cf::CF_NATIVE_BBO;
          for &market_id in &self.dirty_books {
              let Some(book) = self.order_books.get_mut(&market_id) else {
                  continue;
              };
              let row = book.bbo_row();
              if book.saved_bbo() == Some(row) {
                  continue;
              }
              let key = market_id.to_be_bytes();
              let res = if row == [0u8; 32] {
                  self.state.delete_cf_raw(CF_NATIVE_BBO, &key)
              } else {
                  self.state.put_cf_raw(CF_NATIVE_BBO, &key, &row)
              };
              match res {
                  Ok(()) => book.set_saved_bbo(row),
                  Err(e) => latch_save_fault(
                      &mut self.fatal_error,
                      format!("book save: bbo row (market {market_id}): {e}"),
                  ),
              }
          }
      }
  ```
  Called once in `save_order_books` before the counter write (6321) and once in `save_order_books_deferred` before
  its counter write (6520). The returned write count stays the book rows' (tests use it). Delist: no execution path
  exists (open question 8).
- **Verify:** `$L cargo nextest run $P $F -E 'test(dirty_market_bbo_row_follows_the_book) or test(unchanged_top_of_book_writes_no_bbo_row) or test(bbo_cf_is_neither_hashed) or test(warm_equals_cold_every_block)'`
- **Depends on:** none

### B17: the bbo read precompile at 0x080e
- **Test first:**
  - `precompile_tests.rs`: `bbo_reads_the_rows_bid_and_ask` (row present -> the two words; no row -> zeros; another
    selector -> `UnknownSelector`; input shorter than selector + one word -> `InvalidPrecompileInput`; a row of the
    wrong length -> an error, never zeros) and `bbo_is_a_reader_at_0x080e` (`is_precompile`, `is_reader_precompile`,
    `precompile_gas == GAS_PRECOMPILE_READ`, `ALL_PRECOMPILE_ADDRESSES` holds it, `execute_precompile_metered` with
    `read_only = true` succeeds).
  - `evm_tests.rs`, `bbo_call_costs_exactly_16440_gas`: a tx calling 0x080e with `bbo(bytes32)` for a market with a
    row succeeds with `gas_used == intrinsic + 16_440` (intrinsic as in
    `removed_get_open_orders_reverts_for_exactly_the_base` ~618) and returns the row's prices.
- **Implementation** (`torus-core/src/precompiles.rs`): `pub const ADDR_BBO_READER: u16 = 0x080e;` (36);
  `precompile_gas` (215) `0x0800..=0x0803 | ADDR_BBO_READER => GAS_PRECOMPILE_READ`; `ALL_PRECOMPILE_ADDRESSES`
  becomes `[Address; 8]` with it; `is_precompile` (246) adds `| 0x080e`; `is_reader_precompile` (254)
  `matches!(id, ADDR_ORDER_BOOK_READER..=ADDR_STAKING_READER | ADDR_BBO_READER)`; dispatch (588-600)
  `ADDR_BBO_READER => read(bbo_reader(input, state_db, meter), meter),`; module doc map gets the line.
  ```rust
  // ============================================================================
  // BboReader (0x080e) — item 7 step 2 (HL's bbo address)
  // ============================================================================

  /// `bbo(bytes32 marketId) -> (uint128 bid, uint128 ask)`: the market's best bid and ask as of the
  /// last block that changed its book (`cf_native_bbo`), 0 for an empty side or an unknown market.
  /// Owner decision 3, our metering, priced like the other point readers (the point read is in the
  /// base, owner s125): 16,400 + 2 x 20 = 16,440 gas.
  fn bbo_reader(input: &[u8], state_db: &impl StateBackend, meter: &mut ReadMeter) -> Result<Vec<u8>, CoreError> {
      let sel = abi::selector(input)?;
      if sel != selector_for("bbo(bytes32)") {
          return Err(CoreError::UnknownSelector(sel));
      }
      let market_id = abi::decode_market_id(&abi::word(input, 0)?);
      let (bid, ask) = match state_db.get_cf_raw(CF_NATIVE_BBO, &market_id.to_be_bytes())? {
          None => (0i128, 0i128),
          Some(r) if r.len() == 32 => (
              i128::from_be_bytes(r[..16].try_into().expect("16 bytes")),
              i128::from_be_bytes(r[16..].try_into().expect("16 bytes")),
          ),
          Some(r) => {
              return Err(CoreError::InvalidPrecompileInput(format!(
                  "bbo row of market {market_id}: {} bytes, not 32",
                  r.len()
              )))
          }
      };
      let mut out = Vec::with_capacity(64);
      out.extend_from_slice(&abi::encode_u128(bid as u128));
      out.extend_from_slice(&abi::encode_u128(ask as u128));
      Ok(out)
  }
  ```
- **Verify:** `$L cargo nextest run $P $F -E 'test(bbo_)'`
- **Depends on:** B16

### B18: RPC per lane
- **Test first** (`crates/torus-rpc/tests/eth_compliance_tests.rs`; `store_head_with_base_fee` ~360 gets M, the big
  limit and the big base fee; lane rows written into the test DB):
  - `estimate_gas_caps_at_the_callers_lane_limit`: header small 3M, big 10M, M = 50: a 5M call from a big-lane
    caller estimates under 10M; from a small-lane caller it is capped at 3M (fails above it); with M = 0 the big-lane
    caller is capped at 3M.
  - `big_block_gas_price_and_fee_history_per_lane`: `torus_bigBlockGasPrice` = the head's big base fee;
    `eth_feeHistory(.., "big")` returns big base fees, absent or `"small"` the small ones, `"other"` is
    `InvalidParams`.
  - `using_big_blocks_reads_the_lane_row`: true with the row, false without.
  - `block_gas_limit_is_the_heights_cap`: `eth_getBlockByNumber` of height 100 (M = 50) reports 13M, of 105 3M.
- **Implementation:**
  ```rust
  // eth.rs, after block_env_from_header (267-275)
  /// Item 7 step 2: the env of a call from `caller` on `header`: its lane's gas limit and base fee
  /// (the big lane while big blocks are on and its `EvmUserModify` row says so).
  fn lane_block_env(state: &StateDb, header: &TorusBlockHeader, caller: &Address) -> BlockEnvCfg {
      let mut env = block_env_from_header(header);
      let big = header.evm_big_block_interval > 0
          && matches!(
              state.get_cf_raw(torus_state::cf::CF_NATIVE_ORACLE, &torus_state::cf::evm_lane_key(caller)),
              Ok(Some(_))
          );
      if big {
          env.gas_limit = header.evm_big_gas_limit;
          env.base_fee = header.big_base_fee_per_gas;
      }
      env
  }
  ```
  `call` and `estimate_gas` (1111): `let caller = match &tx.from { Some(f) => parse_address(f).map_err(err)?, None =>
  Address::ZERO };` and `Some((h, _, _)) => lane_block_env(&self.state, &h, &caller)`. `fee_history` (1243) gets
  `block_type: Option<String>` (trait 110):
  ```rust
          // Item 7 step 2: "big" = the big lane's base fees; absent or "small" = the small lane's.
          let big = match block_type.as_deref() {
              None | Some("small") => false,
              Some("big") => true,
              Some(other) => {
                  return Err(err(RpcError::InvalidParams(format!(
                      "blockType must be \"small\" or \"big\", got {other:?}"
                  ))))
              }
          };
          let fee_of = |h: &TorusBlockHeader| if big { h.big_base_fee_per_gas } else { h.base_fee_per_gas };
          // both base_fees.push sites (1263, 1297): hex_u64(fee_of(&header))
  ```
  `gasUsedRatio` stays header-based. `gas_limit: hex_u64(header.evm_block_gas_cap())` in the block JSON (612).
  torus.rs trait (~99) and impl:
  ```rust
      // --- Item 7 step 2: EVM lanes ---
      #[method(name = "bigBlockGasPrice")]
      async fn big_block_gas_price(&self) -> RpcResult<String>;
      #[method(name = "usingBigBlocks")]
      async fn using_big_blocks(&self, address: String) -> RpcResult<bool>;
  ```
  `big_block_gas_price` mirrors `gas_price` (eth.rs:782) with `big_base_fee_per_gas` (`crate::eth::eth_head`,
  `crate::eth::get_header_with_hash`, both `pub(crate)`); `using_big_blocks` reads `evm_lane_key(address)` in
  `CF_NATIVE_ORACLE` (the stored choice, not the effective lane).
- **Verify:** `$L cargo nextest run $P $F -E 'test(estimate_gas_caps_at_the_callers_lane_limit) or test(big_block_gas_price_and_fee_history_per_lane) or test(using_big_blocks_reads_the_lane_row) or test(block_gas_limit_is_the_heights_cap) or test(bare_call_and_estimate)'`
- **Depends on:** B2, B7

### B19: docs and the full run
- **Test first:** none new; the full suite and gates (Verification).
- **Implementation:** `docs/configuration-reference.md`: chain config rows for `evm_big_block_interval` and
  `evm_big_gas_limit`; a section "EVM blocks (item 7 step 2)": big heights and per-height caps, the three governance
  keys and ranges, lanes and `EvmUserModify`, fees per lane (F3, floor), the budget text at lines 105-110 replaced
  (decision 6), CoreWriter delay and cap, the bbo precompile, routing (re-send, upcoming N, in-flight filter), the RPC
  methods, and the limit `TORUS_NATIVE_TOTAL_BLOCK_CAP` <= 7,203 while big blocks are on.
  `docs/api/corewriter-results.md` (line 46: the rows of block N come from N - 1, N - K and full earlier targets).
  `docs/perf/read-precompile-gas.md` and `docs/api/read-limits.md`: bbo at 0x080e, 16,440 gas. Status lines of
  `docs/plans/item7-evm-lanes.md` and `docs/plans/item7-step2-design.md`.
- **Verify:** the Verification block below.
- **Depends on:** B1-B18

## Commit groups

Each group compiles and passes its tests and the per-commit gate (TESTING.md: workspace nextest + doc tests).

| group | tasks | content |
|---|---|---|
| C1 | B1-B3 | constants, header fields, genesis keys |
| C2 | B4-B6 | plan fields, governance keys, six-value boundary rule |
| C3 | B7-B8 | `EvmUserModify`, per-lane execution |
| C4 | B9 | F3 fees |
| C5 | B10-B12 | per-height vote caps + o5, two pools + decision 6, big-height drain |
| C6 | B13-B14 | in-flight filter, routing per lane and re-send |
| C7 | B15 | CoreWriter K and cap |
| C8 | B16-B17 | bbo rows and precompile |
| C9 | B18 | RPC |
| C10 | B19 | docs, full gates, devnet, A/B |

C7 (needs B1, B5) and C8 (needs nothing) can be built beside C2-C6; C9 needs C1-C3; the rest is sequential.

## Verification (end-to-end)

Full gates (the workspace runs start in the background, output to a file; read only the summary lines):
```bash
$L cargo nextest run --workspace $F > /tmp/item7s2-nextest.log 2>&1; echo "rc=$?"
$L cargo test --doc -q > /tmp/item7s2-doc.log 2>&1; echo "rc=$?"
CARGO_TARGET_DIR=/home/18c/.cargo-target-item7s2-dw RUSTFLAGS="-D warnings" $L cargo build $P --all-targets
cargo fmt --all -- --check 2>/dev/null | grep -c '^Diff in'      # not above main's count at the merge base
$L cargo clippy $P --all-targets 2>&1 | grep -c '^warning'       # not above main's count; none on changed lines
ci/run-local.sh uniswap                                          # Router deploy on the 10M CI genesis
# before merging to main, TESTING.md's one full run:
$L cargo test --workspace -q > /tmp/item7s2-full.log 2>&1; echo "rc=$?"
```
Never `cargo fmt --all`. A `flaky` count above 0 is reported, not a pass.

**4-validator devnet.** Step 1's tools are at `/home/18c/item7-devnet-tools/` (`scripts/`: devnet.sh, traffic.py,
poller.py, analyze.py, drained.sh, gov.sh; `govtool/`); `/tmp/item7-devnet-check/` does not exist. Copy both to
`/tmp/item7s2-devnet-check/` and in the copies set `RUN=/tmp/item7s2-devnet-check/run`,
`TORUS_NODE_BIN=/home/18c/.cargo-target-item7s2/release/torus-node`, the govtool path and
`W=/home/18c/.cargo-target-item7s2/release/torus-wallet`; give analyze.py's `cadence()` N and M (`--n`, `--m`,
`--m-change B:M2`). Build: `$L cargo build --release $P`.
- a. Default genesis (M = 0), 10 min of traffic through all four RPCs: every EVM-carrying height has `h % N == 0`;
  `gasLimit` is the small limit everywhere; `torus_bigBlockGasPrice` and `eth_feeHistory(.., "big")` answer.
- b. Genesis with `"big_block_interval": 50, "big_gas_limit": 10000000` (genesis may set any M): EVM txs only at
  `h % N == 0` or `h % 50 == 0`; `gasLimit` = small + 10M exactly at `h % 50 == 0`; no `not voting -- EVM` warning on
  any node.
- c. Big lane: two funded EOAs run `torus-wallet evm-user-modify --using-big-blocks true`; `torus_usingBigBlocks` is
  true; each sends a 5M-gas tx (above the 3M small limit): it lands at a height with `h % 50 == 0`, its
  `effectiveGasPrice` >= the big base fee; the same tx from a small-lane EOA is refused (`GasLimitExceeded`).
- d. Routing re-test (decision 7): step 1's traffic check with the default N (11 after the n11 merge): every tx
  lands at most one EVM height after its target (`EVM heights waited` <= 2 for 100% of txs; step 1: 15-19% above 2,
  worst 24 blocks); none stranded; `EVM tx re-sent` lines present.
- e. Governance N change during traffic (gov.sh, `evm_block_interval`): no tx sent before the switch waits more than
  2 EVM heights (step 1: one waited 661 blocks).
- f. Governance M change (`evm_big_block_interval` 0 -> 50 on the default genesis): headers carry M = 50 from the
  expected boundary + L; big heights start there.

**Archy native-only A/B** (after the devnet passes; a bench agent per the boot `bench:` line). Campaign spec for
`tools/matched-bench/campaign/make-driver.py`: arm A = main at the step 2 merge base, arm B = the step 2 head; one
genesis shape (300-market weighted genesis; arm B with the new keys absent, so M = 0); EVM load off (native only);
harness defaults (`TORUS_BOOK_ROWS=3`, so the deferred save path that writes the bbo rows; `BLOCK_CAP=200`); warm-up
cell, n = 4 ABBA rounds of 120 s cells; one 10-market pair as a secondary. Plus a Classic pair (Codex plan check
s125): mode 3 turns checkpoints off (`resolve_checkpoint_interval`, exec_pipeline.rs:134-160, run-cell.sh:412
`TORUS_BOOK_ROWS=3`), while shipped validators run Classic with 15 s checkpoints, where the bbo puts go through
`save_order_books`. Both arms `EXTRA_ENV='TORUS_BOOK_ROWS='` (Classic, the validator default
`VALIDATOR_CHECKPOINT_INTERVAL` 15 s, exec_pipeline.rs:89), 300 markets, n = 4 ABBA, same pass rule; the shape
`tools/matched-bench/campaign-template.sh` already runs (`KNOB=TORUS_CHECKPOINT_INTERVAL_MS=15000` at :38,
`TORUS_BOOK_ROWS=` in `KNOBS` at :65, the `envok` Classic check at :317). Report matched/s (primary), native
blocks/s, `torus_exec_save_books_seconds` per native block (the bbo writes land there), exec `block_ms`, block time
p50/p99, CPU-s/1M, per book shape. Pass, in both shapes: the paired B/A matched/s 95% CI includes 1.0 or lies above
it, and save_books rises by at most 0.3 ms per native block. A failure blocks the merge; the follow-up is in B16
(measure the `saved_bbo` hit rate).
Doc target: `docs/perf/item7-step2-native-ab.md`.

## Rollback

All work is on `feat/item7-step2-big-blocks`; main is untouched until the merge. Fresh genesis, no migration: a
rollback reverts the merge commit and reuses the previous genesis files. After the merge, levers without a release:
big blocks off = governance `evm_big_block_interval` 0 (the big pool moves to the small lane); CoreWriter delay back
to step 1 = `core_writer_order_delay` 1 (the 200 cap stays); `TORUS_EVM_BLOCK_GAS_BUDGET` per node. F3 and the bbo
writes have no switch: F3's floor keeps fees at or above today's 1 gwei, and the A/B gates the bbo cost before the
merge. Groups revert alone along their dependencies (C7, C8 independent of C2-C6; C9 needs C1-C3).

## Item 6 Phase 5 overlap

Phase 5's plan (`docs/item6-phase5-plan` 8c8d6188, decisions 1a 3a 4a 5b 6a 7a 8a) is not built, and Phase 4 under it
is not built either. No conflict blocks step 2, which lands first. Phase 5 must carry:
1. Step 3 (book rows from a dirty set, Classic): `save_order_books` stays only as the reference path there, but it
   now also writes `cf_native_bbo`. The typed path must write the bbo row of each dirty market, and its differential
   (`book_dirty_rows_equal_save_order_books`) must compare that CF too.
2. Task 2.3 (EVM blocks, 4a): step 2 adds no typed-CF writer to EVM execution (lane fees, the lane row and the epoch
   gas row are not positions or balances). But with K = 10, CoreWriter order actions now execute in later, often
   pipelined, non-EVM blocks, i.e. on Phase 5's typed hot path. Its long differential (task 1.2) should include a
   K-delayed CoreWriter drain in a pipelined block and a big EVM height.
3. Step 6 (conditional typed oracle tables): `CF_NATIVE_ORACLE` now holds `"lan"` rows next to `"sub"`, `"agg"`,
   `"sgn"`; a typed table must skip them as it must skip `"sgn"`.
4. Cold raw CFs (1a): `CF_CONSENSUS_META` `evm_epoch_gas` and the unhashed `cf_native_bbo` stay raw overlay writes.
5. `core_writer_due` still reads the raw DB, which stays valid only because rows come from serial EVM blocks (memory
   25f7770c). If item 7 step 3 or Phase 5 ever pipelines EVM blocks, it must read through the overlay.
Phase 5's step 0 re-profile will then include the bbo writes.

## Open questions (owner): answered s125

Owner s125: 1 = 16,440; 2-9 = the plan's answer as written; M stays 0 at genesis (decision 1).

Codex plan check (s125): 8 findings folded into B9, B10, B12, B13, B15 and Verification; owner chose (a) for B13 (no
ecrecover on commit).

1. **bbo gas.** Decision 3 says ~16,940 (the row charged like a scanned row); our point readers (`getPrice`,
   `getPosition`) pay their point read inside the 16,400 base, which would give 16,440. Plan was 16,940
   (`meter.charge_rows(1)` in B17). **Owner s125: 16,440** (the line is deleted from B17).
2. **bbo call shape.** Plan: our convention, `bbo(bytes32 marketId) -> (uint128 bid, uint128 ask)` (FixedPoint raw, 0
   = empty side). HL's 0x080e takes `abi.encode(uint64 asset)` with no selector and returns `(uint64 bid, uint64 ask)` (hyper-evm-lib
   PrecompileLib, checked s125); HL
   contracts need porting either way (ids and price units differ).
3. **"Order actions and vault transfers".** Plan: `PlaceOrder` and all three cancels wait K (one delay, so a
   contract's cancel cannot overtake its order). Torus has no vault kind; lockbox deposit and withdraw (HL's spot send
   and class transfer, which HL does not delay) and the staking kinds keep + 1.
4. **Cap at enqueue (plan) vs at drain (design wording).** Same rule; a drain-time cap would need an overlay-aware
   due-check and a new status-record and RPC key for rows drained after their target.
5. **Ranges and defaults (plan).** M: 0 or 30..=10,000 (30 is ~9 s at 3.2 blocks/s, where a 30M big lane alone would
   add ~3.2M gas/s, about HL's whole EVM); big limit 1M..=30M with genesis 10M; K 1..=100.
6. **F3 floor and step.** Plan: floor 1 gwei (without it an idle chain decays toward 1 wei in about 2 h at -12.5% per
   ~31 s epoch); the epoch counts as one EIP-1559 step (at most +/-12.5% per epoch).
7. **Lane of a tx at execution.** Plan: the sender's row when the block executes, not its position in the block. A
   sender that switches lanes while one of its txs is pooled under the old lane may see that tx charged the new
   lane's fee (skipped if under it).
8. **Delist.** `DelistMarket` proposals execute nothing today (`native_executor.rs:12066`, "text-only"), so "bbo row
   deleted on delist" has no hook. An emptied book deletes its row; a delist implementation must empty the book
   through the dirty-marking path or delete the row.
9. **`eth_feeHistory` per type.** Base fees per type, yes; `gasUsedRatio` stays the header's (always 0, headers are
   built before execution). Per-block, per-lane gas used is not stored (only per epoch).

Not blockers: the consensus message fits (830,864 B <= 1 MiB); the Phase 5 overlap has no ordering conflict; nothing
found contradicts the design.

## Owner decisions during the build (s126), with the alternatives kept

None of these changes block speed or matched/s; they are correctness and UX choices.

1. **C9: where `eth_call` / `eth_estimateGas` read the sender's lane.** Chosen: **B**, the lane row from the ingress
   view (latest executed state), dropped before the call. The call itself still runs on the durable DB (c89ab290).
   Lane answers now agree with admission, routing and `torus_usingBigBlocks`.
   - Alt A (as first built, 1d9e9bfa): the lane from the durable DB, one consistent snapshot, but up to one
     checkpoint (~15 s) after `EvmUserModify` estimates stay capped at the small limit.
   - Alt "HL way": all RPC reads from the latest executed state. Parked for a decision at item 6 Phase 5 (RAM: the
     view holds 6 CFs); see `item6-phase5-impl.md`, "Parked for a decision at Phase 5".
2. **C5 deviation 2: a tx returning to the pool** (own lost proposal, re-proposal, M -> 0 move) joins the lane that
   already holds its sender's txs; brand-new lane-switching txs still get `EvmLaneSwitchPending`.
   - Alt (the plan's literal text): refuse returning txs with `EvmLaneSwitchPending`. Rejected: it drops an
     accepted tx and leaves a nonce gap.
3. **A tx no lane can take** after a limit drop or M -> 0 is dropped with a warning; the sender's later nonces stay
   pooled and wait until the gap is re-sent (as step 1's gas-limit eviction, Ethereum pools, and HL, which keeps the
   next 8 nonces and prunes after 1 day).
   - Alt: drop the sender's later nonces too. Rejected: the user must re-send everything instead of one tx.
4. **C6: EVM txs with trailing bytes.** Chosen: **reject at admission only**, so each tx has one canonical byte
   form. The C6 in-flight filter already decodes txs (`evm_tx_hash`), so it is correct either way.
   - Alt keep: accept trailing bytes (one tx, several byte strings; harmless with the decoding filter).
   - Alt admission + vote: validators also refuse blocks carrying such txs. Rejected for now: it blocks padding
     by a malicious leader, which gains nothing (same tx hash), but if admission and the vote ever disagree, honest
     proposals are refused and views time out. Revisit only if padding becomes a real problem.
5. **C6: txs from a lost proposal.** Chosen: **track them** (back into the pool, re-sent at the next commit if
   still pending), as the plan says; the RPC node's own re-send covers its txs as well.
   - Alt: forward them to the next leader at once. Gains up to one commit (~0.3 s) in a rare case and adds
     duplicate sends. Revisit if the devnet's routing check (d) shows lost-proposal txs waiting more than 2 EVM
     heights.
6. **Propose-defer, the onchain mempool + per-block fees, and RPC freshness the HL way:** parked for item 6
   Phase 5 (rules in `item6-phase5-impl.md`, `item7-step2-design.md` and `docs/perf/bench-queue.md`).

## Effort (one builder)

C1 4-6 h, C2 4-6 h, C3 6-8 h, C4 5-7 h (with the fee clamp), C5 9-13 h (the pool split is the largest task; the
planned drain fee), C6 10-13 h (with `drained` and the executed-height release), C7 5-7 h (mostly pinning K = 1 in
existing tests; the last-target row), C8 5-7 h, C9 3-4 h, C10 3-4 h plus the devnet (~3 h) and the A/B (bench agent,
~9 h wall: two book shapes). About 55-75 builder hours; ~5-6 days wall with C7 and C8 built in parallel. (Codex plan
check s125 added ~6 h.)
