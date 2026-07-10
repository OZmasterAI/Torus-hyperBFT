# S442 testnet relaunch runbook — think-dev merge, FRESH 4-VALIDATOR GENESIS

Operator-facing runbook for the coordinated relaunch onto the think-dev merge.
**Supersedes `docs/testnet-relaunch-instructions.md` (S434 / commit `5a90c2f`).**
Prepared for the think-dev merge window.

- **Pinned commit:** `<COMMIT>`  *(the think-dev merge commit — fill in once merged)*
- **Optional tag:** `<TAG>`  *(annotated release tag, if cut)*
- **Distributed-binary sha256:** `<SHA>`  *(for operators who receive a prebuilt
  `torus-node` instead of building — see per-node steps)*
- **chain_id:** `7778` (unchanged) · **quorum:** 3-of-4 (f = 1)

---

## Why a FRESH genesis (not a resume)

The think-dev merge fixes the state-divergence / body-starvation livelock that
crawled the S435 chain, and it ships a **block-format change** — so the old chain is
**unjoinable by design**: a node on the new binary cannot and must not sync the old
history. This is a coordinated fresh-genesis wipe, same pattern as S434; we reuse the
existing validator keys (peer ids stay the same).

---

## Node roster

| # | node | public addr | libp2p peer id | RPC (localhost) | metrics (localhost) | access |
|---|------|-------------|----------------|-----------------|---------------------|--------|
| 1 | seed | `95.111.231.121:30333` | `12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24` | `8545` | `9090` | local box (coordinator) |
| 2 | val1 | `84.32.108.220:30333` | `12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze` | `[OPERATOR-CONFIRM]` | `[OPERATOR-CONFIRM]` | SSH `-p 58331` (we can drive it) |
| 3 | val3 | *NAT — dials out* | `12D3KooWLBPw…` | `[OPERATOR-CONFIRM]` | `[OPERATOR-CONFIRM]` | **no SSH** — copy-paste operator |
| 4 | friend2 | `103.167.235.250:30333` | `12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu` | `28545` | `29090` | **no SSH** — copy-paste operator |

Public peer multiaddrs (pass every one *except your own*; val3 is NAT and dials out,
so it is never in a `--p2p-peers` list — it reaches everyone else):

```
/ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24
/ip4/84.32.108.220/udp/30333/quic-v1/p2p/12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze
/ip4/103.167.235.250/udp/30333/quic-v1/p2p/12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu
```

---

## Per-node steps

Do steps 1–2 any time before the window. Do steps 3–5 **in lockstep**, on the
coordinator's go.

### 1. Stop the process (do NOT delete anything yet)

```bash
sudo systemctl stop <your-service>       # or: kill -TERM <pid> for a clean RocksDB shutdown
sudo systemctl disable <your-service>    # a prior relaunch FAILED when a node auto-restarted
                                         # and re-served the old chain — disable auto-restart
pgrep -af torus-node                     # MUST print nothing
```

### 2. KEEP the data dir — rename it to `.bak-<date>`, do NOT delete

```bash
mv <your-data-dir> <your-data-dir>.bak-<DATE>   # e.g. .bak-2026-07-10, or $(date +%F)
# keystore + passphrase files are SEPARATE — do NOT touch them; you reuse the same key
```

We never `rm` the old data — it is our forensic copy if the wipe needs auditing.
(Seed convention: `testnet/data` → `testnet/data.bak-fresh-genesis-$(date +%s)`.)

### 3. Get the new binary — build at `<COMMIT>`, or verify the sha256 you were sent

**Build (default):**

```bash
cd <your-torus-hyperbft-repo>
git fetch origin
git checkout <COMMIT>
git log --oneline -1        # must show <COMMIT>
cargo build --release       # produces target/release/torus-node
```

**Or, if you received a prebuilt binary** (glibc 2.39 boxes only):

```bash
sha256sum torus-node        # MUST equal <SHA>
```

A stale binary seeds a *different* genesis state and forks on block 1 — build the
**exact** commit or match the sha256. Then confirm the genesis you'll launch:

```bash
grep '"chain_id"'  testnet/genesis.json     # must be 7778
grep -c '"pubkey"' testnet/genesis.json     # must be 4  (four validators)
grep <your-validator-pubkey> testnet/genesis.json   # your own key must be present
```
*(Genesis field expectations — market count, era timestamp — are `[OPERATOR-CONFIRM]`
until the think-dev merge genesis is pinned.)*

### 4. Launch on the new genesis (still in the window)

Your data dir is empty, so you **must** pass `--genesis` on this first boot:

```bash
target/release/torus-node \
  --genesis testnet/genesis.json \
  --data-dir <your-data-dir> \
  --keystore <your-keystore> --passphrase-file <your-passphrase-file> \
  --retention-blocks 100000 \
  --p2p-listen /ip4/<YOUR-PUBLIC-IP>/udp/30333/quic-v1 \
  --p2p-peers <ALL PUBLIC PEERS EXCEPT YOURSELF, comma-separated — see roster>
# do NOT pass --native-gossip=false
```

val3 (NAT): omit `--p2p-listen` public IP as your operator normally does, pass seed +
val1 + friend2 as peers `[OPERATOR-CONFIRM: val3's exact launch line]`. Once block 1
commits, `--genesis` is ignored on later restarts — harmless to leave in; re-enable
your service afterward.

### 5. Confirm mesh + committing

```bash
curl -s localhost:<your-metrics-port>/metrics | grep -c torus_native_actions_processed_total  # >0 = up
```

- **4-peer mesh:** each node sees the other three connected.
- **Committed height starts near 0 and climbs** — the decisive proof the wipe worked.
  If you see an old height, you re-synced the OLD chain: stop, re-wipe, hunt the stale
  peer.
- **Committing:** chain produces blocks. Ping the coordinator when you're up.

---

## Launch-window coordination

1. Everyone completes steps 1–3 ahead of time (stopped + wiped-to-`.bak` + built).
2. Each operator reports **"down + wiped + built"**.
3. Coordinator confirms **all four down + wiped** — nobody starts before this.
4. All four launch (step 4). **The chain goes live at 3-of-4** — it starts committing
   the moment any three fresh nodes are up and voting; the fourth restores full f = 1
   fault tolerance. A straggler joining late is fine (it syncs the new chain).

---

## Post-launch verification (seed-side)

Expected steady state after the livelock fix: **~15–20 blk/s** (S435 launched ~17
then crawled to ~1.8; the think-dev fix is what should hold it at 15–20).

**Per-height hash cross-check across all four live RPCs** — the tripwire that catches
any state divergence early. Run from the seed box against the 4 validator RPCs:

```bash
python3 devnet/t12-fork-check.py \
  --rpc http://127.0.0.1:8545 \
  --rpc http://84.32.108.220:<VAL1-RPC-PORT> \
  --rpc http://<VAL3-RPC-HOST>:<VAL3-RPC-PORT> \
  --rpc http://103.167.235.250:28545
# exits 70 on ANY per-height hash divergence between nodes
```

*(`devnet/t12-fork-check.py` is the committed fork tripwire. val1/val3 RPC host:port are
`[OPERATOR-CONFIRM]` — see roster. friend2 RPC `28545` and seed `8545` are known.)*

See `verify-live.md` for the full seed-side checklist (block-speed check, tripwire
exit-code handling, first-24h watch list).
