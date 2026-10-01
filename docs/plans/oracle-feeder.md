# Design: Validator price feeder (item 2, option B) — rev. 2

**Status (s517): implemented** on `feat/oracle-feeder` (local), stacked on
`feat/oracle-aggregation` @ `d4fe69c` (option A: time-based on-chain aggregation, window 10 s,
stale 60 s, >= 3 reporters holding > 2/3 of Active stake). Plan:
`docs/plans/oracle-feeder-impl.md` (its "Implementation corrections" section lists every
deviation from the draft). Runbook: `tools/price-feeder/README.md`.

| Commit | Content |
|--------|---------|
| `6b33a7d` | 1. Oracle signer: `SetOracleSigner`, `ValidatorState.oracle_signer`, reverse index, reporter resolution. Consensus, lockstep, fresh genesis. |
| `cbab523` | 2. Mempool priority: cancels, then oracle, then the rest. Active validator/signer gate, 4 per validator, RPC screens, pacing tier. Node-local. |
| `5633829` | 3. RPC: `getValidators.oracleSigner`; ingress checks for oracle submissions. |
| `55cb396` | 4. Wallet: lib + bin; `torus-wallet set-oracle-signer` (flat command, not `validator set-oracle-signer`). |
| `1c4b164` | 5. `tools/price-feeder` library. |
| `d20715b` | 6. Feeder CLI, `/health` + `/metrics`, end-to-end test against a real RPC server. |

Q-S1 is decided: the signer's own account is unrestricted (D-S1), with no per-action lookup.
Q-S2: a Candidate may set its signer; a Tombstoned validator may not.

Rev. 2 applies the user decisions on rev. 1:

* Session keys are replaced by an HL-style hot **oracle signer address**.
* Priority order is cancels first, then oracle.
* Q2, Q3 and Q5 are settled.

## Problem

Option A aggregates `SubmitOraclePrices` on chain, but nothing produces them. Without a feeder,
no mark exists (`AccountReader::mark` is `None`). Margin therefore values positions at entry,
and liquidation cannot work.

## Decisions (user, s517)

1. **Separate program** `tools/price-feeder`, one per validator. It submits
   `SubmitOraclePrices` to the LOCAL node via `torus_submitNativeAction`
   (`crates/torus-rpc/src/torus.rs:142-143, 1111-1165`).
2. **Hot signer address** (HL model: cold validator wallet + hot signer wallet).
   * The validator record gets an optional `oracle_signer` address.
   * A new native action, signed by the validator's own EVM key, sets and rotates it.
   * The signer may ONLY submit `SubmitOraclePrices` on behalf of that validator.
   * It has no expiry. Rotation replaces it.
   * The feeder holds only the signer key (wallet keystore or a 0600 key file).
   * This is a consensus change: lockstep upgrade (fresh genesis is already required by the
     stack).
3. **Ticker mapping in a per-validator TOML config.** HL weights: Binance 3, OKX 2, Bybit 2,
   Kraken 1, KuCoin 1, Gate 1, MEXC 1. Markets that are not configured are not submitted.
4. **Quote:** USDT/USDC count as USD 1:1 by default. An optional Kraken USDT/USD conversion can
   be enabled in the config.
5. **Price** = weighted median of fresh mids (sample <= 5 s old, fetched in <= 2 s).
   * A market is submitted only with **>= 3 sources AND >= 50 % of its configured weight**;
     otherwise it is left out.
   * The feeder **never** sends a submission that fails on-chain validation.
6. **MATIC → POL** venue tickers.
7. **Cadence** 3 s, configurable.
8. **Mempool:**
   * Order: **cancels first, then oracle, then the rest.**
   * Priority and backlog bypass apply only to Active validators or their registered signers.
   * At most **4 pending per validator**.
9. Devnet: unchanged.

## Verified facts this design rests on (d4fe69c)

