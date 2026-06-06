# Prove → Fix: Duplicate Native Inclusion (correctness bug)

> **How to use:** paste this whole file as the prompt into a fresh Claude Code session in the
> `Torus-hyperBFT` repo (branch `cap100-3val-perf`). It is self-contained: it carries the
> verified prior investigation (file:line anchors, RPCs, devnet ops, gotchas, memory IDs) but
> instructs the session to **re-verify live/ephemeral state** and follow THE LOOP with TDD.
>
> Authored 2026-06-06 after confirming the bug (2.93× duplicate inclusion) and the correctness
> gap (no live-path replay dedup). Memory: `b206f59cb555d6d7` (verdict), `282f9818eb3fdbb1`
> (dup confirmed), `ec399809a2440e86` (RPC quirks).

---

## GOAL

In the Torus-hyperBFT repo (branch `cap100-3val-perf`), do these two things **in order**,
following THE LOOP (memory → brainstorm → writing-plans → failing tests → implement → prove →
review → commit). Quality over speed: verify before asserting, never claim "done" without
test/observation evidence, ask before anything destructive/irreversible.

- **TASK 2 (do first):** behaviorally PROVE that a native action included in N consecutive
  blocks is EXECUTED N times (a correctness bug), by getting a single successful
  state-changing action and observing ~3× effect.
- **TASK 1 (do second):** FIX it with a regression test that fails before and passes after.

## MANDATORY STARTUP

1. Read `./CLAUDE.md`, `./LIVE_STATE.json`, and `.claude/GRAPH_REPORT.md` if present.
2. Query memory and READ these (they hold the full prior investigation — trust the CODE
   locations as starting points but re-confirm by reading; treat any RUNTIME claim as a hint):
   - `get_memory id=b206f59cb555d6d7` — correctness verdict
   - `get_memory id=282f9818eb3fdbb1` — dup inclusion 2.93× confirmed
   - `get_memory id=ec399809a2440e86` — `torus_getBlockBody` RPC quirks
   - `get_memory id=e1c8efd0bfe4b4cd` — perf roadmap
   - `search_knowledge "duplicate native inclusion pipeline non-destructive selection correctness replay"`
3. **RE-VERIFY LIVE STATE before acting** (it is ephemeral — do not trust this doc for it):
   devnet up? `docker ps --filter name=devnet`; on latest code? compare
   `target/release/torus-node` mtime + `git log -1` + `find crates -name '*.rs' -newer target/release/torus-node`;
   committing? curl `eth_blockNumber` on `localhost:8645`.

## VERIFIED BACKGROUND (confirm as you go)

**THE BUG:** native actions are committed ~2.93× — almost every action lands in exactly 3
consecutive blocks. Root cause: mempool **non-destructive selection** × HotStuff **3-chain
pipeline** — the proposer builds blocks N, N+1, N+2 before N commits + calls
`remove_committed_native`, so the same actions are re-selected ~3× (= pipeline depth).

**THE CORRECTNESS GAP (code-proven, re-read to confirm):** the LIVE path has **no
execution-time replay dedup**.
- `crates/torus-consensus/src/app.rs`: `validate_block` (~L894) checks signatures/attestation
  ONLY; `on_committed_block` native execution (~L266-351) dispatches every action
  unconditionally and WRITES consumed nonces to `CF_NATIVE_NONCES` only AFTER (~L337-346);
  `validate_block_for_sync` (~L1066) is the SYNC path.
- The `CF_NATIVE_NONCES` replay CHECK exists ONLY in `crates/torus-bridge/src/validator.rs`
  (~L343-356, used by sync/catchup) and `crates/torus-bridge/src/proposer.rs` (~L194,
  selection). Neither guards normal live execution.
- `crates/torus-bridge/src/native_executor.rs`: `exec_dispatch` (~L229) routes each action;
  `exec_place_order` (~L569) is NON-idempotent on success (reserves margin, consumes a global
  order_id ~L621-623, rests/fills) but EARLY-RETURNS a no-op on reject ("insufficient margin"
  ~L595).
- **WHY no harm was seen on the devnet:** `getMarkets` returns `[]` (no markets) and accounts
  have `availableBalance=0x0` (genesis funds are EVM-side only, 0 native margin) → every
  PlaceOrder rejects at the margin check BEFORE mutating state → 3× no-op. This is the ONLY
  reason it's benign; it is NOT replay protection.

**KEY FILES:** `crates/torus-consensus/src/app.rs`,
`crates/torus-bridge/src/{validator.rs,proposer.rs,native_executor.rs}`,
`crates/torus-mempool/src/{lib.rs,rate_limit.rs}` (`NATIVE_TOTAL_BLOCK_CAP=100`),
`crates/torus-state/src/cf.rs` (`CF_NATIVE_NONCES` L54),
`crates/torus-rpc/src/torus.rs` (methods: `getMarkets`, `getOpenOrders`, `getOrderBook`,
`getBalances`, `getPosition`, `getBlockBody`, `getBlockTrades`, `submitNativeAction`),
`crates/torus-core/src/order_book.rs`.

## DEVNET OPS & GOTCHAS

- Start: `./devnet/start.sh` | Stop **+WIPE** state: `./devnet/start.sh down` (runs `down -v`)
  | Logs: `./devnet/start.sh logs`. Fresh chain from genesis each up.
