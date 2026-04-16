# Technical Requirements: CLI Wallet Tool (Section 3.5.4)

**Date:** 2026-04-16
**Status:** Draft v1.0
**Parent:** [implementation-plan.md](./implementation-plan.md) Task 3.5.4
**Depends on:** torus-rpc (1.8), native action format (1.3b), keystore (torus-node keygen)

---

## Summary

A standalone CLI binary (`torus-wallet`) for interacting with a Torus node.
Targets node operators and developers — not end users. Covers: key management,
native action signing/submission, balance/position queries, and staking operations.

**Key decisions:**
- Separate binary crate (`crates/torus-wallet`), not a subcommand of `torus-node`
- Reuses existing EIP-712 signing from `torus-types` and keystore from `torus-node`
- Talks to node via JSON-RPC (`torus_*` and `eth_*` endpoints)
- No EVM transaction construction — users have `cast`/Foundry for that
- Output: JSON by default, human-readable with `--pretty`

---

## 1. Scope

### 1.1 In Scope

| Category | Commands |
|---|---|
| **Key management** | `keygen`, `import-key`, `show-address` |
| **Query** | `balance`, `positions`, `orders`, `staking-info`, `delegations`, `epoch`, `validators`, `proposals` |
| **Trading** | `place-order`, `cancel-order`, `cancel-all`, `modify-order` |
| **Transfers** | `transfer-to-perp`, `transfer-to-spot`, `withdraw` |
| **Staking** | `delegate`, `undelegate`, `permanent-stake`, `claim-rewards`, `top-up-self-stake` |
| **Governance** | `submit-proposal`, `vote` |
| **Validator ops** | `register-validator`, `update-commission`, `jail-vote`, `unjail`, `rotate-key` |

### 1.2 Out of Scope

- EVM transaction construction (use `cast send` / Foundry)
- Batch/scripted operations (users pipe JSON)
- Hardware wallet integration (future)
- Interactive TUI mode

---

## 2. Architecture

### 2.1 Binary Structure

```
torus-wallet <subcommand> [args] --rpc <url> --keystore <path>
```

Uses `clap` with derive API (same pattern as `torus-node`). Global flags:

| Flag | Default | Description |
|---|---|---|
| `--rpc` | `http://localhost:8545` | Node RPC endpoint |
| `--keystore` | None | Path to encrypted keystore file |
| `--passphrase-file` | None | File containing keystore passphrase (avoids prompt) |
| `--pretty` | false | Human-readable output instead of raw JSON |
| `--dry-run` | false | Build and sign the action, print it, but don't submit |

### 2.2 Signing Flow

All write commands follow the same flow:

```
1. Parse CLI args → NativeAction variant
2. Load keystore → decrypt private key
3. Build SignedNativeAction:
   - nonce = current timestamp (nanoseconds)
   - EIP-712 typed data hash (domain: chain_id from node)
   - secp256k1 signature
4. Submit via torus_submitNativeAction RPC
5. Print result (tx hash or error)
```

The EIP-712 signing logic already exists in `torus-types/src/eip712.rs`.
The keystore encrypt/decrypt already exists in `torus-node/src/main.rs` (keygen).

### 2.3 Query Flow

All read commands:

```
1. Parse CLI args
2. Call appropriate RPC method (torus_getBalances, eth_getBalance, etc.)
3. Deserialize response
4. Print JSON (or formatted with --pretty)
```

No keystore needed for queries.

---

## 3. Command Specifications

### 3.1 Key Management

**`keygen`** — Generate a new encrypted keystore file.
Reuse the existing keygen logic from `torus-node`. Prompts for passphrase
(or reads from `--passphrase-file`). Writes to `--output <path>`.

**`import-key --hex <private_key_hex>`** — Import a raw hex private key
into an encrypted keystore. For migration from `--validator-key` (deprecated).

**`show-address`** — Decrypt keystore, derive and print the Ethereum address.
Does not contact the node.

### 3.2 Trading Commands

**`place-order`**:
```
torus-wallet place-order \
  --market <id> \
  --side <buy|sell> \
  --price <decimal> \
  --quantity <decimal> \
  --order-type <limit|market|stop-market|stop-limit> \
  --tif <gtc|ioc|fok|post-only> \
  --trigger-price <decimal>       # for stop orders
  --reduce-only                   # optional flag
  --client-id <string>            # optional
```

All decimal values are parsed to `FixedPoint` (18 decimal places).

**`cancel-order --market <id> --order-id <id>`**

**`cancel-all --market <id>`**

**`modify-order --market <id> --order-id <id> --price <decimal> --quantity <decimal>`**

### 3.3 Transfer Commands

**`transfer-to-perp --amount <decimal>`** — Move from spot to perp margin.

**`transfer-to-spot --amount <decimal>`** — Move from perp to spot balance.

**`withdraw --to <address> --amount <decimal>`** — Withdraw to an address.

