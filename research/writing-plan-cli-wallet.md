# Writing Plan: CLI Wallet Tool

**Spec:** [tech-req-cli-wallet.md](./tech-req-cli-wallet.md)
**Date:** 2026-04-16
**Estimated scope:** ~600 lines new code + ~150 lines tests + ~50 lines refactor

---

## Step 1: Extract keystore module (torus-types)

**File:** `crates/torus-types/src/keystore.rs` (new)

Extract the keystore logic currently in `torus-node/src/main.rs` (~lines 193-250).
Create a reusable module with:

```rust
pub fn generate_keystore(path: &Path, passphrase: &str) -> Result<Address>
pub fn load_signing_key(path: &Path, passphrase: &str) -> Result<SigningKey>
pub fn address_from_keystore(path: &Path, passphrase: &str) -> Result<Address>
```

Add `pub mod keystore;` to `crates/torus-types/src/lib.rs`.

**Also update:** `crates/torus-types/Cargo.toml` — add `eth-keystore` dependency
(already in `torus-node/Cargo.toml`, move to shared crate).

**Verify:** `cargo check -p torus-types`

---

## Step 2: Update torus-node to use shared keystore

**File:** `crates/torus-node/src/main.rs`

Replace inline keystore code with imports from `torus_types::keystore::*`.
Remove duplicated `generate_keystore` / `load_key` functions. The keygen
subcommand and `--keystore` loading both call the shared module.

**Verify:** `cargo test -p torus-node`

---

## Step 3: Create torus-wallet crate scaffold

**Files:**
- `crates/torus-wallet/Cargo.toml`
- `crates/torus-wallet/src/main.rs`
- `crates/torus-wallet/src/rpc_client.rs`
- `crates/torus-wallet/src/commands/mod.rs`

**Cargo.toml dependencies:**
```toml
[dependencies]
torus-types = { path = "../torus-types" }
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { version = "0.12", features = ["json"] }
tokio = { version = "1", features = ["full"] }
rpassword = "7"    # passphrase prompt
```

**main.rs:** Define the top-level `Cli` struct with clap derive. Global
flags: `--rpc`, `--keystore`, `--passphrase-file`, `--pretty`, `--dry-run`.
Subcommand enum dispatches to command modules.

**rpc_client.rs:** Thin wrapper around reqwest for JSON-RPC 2.0 calls:
```rust
pub struct RpcClient { url: String, client: reqwest::Client }
impl RpcClient {
    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T>
}
```

~80 lines total.

Add `"torus-wallet"` to workspace `members` in root `Cargo.toml`.

**Verify:** `cargo check -p torus-wallet`

---

## Step 4: Key management commands

**File:** `crates/torus-wallet/src/commands/keys.rs`

Three commands:

- `keygen --output <path>` — calls `torus_types::keystore::generate_keystore`
- `import-key --hex <key> --output <path>` — wraps raw key into encrypted keystore
- `show-address --keystore <path>` — decrypts and prints address

Each prompts for passphrase via `rpassword::prompt_password()` unless
`--passphrase-file` is set. ~60 lines.

**Verify:** `cargo check -p torus-wallet`

---

## Step 5: Query commands

**File:** `crates/torus-wallet/src/commands/query.rs`

Implement read-only commands. Each is a small function that calls
`rpc_client.call()` and formats the output:

```rust
pub async fn balance(rpc: &RpcClient, address: Address, pretty: bool) -> Result<()>
pub async fn positions(rpc: &RpcClient, address: Address, pretty: bool) -> Result<()>
pub async fn orders(rpc: &RpcClient, market_id: Option<u64>, pretty: bool) -> Result<()>
pub async fn staking_info(rpc: &RpcClient, address: Address, pretty: bool) -> Result<()>
pub async fn delegations(rpc: &RpcClient, address: Address, pretty: bool) -> Result<()>
pub async fn validators(rpc: &RpcClient, pretty: bool) -> Result<()>
pub async fn epoch(rpc: &RpcClient, pretty: bool) -> Result<()>
pub async fn proposals(rpc: &RpcClient, id: Option<u64>, pretty: bool) -> Result<()>
```

`balance` calls both `torus_getBalances` (native) and `eth_getBalance` (EVM)
and merges results. ~120 lines.

**Verify:** `cargo check -p torus-wallet`

---

## Step 6: Signing helper

**File:** `crates/torus-wallet/src/sign.rs`

A single helper that all write commands share:

```rust
pub async fn sign_and_submit(
    rpc: &RpcClient,
    keystore_path: &Path,
    passphrase: &str,
    action: NativeAction,
    dry_run: bool,
) -> Result<()> {
    let key = load_signing_key(keystore_path, passphrase)?;
    let chain_id = rpc.call::<String>("eth_chainId", json!([])).await?;
    let chain_id = u64::from_str_radix(&chain_id[2..], 16)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let signed = SignedNativeAction::sign(action, nonce, chain_id, &key)?;

    if dry_run {
        println!("{}", serde_json::to_string_pretty(&signed)?);
        return Ok(());
    }

    let result = rpc.call::<Value>("torus_submitNativeAction", json!([signed])).await?;
    println!("{}", result);
    Ok(())
}
```

~40 lines. Uses existing `SignedNativeAction::sign()` from `torus-types`.

**Verify:** `cargo check -p torus-wallet`

---

## Step 7: Trading commands

**File:** `crates/torus-wallet/src/commands/trading.rs`

Four commands, each parses CLI args into a `NativeAction` variant and
calls `sign_and_submit()`:

- `place_order` → `NativeAction::PlaceOrder { market_id, side, price, quantity, order_type, tif, ... }`
- `cancel_order` → `NativeAction::CancelOrder { market_id, order_id }`
- `cancel_all` → `NativeAction::CancelAllOrders { market_id }`
- `modify_order` → `NativeAction::ModifyOrder { market_id, order_id, new_price, new_quantity }`

The decimal-to-FixedPoint parsing: `"1.5"` → multiply by 10^18 → store as u128.
Use `str::split('.')` approach, no floating point. ~100 lines.

**Verify:** `cargo check -p torus-wallet`

---

## Step 8: Staking commands

**File:** `crates/torus-wallet/src/commands/staking.rs`

Five commands:

- `delegate` → `NativeAction::Delegate { validator, amount }`
- `undelegate` → `NativeAction::Undelegate { validator, amount }`
- `permanent_stake` → `NativeAction::PermanentStake { amount }`
- `claim_rewards` → `NativeAction::ClaimRewards {}`
- `top_up_self_stake` → `NativeAction::TopUpSelfStake { amount }`

~60 lines. Same pattern as trading.

**Verify:** `cargo check -p torus-wallet`

---

## Step 9: Governance + validator commands

**File:** `crates/torus-wallet/src/commands/governance.rs`

- `submit_proposal` → `NativeAction::SubmitProposal { ... }`
- `vote` → `NativeAction::Vote { proposal_id, vote }`
- `register_validator` → `NativeAction::RegisterValidator { pubkey, commission_bps }`
- `update_commission` → `NativeAction::UpdateCommission { commission_bps }`
- `jail_vote` → `NativeAction::JailVote { validator }`
- `unjail` → `NativeAction::UnjailSelf {}`
- `rotate_key` → `NativeAction::RotateValidatorKey { new_pubkey }`

~80 lines.

**Verify:** `cargo check -p torus-wallet`

---

## Step 10: Tests

**File:** `crates/torus-wallet/src/main.rs` (mod tests) +
`crates/torus-wallet/tests/integration.rs`

**Unit tests:**
- `parse_decimal_to_fixed_point` — "1.5", "0.001", "999999", "0"
- `cli_parse_place_order` — verify clap parses all flags correctly
- `cli_parse_global_flags` — --rpc, --keystore, --pretty, --dry-run
- `sign_roundtrip` — sign a NativeAction, deserialize, verify fields

**Integration tests** (require running node — mark `#[ignore]`):
- `wallet_balance_query`
- `wallet_delegate_and_query`

**Verify:** `cargo test -p torus-wallet`

---

## Dependency Chain

```
Step 1 (extract keystore) ── Step 2 (update torus-node) ──┐
                                                           │
Step 3 (scaffold) ─── Step 4 (keys) ──┐                   │
                  │                    │                    │
                  ├── Step 5 (query) ──┤                   │
                  │                    │                    │
                  └── Step 6 (sign) ───┼── Step 7 (trading)│
                                       │                   │
                                       ├── Step 8 (staking)│
                                       │                   │
                                       └── Step 9 (gov)    │
                                                           │
                                          Step 10 (tests) ─┘
```

Steps 1+3 can start in parallel.
Steps 4, 5, 6 can start after 3 (and 4 needs 1).
Steps 7, 8, 9 need 6.
Step 10 is last.