| Fact | Where |
|------|-------|
| `ValidatorState` uses custom borsh with 8 fields and no signer. It is stored in `CF_STAKING_VALIDATORS`, keyed by address. `all_validators` decodes EVERY value in that CF, so foreign keys there would break it | `torus-economics/src/types.rs:77-134`; `staking.rs:784, 913-923` |
| `CF_STAKING_VALIDATORS` (bucket 5) and `CF_NATIVE_ORACLE` (bucket 3) are native-root CFs | `torus-state/src/native_trie.rs:48-53` |
| Oracle CF prefixes in use: `"sub"` and `"agg"`, read only by prefix scans | `torus-core/src/oracle.rs:181-200, 353, 369, 381` |
| `exec_submit_oracle_prices(ctx, sender, …)` checks Active on `sender` and writes rows with `submit_price(sender, …)` | `torus-bridge/src/native_executor.rs:6512-6560` |
| Dispatch arm. `classify_action` maps `SubmitOraclePrices` → `Oracle` and anything unknown → `Other`. Post-EVM order: GTC, lockbox, **oracle**, governance, staking, **other** | NE:3383-3388, 7161-7200 |
| `NativeAction` variants are appended at the end (serde index stability). Canonical tags currently go up to 26 (`ClaimUnbonded`) | `torus-types/src/lib.rs:530-635, 681-903` |
| EIP-712 struct-hash dispatch; `requires_eip712`; the `SessionScope::Full` exclusion list | `eip712.rs:195-243, 757-769`; `lib.rs:225-236` |
| There are 10 `ValidatorState { … }` literals in 6 files (types, staking, genesis, app.rs, oracle_block_tests, chaos) | grep at d4fe69c |
| Pool `SortKey = (priority, sender, nonce, seq)`, with cancels at priority 0; eviction; the cancels-only walk; `is_cancel` | `torus-mempool/src/native_pool.rs:13-35, 174-232, 398-440, 540-547` |
| Every pool insert path ends in `submit_native_action_inner` | `torus-mempool/src/lib.rs:643-669` (callers :449, :479, :607) |
| Selection runs only on the proposer path | `torus-consensus/src/app.rs:5713, 5727` |
| The RPC backlog and pool-full screens let only cancels through | `torus.rs:442-466, 1121-1131` |
| Ingress `validate_known_markets` checks orders only | `torus.rs:257-281` |
| `getValidators` returns `address, pubkey, power, commissionBps, status` | `torus.rs:1040-1075`; `types.rs:414-422` |
| The wallet is a **binary-only** crate. Its keystore (Argon2id + AES-GCM) is in `keystore.rs` | `tools/wallet/src/main.rs:3-9`, `keystore.rs` |

## Design

### 1. Oracle signer (consensus, own commit)

**State**

* `ValidatorState.oracle_signer: Option<Address>`, appended to the borsh layout. This is the
  user-requested field on the validator record. The 10 literals get `oracle_signer: None`.
* A reverse index in `CF_NATIVE_ORACLE`: key `"sgn" ‖ signer(20)` → `validator(20)`.
  * The key builder is `torus_state::cf::oracle_signer_key`. It is the single source, like
    `native_nonce_key`, so exec and the mempool share it.
  * The index cannot live in `CF_STAKING_VALIDATORS`, because `all_validators` decodes every
    value there. Both CFs are in the native root.

**Action:** `NativeAction::SetOracleSigner { signer: Address }`, appended after `ClaimUnbonded`.

