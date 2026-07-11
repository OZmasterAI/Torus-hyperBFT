# Removed validators (S445) — val3 + friend2 preservation record

**Status:** Removed at **S445** in preparation for a new fresh-genesis relaunch that
will use the incoming **"18c"** validator. This document captures EVERYTHING needed to
re-add val3 and friend2 later, so re-adding is a trivial copy-paste.

- **seed** (`95.111.231.121`) and **val1** (`84.32.108.220`, smallserver) **REMAIN** in
  the new genesis.
- **val3** (NAT box, no SSH) and **friend2 / bigserver** (aka "val2", no SSH) are
  **REMOVED** and preserved here.
- Do **not** commit this file — it is left untracked for user review.

> **UPDATE (post-launch):** genesis **`7157aee`** (think-dev) launched the 3-validator
> set **seed + val1 + 18c**; **era `1783814400`**. val3 and friend2 were NOT included and
> **rejoin DYNAMICALLY on-chain — no new genesis needed** (verified). Follow the dynamic
> re-add checklist in each validator's section below (the old "insert into genesis +
> coordinated wipe" flow is superseded).

## Source of truth

- **Live genesis file:** `testnet/genesis.json`
  - Content pinned by commit **`470970e`** "feat(genesis): add 4th validator (friend2)
    + bump era to 1783555200 (S434)".
  - Relaunch instructions pinned to commit **`5a90c2f`** (S434) — see
    `docs/testnet-relaunch-instructions.md`.
  - Repo HEAD at capture: `1ba6c46`, branch `integration/bs4a-livelock-s428`.
- **chain_id:** `7778` · **era/timestamp:** `1783555200` (2026-07-09 00:00 UTC)
- **Per-validator stake:** `2000000000000000000000000` (2,000,000 TRS) · **commission:**
  `500` bps (5%) — identical for all four validators.
- **Operator relaunch drafts:** `docs/ops/s442-relaunch/` on branch `origin/think-dev`
  (`msg-val3.md`, `msg-friend2.md`, `msg-val1.md`, `verify-live.md`,
  `relaunch-runbook.md`).

## Current live genesis validator set (all 4, as parsed from `testnet/genesis.json`)

| # | node | genesis `address` (EVM payout) | consensus `pubkey` | libp2p peer id | stake | commission |
|---|------|--------------------------------|--------------------|----------------|-------|------------|
| 1 | **seed** (stays) | `0x1000000000000000000000000000000000000001` | `0xdc4ca7a6ab405ad1da72806658b4f87282b25e448eb438b09a3034c7328cecf5` | `12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24` | 2M | 500 |
| 2 | **val1** (stays) | `0x1b8b5853cd3a0ddeeee259f12fbb8bbe2d2d337d` | `0x899396cf13f070699196bed73c8bdc27187a8fa925cb1a0abd7a35614235fdbb` | `12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze` | 2M | 500 |
| 3 | **val3** (REMOVE) | `0x7e7cfdb06a5c0e3cc85510ccaeed896f39c179f5` | `0x99f813745e8347609dda017afbbe4a963921ed32df40e1d215c56bb9ddb3a59f` | `12D3KooWLBPwqPS6WAeXvkPvfY1iac58F286hSTyUBnZSxcyxtdL` | 2M | 500 |
| 4 | **friend2** (REMOVE) | `0x0936adb653ffd8ef521029ffbf27bc1c815a7cbb` | `0x537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c` | `12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu` | 2M | 500 |

**Important identity note:** one ed25519 keystore key backs BOTH the consensus `pubkey`
AND the libp2p peer id for a node (main.rs: network `.398`, consensus `.435`; verified
in memory `8fbf5cd7`). The genesis `address` is parsed independently of `pubkey`
(lib.rs:382) and is used for **EVM payout only** — it does not have to relate to the
key. So `pubkey` ↔ `peer id` is the real identity binding; `address` is bookkeeping.

**Confidence on the val1/val3 split:** val1 = `0x1b8b5853` is confirmed — it was added
as the "second validator" (commit `60cb1e3`), and val1/smallserver was the second box to
join. val3 = `0x7e7cfdb0` is by elimination (seed and friend2 pubkeys are independently
confirmed; the remaining entry must be val3). We do **not** have SSH to val3 and cannot
grep its keystore, so **have the val3 operator confirm** `0x99f813745e83…` is their
pubkey (`grep <their-pubkey> testnet/genesis.json`) before trusting the address swap.

---

# val3 (NAT box, no SSH — copy-paste operator)

## 1. Genesis JSON to re-insert

Append this object into `testnet/genesis.json` → `validators[]` (verbatim from
`testnet/genesis.json` @ `470970e`):

