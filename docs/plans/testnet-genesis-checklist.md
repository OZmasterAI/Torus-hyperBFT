# Testnet genesis and launch: checklist (draft)

Status: draft, s136 (2026-10-22), refreshed s140 (2026-10-24) against main `ae2101fb`. For the owner to decide later.
This file lists every open question about the testnet genesis file and the launch, with its options and what we know.
It recommends nothing. Items already decided are marked **decided**, with the session and the source; code that has
landed is marked **landed** with its main commit (each checked with `git merge-base --is-ancestor <c> origin/main`).
No testnet exists yet; every run so far is a local devnet or a one-host bench. The trial net (archy + cachyoz + 18c,
roadmap "Trial net") comes first.

**Genesis draft (s140):** `testnet/testnet-genesis-draft-{3,4,5}val.json`, with the placeholder table, the source of
every filled value and the validation command in `testnet/testnet-genesis-draft.md`. The draft holds the decisions
below; every deferred value is a `"FILL:<key>"` string the node refuses at start.

Paths: **RM** = `docs/Torus-hyperBFT-roadmap.md` (line numbers at the s136 refresh), **GEN** =
`crates/torus-genesis/src/lib.rs` on main, **CR** = `docs/configuration-reference.md`, **DRAFT** =
`testnet/testnet-genesis-draft.md`.
Status values: **decided**, **open**, **conflict** (sources disagree; owner to confirm), **pending code** (decided,
waiting for a merge), **landed** (on main). "Blocks" names what cannot be finished until the item is settled.

## 0. Owner decisions s138 (2026-10-22)

| Item | Decision |
|---|---|
| Price band before a market's first mark | **A + C.** A (ops): list no markets / keep trading closed until every validator's oracle signer is registered and marks are fresh. C (code, pre-genesis): reject order placement on a market that has never had a mark until its first mark lands; listing a market therefore needs a feeder that quotes it. **C landed `9b1d564c` (batch A `d595f77e`).** In the draft `markets` is a placeholder; `[]` is a valid fill |
| RPC freshness option (b) | **(a) for the testnet:** not built; wallets use `--rpc-only` nodes (checkpoint interval 0) |
| M2 timestamp lower bound | **Before genesis.** **Landed `e7b8f1e2` (batch C `a300c7f3`):** 30 s vote-rule lower bound with re-proposal exemptions |
| 9n (8f deep-book re-run) gates the launch | **Yes.** **9n passed** (perf sections 126 / 127): deep-book slowdown gone on `9083b278` (blocks/s 21.9 vs 22.6 standard), RAM high-water mark <= 6.85 GB, restarts recover (AGREE) but are ready in 9.6-11.3 s vs 3.0-3.5 s in section 93.5 (4.7 s of it is loading the 295 MB bench genesis); the ~10 s restart is accepted (s140). Follow-up on `perf/genesis-restart-digest` |
| Validator count (3 / 4 / 5) | Deferred: owner decides right before the testnet. The draft has a 3-, 4- and 5-validator variant |
| Liquidator-vault seed amount | Deferred: owner decides right before the testnet. Draft placeholder `FILL:liquidator_vault_seed` |
| Item 7 T timing | **After genesis, activated through P0.0** (a new `Feature` bit switched on at a height; replaces the s127 "before genesis"). **Built and landed** behind the unscheduled bit `same_block_core_writer` (id 2) (`93932c2d`, product batch `8964bd4d`); not scheduled in the draft |
| M2 design | **Ship the current vote-rule bound for the testnet if its delta review is clean; build the time floor from timeout votes as the pre-mainnet version. If the review finds another P1, switch to the time floor now** (owner s138). The vote-rule bound landed (row above) |
| Funding (`feat/funding-engine`, P0.0 id 3) | Settle in cash at the position's next fill (possible later upgrade: Hyperliquid-style hourly settlement to every position, own P0.0 bit, no O(positions) spike); HL default values, impact notional 10000 for all markets; pro-rate the first partial interval; +32 MB per 1M positions accepted; one chain-wide cap now, per-market later (owner s138). **Landed `af34e407` (product batch `8964bd4d`), unscheduled; not scheduled in the draft** |
| Trading fees (`feat/trading-fees`, P0.0 id 4) | Liquidation fills as built (book fills pay, backstop / ADL / delist exempt). Fee not reserved at placement, as built; revisit (possible upgrade: reserve the worst-case fee). One global taker / maker pair, as built; **revisit pre-testnet or during the testnet: per-market rates or Hyperliquid-style per-user tiers (owner leans per-user)**. Rates and rebate values: decided right before the testnet. Destination: pending (likely burn / validators / treasury split, no dev share, paid once per block) (owner s138). **Landed `111b9138` (product batch `8964bd4d`), unscheduled; not scheduled in the draft.** On main the rates are governance keys (`trading_taker_fee_ppm` / `trading_maker_fee_ppm`, defaults 450 / 150 ppm), not genesis fields, and the fee goes to the treasury's native balance |
| Jailing / `JailVote` gate | **Leave as is for the testnet; fix before mainnet.** `JailVote` stays ungated (every testnet validator is owner-run), and jailing (J2 + E1/E2) plus the `JailVote` gate are pre-mainnet items. A testnet upgrade through P0.0 stays possible if live testing is wanted later. Dead-leader skip (b1) still handles dead leaders on the testnet |

