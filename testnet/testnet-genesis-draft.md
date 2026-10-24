# Testnet genesis draft (s140)

Status: DRAFT, written against main `ae2101fb` (s140, 2026-10-24). Not launchable: every value the owner decides
right before launch is the string `"FILL:<key>"`, and the node refuses each one at start (proved by
`tools/genesis-draft/check.sh`, section 4). Decisions: `docs/plans/testnet-genesis-checklist.md` (section 0 and the
"decided" rows). This file recommends nothing beyond what the owner already decided.

## 1. Files

| File | What |
|---|---|
| `testnet/testnet-genesis-draft-3val.json` | 3 validators, dead-leader skip OFF (`leader_miss_threshold` 0) |
| `testnet/testnet-genesis-draft-4val.json` | 4 validators, dead-leader skip K = 3, j cap 6 |
| `testnet/testnet-genesis-draft-5val.json` | 5 validators, dead-leader skip K = 3, j cap 6 |
| `tools/genesis-draft/fill-dummy.py` | fills the placeholders with INSECURE devnet values (validation only) |
| `tools/genesis-draft/check.sh` | shape check, refusal check, boot of each variant as a local devnet |

The three variants are identical except `validators` and `consensus.leader_miss_threshold` (`check.sh` asserts it).
The 3-validator variant must keep dead-leader skip off: at 3 validators f = 0, and at 2:2:1 with the stake-1 validator
down no validator-set update commits, so b1 would halt the chain at its first exclusion (owner s138;
`docs/plans/dead-leader-skip.md` section 8 finding 4; `torus_types::DEFAULT_LEADER_MISS_THRESHOLD` doc). Owner s138:
K = 3 only with >= 4 validators. Dead-leader skip on main still has a Decide-justify gap at n > 4, being fixed on
`fix/dl-decide-justify-n5`: the 5-validator variant with K = 3 waits for that fix (or drops to K = 0). Bench 9p
(dead-leader phase 4, 4 validators, main `3c5d02ea`) is the test of the 4-validator shape.

JSON has no comments and the genesis digest covers every key (also keys the loader ignores), so all notes are here.
Fill a draft by replacing each `"FILL:<key>"` string with a value of the field's type (numbers unquoted). Delete
the other two variants and rename the chosen one when it is final.

## 2. Placeholders (the owner fills these)

"Refused by" is what makes the node refuse the draft when that placeholder alone is left (`check.sh` step 2).