```json
{
    "address": "0x7e7cfdb06a5c0e3cc85510ccaeed896f39c179f5",
    "pubkey": "0x99f813745e8347609dda017afbbe4a963921ed32df40e1d215c56bb9ddb3a59f",
    "stake": "2000000000000000000000000",
    "commission_bps": 500
}
```

**No other entries to restore.** val3's address appears ONLY in `validators[]` (line 62
of the current file) — there is NO matching `permanent_stakes`, `native_balances`, or
`accounts` entry. Re-adding = this one object.

## 2. Network identity

- **Role:** validator, **NAT — dials out** (no public inbound). Because it dials out, it
  is **never** listed in anyone's `--p2p-peers`; it reaches seed/val1/friend2 itself.
- **libp2p peer id (full):** `12D3KooWLBPwqPS6WAeXvkPvfY1iac58F286hSTyUBnZSxcyxtdL`
  (deterministic from its keystore key; confirmed live in memory `eee78997` — the
  long-mysterious "LBPw" peer that dialed the seed for many sessions IS val3).
- **Public IP — AMBIGUOUS, flagged:** sources disagree and it does not matter
  operationally (NAT/dials-out, never in a peer list):
  - task brief: `213.190.25.21`
  - S433 mesh-peering memory `6134227`: `23.88.46.145` (marked "PUBLIC but PORT
    UNCONFIRMED")
  - S442 relaunch roster: listed simply as "*NAT — dials out*", no inbound IP.
  → If val3 ever needs to be dialed (it normally isn't), get the current IP/port from
  the operator.
- **P2P port:** `30333/udp/quic-v1` convention (unconfirmed for val3 — NAT).
- **RPC / metrics ports:** `[OPERATOR-CONFIRM]` — unknown, not documented anywhere found.

## 3. Operator / contact + how updates reach them

- **No SSH.** Coordinated via copy-paste message. The ready-to-send draft is
  `docs/ops/s442-relaunch/msg-val3.md` (on `origin/think-dev`).
- Procedure: send the message → operator stops+disables node, renames data dir to
  `.bak-<date>` (keeps keystore = same key/peer id), builds the pinned commit, pings
  "down + wiped + built", then holds until coordinator's "all four down — GO", then
  launches passing seed+val1(+friend2) as `--p2p-peers` (val3 dials out).

## 4. History / caveats at re-add time

- **Livelock / crawl history (the big one):** on prior chains val3's node kept failing a
  chunk of its leader turns; whenever it did, the whole chain collapsed from ~15–20
  blk/s to a ~1s-per-block crawl (down to ~1.8 blk/s). Root-caused as a
  body-starvation / missing-body livelock — a real **node bug**, NOT val3's box or
  network. See memories `28e1a8212dbb6ab4` (livelock @151859 root cause),
  `ca7ead68dc1b0c70` (S426 reproducible 2/2), `32ff8a7bc043474d` (S426 escalation). The
  think-dev merge is the fix; expect steady 15–20 blk/s after re-add.
- **Identity confusion resolved:** val3 was long unidentified as the "LBPw" peer because
  it ran `mode=rpc-only` and never voted; since S432 it is `mode=validator`
  (memory `eee78997`). Leader-for-view = `validators_sorted_by_pubkey_ASC[view % N]`, so
  watch that val3 isn't eating a disproportionate share of NewView/timeouts (its old
  livelock signature).
- Quorum with 4 validators is 3-of-4 (f=1); removing val3 + friend2 drops the set to
  seed+val1+18c = 3 validators = 3-of-3 (a single node down halts). Confirm the new set
  size before launch.

## 5. How to re-add — VERIFIED DYNAMIC FLOW (no new genesis)

Follow the canonical **"How to re-add a validator dynamically"** checklist below. val3
specifics:

- **Consensus pubkey to register:** `0x99f813745e8347609dda017afbbe4a963921ed32df40e1d215c56bb9ddb3a59f`
  (INFERRED by elimination — MUST be confirmed against the val3 operator's keystore
  before the RegisterValidator submit; a typo'd pubkey creates a validator that can
  never sign, staking.rs:42 does no ownership/uniqueness check).
- **EVM address to fund** = val3's chosen self-stake payout address. The genesis payout
  address was `0x7e7cfdb06a5c0e3cc85510ccaeed896f39c179f5`, but under dynamic
  registration the self-stake comes from whatever EVM account the operator funds and
  signs from — coordinate the exact address with the operator.
- **Old data dir is mis-keyed** for genesis `7157aee` — val3 must launch with a **fresh
  data dir** and the F-2-lineage binary; NAT/dials-out, so `--p2p-peers` = the 3 running
  validators (seed + val1 + 18c).
- **Register val3 and friend2 in SEPARATE epochs** (rotation cap — see canonical
  checklist step 6).

---

# friend2 / bigserver (aka "val2", no SSH — copy-paste operator)

## 1. Genesis JSON to re-insert

Append this object into `testnet/genesis.json` → `validators[]` (verbatim from
`testnet/genesis.json` @ `470970e`; this is the exact entry that commit ADDED):

```json
{
    "address": "0x0936adb653ffd8ef521029ffbf27bc1c815a7cbb",
    "pubkey": "0x537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c",
    "stake": "2000000000000000000000000",
    "commission_bps": 500
}
```

**No other entries to restore.** friend2's address appears ONLY in `validators[]` (line
68 of the current file) — NO matching `permanent_stakes`, `native_balances`, or
`accounts` entry. Re-adding = this one object.

