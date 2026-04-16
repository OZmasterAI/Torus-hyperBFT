# Technical Requirements: CLI Wallet Tool (Section 3.5.4)

**Date:** 2026-04-16 (revised)
**Status:** Draft v2.0 (matches actual codebase)
**Parent:** [implementation-plan.md](./implementation-plan.md) Task 3.5.4
**Depends on:** `torus-rpc` (§1.8), native action format (§1.3b), secp256k1 keystore (already in `tools/wallet`)

---

## Summary

A standalone CLI binary (`torus-wallet`) for interacting with a Torus node.
Targets node operators and developers — not end users. Covers: key management,
native action signing/submission, balance/position queries, and staking operations.

**Ground truth from the codebase:**

- Binary lives at `tools/wallet/` (workspace member), NOT `crates/torus-wallet/`.
- Has its own secp256k1 keystore (AES-256-GCM + Argon2id) at `tools/wallet/src/keystore.rs`.
  Independent of `torus-node`'s ed25519 keystore — **do not extract or share**.
- Signing uses the free function `torus_types::eip712::sign_native_action(action, nonce, &key) -> SignedNativeAction`.
  There is NO `SignedNativeAction::sign(...)` method. The function takes **three** args; chain_id is
  baked into the EIP-712 domain separator via `TORUS_CHAIN_ID`.
- Nonce is **milliseconds since epoch** (`NONCE_WINDOW_MS = 60_000`), not nanoseconds.
- `FixedPoint` is `i128` with `SCALE = 10^8` (`DECIMALS = 8`), NOT 10^18. All trading prices/quantities
  use this. All TRS transfers/staking amounts use `U256` with 18 decimals (wei).
- `torus_submitNativeAction` expects a **hex-encoded JSON byte string** (`parse_bytes` → `serde_json::from_slice`).
  The existing `tools/wallet/src/rpc.rs:105` sends raw JSON — this is a bug to fix in Phase 0.
- Output: human-readable by default, JSON with `--json`. This is the inverse of earlier drafts
  that proposed `--pretty`; `--json` is already wired into every handler.

---

## 1. Scope

### 1.1 In Scope

| Category | Commands |
|---|---|
| **Key management** | `keygen`, `import`, `address` |
| **Query** | `balance`, `positions`, `orders`, `staking`, `delegations`, `epoch`, `validators`, `proposals`, `block`, `tx`, `orderbook`, `position` |
| **Trading** | `place-order`, `cancel-order`, `cancel-all`, `modify-order` |
| **Transfers** | `send` (EVM), `transfer-to-perp`, `transfer-to-spot`, `withdraw` |
| **Staking** | `delegate`, `undelegate`, `permanent-stake`, `claim-rewards`, `top-up-self-stake` |
| **Governance** | `submit-proposal`, `vote` |
| **Validator ops** | `register-validator`, `update-commission`, `jail-vote`, `unjail`, `rotate-key` |

### 1.2 Out of Scope

- EVM contract calls beyond `send` (use `cast send` / Foundry)
- Batch/scripted operations (users pipe JSON)
- Hardware wallet integration (future)
- Interactive TUI mode

---

## 2. Architecture

### 2.1 File Layout (modular)

```
tools/wallet/
  Cargo.toml
  src/
    main.rs              # Cli struct, Command enum, dispatch — ~200 lines
    keystore.rs          # existing secp256k1 + AES-256-GCM + Argon2id
    rpc.rs               # JSON-RPC client (eth_* + torus_*)
    parse.rs             # parse_trs_to_wei, parse_decimal_to_fixed_point,
                         # parse_address, parse_pubkey
    sign.rs              # load_signing_key, prompt_passphrase, now_ms,
                         # submit_native_action, build_and_sign_eip1559_tx
    commands/
      mod.rs             # re-exports
      keys.rs            # keygen, import, address
      query.rs           # balance, validators, staking, block, tx, orderbook,
                         # position, proposals, orders, positions, epoch, delegations
      trading.rs         # place_order, cancel_order, cancel_all, modify_order
      transfer.rs        # send, transfer_to_perp, transfer_to_spot, withdraw
      staking.rs         # delegate, undelegate, permanent_stake,
                         # claim_rewards, top_up_self_stake
      governance.rs      # submit_proposal, vote
      validator.rs       # register_validator, update_commission, jail_vote,
                         # unjail, rotate_key
```