| Field | Placeholder | Source decision | Who decides | Refused by |
|---|---|---|---|---|
| `chain_id` and `evm.chain_id` (same value) | `FILL:chain_id` | Checklist section 1: testnet and mainnet need their own id; not 7777 / 7778 (devnet, `TORUS_CHAIN_ID`). The loader refuses `evm.chain_id != chain_id` (`71333e7e`). P0.0's built-in table is keyed by this id (empty today) | owner (open) | JSON type error (u64) |
| `timestamp` | `FILL:timestamp` | Checklist section 4 "Other genesis values". No crate reads it on main (the genesis parent header has timestamp 0, `torus-bridge/src/proposer.rs` `genesis_parent_header`); it is only part of the genesis digest | owner, at the freeze | JSON type error (u64) |
| `validators` (count) | the 3 / 4 / 5 variant | Owner s138: deferred, decided right before the testnet | owner (deferred s138) | — |
| `validators[i].address` | `FILL:validator<i>_address` | The validator's secp256k1 ACCOUNT address (attests with `--state-hash-attest-key`, registers its oracle signer) | owner / each operator | invalid address |
| `validators[i].pubkey` | `FILL:validator<i>_pubkey` | The validator's Ed25519 consensus key (32 bytes hex; `torus-node keygen`) | each operator | invalid hex |
| `validators[i].stake` | `FILL:validator<i>_stake` | Owner s138: deferred. Wei; voting power = whole TRS (`stake_to_power`). f = 1 needs 4 equal stakes (T1.4). IWRR normalization (dead-leader Q4) is still open | owner (deferred s138) | invalid amount (at genesis apply) |
| `native_balances[0].available` (address `0x746f7275732d6c697175696461746f722d766c74` = `LIQUIDATOR_VAULT`) | `FILL:liquidator_vault_seed` | Owner s131: seed the liquidator vault in the genesis, ADL as the backstop. Owner s138: amount deferred. Decimal, 8 fractional digits max | owner (deferred s138) | invalid amount (at genesis apply) |
| `markets` (whole list) | `FILL:markets` (one row, every field) | Owner s138 price band A: list no markets, or keep trading closed, until every validator's oracle signer is registered and marks are fresh; C (on main `9b1d564c`): orders on a never-marked market are refused. So `[]` is a valid fill (list markets later by governance, see note 5) | owner (open) | JSON type error (`market_id` u64) |
| `economics.treasury_address` + `accounts[0]` | `FILL:treasury_address`, `FILL:treasury_balance` | Checklist section 4 "Other genesis values": open. Receives the treasury share of EVM fees and (when `trading_fees` is scheduled) every trading fee | owner (open) | invalid address in `accounts[0]` (see note 1) |
| `economics.dev_pool_address` + `accounts[1]` | `FILL:dev_pool_address`, `FILL:dev_pool_balance` | As above. The built-in EVM fee split gives the dev pool 45% at the start (note 2) | owner (open) | invalid address in `accounts[1]` (note 1) |
| `accounts[2]` (faucet) | `FILL:faucet_address`, `FILL:faucet_balance` | Not decided; `tools/faucet` exists. Drop the row if the testnet has no faucet | owner (open) | invalid address |
| `permanent_stakes` | `FILL:governance_voter_address`, `FILL:governance_voter_amount` | Not decided. Governance vote weight = delegated stake + permanent stake x 1.5; validator self-stake does not count (`torus-economics/src/governance.rs` header). With none, no proposal can pass until someone delegates (note 5). `[]` is a valid fill | owner (open) | invalid address |
| Trading fee rates and destination split | not a genesis field | Owner s138: rates decided right before the testnet; destination pending (likely burn / validators / treasury, no dev share). On main the rates are the governance keys `trading_taker_fee_ppm` / `trading_maker_fee_ppm` (defaults 450 / 150 ppm, `torus-types/src/trading_fees.rs`) and the fee goes to the treasury's native balance; the feature (`trading_fees`, id 4) is NOT scheduled in this draft | owner (deferred s138) | — (governance `ParameterChange`, or a code change for the split) |

## 3. Filled values

"Decided" = an owner decision (checklist). "Carried" = the value of `testnet/genesis-weighted-base.json` (the bench /
devnet base) or the code default, written explicitly; not an owner decision, change it freely before the freeze.