### 3.4 Staking Commands

**`delegate --validator <address> --amount <decimal>`**

**`undelegate --validator <address> --amount <decimal>`**

**`permanent-stake --amount <decimal>`**

**`claim-rewards`** — Claims all pending rewards.

**`top-up-self-stake --amount <decimal>`** — Validator adds to own stake.

### 3.5 Governance Commands

**`submit-proposal --type <param-change|treasury-spend|market-listing> --title <str> --description <str> --params <json>`**

**`vote --proposal-id <id> --vote <yes|no|abstain>`**

### 3.6 Validator Operations

**`register-validator --pubkey <hex> --commission-bps <u16>`**

**`update-commission --commission-bps <u16>`**

**`jail-vote --validator <address>`**

**`unjail`**

**`rotate-key --new-pubkey <hex>`**

### 3.7 Query Commands

**`balance [--address <addr>]`** — Native + EVM balances. Defaults to keystore address.

**`positions [--address <addr>]`** — Open positions across all markets.

**`orders [--market <id>]`** — Open orders. Optional market filter.

**`staking-info [--address <addr>]`** — Delegation amounts, pending rewards, permanent stake.

**`delegations [--address <addr>]`** — All delegations for an address.

**`validators`** — List all validators with status, stake, commission.

**`epoch`** — Current epoch number, height, time remaining.

**`proposals [--id <id>]`** — List proposals or get one by ID.

---

## 4. RPC Mapping

| Command | RPC Method |
|---|---|
| `balance` | `torus_getBalances` + `eth_getBalance` |
| `positions` | `torus_getPosition` |
| `orders` | `torus_getOrderBook` (filtered) |
| `staking-info` | `torus_getStakingInfo` |
| `delegations` | `torus_getDelegations` |
| `validators` | `torus_getValidators` |
| `epoch` | `torus_getEpoch` |
| `proposals` | `torus_getProposals` / `torus_getProposal` |
| All write commands | `torus_submitNativeAction` |

---

## 5. Shared Code Extraction

### 5.1 Keystore Module

The keystore logic currently lives in `torus-node/src/main.rs` (lines ~193-250).
Extract to a shared module:

```
crates/torus-types/src/keystore.rs  (or a new torus-crypto crate)
  pub fn generate_keystore(path, passphrase) -> Result<Address>
  pub fn load_keystore(path, passphrase) -> Result<SigningKey>
  pub fn address_from_keystore(path, passphrase) -> Result<Address>
```

Both `torus-node` and `torus-wallet` import from the shared location.

### 5.2 NativeAction Construction

`torus-types/src/eip712.rs` already has `SignedNativeAction::sign()`.
`torus-wallet` calls this directly — no new signing code needed.

---

## 6. Error Handling

| Error | Behavior |
|---|---|
| Node unreachable | Print RPC URL + connection error, exit 1 |
| Invalid keystore passphrase | "Failed to decrypt keystore", exit 1 |
| Action rejected by node | Print error message from RPC response, exit 1 |
| Invalid CLI args | clap handles this automatically |
| `--dry-run` | Print signed action JSON to stdout, exit 0 (no submission) |

---

## 7. Testing

### 7.1 Unit Tests

| Test | Verifies |
|---|---|
| `parse_place_order_args` | All order types, tif variants, optional flags |
| `parse_decimal_to_fixed_point` | "1.5" -> FixedPoint, "0.001" precision, "999999" large |
| `sign_and_verify_roundtrip` | Sign a NativeAction, verify signature recovers correct address |
| `dry_run_output_format` | `--dry-run` produces valid JSON with all fields |

### 7.2 Integration Tests

| Test | Verifies |
|---|---|
| `wallet_balance_query` | Connects to test node, returns balance |
| `wallet_delegate_and_query` | delegate → query staking-info → verify |
| `wallet_place_and_cancel_order` | place-order → cancel-order → verify |

---

## 8. Files

| File | Change | Scope |
|---|---|---|
| `crates/torus-wallet/Cargo.toml` | New crate | Small |
| `crates/torus-wallet/src/main.rs` | CLI entry + clap derive | Medium |
| `crates/torus-wallet/src/commands/mod.rs` | Command dispatch | Small |
| `crates/torus-wallet/src/commands/trading.rs` | Order commands | Medium |
| `crates/torus-wallet/src/commands/staking.rs` | Staking commands | Small |
| `crates/torus-wallet/src/commands/governance.rs` | Governance commands | Small |
| `crates/torus-wallet/src/commands/query.rs` | Read-only queries | Medium |
| `crates/torus-wallet/src/commands/keys.rs` | Keygen, import, show | Small |
| `crates/torus-wallet/src/rpc_client.rs` | JSON-RPC client wrapper | Small |
| `crates/torus-types/src/keystore.rs` | Extract from torus-node | Small |
| `crates/torus-node/src/main.rs` | Import keystore from shared | Small (refactor) |
