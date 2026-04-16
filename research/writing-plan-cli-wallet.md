# Writing Plan: CLI Wallet Tool

**Spec:** [tech-req-cli-wallet.md](./tech-req-cli-wallet.md)
**Date:** 2026-04-16 (revised to match actual codebase)
**Starting point:** `tools/wallet/` already exists — ~705 lines of `main.rs`, keystore.rs,
rpc.rs, 16 commands (Keygen, Import, Address, Balance, Validators, Staking, Block, Tx,
Send, Delegate, Undelegate, ClaimRewards, Orderbook, Position, Proposals, Vote).
**Goal:** Bring to full parity with tech-req — 19 new commands, modular structure,
global flags, hex-encoding bug fix, ~500 lines new code + tests.
**Estimated size:** ~800 lines new code + ~300 lines tests + ~700 lines moved.

Every phase ends with `cargo check -p torus-wallet && cargo test -p torus-wallet`
passing green. If either fails, stop and fix before the next phase.

---

## Phase 0: Fix RPC hex-encoding bug (5 minutes)

**File:** `tools/wallet/src/rpc.rs` line 105-108.

The server's `torus_submitNativeAction` does `parse_bytes(&s)` → `hex::decode` →
`serde_json::from_slice`. The client currently sends raw JSON, which fails at the
hex-decode step. Fix:

```rust
pub async fn submit_native_action(&self, signed_action_json: &str) -> Result<String, String> {
    let hex = format!("0x{}", hex::encode(signed_action_json.as_bytes()));
    let r = self.call("torus_submitNativeAction", serde_json::json!([hex])).await?;
    r.as_str().map(|s| s.to_string()).ok_or("result not string".to_string())
}
```

**Add test** (`tools/wallet/src/rpc.rs` test module):

```rust
#[test]
fn test_submit_hex_roundtrip() {
    let json = r#"{"action":"ClaimRewards","nonce":123}"#;
    let hex = format!("0x{}", hex::encode(json.as_bytes()));
    assert!(hex.starts_with("0x"));
    let decoded = hex::decode(&hex[2..]).unwrap();
    assert_eq!(std::str::from_utf8(&decoded).unwrap(), json);
}
```

**Verify:** `cargo test -p torus-wallet`.

---

## Phase 1: Modular scaffold (move existing handlers into modules)

Create the module directory and move existing handlers without changing behavior.
This isolates the refactor from the feature additions — so if anything breaks, it's
obvious.

**New files:**

- `tools/wallet/src/parse.rs` — move `parse_trs_to_wei`, `parse_address`, add empty
  stubs for `parse_decimal_to_fixed_point` and `parse_pubkey` (populated in Phase 2).
- `tools/wallet/src/sign.rs` — move `load_signing_key`, `prompt_passphrase`, `now_ms`,
  `build_and_sign_eip1559_tx`, `submit_native_action`. `submit_native_action` is extended
  in Phase 2 with `--dry-run`; for now just move it verbatim.
- `tools/wallet/src/commands/mod.rs` — `pub mod keys; pub mod query; pub mod trading;
  pub mod transfer; pub mod staking; pub mod governance; pub mod validator;`
- `tools/wallet/src/commands/keys.rs` — move `cmd_keygen`, `cmd_import`, `cmd_address`.
- `tools/wallet/src/commands/query.rs` — move `cmd_balance`, `cmd_validators`,
  `cmd_staking`, `cmd_block`, `cmd_tx`, `cmd_orderbook`, `cmd_position`, `cmd_proposals`.
- `tools/wallet/src/commands/trading.rs` — empty, `// Phase 3` comment.
- `tools/wallet/src/commands/transfer.rs` — move `cmd_send`.
- `tools/wallet/src/commands/staking.rs` — move `cmd_delegate`, `cmd_undelegate`,
  `cmd_claim_rewards`.
- `tools/wallet/src/commands/governance.rs` — move `cmd_vote`.
- `tools/wallet/src/commands/validator.rs` — empty, `// Phase 7` comment.

**Changes to `main.rs`:**

- Replace inline function definitions with `mod parse; mod sign; mod commands;`.
- Keep the `Cli` struct, `Command` enum, and `main()` dispatch.
- Update dispatch to call `commands::keys::cmd_keygen(...)` etc.
- Tests module stays in `main.rs` for now; individual parse/sign tests migrate in later phases.

**Visibility:** mark moved functions `pub(crate)` (or `pub` if tested from outside).
Re-export shared types (`Cli`, `RpcClient`) as needed.