| Field | Value | Source |
|---|---|---|
| `chain_name` | `torus-testnet` | carried (base) |
| `consensus.protocol` | `hotstuff` | carried |
| `consensus.epoch_length` | 100 | carried (every shipped genesis; `ChainConfig` default is 100,000, CR). Dead-leader re-admission floor is `L * 2^j` heights |
| `consensus.timeout_base_ms` / `timeout_max_ms` | 1200 / 30000 | carried (s69 1200 ms base, shipped-genesis test) |
| `consensus.backoff_factor` / `backoff_cap` | 2 / 8 | carried (code defaults) |
| `consensus.commit_lag_backoff_cap` | 0 | carried (code default, off; owner s108: genesis value, env override for tests) |
| `consensus.reputation_leader_selection` | false | decided: required since the TC set (`76862c2c`, in `d3de0abb`): the node refuses `true` |
| `consensus.book_mode` | `classic` | decided s136 (Classic). Explicit; absent means the same mode but a different digest |
| `consensus.activations` | `{"running_state_hash": 1, "book_window_commit": 1}` | decided s136: running hash + attestation ON at genesis, L6.1 with it, named EXPLICITLY (a genesis naming no activation means off, batch 10 `2d2f3ed9`). The aliases `state_hash_activation_height` and the bool `book_window_commit` are left out (one spelling) |
| not scheduled: `same_block_core_writer` (2), `funding_engine` (3), `trading_fees` (4) | absent | decided s138: item 7 T after genesis through P0.0; funding and fees unscheduled (product batch `8964bd4d`) |
| `consensus.leader_miss_threshold` | 0 (3val) / 3 (4val, 5val) | decided s138 (K = 3 only with >= 4 validators; default 0 = off, `68dd5907`) |
| `consensus.leader_exclusion_cap` | 6 | decided s134 (j cap default 6); inert at K = 0 |
| `evm.spec_id` / `gas_limit` | `cancun` / 3,000,000 | carried (every shipped genesis: 3M / N 11) |
| `evm.base_fee_initial` | 1,000,000,000 | carried; the genesis parent header carries 1 gwei for both lanes whatever this says (CR "base_fee_per_gas") |
| `evm.block_interval` | 11 | decided s131 (Q12: N = 11 for the testnet) |
| `evm.big_block_interval` / `big_gas_limit` | 0 / 10,000,000 | carried (code defaults; big blocks off) |
| `economics.fee_split` | start 1000/0/4500/4500, end 2500 each, 1825 epochs | forced: the loader refuses any other split (row 80 option D, `check_fee_split`) |
| `economics.permanent_staking` | 500 bps / 15000 bps | carried |
| `economics.validator` | min self-delegation 10,000 TRS, max 21 validators, unbonding 604,800 blocks, max commission 5000 bps, max change 100 bps | carried |
| `validators[].commission_bps` | 500 | carried |
| `evm_alloc` / `precompiles` | `{}` / `{}` | carried (`precompiles` is parsed but read by nothing) |
| `native_balances` | the liquidator vault row only | decided s131 (vault seeded at genesis); amount is a placeholder |
| removed vs the base | bench accounts (hardhat #0-#9 and others), their native balances, 10 hardhat permanent stakers, 100 bench markets | the base's keys are public (hardhat / dev mnemonic): never on a testnet |

## 4. Validation

```
CARGO_TARGET_DIR=/home/18c/.cargo-target-genesis-draft flock -w 60 -o "$HOME/projects/wt/.cargo-heavy.lock" \
    cargo build --release -p torus-node
TORUS_NODE_BIN=/home/18c/.cargo-target-genesis-draft/release/torus-node tools/genesis-draft/check.sh
```

Steps: (1) shape of each variant; (2) the node refuses each draft as checked in, and the 5-validator draft with each
placeholder left alone while all others are filled; (3) each variant filled with dummy devnet values (consensus keys
01..0N, equal 1M TRS stakes, chain id 7778, one BTC-USD market) boots N local validators attesting with their account
keys, until every node has TARGET (default 5) committed blocks. Every node is killed on exit.

Result on main `ae2101fb` (s140): section 6.

## 5. Notes for the owner

1. `economics.treasury_address` / `dev_pool_address` with a malformed value are NOT refused: `chain_config` and
   `initialize` map a bad address to the zero address (`parse_address(..).ok()` ... `unwrap_or(Address::ZERO)`). The
   draft is refused only because the same placeholder is in `accounts[0]` / `accounts[1]`. Check the two addresses by
   hand when filling them (or make the loader strict, a code change).
2. The EVM gas fee split is fixed in code (dev pool 45% at the start, 25% at the end); the owner's "no dev share"
   (s138) is about trading fees. Changing the gas split needs a code change, not a genesis edit.
3. Freeze before launch: a data dir is bound to the genesis file's digest (P0.0 phase 4) and peers with a different
   digest refuse each other (batch A `758a79f9`). Any byte-level value change after the first boot (also a key the
   loader ignores, or `classic` written vs absent) is a different chain.
4. Node-local settings, not in the genesis: checkpoint interval (15 s default on validators, 0 on `--rpc-only`),
   L2.1 streamed settle (on by default since `2d2f3ed9`), the hash fail-stop (on), `--state-hash-attest-key` per
   validator (its account key, mode 0600), `--archive` on at least one node (genesis-sync source),
   `TORUS_EPOCH_PLAN_HANDOFF` (default off; goes on only after bench 9q Part B passes).
5. Governance at launch: listing markets later, changing fee rates or `delist_grace_blocks` all need a passing
   proposal; weight comes only from delegations and permanent stakes. With `permanent_stakes = []` the first
   proposal waits for delegations.
6. Validators start with no EVM / native balance in this draft. In the boot check (node defaults, no env
   set) every validator submitted its attestations at heights 100 and 200 with no `not admitted` line and no hash
   mismatch; that the attestations reached the chain was not read back over RPC. `SetOracleSigner` and price
   submissions from an unfunded validator were not tried.

## 6. Validation output (s140, 18c)

Binary: `cargo build --release -p torus-node` of main `ae2101fb` (this branch changes no crate), md5
`a1e719010f82dcc4018dd21b987faa6b`. Command: `TORUS_NODE_BIN=/home/18c/.cargo-target-genesis-draft/release/torus-node
WORK=/tmp/gdraft/run4 tools/genesis-draft/check.sh` -> `RESULT: PASS`, exit 0. Condensed output:

```
=== 1. shape ===
  3val: ok (validators 3, K 0, activations hash + L6.1 from block 1)
  4val: ok (validators 4, K 3, activations hash + L6.1 from block 1)
  5val: ok (validators 5, K 3, activations hash + L6.1 from block 1)
=== 2. refusal ===   (30 runs, all "refused (exit 1)", 0 "NOT REFUSED")
  draft-{3,4,5}val-as-checked-in  refusing to start: genesis ...: JSON parse error: invalid type: string "FILL:chain_id" ...
  only FILL:chain_id / timestamp / markets      JSON parse error: invalid type: string "FILL:...", expected u64
  only FILL:validator<i>_address                invalid address: FILL:validator<i>_address
  only FILL:validator<i>_pubkey                 invalid hex string: FILL:validator<i>_pubkey
  only FILL:validator<i>_stake                  genesis initialize: invalid balance/amount: FILL:validator<i>_stake
  only FILL:treasury_address / dev_pool_address / faucet_address   invalid address: FILL:...
  only FILL:treasury_balance / dev_pool_balance / faucet_balance   genesis initialize: invalid balance/amount: FILL:...
  only FILL:governance_voter_address            genesis initialize: invalid address: FILL:governance_voter_address
  only FILL:governance_voter_amount             genesis initialize: invalid balance/amount: FILL:governance_voter_amount
  only FILL:liquidator_vault_seed               genesis initialize: invalid balance/amount: FILL:liquidator_vault_seed
=== 3. boot (TARGET=5 committed blocks per node; attestation check at height 250) ===
  3val nodes 0-2: committed 263 / 270 / 270, peers 2, "genesis digest stored"
       attestations_submitted 2 each, hash_height 200, mismatch 0, no_quorum 0, unverified 0, not_admitted 0
  4val nodes 0-3: committed 328 / 333 / 337 / 347, peers 3, "genesis digest stored"
       attestations_submitted 3 each, hash_height 300, mismatch 0, no_quorum 0, unverified 0, not_admitted 0
  5val nodes 0-4: committed 255 / 259 / 263 / 266 / 271, peers 4, "genesis digest stored"
       attestations_submitted 2 each, hash_height 200, mismatch 0, no_quorum 0, unverified 0, not_admitted 0
  node0 log (every variant): "epoch plan hand-off (TORUS_EPOCH_PLAN_HANDOFF) ... on=false",
       "L3.1 coalesced Classic book save ON", "running state hash: automatic AttestStateHash enabled addr=0x8C32...",
       "running state hash: activation follows the committed headers' RunningStateHash bit height=1"
RESULT: PASS
```

Every node the script started was killed at the end of its variant (no `torus-node` left with a
`/tmp/gdraft/run4` data dir). The dead-leader parameters were range-checked at load (K = 3 accepted); the boot check
kills no validator, so it does not exercise an exclusion (that is bench 9p).