- **PORTS ARE REMAPPED** (uncommitted docker-compose): validator-0 RPC=`localhost:8645`,
  P2P=`30433/udp`, metrics=`9091`. Port **8545 is a SEPARATE live testnet — NEVER touch it.**
  Other validators: RPC 8546/8547/8548, rpc-node 8549. chainId=7778 (0x1e62).
- Image = fast COPY of `target/release/torus-node`. After ANY rust change:
  `cargo build --release -p torus-node` THEN `./devnet/start.sh --build`.
- Flood tool (already fixed): `devnet/scripts/native-order-flood.py`
  `python3 native-order-flood.py http://localhost:8645 <senders> <ops/s> <duration_s>`.
  Needs key cache `/tmp/hardhat_accounts.txt`; if missing, rebuild via `/tmp/build_keys.py`
  (derives hardhat keys with `cast`). Orders are IOC PlaceOrder on market 1.
- RPC: `torus_getBlockBody` takes a **u64** height (NOT hex) → camelCase
  `{nativeActions[], nativeActionCount}`. To inspect an action's blocks, key by canonical JSON
  or nonce.
- **Measurement gotchas:** ANSI-strip docker logs with `sed -r 's/\x1b\[[0-9;]*m//g'` before
  grepping structured fields (`height=N`). `on_commit_block` DOUBLE-LOGS (~1.33×/height) — get
  block RATE from `eth_blockNumber` or UNIQUE heights, not line counts. Use
  `docker logs --tail N` / `--since 30s` on long containers (a full dump makes sequential reads
  non-simultaneous and fakes height spreads). Don't `pkill -f native-order-flood.py` (matches
  your own shell).

## TASK 2 — PROVE THE HARM

Objective: one submitted state-changing action → ~3× observed effect, on a fresh devnet.

1. **DISCOVER how to get a successful action.** Investigate (read, don't guess):
   - How markets are created (`devnet/genesis.json` market/config section? a governance
     `SubmitProposal`? an admin/registration native action?). `getMarkets=[]` today.
   - How native margin gets funded (EVM→native bridge/deposit? `TransferToPerp`/
     `TransferToSpot`? genesis native balances?). Accounts show `evmBalance>0` but
     `nativeBalance`/`availableBalance=0`.
   Prefer the SIMPLEST reproducible setup — likely editing `devnet/genesis.json` to define a
   market AND seed a few accounts with native margin, then a fresh devnet. Confirm via
   `torus_getMarkets` and `torus_getBalances`.
2. **DESIGN the cleanest decisive observable.** Strongest options:
   - A **GTC (resting) limit order**: submit ONCE, then `torus_getOpenOrders(sender)` →
     1 resting order = safe, ~3 = triple-execution bug. (sign with order_type=Limit(0),
     time_in_force=GTC(0); the flood's `sign_place_order` supports these args.)
   - A **value action** (e.g. a transfer/withdraw): submit ONCE, read balance before/after →
     1× vs ~3× delta. (Even more compelling for fund-safety; requires signing that action type
     — check `crates/torus-types/src/eip712.rs` for its typehash/struct.)
   Use at least one; ideally both.
3. **CAPTURE EVIDENCE:** the submitted action's nonce, the blocks it landed in
   (`torus_getBlockBody` across the range — expect ~3 consecutive), AND the multiplied effect
   (open-orders count / balance delta). Save a written record to memory.

**ACCEPTANCE:** a reproducible demonstration that 1 submission → ~3 executions with real state
effect (or a documented, evidence-backed reason it cannot, which would change the verdict).

## TASK 1 — FIX IT

1. Write a **FAILING test first** (TDD) that reproduces the bug deterministically. Look for the
   existing harness (`crates/torus-integration-tests`, plus unit tests in torus-consensus/
   torus-bridge/torus-mempool; the repo has a 4-node consensus integration test). The test
   should assert a duplicated action is executed/effected exactly once.
2. brainstorm + writing-plans on the two fix directions; decide with the architecture in mind
   (**recommend doing BOTH**):
   - **DEFENSE-IN-DEPTH (safety):** live-path execution replay guard — in app.rs native
     execution (~L301-310, where `consumed_nonces` is built), read `CF_NATIVE_NONCES` per
     action and SKIP already-consumed `(sender,nonce)`; mirror `validator.rs:343-356`.
     Guarantees correctness even if selection misses.
   - **EFFICIENCY (root cause):** pipeline-aware selection — exclude in-flight
     proposed-but-uncommitted action hashes from selection so dups never enter N+1/N+2.
     Relevant: app.rs `produce_block` (~L819-836) + `self.pending_proposals` (~L880);
     `crates/torus-mempool/src/lib.rs` `select_native_for_block_with_senders`. This also
     recovers the ~3× wasted native block space.
3. Implement, keep the proposer-attestation/full-block path (commit `66da812`) intact, rebuild,
   and **PROVE:** the new test passes; the Task-2 demonstration now shows 1× effect;
   `cargo test` green (incl. the 4-node consensus test); a fresh devnet flood shows duplication
   factor ≈ 1.0 via the `getBlockBody` analysis. Watch for regressions to block rate / liveness
   (idle was ~14–18 blk/s; do not reintroduce stalls).
4. `/code-review` the diff, then commit with a clear message (end with the required
   `Co-Authored-By` trailer). **Do NOT push unless asked.**

## QUALITY BAR

TDD (failing test before code); prove every claim with command output; record findings/fixes
to memory; if a framework gate misfires on this Python tooling (it has before), verify with
`py_compile`/`ruff` and proceed via the prescribed unblock path; ask before destructive ops.
Save a wrap-up + `LIVE_STATE` update at the end.