**Verify:** `cargo test -p torus-wallet` — all existing tests still pass. No behavior change.

---

## Phase 2: Global flags + parse helpers

### 2a. `Cli` struct (main.rs)

Add fields:

```rust
#[arg(long, global = true, default_value = "false")]
dry_run: bool,

#[arg(long, global = true)]
passphrase_file: Option<PathBuf>,
```

### 2b. `sign.rs`

Update `load_signing_key` to check `cli.passphrase_file` before prompting:

```rust
pub(crate) fn read_passphrase(cli: &Cli) -> Result<String, String> {
    if let Some(path) = &cli.passphrase_file {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("passphrase file not found: {}: {e}", path.display()))?;
        Ok(contents.trim_end_matches(|c: char| c.is_whitespace()).to_string())
    } else {
        Ok(prompt_passphrase("Enter keystore passphrase: "))
    }
}
```

Use `read_passphrase` in `load_signing_key` and anywhere else that prompts.

Update `submit_native_action` to honor `--dry-run`:

```rust
pub(crate) async fn submit_native_action(
    cli: &Cli,
    rpc: &RpcClient,
    action: NativeAction,
) -> Result<(), String> {
    let key = load_signing_key(cli)?;
    let nonce = now_ms();
    let signed = sign_native_action(action, nonce, &key);
    if cli.dry_run {
        println!("{}", serde_json::to_string_pretty(&signed)
            .map_err(|e| format!("serialize: {e}"))?);
        return Ok(());
    }
    let json = serde_json::to_string(&signed).map_err(|e| format!("serialize: {e}"))?;
    let result = rpc.submit_native_action(&json).await?;
    if cli.json {
        println!("{}", serde_json::json!({"result": result}));
    } else {
        println!("Native action submitted: {result}");
    }
    Ok(())
}
```

### 2c. `parse.rs` — new helpers

```rust
use torus_types::FixedPoint;

pub(crate) fn parse_decimal_to_fixed_point(s: &str) -> Result<FixedPoint, String> {
    // Accept "1.5", "100", "-1.5", "0.00000001". Max 8 decimals. SCALE = 100_000_000.
    // Implementation: split on '.', pad fractional part to 8 digits, combine.
    // Negative handling via leading '-' sign.
}

pub(crate) fn parse_pubkey(s: &str) -> Result<torus_types::PublicKey, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("invalid pubkey: expected 64 hex characters".into());
    }
    let bytes = hex::decode(s).map_err(|e| format!("pubkey hex: {e}"))?;
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(torus_types::PublicKey(arr))
}
```

### 2d. Tests (parse.rs)

```rust
#[test]
fn test_parse_decimal_to_fixed_point() {
    assert_eq!(parse_decimal_to_fixed_point("1.5").unwrap().raw(), 150_000_000);
    assert_eq!(parse_decimal_to_fixed_point("100").unwrap().raw(), 10_000_000_000);
    assert_eq!(parse_decimal_to_fixed_point("0.00000001").unwrap().raw(), 1);
    assert_eq!(parse_decimal_to_fixed_point("0.001").unwrap().raw(), 100_000);
    assert_eq!(parse_decimal_to_fixed_point("-1.5").unwrap().raw(), -150_000_000);
    assert!(parse_decimal_to_fixed_point("1.123456789").is_err());  // 9 decimals
    assert!(parse_decimal_to_fixed_point("").is_err());
    assert!(parse_decimal_to_fixed_point("abc").is_err());
}

#[test]
fn test_parse_pubkey() {
    let hex64 = "0".repeat(63) + "1";
    assert_eq!(parse_pubkey(&hex64).unwrap().0[31], 1);
    assert_eq!(parse_pubkey(&format!("0x{hex64}")).unwrap().0[31], 1);
    assert!(parse_pubkey("0x123").is_err());
    assert!(parse_pubkey(&"z".repeat(64)).is_err());
}

#[test]
fn test_passphrase_file_trim() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "secret\n\n").unwrap();
    // build a Cli with passphrase_file = Some(tmp.path())
    // call read_passphrase, assert == "secret"
}
```

**Verify:** `cargo test -p torus-wallet`.

---

## Phase 3: Trading commands (`commands/trading.rs`)

Add `Command` variants in main.rs:

```rust
PlaceOrder {
    #[arg(long)] market: u64,
    #[arg(long)] side: String,          // "buy" | "sell"
    #[arg(long)] price: String,
    #[arg(long)] quantity: String,
    #[arg(long, default_value = "limit")] order_type: String,
    #[arg(long, default_value = "gtc")] tif: String,
    #[arg(long)] trigger_price: Option<String>,
    #[arg(long, default_value = "false")] reduce_only: bool,
    #[arg(long)] client_id: Option<u64>,
},
CancelOrder {
    #[arg(long)] order_id: u128,
},
CancelAll {
    #[arg(long)] market: u64,
},
ModifyOrder {
    #[arg(long)] order_id: u128,
    #[arg(long)] price: Option<String>,
    #[arg(long)] quantity: Option<String>,
},
```

Handlers in `commands/trading.rs`:

```rust
pub(crate) async fn cmd_place_order(
    cli: &Cli, rpc: &RpcClient,
    market: u64, side: &str, price: &str, quantity: &str,
    order_type: &str, tif: &str, trigger_price: Option<&str>,
    reduce_only: bool, client_id: Option<u64>,
) -> Result<(), String> {
    let is_buy = match side.to_lowercase().as_str() {
        "buy" => true,
        "sell" => false,
        _ => return Err("side must be 'buy' or 'sell'".into()),
    };
    let price_fp = parse_decimal_to_fixed_point(price)?;
    let qty_fp = parse_decimal_to_fixed_point(quantity)?;
    let order_type_enum = match order_type.to_lowercase().as_str() {
        "limit" => OrderType::Limit,
        "market" => OrderType::Market,
        "stop-market" => {
            let t = trigger_price.ok_or("--trigger-price required for stop-market")?;
            OrderType::StopMarket { trigger: parse_decimal_to_fixed_point(t)? }
        }
        "stop-limit" => {
            let t = trigger_price.ok_or("--trigger-price required for stop-limit")?;
            OrderType::StopLimit {
                trigger: parse_decimal_to_fixed_point(t)?,
                limit: price_fp,
            }
        }
        _ => return Err(format!("unknown order-type: {order_type}")),
    };
    let tif_enum = match tif.to_lowercase().as_str() {
        "gtc" => TimeInForce::GTC,
        "ioc" => TimeInForce::IOC,
        "fok" => TimeInForce::FOK,
        "post-only" => TimeInForce::PostOnly,
        _ => return Err(format!("unknown tif: {tif}")),
    };
    let params = PlaceOrderParams {
        market_id: market, is_buy, price: price_fp, quantity: qty_fp,
        order_type: order_type_enum, time_in_force: tif_enum,
        reduce_only, client_order_id: client_id,
    };
    submit_native_action(cli, rpc, NativeAction::PlaceOrder(params)).await
}

// cancel_order, cancel_all, modify_order — similar pattern
// modify_order requires at least one of price/quantity
```

### Tests

```rust
#[test]
fn test_place_order_signing_roundtrip() {
    // Build PlaceOrderParams with stop-limit + client_order_id
    // sign_native_action → recover_sender → assert matches derived address
}
```

**Verify:** `cargo test -p torus-wallet`.

---

## Phase 4: Transfer commands (`commands/transfer.rs`)

Add `Command` variants:

```rust
TransferToPerp { #[arg(long)] amount: String },
TransferToSpot { #[arg(long)] amount: String },
Withdraw { #[arg(long)] to: String, #[arg(long)] amount: String },
```

Handlers: `parse_trs_to_wei(amount)` → `NativeAction::{TransferToPerp,TransferToSpot,Withdraw}`.
`Withdraw` also calls `parse_address(to)`.

### Tests

Sign each variant, verify sender recovery. Add `test_transfer_variants_sign_correctly`.

**Verify:** `cargo test -p torus-wallet`.

---

## Phase 5: Staking additions (`commands/staking.rs`)

Add `Command` variants:

```rust
PermanentStake { #[arg(long)] amount: String },
TopUpSelfStake { #[arg(long)] amount: String },
```

Handlers follow existing `cmd_delegate` pattern.

### Tests

Sign + recover for both new variants.

**Verify:** `cargo test -p torus-wallet`.

---

## Phase 6: Governance additions (`commands/governance.rs`)

Add `Command` variant:

```rust
SubmitProposal {
    #[arg(long)] title: String,
    #[arg(long)] description: String,
    #[arg(long)] proposal_type: String,
    #[arg(long)] params: String,  // JSON string
},
```

Handler maps `--proposal-type` + JSON → `ProposalAction`:

```rust
pub(crate) async fn cmd_submit_proposal(
    cli: &Cli, rpc: &RpcClient,
    title: &str, description: &str, proposal_type: &str, params_json: &str,
) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(params_json)
        .map_err(|e| format!("invalid --params JSON: {e}"))?;
    let action = match proposal_type {
        "param-change" => {
            let key = v.get("key").and_then(|x| x.as_str())
                .ok_or("params.key required")?.to_string();
            let value = v.get("value").and_then(|x| x.as_str())
                .ok_or("params.value required")?.to_string();
            ProposalAction::ParameterChange { key, value }
        }
        "list-market" => {
            ProposalAction::ListMarket(MarketListing {
                base_asset: v.get("base_asset").and_then(|x| x.as_str())
                    .ok_or("params.base_asset required")?.to_string(),
                quote_asset: v.get("quote_asset").and_then(|x| x.as_str())
                    .ok_or("params.quote_asset required")?.to_string(),
                tick_size: parse_decimal_to_fixed_point(
                    v.get("tick_size").and_then(|x| x.as_str())
                        .ok_or("params.tick_size required")?)?,
                lot_size: parse_decimal_to_fixed_point(
                    v.get("lot_size").and_then(|x| x.as_str())
                        .ok_or("params.lot_size required")?)?,
                max_leverage: v.get("max_leverage").and_then(|x| x.as_u64())
                    .ok_or("params.max_leverage required")? as u32,
                maintenance_margin_bps: v.get("maintenance_margin_bps").and_then(|x| x.as_u64())
                    .ok_or("params.maintenance_margin_bps required")? as u32,
            })
        }
        "delist-market" => {
            let market_id = v.get("market_id").and_then(|x| x.as_u64())
                .ok_or("params.market_id required")?;
            ProposalAction::DelistMarket { market_id }
        }
        "update-market-params" => {
            let market_id = v.get("market_id").and_then(|x| x.as_u64())
                .ok_or("params.market_id required")?;
            ProposalAction::UpdateMarketParams {
                market_id,
                params: MarketParams {
                    tick_size: parse_decimal_to_fixed_point(
                        v.get("tick_size").and_then(|x| x.as_str())
                            .ok_or("params.tick_size required")?)?,
                    lot_size: parse_decimal_to_fixed_point(
                        v.get("lot_size").and_then(|x| x.as_str())
                            .ok_or("params.lot_size required")?)?,
                    max_leverage: v.get("max_leverage").and_then(|x| x.as_u64())
                        .ok_or("params.max_leverage required")? as u32,
                    maintenance_margin_bps: v.get("maintenance_margin_bps").and_then(|x| x.as_u64())
                        .ok_or("params.maintenance_margin_bps required")? as u32,
                    max_funding_rate_bps: v.get("max_funding_rate_bps").and_then(|x| x.as_u64())
                        .ok_or("params.max_funding_rate_bps required")? as u32,
                },
            }
        }
        "validator-registration" => {
            let addr = v.get("candidate").and_then(|x| x.as_str())
                .ok_or("params.candidate required")?;
            ProposalAction::ValidatorRegistration { candidate: parse_address(addr)? }
        }
        _ => return Err(format!("unknown --proposal-type: {proposal_type}")),
    };
    let proposal = Proposal {
        title: title.to_string(),
        description: description.to_string(),
        action,
    };
    submit_native_action(cli, rpc, NativeAction::SubmitProposal(proposal)).await
}
```

### Tests

One test per `ProposalAction` variant — build, sign, recover, compare.

**Verify:** `cargo test -p torus-wallet`.

---

## Phase 7: Validator commands (`commands/validator.rs`)

Add `Command` variants:

```rust
RegisterValidator {
    #[arg(long)] pubkey: String,
    #[arg(long)] commission_bps: u16,
},
UpdateCommission { #[arg(long)] commission_bps: u16 },
JailVote { #[arg(long)] validator: String },
Unjail,
RotateKey { #[arg(long)] new_pubkey: String },
```

Handlers use `parse_pubkey()` for pubkey parsing, `parse_address()` for the
JailVote target.

### Tests

Sign + recover each of the 5 variants.

**Verify:** `cargo test -p torus-wallet`.

---

## Phase 8: Query additions + address fallback (`commands/query.rs`, `rpc.rs`)

### 8a. New RPC methods (`rpc.rs`)

```rust
pub async fn get_open_orders(
    &self, trader: &str, market_id: Option<&str>,
) -> Result<serde_json::Value, String> {
    self.call("torus_getOpenOrders", serde_json::json!([trader, market_id])).await
}

pub async fn get_epoch(&self) -> Result<serde_json::Value, String> {
    self.call("torus_getEpoch", serde_json::json!([])).await
}

pub async fn get_markets(
    &self, offset: Option<u32>, limit: Option<u32>,
) -> Result<serde_json::Value, String> {
    self.call("torus_getMarkets", serde_json::json!([offset, limit])).await
}

pub async fn get_proposal(&self, id: u64) -> Result<serde_json::Value, String> {
    self.call("torus_getProposal", serde_json::json!([id])).await
}
```