## 0a. Status items s140 (2026-10-24)

| Item | Status | Blocks |
|---|---|---|
| Epoch-plan hand-off (`TORUS_EPOCH_PLAN_HANDOFF`) | **Landed `b17dab16`, node-local, default off.** Bench 9q Part A PASS (section 127: boundary WARNs 10 -> 0, timeouts 29 -> 0, matched/s -0.1%); Part B runs with 9p (4 validators, `DL_K=3`, main `3c5d02ea`). The flag goes default-on only after Part B passes. Not a genesis field | validator launch config, not the genesis |
| Dead-leader post-exclusion freeze | **Fixed, landed `3c5d02ea`** (the a300c7f3 `DL_K=3` smoke froze ~30-35 s after the exclusion; a Decide nudge's Commit certificate is applied before the proposer check, stale old-set Decide certificates are dropped). 9p re-pinned to `3c5d02ea` | 9p |
| Dead-leader Decide-justify gap at n > 4 | Being fixed on `fix/dl-decide-justify-n5` (not on main). Affects the 5-validator variant with K = 3 | the 5-validator draft with dead-leader on |
| 9n restart time | Accepted at ~10 s (s140). Genesis-digest restart follow-up on `perf/genesis-restart-digest` (not on main). The testnet genesis is small (no bench accounts), so its load time is not the 4.7 s of the bench genesis | nothing at genesis |
| All-up throughput drop at batch A | Section 128: the 9o all-up -3.3% matched/s lands at batch A `d595f77e` (process CPU per fill +8.3%, ~95% off the exec thread); bench 9s measures it. Perf, not genesis-bound | nothing at genesis |

## 1. Chain identity and validators

| Question | Options | What we know | Status | Blocks |
|---|---|---|---|---|
| Chain id | Any id not used by a devnet or mainnet; the value is open | Genesis `chain_id` (GEN:80), default 7778 (`TORUS_CHAIN_ID`, `crates/torus-types/src/eip712.rs:26`). It is the EIP-712 domain chain id, so testnet and mainnet need distinct ids (RM:348, `034adf2`). P0.0's built-in schedule is keyed by chain id and is empty until the testnet id exists (`plans/p00-activation-height-gating.md` build phases; tracker s134 "P0.0 phase 1 open questions"). The genesis also has a separate `evm.chain_id`: a mismatch is refused at genesis load (**landed `71333e7e`**). Consensus certificates are bound to the chain id (**landed `2da29bec`**, s138 SAFETY fix). Draft placeholder `FILL:chain_id` in both fields | open | genesis file; P0.0 built-in table; wallet / feeder / bench `--chain-id` |
| Validator count: 3, 4 or 5 | (a) 3 validators, f = 0; (b) 4 validators, f = 1; (c) 5 validators, f = 1 | T1.4 (RM:120) sets f = 1 as the minimum credible posture. 3 validators: any node down halts the chain, and with 3 equal stakes epoch-change views need more than any two validators (RM:122). 4 equal stakes: f = 1; 4-validator genesis and epoch rejoin exist since July (`1b4c3b9f`, `d995f68a`). The trial net plan has 3 (RM:548). Dead-leader skip only with >= 4 (owner s138); at 5 it waits for `fix/dl-decide-justify-n5` (section 0a). Owner s138: deferred to right before the testnet; blocks nothing in code | open (deferred s138) | genesis `validators`; trial net hosts |
| Stakes per validator | (a) equal stakes; (b) unequal (the one-down bench uses 2/2/1); the amounts are open | Genesis `validators[].stake` (GEN:228-232); voting power is whole TRS (`plans/dead-leader-skip.md` section 1). f = 1 needs 4 equal stakes (T1.4). Leader rotation is IWRR over powers; dead-leader open question 4: normalize powers before genesis (gcd or smooth WRR)? consensus-visible (`plans/dead-leader-skip.md` section 7). Draft placeholders `FILL:validator<i>_stake` | open | genesis `validators` |
| Testnet validator hosts and operators | Open; the trial net uses archy + cachyoz + 18c (two home PCs on one LAN + 18c) | Trial net plan RM:548 (roadmap commit `2bf267f3`): LAN peering + dial-out to 18c, no port forwarding; archy cannot bench while it validates. Nothing recorded for the real testnet | open | real testnet launch |
| Trial net before the real testnet | — | **Owner s131:** a trial testnet first; a pre-testnet milestone (owner s136). It doubles as the WAN test (RM:548-567) | decided (s131 / s136) | real testnet |

## 2. Consensus

| Question | Options | What we know | Status | Blocks |
|---|---|---|---|---|
| TC required + tip signing | — | Option B (owner s134): before genesis (message and header formats are genesis-bound). **Landed: TC set `d3de0abb`** (phases A-C, tc-liveness fix, past-deadline rebase, VSU window rule, `CONSENSUS_DB_FORMAT` v1); **TC tip rule `cffdbcc9`**; **batch 12 `cc8c9c4a`** (D2 membership-change sync, lost-view timeout vote, reflexive lock rule); **chain-id binding of certificates `2da29bec`**; **batch B `9083b278`** (deep all-up halt fix, post-kill log-flood / fetch fix, rpc-only fetch cap); sibling unlock + compatible-tip rule and the stored Commit PC in batch C `a300c7f3`. Phase D = bench 9o (section 126): one-down blocks/s +5.2% PASS; all-up matched/s -3.3% FAIL, exec-side, not attributable to the TC set (section 128 locates it at batch A) | landed | — |
| Consensus DB format | — | **Format 2 since batch C** (dead-leader phases 1-3, `792bf404`); older data dirs are refused at start. A fresh genesis starts with empty dirs | landed | every data dir from before `a300c7f3` |
| Reputation leader selection | — | The node refuses reputation-weighted leader selection at start (**landed `76862c2c`**, in `d3de0abb`), so genesis `reputation_leader_selection` (absent = false) must be false. Draft: `false` | landed | — |
| Dead-leader skip (b1) | — | **Landed phases 1-3 `a300c7f3`** (`792bf404`), off by default (`68dd5907`), post-exclusion freeze fix `3c5d02ea`. Phase 4 = bench 9p (4 validators, `DL_K=3`, main `3c5d02ea`). The Decide-justify gap at n > 4 is being fixed on `fix/dl-decide-justify-n5` | landed (phases 1-3); 9p and the n > 4 fix pending | 9p; the 5-validator variant |
| Dead-leader K and j cap | K: 0 = off, else 2-64; j cap: 0-10 | **Owner s138: off by default (absent K = 0); set K = 3 explicitly in the testnet genesis, only with >= 4 validators** (at 2:2:1 with the stake-1 validator down b1 halts the chain, `plans/dead-leader-skip.md` section 8 finding 4). Owner s134: genesis parameters, j cap default 6. Fields `consensus.leader_miss_threshold` / `leader_exclusion_cap`, range-checked at genesis load (`DeadLeaderParams::check`). Draft: K 0 at 3 validators, K 3 / cap 6 at 4 and 5. Still listed open in `plans/dead-leader-skip.md` section 7: re-admission evidence (Q3), IWRR normalization (Q4), delete B3 (Q5), per-epoch seeded rotation (Q7) | decided (s134 / s138); Q3 / Q4 / Q5 / Q7 open | genesis `consensus` block |
| Dead-leader (a2) short timeout | (a) add; (b) not | Owner s134: b1 first, a2 decided after the one-down bench; local timers only, no format change, so not genesis-bound | open | nothing at genesis |
| Jailing | — | **Owner s138: before mainnet** (J2 + E1/E2 and the `JailVote` gate; `JailVote` stays ungated on the owner-run testnet; a testnet upgrade through P0.0 stays possible). Design `7da1c0f1`; `DowntimeTracker` has no caller | decided (s138) | nothing at genesis |
| Block timestamp lower bound (M2) | — | **Owner s138: before genesis. Landed `e7b8f1e2`** (batch C): 30 s vote-rule lower bound with re-proposal exemptions; the time floor from timeout votes is the pre-mainnet version (section 0) | landed | — |
| Header re-send interval | — | **Decided s128:** code default 250 ms (`TORUS_HEADER_RESEND_MS`, `3429855d`), node-local (RM:466-477). The WAN check of 250 ms waits for the trial net | decided (s128); WAN check open | nothing at genesis |
| Shard custody | — | **Decided s128:** off by default until T3.1 (`d6c4d0d2`; only `TORUS_SHARD_CUSTODY=1` enables) (RM:478) | decided (s128) | nothing |
| EVM cadence N (EVM proposer concentration) | — | **Decided s131:** keep N = 11 for the testnet (coprime to 3, 4 or 5 validators); revisit before mainnet (rotate the EVM proposer by EVM height) (RM:512-519). Genesis `evm.block_interval`, absent = 11. Draft: 11 | decided (s131) | genesis `evm` block |

## 3. State, hashing and activation

| Question | Options | What we know | Status | Blocks |
|---|---|---|---|---|
| Genesis book mode | — | **Decided s136: Classic.** 9j stage 2 (perf doc section 124, `af53295c`): CL21 beats mode 3 M15 on both shapes. Genesis `consensus.book_mode` absent = Classic. b1 / mode 3 / b0 parked, not dropped (RM:568). Draft: `"classic"` written explicitly | decided (s136) | — |
| Running state hash + attestation | — | **Decided s136: ON at genesis, with attestation.** Batch 10 **landed `2d2f3ed9`**. A genesis FILE naming no activation means OFF (the Codex P1 fix in batch 10 replaced `970c5f13`'s default; only generated files and nodes started without `--genesis` default on), so the testnet genesis names it: `consensus.activations.running_state_hash: 1`. Validators attest with `--state-hash-attest-key` (their account key, `docs/validator-guide.md`) | decided (s136); landed | validator setup |
| Checkpoint interval | — | **Decided: 15 s** (s136 list; the 9j winner ran 15 s). Node-local, not a genesis field: `TORUS_CHECKPOINT_INTERVAL_MS`, unset = 15000 on validators, 0 on `--rpc-only` nodes (CR). The running hash checkpoints every 100 blocks | decided (s136) | nothing at genesis |
| L6.1 window-committed books: genesis key and spelling | (a) name no activations: OFF; (b) explicit `consensus.activations` naming `book_window_commit`; (c) the bool `consensus.book_window_commit` (alias at the hash's height). `"activations": {}` opts out of every default | **Landed** in batch 10 `2d2f3ed9` as P0.0 feature `book_window_commit` (id 1); needs the running hash and Classic. The bool alias is read on main (GEN `activation_schedule`). With the hash on, L3.1 keeps its gain only with L6.1 (RM:593-596). **Draft uses (b)**: `"activations": {"running_state_hash": 1, "book_window_commit": 1}`, no alias fields | L6.1 on: decided (s136); spelling drafted as (b) | genesis `consensus` block |
| L2.1 streamed settle default | — | **Decided s136: ON by default; landed** (batch 10 `2d2f3ed9`: `TORUS_SETTLE_STREAMED` unset = on, `0` turns it off); the hash fail-stop (`TORUS_STATE_HASH_FAILSTOP`) is on by default too. Node-local, byte-identical, can differ per validator (+0.75% exec CPU per fill accepted for +8.6% deep blocks/s, section 123) | decided (s136); landed | nothing at genesis |
| P0.0 activation schedule | — | **Decided s131:** built-in table per chain id now (plus genesis `consensus.activations`), governance `ScheduleActivation` later, before mainnet (phase 7). Old binaries past an activation height stop (UPGRADE REQUIRED). Phases 1-6 on main `40705baf` | decided (s131) | — |
| P0.0: which features the genesis schedules | (a) hash + L6.1 from block 1 (the generator default `DEFAULT_GENESIS_ACTIVATIONS`); (b) another explicit map; (c) none (`{}`) | Built-in table empty for every chain id on main (`builtin_activations`). A genesis-file and built-in height for the same feature that differ refuse to start. Features on main: `running_state_hash` 0, `book_window_commit` 1, `same_block_core_writer` 2, `funding_engine` 3, `trading_fees` 4. **Draft: (a), explicit; ids 2 / 3 / 4 NOT scheduled** (owner s138: T after genesis through P0.0; funding and fees unscheduled) | drafted per s136 / s138 | genesis `consensus.activations` |
| P0.0 with 3 validators | (a) accept a stall at H if one validator has not upgraded; (b) avoid upgrades on the trial net; (c) other | P0.0 open question 3: with 3 validators (f = 0) every upgrade needs all of them (`plans/p00-activation-height-gating.md` section 6). Scheduling T / funding / fees later is such an upgrade. No answer recorded | open | upgrade policy on the trial net |
| Genesis digest | — | **Decided s131:** a node refuses to start when the on-disk genesis file differs from the applied one (phase 4, `40705baf`). **Owner s138 (K mismatch option B): peers compare genesis digests and refuse a mismatch, landed `758a79f9`** (batch A `d595f77e`). Any edit to the genesis file after launch is refused; the digest covers every key, also keys the loader ignores | decided; landed | freezing the genesis file before launch |
| History retention | — | **Decided s131:** prune by default, N = 100,000 blocks; `--archive` (or `--retention-blocks 0`) opts out (RM:520-547; `b452b4af`; 9i soak PASS, section 121). Node-local. At least one archive node per network as the genesis-sync source (no state sync) | decided (s131) | — |
| Which node runs `--archive` | Any validator or a separate RPC node | Owner s134: decided at the trial net | open (decided at trial net) | trial net |
| Aliased position rows | — | Cannot exist on a fresh genesis (s132); reject-on-load merged `b57b3fd4` (RM:489-511) | decided (done s134) | — |

## 4. Markets, risk and economics

| Question | Options | What we know | Status | Blocks |
|---|---|---|---|---|
| Liquidator vault seed amount | Amount open | **Rule decided (owner s131, as s99):** seed the vault with capital in the genesis, ADL as the backstop once it runs dry; an HLP-style deposit vault before mainnet (RM:40, P5 RM:450-460). Mechanism: genesis `native_balances` for the vault address `0x746f7275732d6c697175696461746f722d766c74` (`LIQUIDATOR_VAULT`), no code needed. Draft placeholder `FILL:liquidator_vault_seed` | rule decided (s131); amount deferred (s138) | genesis `native_balances` |
| `delist_grace_blocks` | — | **Decided (owner s131, option C):** governance key, default 43,200 blocks (24 h at 2 s), range 0..=302,400 (RM:384-385). No genesis field; changed by `ParameterChange` | decided (s131) | — |
| Price band (`price_band_bps`) | — | Governance key, default 5000 (+-50% of the mark), from the s94 bad-debt fix `45e6de0` (RM:362). No genesis field. Before the first mark: orders refused (C, `9b1d564c`) | decided (s94 code default; s138 C) | — |
| Read-precompile gas | — | **Decided s100:** base 16,400, `getPosition` 16,500, scans base + 500 per row + 20 per word, `getOpenOrders` removed, `getOrderBook` 64 levels per side (`00bf7346`); tombstone bound accepted for testnet, in-memory `getOrderBook` before mainnet (`b26bd3b3`) (RM:342) | decided (s100) | — |
| Markets listed at genesis | Which markets, tick / lot / initial margin; or none (`[]`, list later by governance) | Genesis `markets`. An action on an unlisted market is rejected in consensus (`UnknownMarket`, `de70325d`). Owner s138 A: no markets / trading closed until signers are registered and marks fresh. Listing later needs a passing governance proposal (row "Governance voters"). Draft placeholder `FILL:markets` | open | genesis `markets` |
| Collateral / quote currency | (a) TRS, as today; (b) a stablecoin | RM P5: "decide before building P5 assets"; no testnet deadline recorded | open | genesis `markets` quote asset |
| Governance voters at genesis | (a) genesis `permanent_stakes` for named holders; (b) none, delegations after launch | Vote weight = delegated stake + permanent stake x 1.5; validator self-stake does not count (`torus-economics/src/governance.rs`). The bench base gives 10 hardhat keys (public) permanent stake: not usable on a testnet. With none, no proposal (market listing, fee rates, delist) can pass until someone delegates. Draft placeholder `FILL:governance_voter_*` | open (new s140) | genesis `permanent_stakes`; any governance change after launch |
| Treasury, dev pool and faucet accounts | Addresses and balances | `economics.treasury_address` / `dev_pool_address` plus their `accounts` rows; a faucet row (`tools/faucet`) if there is one. A malformed treasury / dev-pool address in `economics` alone is NOT refused (mapped to the zero address, GEN `chain_config` / `initialize`): DRAFT note 1. The EVM gas fee split is fixed in code (dev pool 45% at the start, 25% at the end; the loader refuses another split): DRAFT note 2 | open | genesis `economics`, `accounts` |
| Other genesis values | — | Drafted with the base / code-default values, each listed with its source in DRAFT section 3 (epoch length 100, timeouts 1200 / 30000, backoff 2 / 8, commit-lag cap 0, EVM 3M gas / N 11 / big blocks off, permanent staking 500 / 15000 bps, validator constraints incl. unbonding 604,800 blocks, commission 500 bps). `timestamp` is a placeholder; no crate reads it on main (only the digest) | drafted (carried values, not owner-decided) | genesis file |
| Product scope at launch | Which product-parity items must be live at testnet launch | Product batch **landed `8964bd4d`**: item 7 T (id 2), funding (id 3), trading fees (id 4) behind unscheduled P0.0 bits; node-local `l2Book` / `orderUpdates` WS streams (`9b1054ed`). Scheduling any of them is a P0.0 upgrade at a height (with 3 validators: all must upgrade). No owner decision recorded on what must be live at launch | open | testnet launch scope |
| Trading fee rates and destination | Rates; split of the destination | Owner s138: rates decided right before the testnet; destination pending (section 0). Governance keys with defaults 450 / 150 ppm; the destination is the treasury in code (a split needs code). Not genesis fields | open (deferred s138) | trading-fee activation, not the genesis |

## 5. Keys, oracle and operations

| Question | Options | What we know | Status | Blocks |
|---|---|---|---|---|
| Oracle signers: genesis field or post-launch registration | (a) each validator registers its oracle signer with `SetOracleSigner` after launch (`torus-wallet set-oracle-signer`); (b) add an oracle-signer field to the genesis (code change) | Production feeder signs with a separate hot oracle signer registered by `SetOracleSigner` (`docs/validator-guide.md`, `tools/price-feeder/README.md`). torus-genesis has no oracle-signer field: genesis validators are seeded with `oracle_signer: None`. The hot signer is optional; an Active validator can submit prices with its account key. A price needs >= 3 reporters and > 2/3 of Active stake after outlier removal (`crates/torus-core/src/oracle.rs`), so one signer never sets it alone; with 3 equal validators one feeder down makes marks stale after 60 s. No price yet: no panic or stall, liquidation skipped, positions valued at entry. The s138 launch risk (limit orders on a never-marked market trade at any price) is closed by price band C (**landed `9b1d564c`**: such orders are refused) plus ops rule A (section 0) | open (mechanism (a) works on main) | oracle prices at launch |
| State-hash attestation key operations | Custody, file handling, address check | Each validator runs `--state-hash-attest-key <file>` with its secp256k1 **account** key (not the Ed25519 consensus key), file mode 0600, 64 hex chars; the node submits nothing with a wrong key or while the validator is not Active and logs `addr=` at startup (`docs/validator-guide.md`, `docs/node-operator-guide.md`, CR). Separate from the oracle signer. A validator that lost its hash rows keeps voting, hash-unverified, and does not attest (P0.0 owner s134 option b). Not checked: whether a validator account needs a balance for its own attestations under the anti-spam defaults (DRAFT note 6) | mechanism decided; ops plan open | validator setup |
| Epoch-plan hand-off on validators | (a) off (code default); (b) on | Node-local; goes default-on only after 9q Part B passes (section 0a) | pending bench (9q Part B) | validator launch config |
| PE (`TORUS_PARALLEL_ENGINE`) on WAN | (a) off (code default); (b) on per validator after a WAN A/B and a mixed-flag agree check | Node-local, byte-identical; off by default (owner s128, section 83) (RM:481-485). Owner s136: waits for the trial net | open (trial net) | nothing at genesis |
| Push-manifest threshold on WAN | Value open | Benches 8 MB, devnet compose 6 MB, code default 512 KB; the WAN value needs a WAN measurement (RM:486-488). Owner s136: waits for the trial net | open (trial net) | nothing at genesis |
| Block cap 400 on WAN | Keep 400 or change | T2.5: the cap went to 400 on loopback benches only, unproven on a WAN (RM:190-192) | open (trial net) | nothing at genesis |
| Trading app / EIP-712 signer | — | Parked (owner s134: do not touch the app now, it may be rebuilt). Its TS signer does not match the governance signing change of `9f02839a`; any client must match `tests/fixtures/eip712_vectors.json` (RM:351-355, Trading UI RM:462) | decided: parked (s134) | a trading UI at launch |

## 6. Before the genesis file can be final

Still open:

- The owner fills the placeholders (DRAFT section 2): validator count, keys and stakes, chain id, liquidator-vault
  seed, markets (or `[]`), treasury / dev pool / faucet accounts, governance voters, timestamp. Then the file is
  frozen (genesis digest, section 3).
- Bench 9p (dead-leader phase 4, 4 validators, `DL_K=3`, main `3c5d02ea`): only if the genesis enables dead-leader
  skip (the 4- or 5-validator variant).
- `fix/dl-decide-justify-n5` (Decide-justify gap at n > 4): only for the 5-validator variant with K = 3.
- Bench 9q Part B: decides the `TORUS_EPOCH_PLAN_HANDOFF` default (validator config, not the genesis).
- The trial net (section 1), and the WAN items of section 5.

Done (moved here from the s136 list; all on main):

- TC set (batch 9) `d3de0abb`, TC tip rule `cffdbcc9`, batch 12 `cc8c9c4a`, chain-id safety `71333e7e` + `2da29bec`,
  batch B `9083b278`; phase D = 9o (section 126).
- Batch 10 (L6.1 + L2.1 with P0.0, the defaults) `2d2f3ed9`.
- Batch A `d595f77e`: price band C `9b1d564c`, genesis-digest handshake `758a79f9`.
- Batch C `a300c7f3`: M2 `e7b8f1e2`, dead-leader phases 1-3 `792bf404` (K = 0 default `68dd5907`), stored Commit PC
  `e01a85cb`, sibling unlock `72d86da5`; post-exclusion freeze fix `3c5d02ea`.
- Epoch-plan hand-off `b17dab16` (default off).
- Product batch `8964bd4d` (T / funding / fees, unscheduled; not needed at genesis).
- Capacity (launch-readiness, not a genesis field): 1M-account RAM check PASSED (8f, section 93); 9n PASSED
  (sections 126 / 127), ~10 s restart accepted (s140).