* `Address::ZERO` clears the signer.
* EIP-712 type: `SetOracleSigner(address signer,uint64 nonce)`. Canonical tag 28 (rebase s87: 26 = main's AttestStateHash, 27 = ClaimUnbonded).
* It is added to `requires_eip712` and to the `SessionScope::Full` exclusion, so only the
  validator's EVM key can sign it.
* `classify_action` maps it to `Other`.

**Exec rules** (`exec_set_oracle_signer`). Everything is validated before anything is written,
because there is no per-action rollback (NE:3291).

* The sender must have a validator record, in any status except Tombstoned. A non-validator is
  rejected.
* Any status except Tombstoned is allowed so the signer can be set before activation.
* `signer != sender`, and the signer must not have a validator record of its own.
* **A signer serves at most one validator.** If `"sgn"‖signer` maps to another validator, the
  action is rejected with "signer already serves validator X".
* Re-setting the current signer is a no-op success.
* Writes: delete the old index entry, write the new one, update the record. Rotation therefore
  invalidates the old signer immediately.

**Reporter resolution** in `exec_submit_oracle_prices`:

1. If `sender` has a validator record, the reporter is `sender`. Direct submission keeps working,
   as in the existing tests.
2. Otherwise, if `"sgn"‖sender` maps to V **and** `V.oracle_signer == Some(sender)`, the reporter
   is V.
3. Otherwise the action is rejected with "not a registered validator or oracle signer".

The Active check, the `(market, validator)` row and the stake weight all use the reporter.

**Ordering note.** Within one block, `SetOracleSigner` (category Other) runs after
`SubmitOraclePrices` (category Oracle). A rotation therefore takes effect from the next block.
This is pinned by a test.

**Decision D-S1: what "nothing else is authorized for the signer" means.**

* The signer has no authority over the validator's account.
* Every other handler keys off `sender`, which is the signer's own address. A signer's orders,
  cancels, withdrawals or staking actions can therefore never touch the validator's balances,
  positions, orders or stake.
* This holds by construction. The test checks that the validator's state is byte-identical
  afterwards.
* The alternative is to block every non-oracle action sent FROM a signer address, including
  actions on its own account. That costs one index read per native action on the hot exec path,
  across several dispatch paths. It is offered as Q-S1, not planned.
* Operators should use a fresh, unfunded signer address.

### 2. Mempool priority (own commit, node-local)

* `SortKey` priority classes: **0 = cancel, 1 = oracle submission, 2 = everything else.**
* New functions `is_oracle_submission` and `is_priority`; `is_cancel` stays.
* Everywhere cancels get special treatment today, oracle submissions get it too:
  * eviction of a normal entry when the pool is full;
  * the RPC backlog and pool-full pre-verify screens;
  * the deepest pacing tier, which becomes priority-only (function names stay).
* **Gate.** In `Mempool::submit_native_action_inner`, an oracle submission is admitted only if:
  * its sender is an Active validator, or
  * its sender is a signer whose index entry points to an Active validator with
    `oracle_signer == Some(sender)`.

  This costs at most 2 point reads.
* **Cap: 4 pending per validator.**
  * Counted over the validator's own address plus its signer, with `BTreeMap::range` on
    `(1, addr, ..)`.
  * The check and the insert happen under one pool write lock.
* **Not consensus-visible.** Admission and selection run only on the proposer, and validators
  never re-derive selection. It still ships with the lockstep signer upgrade.

### 3. RPC (own commit)

* `getValidators` gets an additional optional field `oracleSigner`, so the feeder can verify its
  registration.
* `validate_known_markets` applies the exec submission rules at ingress (1..=256 entries, no
  duplicates, listed markets, `valid_oracle_price`). The feeder then gets an error back when it
  submits, instead of a silent exec failure. This check runs at ingress only.

### 4. Operator tooling

* `tools/wallet` becomes lib + bin (`src/lib.rs` exports `keystore`). The feeder reuses the
  keystore code instead of copying it.
* `torus-wallet --keystore <validator EVM keystore> set-oracle-signer --signer <addr>` (s517: flat command; the wallet has no `validator` group)
  (or `--clear`). The operator runs it once, and again only to rotate.
* `price-feeder keygen --keystore <out>` creates the signer keystore with the wallet code. It
  prints the signer address and the exact `set-oracle-signer` command.

### 5. `tools/price-feeder`

```
config.rs    TOML schema + validation (fatal at startup)
price.rs     decimal -> FixedPoint, mid, lower weighted median, per-market aggregation
exchange.rs  Exchange enum (7), URL builders, PURE response parsers, HttpGet trait
fetch.rs     concurrent per-exchange fetch, 2 s timeout, per-exchange backoff, Clock
submit.rs    listed/valid filter, <= 256 chunking, monotonic nonce, EIP-712 signing (signer key)
node.rs      NodeApi trait + JSON-RPC client; startup checks
feeder.rs    Feeder<H, N, C>::run_cycle -> CycleReport; run loop
health.rs    /health (JSON, 200/503) + /metrics (Prometheus text), tiny TCP server (faucet pattern)
main.rs      clap: keygen | address | check | run
```

**Tests.** No test touches the live network.

* Parsers run over recorded fixtures.
* `HttpGet`, `NodeApi` and `Clock` each have a fake.
* One end-to-end test runs a real `RpcServer` + `Mempool` on a temp `StateDb` holding an Active
  validator and its registered signer.

**Venues.** One bulk request per venue per cycle. Real fixtures are recorded once at
implementation time; tests never fetch.

| Exchange | Endpoint | Fields |
|---|---|---|
| Binance | `/api/v3/ticker/bookTicker?symbols=[...]` | `symbol,bidPrice,askPrice` |
| OKX | `/api/v5/market/tickers?instType=SPOT` | `data[].instId,bidPx,askPx` |
| Bybit | `/v5/market/tickers?category=spot` | `result.list[].symbol,bid1Price,ask1Price` |
| Kraken | `/0/public/Ticker?pair=A,B` | `result.<KEY>.b[0], .a[0]` |
| KuCoin | `/api/v1/market/allTickers` | `data.ticker[].symbol,buy,sell` |
| Gate | `/api/v4/spot/tickers` | `[].currency_pair,highest_bid,lowest_ask` |
| MEXC | `/api/v3/ticker/bookTicker` | `[].symbol,bidPrice,askPrice` |

**Price path (integers only).**

* Mid = (bid + ask) / 2 on raw `FixedPoint` values.
* A quote is invalid if bid <= 0 or bid > ask.
* Decimals are parsed digit by digit and truncated past 8 places. Signs, exponents and overflow
  are rejected.
* Response bodies are capped at 4 MiB.

**Freshness and backoff.**

* A sample counts iff `now − fetched_at <= 5000 ms`. A fetch times out at 2000 ms.
* A failing venue keeps its last good quotes, which age out by the same rule.
* Backoff after k consecutive failures: `min(1 s · 2^(k−1), 60 s)`. A success resets it.

**Aggregation per market.**

1. Take the fresh, valid sources.
2. Convert to USD:
   * `quote_mode = "par"` (default): USDT and USDC count 1:1.
   * `"kraken_usdt"`: USDT mids are multiplied by Kraken `USDTZUSD`. If the rate is missing,
     the USDT sources are dropped for that cycle.
3. Require **n >= 3 AND weight >= 50 %** of the market's configured weight. Otherwise the market
   is left out.
4. Price = lower weighted median: the first price, in ascending order, where `2·cum >= total`.

**Submission (never invalid).**

* `getMarkets` (paged, 500) is called every cycle.
* Kept: configured markets that are listed and pass `valid_oracle_price` (imported from
  `torus-core`).
* Markets are de-duplicated in a `BTreeMap` and split into chunks of 256. If nothing is left,
  nothing is sent.
* Nonce: `max(now_ms, last + 1)`. Signature: `sign_native_action(…, &signer_key)`.
* Submit errors are classified:
  * Retryable ("busy" / "overloaded"): the next cycle tries again.
  * NotAuthorized ("not a registered validator or oracle signer" / "not an active
    validator"): health goes down.
  * Other.

**Startup checks** (`check` and `run`; re-checked every 60 s while running).

* The config is valid. The key loads; a key file must be mode <= 0600.
* `getValidators` lists `validator_address`:
  * `oracleSigner` must equal the feeder's signer address. Otherwise this is fatal, and the
    error includes the registration command.
  * The status must be active. Otherwise the feeder warns and stays idle.
* Every configured market that is listed must have the same `baseAsset` (case-insensitive) and
  `quoteAsset == "USD"`. Otherwise this is fatal.
* Configured markets that are not listed produce a warning and are skipped.

**Health.**

* `status`: ok, degraded or down. Down (HTTP 503) means no accepted submission in 3 × interval,
  or a NotAuthorized error.
* Per market: last price, number of sources, weight, and the reason it was left out.
* Per venue: ok, latency, last error, backoff.
* Metrics: `feeder_cycles_total`, `feeder_submit_total{result}`,
  `feeder_source_errors_total{exchange}`, `feeder_market_omitted_total{market,reason}`,
  `feeder_last_submit_ok_unixtime_ms`.

### 6. Example config (`tools/price-feeder/feeder.example.toml`)

```toml
rpc_url = "http://127.0.0.1:8545"
signer_keystore = "/etc/torus/price-feeder/signer.keystore"   # or: signer_key_file (hex, 0600)
passphrase_file = "/etc/torus/price-feeder/passphrase"
validator_address = "0x…"            # the cold validator address that registered this signer
interval_ms = 3000
fetch_timeout_ms = 2000
max_source_age_ms = 5000
min_sources = 3
min_weight_bps = 5000
quote_mode = "par"                   # or "kraken_usdt"
health_listen = "127.0.0.1:9466"

[[markets]]
market_id = 1
base_asset = "BTC"
symbols = { binance = "BTCUSDT", okx = "BTC-USDT", bybit = "BTCUSDT", kraken = "XXBTZUSD",
            kucoin = "BTC-USDT", gate = "BTC_USDT", mexc = "BTCUSDT" }

[[markets]]                          # MATIC market: POL tickers (decision 6)
market_id = 9
base_asset = "MATIC"
symbols = { binance = "POLUSDT", okx = "POL-USDT", bybit = "POLUSDT", kraken = "POLUSD",
            kucoin = "POL-USDT", gate = "POL_USDT", mexc = "POLUSDT" }
```

* The startup check compares `base_asset` with the on-chain `baseAsset`. For the MATIC market,
  `base_asset` stays `"MATIC"`; only the venue symbols are POL.
* A symbol's quote defaults to USD for Kraken and USDT for the other venues. It can be
  overridden per symbol with `{ symbol = "...", quote = "USDC" }`.

## Risks

* **Hot signer theft.** The thief can submit wrong prices for one validator until the operator
  rotates the signer.
  * On chain this is bounded by the stake-weighted median and the 3×MAD cut.
  * The thief cannot touch the validator's funds (D-S1).
* **Same-block rotation.** A submission from the old signer still counts in the block that
  rotates it (category order). From the next block, the old signer is rejected.
* **Signer reuse.**
  * Registering a signer that already serves another validator is rejected (one validator per
    signer).
  * If a signer address later registers as a validator, it resolves as itself (rule 1). The
    stale index entry is ignored because of the `oracle_signer == Some(sender)` cross-check.
* **Stablecoin depeg** under `par`: switch to `kraken_usdt`.
* **Symbol drift**: visible in `/health`, and `check` lists missing symbols.
* **Bulk bodies**: KuCoin, OKX and Gate return all tickers (about 0.5–1 MB each). Capped at
  4 MiB.
* **Delist race**: a market delisted between `getMarkets` and exec costs one missed round.
* **No exec feedback** after RPC acceptance: only the aggregate is observable.