### 8b. `resolve_address` helper (`sign.rs`)

```rust
pub(crate) fn resolve_address(cli: &Cli, explicit: &Option<String>) -> Result<String, String> {
    if let Some(addr) = explicit {
        return Ok(addr.clone());
    }
    if cli.keystore.is_some() || cli.key.is_some() {
        let key = load_signing_key(cli)?;
        let addr = address_from_key(&key);
        return Ok(format!("0x{}", hex::encode(addr)));
    }
    Err("address required: provide --address or --keystore".into())
}
```

### 8c. New/updated `Command` variants

```rust
Balance { #[arg(long)] address: Option<String> },   // was String, now Option
Staking { #[arg(long)] address: Option<String> },   // was String, now Option
Orders {
    #[arg(long)] address: Option<String>,
    #[arg(long)] market: Option<u64>,
},
Positions { #[arg(long)] address: Option<String> },
Delegations { #[arg(long)] address: Option<String> },
Epoch,
```

### 8d. Handlers

- `cmd_balance` — call `resolve_address` when `address` is None
- `cmd_staking` — same
- `cmd_orders(cli, rpc, address_opt, market_opt)` — resolve address, call `get_open_orders`
- `cmd_positions(cli, rpc, address_opt)` — resolve, iterate `get_markets`, call `get_position` per market
- `cmd_delegations(cli, rpc, address_opt)` — resolve, call `get_delegations`
- `cmd_epoch(rpc)` — call `get_epoch`, format: `Epoch <n>  blocks <start>-<end>  <remaining> remaining`

### 8e. Update `cmd_proposals` to accept optional id

```rust
Proposals { #[arg(long)] id: Option<u64> },
```

If `id` Some → `get_proposal(id)`, else `get_proposals()`.

### Tests

`test_resolve_address_from_keystore` — build minimal Cli with keystore, assert
`resolve_address(cli, &None)` returns the expected hex address.

**Verify:** `cargo test -p torus-wallet`.

---

## Phase 9: Final verification

### 9a. Full test run

```
cargo test -p torus-wallet 2>&1 | tee /tmp/wallet-test.log
```

All tests green. Paste the summary into the PR.

### 9b. Clippy

```
cargo clippy -p torus-wallet -- -D warnings
```

No warnings.

### 9c. Smoke test (manual, if a local node is running)

```
# Dry-run trading
target/debug/torus-wallet place-order \
  --keystore /tmp/test.keystore \
  --passphrase-file /tmp/pass.txt \
  --market 0 --side buy --price 1.5 --quantity 0.1 \
  --dry-run

# Should print SignedNativeAction JSON, not submit.

# Real query
target/debug/torus-wallet epoch --rpc-url http://localhost:8545
```

### 9d. Line count sanity

```
find tools/wallet/src -name '*.rs' | xargs wc -l
```

`main.rs` should be roughly 200-300 lines. No single module > 500 lines.

---

## Dependency Chain

```
Phase 0 (RPC fix) ────────────────────────────────────────┐
                                                            │
Phase 1 (scaffold) ──► Phase 2 (flags + helpers)           │
                            │                               │
                            ├── Phase 3 (trading)           │
                            │                               │
                            ├── Phase 4 (transfer)          │
                            │                               │
                            ├── Phase 5 (staking +)         │
                            │                               │
                            ├── Phase 6 (governance)        │
                            │                               │
                            ├── Phase 7 (validator)         │
                            │                               │
                            └── Phase 8 (query + fallback) ─┤
                                                            │
                                       Phase 9 (verify) ────┘
```

Phase 0 is independent and can run first or last; easiest first.
Phase 1 must complete before any feature phase.
Phase 2 gates every write command (needs `--dry-run` and `read_passphrase`).
Phases 3–8 are independent of each other after Phase 2; can be done sequentially or in any order.
Phase 9 is last.

## Quality gates (per phase)

- `cargo check -p torus-wallet` passes.
- `cargo test -p torus-wallet` all green.
- No `unwrap()` in non-test code (use `?` + `Result<(), String>`).
- No `println!` for errors — errors return `Err(String)` to `main()`.
- Each new handler has at least one unit test covering the signing roundtrip.
