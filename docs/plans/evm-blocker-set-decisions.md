# EVM Blocker Set — User Decisions (S391, 2026-07-03)

Decisions made interactively with OZ; implementation deferred to next session.
Source audits: mem `8866ad66a893067d` (launch blockers), `92f624e8541ff2cc`
(secondary gaps). DO NOT re-litigate — these are decided; open the referenced
files and build.

## D1 — Cap replacement: gas budget + config-gated sender share
- Delete `EVM_TOTAL_BLOCK_CAP` (20) and `EVM_PER_BLOCK_CAP` (4/sender)
  (`crates/torus-mempool/src/rate_limit.rs:26-30`).
- Core mechanism: per-block EVM **gas budget** passed to the drain
  (`EvmPool::drain` is already gas-budgeted — `evm_pool.rs:319-370`,
  `lib.rs:284-294`, consumed at `torus-consensus/src/app.rs:1316`).
- Initial budget **15_000_000** (half the 30M block gas limit),
  **env-overridable** (same pattern as `TORUS_HASH_ONLY_PUSH_THRESHOLD`).
- Per-sender share cap ~**25% of the block's EVM gas budget**, behind a
  **config flag: ON for testnet, OFF for mainnet** (mainnet = pure gas budget,
  Ethereum-style; the share cap is only a faucet-era spam guard).
- Also DELETE the dormant `RateTracker` 50/100-blocks window
  (`rate_limit.rs:19`; its `notify_block_committed` feed is test-only).
- Keep the anti-MEV shuffle (`evm_pool.rs:75-84`) and fee-ordered k-way merge.
- Acceptance: devnet swap-load bench fits exec budget; >100 swaps/block land.

## D2 — eth_call / estimateGas: standard geth semantics
- Simulation path (call-only executor config): `disable_base_fee` +
  no balance requirement when fee fields are omitted — exactly geth/reth.
- Files: `crates/torus-rpc/src/eth.rs:817-920` (call + estimate),
  `torus-evm/src/executor.rs:95-130` (needs a call-mode cfg).
- Acceptance: bare eth_call at height>0 with base_fee=1gwei from an
  EMPTY account succeeds (add exactly this test — all current tests pass
  gas_price=base_fee or run at height 0, which is how the bug hid).

## D3 — Blob/type-4 txs: reject at admission + per-tx exec isolation
- Reject tx types 3/4 in `validate_evm_tx` (`torus-mempool/src/validate.rs:32-55`)
  with a standard "transaction type not supported" error at RPC.
- AND make execution decode per-tx (`torus-bridge/src/decode.rs:94-98`) so an
  undecodable tx skips only itself — never the whole block's EVM execution
  (current failure: `app.rs:285-288` skips everything = whole-block poisoning
  by a malicious proposer).
- Acceptance: block containing a crafted type-3 tx still executes+receipts
  every other tx.

## D4 — Fee floor: dynamic, max_fee >= current base fee
- Enforce at admission (`validate.rs:91-102`) and re-check at drain.
- Frozen 1-gwei base fee makes this a fixed floor today; automatically correct
  once the fee market unfreezes (roadmap item 10, NOT in this set).
- Kills the silent-drop/stuck-nonce trap. Native actions stay fee-free for
  now (separate readiness decision, not taken).
- Acceptance: 0-gwei tx rejected at eth_sendRawTransaction with a clear error;
  sender nonce chain unaffected.

## D5 — Receipts: alignment fix only
- Map receipts through `included_indices` (`executor.rs:171-179`) in
  `validator.rs:129-134` and `committer.rs:183-190` so every receipt carries
  the right tx hash when a tx is skipped mid-block.
- Skipped txs get NO receipt (honest null). Synthetic status-0 receipts
  explicitly REJECTED for now (may revisit as explorer polish).
- Acceptance: test with one skipped tx mid-block — later receipts match their
  tx hashes.

## D6 — Uniswap proof: full (Factory + CREATE2 test + CI e2e + Multicall3)
- Write missing `devnet/uniswap/src/Factory.sol` (~25 lines; Router already
  codes against IFactory — `Router.sol:6-9,35-37`); unbreak `deploy.sh`.
- CREATE2 cargo test in `torus-evm/tests/evm_tests.rs` (zero CREATE2 coverage
  repo-wide today; pairFor address math depends on it).
- CI job: foundry container -> 1-node devnet -> deploy V2 -> addLiquidity ->
  swap -> assert balances & receipts. (`deploy.sh` complete otherwise;
  `load.sh` gives 100-account swap load.)
- Deploy canonical Multicall3 at 0xcA11...bce3 (Nick's-method presigned legacy
  tx; chain accepts legacy txs — just fund the deployer).
- Acceptance: CI green on the full deploy->swap path.

## Suggested implementation order (next session)
D3 -> D4 -> D5 (small, independent, testable together) -> D2 (call executor
cfg) -> D1 (caps, needs bench) -> D6 (proves everything end-to-end).

## Explicitly out of scope for this set
Fee-market unfreeze (base fee dynamics), logs index CF, WS subscription fixes,
historical state, receipts trie in headers — see roadmap items 8-16
(mem 92f624e8541ff2cc) and the header-commitment decision list (separate).