The `Command` enum in `main.rs` stays flat — users type `torus-wallet place-order`, not
`torus-wallet trading place-order`. Each variant dispatches to a handler in the
appropriate module.

### 2.2 Global Flags

| Flag | Default | Description |
|---|---|---|
| `--rpc-url <url>` | `http://localhost:8545` | Node RPC endpoint |
| `--keystore <path>` | None | Encrypted keystore file (required for write commands, optional for queries with fallback) |
| `--key <hex>` | None | Raw private key (UNSAFE; warns on use) |
| `--chain-id <u64>` | from `eth_chainId` | Override chain ID |
| `--json` | false | Emit JSON instead of human-readable (already exists; kept as-is) |
| `--dry-run` | false | Build + sign, print SignedNativeAction JSON, do NOT submit |
| `--passphrase-file <path>` | None | Read passphrase from file (trims trailing whitespace). Exits 1 if file missing |

**Deviation from earlier drafts:** `--pretty` was dropped. `--json` stays as the opt-in
JSON flag (inverse logic). Every existing handler already branches on `cli.json` — flipping
this would touch every handler for no user benefit.

### 2.3 Signing Flow (write commands)

```
1. Parse CLI args → NativeAction variant
2. Resolve signing key:
   - If --key <hex>: decode hex, warn to stderr
   - Else if --keystore: read --passphrase-file (trim) OR prompt, decrypt
   - Else: error
3. nonce = ms since epoch (u64)
4. signed = sign_native_action(action, nonce, &key)   // free function, 3 args
5. If --dry-run: println!("{}", serde_json::to_string_pretty(&signed)); exit 0
6. json_str = serde_json::to_string(&signed)
7. hex_payload = format!("0x{}", hex::encode(json_str.as_bytes()))
8. rpc.call("torus_submitNativeAction", [hex_payload])
9. Print returned tx hash
```

The hex-encoding at step 7 is **required** — the server does `parse_bytes(&arg)` →
`hex::decode()` → `serde_json::from_slice()`. Sending raw JSON produces `bad hex` at
step 7 server-side.

### 2.4 Query Flow

```
1. Parse CLI args
2. Resolve address (if query takes optional address): explicit --address, else keystore
3. rpc.call("<method>", [...])
4. If --json: pretty-print JSON
   Else: format per command's human-readable template
```

No keystore needed unless address fallback triggers.

---

## 3. Command Specifications

Decimal convention:

- **TRS / wei** (18 decimals): `parse_trs_to_wei(&str) -> U256`. Used for: `send`,
  `transfer-to-perp`, `transfer-to-spot`, `withdraw`, all staking amounts.
- **FixedPoint** (8 decimals, `i128`): `parse_decimal_to_fixed_point(&str) -> FixedPoint`.
  Used for: `place-order` price/quantity/trigger, `modify-order`, all `MarketParams` /
  `MarketListing` fields in `submit-proposal`.

These MUST NOT be confused. Trading uses FixedPoint; money movement uses wei.

### 3.1 Key Management

**`keygen --output <path>`** — Generate secp256k1 keypair, prompt passphrase, write encrypted keystore.

**`import --key <hex> --output <path>`** — Import raw hex private key into encrypted keystore.

**`address --keystore <path>`** — Decrypt keystore, print Ethereum address. No RPC.

### 3.2 Trading Commands

**`place-order`** → `NativeAction::PlaceOrder(PlaceOrderParams { ... })`

```
--market <u64>                                       MarketId
--side <buy|sell>                                    → is_buy: bool
--price <decimal>                                    FixedPoint
--quantity <decimal>                                 FixedPoint
--order-type <limit|market|stop-market|stop-limit>   OrderType variant
--tif <gtc|ioc|fok|post-only>                        TimeInForce variant
--trigger-price <decimal>                            required for stop-market|stop-limit
--reduce-only                                        flag, default false
--client-id <u64>                                    optional (maps to client_order_id)
```

Mappings:

- `limit` → `OrderType::Limit`
- `market` → `OrderType::Market`
- `stop-market` → `OrderType::StopMarket { trigger }` (error if `--trigger-price` absent)
- `stop-limit` → `OrderType::StopLimit { trigger, limit: <price value> }` (error if `--trigger-price` absent)
- `gtc`/`ioc`/`fok`/`post-only` → `TimeInForce::{GTC,IOC,FOK,PostOnly}`

**`cancel-order --order-id <u128>`** → `NativeAction::CancelOrder { order_id }`.
**No `--market` flag** — `CancelOrder` variant has no market_id field.

**`cancel-all --market <u64>`** → `NativeAction::CancelAllOrders { market_id: Some(mid) }`.
CLI always requires `--market`; to cancel across multiple markets, run per-market. The
underlying `Option<MarketId>` accepts `None`, but we don't expose that (prevents accidents).

**`modify-order --order-id <u128> [--price <decimal>] [--quantity <decimal>]`**
→ `NativeAction::ModifyOrder { order_id, new_price, new_qty }`.
**No `--market` flag.** At least one of `--price` or `--quantity` MUST be provided
(error otherwise). Unset fields become `None`.

### 3.3 Transfer Commands

**`send --to <addr> --value <trs>`** — EIP-1559 EVM transfer (already implemented).

**`transfer-to-perp --amount <trs>`** → `NativeAction::TransferToPerp { amount: U256 }`.

**`transfer-to-spot --amount <trs>`** → `NativeAction::TransferToSpot { amount: U256 }`.

**`withdraw --to <addr> --amount <trs>`** → `NativeAction::Withdraw { amount, to }`.

### 3.4 Staking Commands

**`delegate --validator <addr> --amount <trs>`** (already implemented).

**`undelegate --validator <addr> --amount <trs>`** (already implemented).

**`permanent-stake --amount <trs>`** → `NativeAction::PermanentStake { amount }`. Irreversible.

**`claim-rewards`** (already implemented).

**`top-up-self-stake --amount <trs>`** → `NativeAction::TopUpSelfStake { amount }`. Validator-only.

### 3.5 Governance Commands

**`vote --proposal <u64> --option <yes|no|abstain>`** (already implemented).

**`submit-proposal --title <str> --description <str> --proposal-type <kind> --params <json>`**
→ `NativeAction::SubmitProposal(Proposal { title, description, action })`

`--params` is a JSON string. The 5 `ProposalAction` variants map as follows:

| `--proposal-type` | `--params` JSON | `ProposalAction` |
|---|---|---|
| `param-change` | `{"key":"epoch_length","value":"1000"}` | `ParameterChange { key, value }` |
| `list-market` | `{"base_asset":"BTC","quote_asset":"USD","tick_size":"0.01","lot_size":"0.001","max_leverage":50,"maintenance_margin_bps":300}` | `ListMarket(MarketListing { ... })` |
| `delist-market` | `{"market_id":5}` | `DelistMarket { market_id }` |
| `update-market-params` | `{"market_id":1,"tick_size":"0.01","lot_size":"0.001","max_leverage":20,"maintenance_margin_bps":500,"max_funding_rate_bps":100}` | `UpdateMarketParams { market_id, params: MarketParams { ... } }` |
| `validator-registration` | `{"candidate":"0xabc..."}` | `ValidatorRegistration { candidate }` |

Notes:

- `MarketListing` fields: `base_asset`, `quote_asset`, `tick_size`, `lot_size`, `max_leverage`, `maintenance_margin_bps` (no `max_funding_rate_bps`).
- `MarketParams` fields: `tick_size`, `lot_size`, `max_leverage`, `maintenance_margin_bps`, `max_funding_rate_bps`.
- `tick_size` and `lot_size` are decimal strings → `parse_decimal_to_fixed_point`.
- Unknown `--proposal-type` or missing JSON fields → exit 1 with a clear error.

### 3.6 Validator Operations

**`register-validator --pubkey <hex64> --commission-bps <u16>`**
→ `NativeAction::RegisterValidator { pubkey: PublicKey([u8;32]), commission }`.
`--pubkey` is 64 hex chars (optional `0x` prefix) = 32 bytes.