> Stake note: current live genesis uses **2M** (`2000000000000000000000000`). An older
> S378 memory (`79b2d740`) records friend2 at **1M** when it was first added — that value
> is **stale**; use the current 2M above to match the rest of the set.

## 2. Network identity

- **Public addr:** `103.167.235.250:30333` (listens; others dial it). Keep **inbound UDP
  30333 open**.
- **libp2p peer id (full):** `12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu`
  (deterministic from its keystore key; confirmed = friend2/"val2" node in memory
  `779b967f`).
- **Full multiaddr:**
  `/ip4/103.167.235.250/udp/30333/quic-v1/p2p/12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu`
- **P2P listen:** `--p2p-listen /ip4/103.167.235.250/udp/30333/quic-v1`
- **RPC:** `127.0.0.1:28545` (known) · **Metrics:** `127.0.0.1:29090` (known)
- Peers friend2 passes on launch: **seed + val1** (val3 dials friend2, so val3 is not in
  friend2's peer list).

## 3. Operator / contact + how updates reach them

- **No SSH.** Operator has historically been away/on vacation; node has run unattended on
  old build+genesis across sessions (memory `779b967f`). Coordinate via copy-paste.
- Ready-to-send draft: `docs/ops/s442-relaunch/msg-friend2.md` (on `origin/think-dev`) —
  includes the exact launch line (ports `28545`/`29090`, listen on
  `103.167.235.250/udp/30333`, peers = seed+val1).
- Same handshake as val3: stop+disable → rename data dir to `.bak` (keep keystore) →
  build pinned commit → "down + wiped + built" → hold for GO → launch.

## 4. History / caveats at re-add time

- **friend2 = the throughput star.** Its box produced the **~207k orders/s testnet
  record** (RPC 28545, offset-20 senders) — a TESTNET number NOT reproducible on the
  seed VPS devnet (~65–70k o/s ceiling). See memories `000a6354` (S444 correction) and
  `9af8b543` (200k o/s decision). Re-measure once the mesh is healthy.
- friend2 was **previously removed** at S378 (memory `79b2d740`) as the last
  unmeasured/uncoordinated validator suspected of pacing block time to ~2s while its
  operator was away, then re-added at S434 (commit `470970e`). It has been removed and
  re-added before — this is the documented, expected pattern.
- Aliases: the user calls this node **friend2**, **bigserver**, and **val2** — all the
  same node/peer id `12D3KooWFSJj…`.
- On old chains, `FSJj` was seen ban/redial-spamming and at times had not dialed the
  seed (possibly wedged on an old build) — benign, but expect a fully coordinated wipe
  so it can't re-serve an old chain.

## 5. How to re-add — VERIFIED DYNAMIC FLOW (no new genesis)

Follow the canonical **"How to re-add a validator dynamically"** checklist below.
friend2 specifics:

- **Consensus pubkey to register:** `0x537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c`
  (confirmed = friend2's node key; still triple-check against the operator's keystore —
  staking.rs:42 does no ownership/uniqueness check).
- **EVM address to fund** = friend2's chosen self-stake payout address (genesis payout
  was `0x0936adb653ffd8ef521029ffbf27bc1c815a7cbb`; under dynamic registration the
  self-stake is drawn from whatever EVM account the operator funds and signs from —
  confirm the address).
- **Network:** friend2 listens on `103.167.235.250:30333` — keep inbound UDP 30333 open;
  fresh data dir; F-2-lineage binary; `--p2p-peers` = the 3 running validators.
- **Register val3 and friend2 in SEPARATE epochs** (rotation cap — see canonical
  checklist step 6).
- Operator was previously away/unattended — confirm reachability before starting the
  governance flow (which has a ~7-day / 302400-block whitelist expiry window).

---

## How to re-add a validator dynamically (canonical, VERIFIED — no new genesis)

This on-chain flow adds a validator to the LIVE chain (genesis `7157aee`, era
`1783814400`) with **no genesis edit and no coordinated wipe**. Applies to both val3 and
friend2 (see each section above for the validator-specific pubkey/address).

