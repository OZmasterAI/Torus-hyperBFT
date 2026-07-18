# Anchored testnet genesis — notes

## Live / pushed relaunch genesis
- **Branch:** `feat/precompile-0800-topn-gas` @ **`ef2cf01`** (origin) — the relaunch commit
  everyone builds. `testnet/genesis.json` on that commit **is** the launch file.
- **5 validators, solo-capable, `timeout_base_ms: 500`, `chain_id 7778`, era `1784592000`.**
- Base: live genesis (`torus-build`, era 1783814400) — accounts(52)/native_balances(60)/
  permanent_stakes(10)/markets(10)/economics/evm carried verbatim.

| Node | Owner | Address | Pubkey | Stake |
|------|-------|---------|--------|-------|
| seed (8c) | ours | `0x1000…0001` | `0xdc4c…ecf5` | 3M |
| c18 (18c) | ours | `0x991f…0276` | `0x00fd…8e1c` | 3M |
| home (16c, WSL2, dial-out) | ours | `0xBd82…206c` | `0x99a8…5780` | 3M |
| val2 (bigserver) | friend | `0x0936…7cbb` | `0x537f…a63c` | 2M |
| val3 (NAT, dial-out) | friend | `0x7e7c…79f5` | `0x99f8…a59f` | 1M |

**Dropped:** val1/smallserver. **Demoted:** val2 3M→2M. **Promoted:** seed 2M→3M.

## Quorum (verified: `Q = ⌊2·T/3⌋ + 1`)
- T = **12M**, **Q = 9M**. Tolerance `T−Q = 3M`.
- **Ours (seed+c18+home) = 9M = Q → we can commit solo**; friends (val2+val3 = 3M) can't halt.
- Survives any one 3M anchor down. **Two 3M down = halt.**
- **Solo caveat:** ours = 9 = Q with *zero slack*. Solo needs **all three** of our boxes —
  incl. **home** (the flaky home PC). If home is down while friends are down → halt. So
  "run without friends" holds only while home stays up (do the always-on setup on home).

## 6th validator — val4 (new friend), not yet in genesis
- `testnet/genesis-anchored-6val.TEMPLATE.json` holds the target **6-val** set with val4 at
  **1M** as a **fail-safe placeholder** (`0xREPLACE_…` non-hex pubkey → node refuses to boot
  until filled; `torus-genesis/lib.rs:363`).
- When his key arrives: overwrite the two `REPLACE_…` values → 13M, Q=9 (ours still 9 =
  solo-capable, friends 4M). Then either **re-push as a new relaunch commit** (fresh chain)
  or add him **on-chain dynamically** to the running 5-val chain (no wipe;
  `removed-validators-s445.md`).

## Isolation (verified s332)
Timestamp does NOT isolate a chain. This relaunch is isolated by: new validator set +
topn's changed state-root preimage (old-binary nodes can't follow) + **coordinated wipe**
(all nodes down before any restart). `chain_id` kept 7778.

## Launch mechanics
- Public nodes (seed, c18, val2): `--p2p-listen /ip4/<IP>/udp/30333/quic-v1` + inbound UDP
  30333 open + `--p2p-peers` = the others.
- Dial-out nodes (val3, home): **omit** `--p2p-listen`; `--p2p-peers` = everyone else.
- Do **not** pass `--native-gossip=false`.