**`update-commission --commission-bps <u16>`** → `NativeAction::UpdateCommission { new_rate }`.

**`jail-vote --validator <addr>`** → `NativeAction::JailVote { target }`.

**`unjail`** → `NativeAction::UnjailSelf`.

**`rotate-key --new-pubkey <hex64>`** → `NativeAction::RotateValidatorKey { new_pubkey }`.

### 3.7 Query Commands

All queries that accept `--address` default to the keystore address if `--keystore` is set
(`resolve_address()` helper). If neither `--address` nor `--keystore` is given, exit 1
with `address required: provide --address or --keystore`.

**`balance [--address <addr>]`** — EVM balance + `torus_getBalances` (spot/perp).

**`positions [--address <addr>]`** — iterate `torus_getMarkets`, call `torus_getPosition` per market, collect non-null.

**`orders [--address <addr>] [--market <u64>]`** — `torus_getOpenOrders(trader, market_id_opt)`.

**`staking [--address <addr>]`** (already `Staking`, rename arg to optional) — `torus_getStakingInfo` + `torus_getDelegations`.

**`delegations [--address <addr>]`** — `torus_getDelegations` only.

**`validators`** (already implemented) — `torus_getValidators`.

**`epoch`** — `torus_getEpoch`. Display: epoch, start block, end block, blocks remaining.

**`proposals [--id <u64>]`** — `torus_getProposal(id)` if `--id`, else `torus_getProposals(None)`.

**`block [<height>]`**, **`tx <hash>`**, **`orderbook <market_id>`**, **`position <addr> <market_id>`** — already implemented.

---

## 4. RPC Mapping

| Command | RPC Method | Note |
|---|---|---|
| `balance` | `eth_getBalance` + `torus_getBalances` | |
| `positions` | `torus_getMarkets` → `torus_getPosition` loop | No single-call endpoint |
| `orders` | `torus_getOpenOrders` | New method to add in rpc.rs |
| `staking` | `torus_getStakingInfo` + `torus_getDelegations` | Existing |
| `delegations` | `torus_getDelegations` | Existing |
| `validators` | `torus_getValidators` | Existing |
| `epoch` | `torus_getEpoch` | New method to add in rpc.rs |
| `proposals` | `torus_getProposals` / `torus_getProposal` | Existing: getProposals; add getProposal |
| All write commands | `torus_submitNativeAction` (hex-encoded JSON) | Fix encoding in Phase 0 |

---

## 5. Code Reuse

### 5.1 Keystore

`tools/wallet/src/keystore.rs` (secp256k1 + AES-256-GCM + Argon2id) is the single source.
**Do not** extract to a shared crate or share with `torus-node` — those use different
key types (ed25519 validator keys, different threat model).

### 5.2 Signing

`torus_types::eip712::sign_native_action(action, nonce, &key)` — free function, 3 args,
returns `SignedNativeAction` directly (not `Result`). Use as-is; no wallet-side signing
code needed.

`torus_types::eip712::ecrecover` and `SignedNativeAction::recover_sender` are available
for test-side signature verification.

---

## 6. Error Handling

| Condition | Behavior |
|---|---|
| Node unreachable | Print RPC URL + reqwest error, exit 1 |
| Invalid keystore passphrase | `Failed to decrypt keystore`, exit 1 |
| `--passphrase-file` missing | `passphrase file not found: <path>`, exit 1 |
| Action rejected by node | Print RPC error message, exit 1 |
| Invalid CLI args | clap's default error, exit 2 |
| `--dry-run` | Print signed action JSON to stdout, exit 0 (no submission) |
| Missing `--trigger-price` on stop-market/stop-limit | exit 1 with explicit message |
| Neither `--price` nor `--quantity` on `modify-order` | exit 1 with explicit message |
| Too many decimals in FixedPoint parse (>8) | exit 1: `too many decimal places (max 8)` |
| Invalid hex pubkey | exit 1: `invalid pubkey: expected 64 hex characters` |

No `unwrap()` in non-test code. All errors are `Result<(), String>` propagated through `main()`.

---

## 7. Testing

