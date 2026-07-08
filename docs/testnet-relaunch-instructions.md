# Torus-hyperBFT testnet relaunch — FRESH 4-VALIDATOR GENESIS (era `1783555200`)

Universal copy-paste instructions for **every validator operator** (seed, val1, val3,
and the new friend2 / bigserver). **Supersedes `val3-upgrade-instructions-s392.md`
and all earlier per-node upgrade notes.** Prepared 2026-07-08.

---

## What's different this time

- **A 4th validator joins** — friend2 (bigserver). The set goes 3 → **4 validators**,
  2M stake each.
  - Quorum is now **3-of-4** (fault tolerance **f = 1**): the chain keeps committing
    even if any **one** node is down. (The old 3-validator set was 3-of-3 — a single
    node down **halted** it.)
- **New era**: `timestamp` bumped `1783468800` → **`1783555200`** (2026-07-09 00:00 UTC).
  `chain_id` stays **7778**.
- **Everything else is unchanged** from the last genesis: the same 10 perpetual
  markets, the same governance stakers, accounts, and balances. The existing three
  validators reuse the **same keys** (same libp2p peer ids).

Because the validator set **and** the timestamp change, the **genesis hash changes** —
so this is a **coordinated fresh-genesis wipe**, not a resume.

---

## ⚠️ The wipe MUST be coordinated (this has bitten us before)

We reuse the existing validator keys, so a freshly-wiped node will happily
**re-sync the OLD chain** from any old peer still serving it — neither the new
timestamp nor the chain_id isolates it. The only thing that makes the new chain take
hold is:

> **All FOUR nodes DOWN + data WIPED + auto-restart DISABLED before ANY node starts
> on the new genesis.** One straggler re-infects everyone.

**Do not start on the new genesis until the coordinator confirms all four are down +
wiped.** No key change — reuse your keystore (your libp2p peer id must stay the same).

---

## Node roster

| # | node | public addr | libp2p peer id | notes |
|---|------|-------------|----------------|-------|
| 1 | seed | `95.111.231.121:30333` | `12D3KooWQeKf…MK24` | coordinator |
| 2 | val1 | `84.32.108.220:30333` | `12D3KooWK5QY…CPUze` | smallserver (ssh :58331) |
| 3 | val3 | *NAT — dials out* | `12D3KooWLBPw…` | VPS, no inbound IP |
| 4 | **friend2** | `103.167.235.250:30333` | `12D3KooWFSJj…KGPu` | **NEW** — bigserver; rpc `127.0.0.1:28545`, metrics `127.0.0.1:29090` |

---

## 1. Update + rebuild (safe to do now, before the window)

```bash
cd <your-torus-hyperbft-repo>
git fetch origin integration/bs4a-livelock-s428
git checkout <RELAUNCH-COMMIT>     # coordinator gives the exact hash once it's pushed
cargo build --release
```

Then **verify the genesis you'll launch** — this is the real safety check
(do **not** hand-edit it):

```bash
grep -c '"market_id"' testnet/genesis.json      # must be 10
grep    '"timestamp"' testnet/genesis.json      # must be 1783555200
grep -c '"pubkey"'    testnet/genesis.json      # must be 4  (four validators)
grep    <your-validator-pubkey> testnet/genesis.json   # your own key must be present
```

Build this **exact** commit — a stale binary can seed a different genesis state
(markets / stakers / validator set) and fork on block 1. Building does not touch a
running node; it just produces `target/release/torus-node`.

---

## 2. Coordinated wipe window (ALL FOUR nodes, on the coordinator's go)

**a. Stop AND disable your service so nothing auto-restarts it:**

```bash
sudo systemctl stop <your-service>
sudo systemctl disable <your-service>   # a prior relaunch FAILED when a node
                                        # auto-restarted and re-served the old chain
pgrep -af torus-node                    # must print NOTHING — confirm it's really gone
```

(If you run via `nohup` instead of systemd: `kill -TERM <pid>` for a clean RocksDB
shutdown, then make sure nothing relaunches it.)

**b. Back up + wipe your chain data (KEEP your keystore):**

```bash
mv <your-data-dir> <your-data-dir>.bak-fresh-genesis-$(date +%s)
# keystore + passphrase files are separate — do NOT touch them
```

**c. Report "down + wiped". Wait for the coordinator's "all four down + wiped"
before starting.** This is the make-or-break step.

---

## 3. Start on the new genesis (still in the window)

Your data dir is now empty, so you **must** pass `--genesis` on this first boot
(a resume never needed it):

```bash
target/release/torus-node \
  --genesis testnet/genesis.json \
  --data-dir <your-data-dir> \
  --keystore <your-keystore> --passphrase-file <your-passphrase-file> \
  --retention-blocks 100000 \
  --p2p-listen /ip4/<YOUR-PUBLIC-IP>/udp/30333/quic-v1 \
  --p2p-peers <ALL PUBLIC PEERS EXCEPT YOURSELF — comma-separated, see below>
# do NOT pass --native-gossip=false
```

**Public peer multiaddrs** — include every one *except your own* (val3 is NAT and
dials out, so it isn't listed here; it will reach you):

```
/ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24
/ip4/84.32.108.220/udp/30333/quic-v1/p2p/12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze
/ip4/103.167.235.250/udp/30333/quic-v1/p2p/12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu
```

If you launch via systemd, put `--genesis testnet/genesis.json` in the unit's
`ExecStart` **for this first boot**, then re-enable: `sudo systemctl enable
<your-service>`. (Once block 1 commits, `--genesis` is ignored on later restarts —
harmless to leave in.) Keep `--retention-blocks 100000` and do **not** pass
`--native-gossip=false`.

### friend2 (bigserver) — first-time validator notes
- You're joining as a **validator** for the first time — run in validator mode with
  your keystore, **not** `--rpc-only`. Your `validator.keystore` must hold the key for
  pubkey `0x537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c`
  (it backs your peer id `12D3KooWFSJj…`).
- Ports: `--p2p-listen /ip4/103.167.235.250/udp/30333/quic-v1`,
  `--rpc-addr 127.0.0.1:28545`, `--metrics-addr 127.0.0.1:29090`.
- **Open inbound UDP 30333** on `103.167.235.250` so the others can dial you.
- Peers to pass: the **seed + val1** multiaddrs above (val3 dials you).

### seed / val1 / val3 — the one change from last time
Add **friend2's multiaddr** (`/ip4/103.167.235.250/…/12D3KooWFSJj…`) to your
`--p2p-peers` list alongside the others. Everything else stays as it was.

---

## 4. Verify after start

```bash
curl -s localhost:<your-metrics-port>/metrics | grep -c torus_native_actions_processed_total   # present = node up
```

- **Committed height starts near 0 and climbs** — the decisive check the wipe worked.
  If you see the old height (~130k+), your node re-synced the **OLD** chain: stop,
  wipe again, and hunt down whichever old peer is still serving it.
- With **4 validators**, the chain commits once **≥ 3** are up and voting (3-of-4
  quorum). It starts producing as soon as three fresh nodes are up; the fourth gives
  full f = 1 fault tolerance.

Ping the coordinator when you're up — we'll confirm all 10 markets are live and all
four validators are voting, then re-measure block speed.