1. **PREREQ — binary lineage + fresh data dir.** The returning validator's node binary
   MUST be at or past the **F-2 commit `783f4d0`** lineage (`compute_action_hash` now
   binds the signature; pre-F-2 binaries stall against the new chain with
   `MissingData` / no-progress symptoms). The old data dir is mis-keyed for the new
   genesis — use a **fresh data dir**.
2. **Fund the validator's EVM address with EXACTLY the intended self-stake.** Must be
   **>= 10,000 TRS** (`MIN_SELF_DELEGATION`). Registration consumes the **ENTIRE
   balance** as self-stake (`native_executor.rs:1317,1336`), so fund exactly the amount
   you want staked — no more, no less.
3. **Governance whitelist the candidate.** From any funded account:
   - Delegate **>= 1,000 TRS** to an active validator (`min_proposal_stake`).
   - `SubmitProposal` with `ProposalAction::ValidatorRegistration{candidate}`.
   - `Vote` yes, then finalize + execute after the voting period + timelock.
   - Quorum base **excludes** validator self-stake (`governance.rs:1386`), so this is
     trivially passable this early.
   - The whitelist **EXPIRES after 302400 blocks (~7 days)** — the RegisterValidator in
     step 4 must land within that window.
4. **Validator submits `NativeAction::RegisterValidator{pubkey: <their ed25519 consensus
   pubkey>, commission}`**, EIP-712-signed from their EVM address.
   **WARNING:** the `pubkey` is self-declared with **NO ownership/uniqueness validation**
   (`staking.rs:42`) — a typo creates a validator that can **never sign**. Triple-check
   the pubkey against the operator's keystore.
5. **Enactment:** takes effect at the **next epoch boundary**
   (`block_height % epoch_length == 0`; confirm `epoch_length` in the testnet genesis
   `chain_config`). The joiner does **not** need to be online for enactment; the chain
   keeps running (3+1 set → quorum 3-of-4).
6. **ROTATION CAP — one set-change per epoch.** A 3-validator set allows only **1**
   validator-set change per epoch (`epoch.rs:140`, cap = `size/3`). Two simultaneous
   arrivals BOTH get deferred, so **re-add val3 and friend2 in SEPARATE epochs**.
7. **Start the node:** F-2-lineage binary, matching **ed25519 keystore**, **fresh data
   dir**, `--p2p-peers` = the 3 currently-running validators (seed + val1 + 18c).
8. **Recommended — rehearse on devnet first.** Validator-set updates have livelock
   history on this codebase; `dynamic_validators.rs` tests pass, but a live devnet
   rehearsal of the whole join flow is cheap insurance before touching testnet.

---

## Appendix — verbatim `validators[]` block being removed (for a clean diff)

The current live `validators[]` (commit `470970e`) is exactly these four objects, in
order. Removing val3 + friend2 leaves objects #1 (seed) and #2 (val1); the new genesis
will add the incoming **18c** validator in their place.

```json
"validators": [
    { "address": "0x1000000000000000000000000000000000000001", "pubkey": "0xdc4ca7a6ab405ad1da72806658b4f87282b25e448eb438b09a3034c7328cecf5", "stake": "2000000000000000000000000", "commission_bps": 500 },
    { "address": "0x1b8b5853cd3a0ddeeee259f12fbb8bbe2d2d337d", "pubkey": "0x899396cf13f070699196bed73c8bdc27187a8fa925cb1a0abd7a35614235fdbb", "stake": "2000000000000000000000000", "commission_bps": 500 },
    { "address": "0x7e7cfdb06a5c0e3cc85510ccaeed896f39c179f5", "pubkey": "0x99f813745e8347609dda017afbbe4a963921ed32df40e1d215c56bb9ddb3a59f", "stake": "2000000000000000000000000", "commission_bps": 500 },
    { "address": "0x0936adb653ffd8ef521029ffbf27bc1c815a7cbb", "pubkey": "0x537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c", "stake": "2000000000000000000000000", "commission_bps": 500 }
]
```

## Open items / things to request from operators

- **val3 pubkey/address confirmation** — inferred (`0x7e7cfdb0` / `0x99f813745e83…`) by
  elimination; confirm with operator (no SSH, can't grep their keystore).
- **val3 RPC/metrics ports** — unknown; request if a fork-check/monitoring endpoint is
  needed.
- **val3 public IP** — ambiguous (213.190.25.21 vs 23.88.46.145 vs "NAT/none");
  operationally irrelevant since val3 dials out, but confirm if ever needed.
- Everything else (both validator genesis entries, friend2 IP/ports/peer id, val3 peer
  id) is captured verbatim above.