### 7.1 Unit tests (in `#[cfg(test)] mod tests`, per-module where natural)

| Test | Module | Verifies |
|---|---|---|
| `test_parse_decimal_to_fixed_point` | parse.rs | `"1.5"`→150_000_000, `"0.00000001"`→1, `"100"`→10_000_000_000, `"-1.5"`→-150_000_000, `"1.123456789"`→error, `""`→error |
| `test_parse_trs_to_wei` | parse.rs | existing (keep) |
| `test_parse_pubkey` | parse.rs | valid 64-char with/without 0x, wrong length, non-hex |
| `test_parse_address` | parse.rs | existing (keep) |
| `test_passphrase_file_trim` | sign.rs | trailing newline/spaces trimmed correctly |
| `test_place_order_roundtrip` | trading.rs | build PlaceOrderParams (stop-limit with trigger + client_id) → sign → recover sender → address matches |
| `test_all_new_variants_sign_correctly` | main.rs or individual | each new variant (TransferToPerp, Withdraw, PermanentStake, TopUpSelfStake, SubmitProposal×5 variants, RegisterValidator, UpdateCommission, JailVote, UnjailSelf, RotateValidatorKey) signs and recovers |
| `test_dry_run_output_is_valid_json` | sign.rs | build NativeAction::ClaimRewards → serialize pretty → round-trip deserialize to SignedNativeAction; fields match |
| `test_submit_hex_encoding` | rpc.rs | given raw JSON, produced payload starts with `0x` and decodes back to the same JSON |

### 7.2 Integration (manual / `#[ignore]`)

Against a running local node:

- `torus-wallet balance --keystore <k>` — returns balance
- `torus-wallet place-order --dry-run ...` — prints valid JSON, no submission
- `torus-wallet delegate ...` followed by `torus-wallet staking` — delegation visible

### 7.3 Success criterion

```
cargo test -p torus-wallet
```

Must pass green. Paste output in the implementation PR.

---

## 8. Files Changed

| File | Change |
|---|---|
| `tools/wallet/src/main.rs` | Shrink to ~200 lines (Cli struct, Command enum, dispatch) |
| `tools/wallet/src/rpc.rs` | **Fix hex-encoding in `submit_native_action`** + add `get_open_orders`, `get_epoch`, `get_markets`, `get_proposal` |
| `tools/wallet/src/keystore.rs` | Unchanged |
| `tools/wallet/src/parse.rs` | NEW — `parse_trs_to_wei`, `parse_decimal_to_fixed_point`, `parse_address`, `parse_pubkey` |
| `tools/wallet/src/sign.rs` | NEW — `load_signing_key`, `prompt_passphrase`, `read_passphrase_from_file`, `now_ms`, `submit_native_action`, `build_and_sign_eip1559_tx`, `resolve_address` |
| `tools/wallet/src/commands/mod.rs` | NEW |
| `tools/wallet/src/commands/keys.rs` | NEW — move existing `cmd_keygen`, `cmd_import`, `cmd_address` |
| `tools/wallet/src/commands/query.rs` | NEW — move existing reads + add `cmd_orders`, `cmd_positions`, `cmd_epoch`, `cmd_delegations` |
| `tools/wallet/src/commands/trading.rs` | NEW — `cmd_place_order`, `cmd_cancel_order`, `cmd_cancel_all`, `cmd_modify_order` |
| `tools/wallet/src/commands/transfer.rs` | NEW — move `cmd_send` + add `cmd_transfer_to_perp`, `cmd_transfer_to_spot`, `cmd_withdraw` |
| `tools/wallet/src/commands/staking.rs` | NEW — move `cmd_delegate`, `cmd_undelegate`, `cmd_claim_rewards` + add `cmd_permanent_stake`, `cmd_top_up_self_stake` |
| `tools/wallet/src/commands/governance.rs` | NEW — move `cmd_vote` + add `cmd_submit_proposal` |
| `tools/wallet/src/commands/validator.rs` | NEW — `cmd_register_validator`, `cmd_update_commission`, `cmd_jail_vote`, `cmd_unjail`, `cmd_rotate_key` |
| `tools/wallet/Cargo.toml` | No changes — all deps already present |

No changes to any other crate.
