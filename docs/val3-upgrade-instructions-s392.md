# val3 relaunch instructions — FRESH 10-MARKET GENESIS (commit `831187c`)

> **HISTORICAL — superseded by `testnet/val3-launch-s448.md`. Do not follow.**
> This targets the 2026-07-08 era (commit `831187c`, 10 markets, era `1783468800`)
> and is preserved as a record of what was actually sent. Three things in it are now
> actively wrong: it boots `testnet/genesis.json`, which **no longer exists**; it
> lists `84.32.108.220` (val1) as a peer, and val1 was **dropped** from the set; and
> it expects 10 markets where the current genesis seeds **100**.
> Current era: 4 validators, genesis sha256
> `2c0d9cb52cd10c4996f2e4f43b29d54b97016e3ed91bb3984ce2725c66beb4ab`, branch
> `perf/re-proof5`. (S395 already lost a relaunch window to a stale peer id in this
> exact file — check `val3-launch-s448.md` is what you're sending.)

Copy-paste for the val3 operator (3rd validator, self-hosted). **Supersedes the
S433/928b700 upgrade instructions entirely.** Prepared 2026-07-08.

---

## ⚠️ This is a FRESH GENESIS relaunch — we WIPE the chain this time

Unlike every recent upgrade, this is **not** a resume. We're starting a brand-new
chain from a new `genesis.json` that ships **10 perpetual markets** (was 1) plus
pre-seeded governance stakers, so we can test adding markets by on-chain vote.
**The current chain is being abandoned.** Same validator keys, same `chain_id`
(7778) — the fresh start comes entirely from *every node wiping its data dir
together*.

### Why the wipe must be coordinated (this bit us twice before)

Because we reuse the same validator keys, a freshly-wiped node running the new
genesis will still **re-sync the OLD chain** from any old peer that is still up —
the old chain is consensus-compatible with our key set, and neither the timestamp
nor the chain_id isolates it. So the *only* thing that makes the new chain take
hold is:

> **All three nodes DOWN + data WIPED + auto-restart DISABLED before ANY node
> starts on the new genesis.** One straggler re-infects everyone.

**Do not start your node on the new genesis until I confirm all three are down
and wiped.** No key change — reuse your keystore (your libp2p peer id must stay
the same).

---

## 1. Update + rebuild (safe to do now, before the window)

```bash
cd <your-torus-hyperbft-repo>
git fetch origin integration/bs4a-livelock-s428
git checkout 831187c
git log --oneline -1        # must print: 831187c feat(genesis): fresh 10-market testnet genesis + governance stakers (S434)
cargo build --release
```

Build this **exact** commit. A stale binary seeds a *different* genesis state
(markets + permanent stakes) and forks off on block 1. Building does not touch a
running node — it just produces `target/release/torus-node`.

Confirm the genesis you'll launch is the one from git (do **not** hand-edit it):

```bash
grep -c '"market_id"' testnet/genesis.json    # must be 10
grep '"timestamp"'   testnet/genesis.json     # must be 1783468800 (2026-07-08 era)
```

---

## 2. The coordinated wipe window (do these together, on my go)

Ping me when you're built and ready. Then, in lockstep with us:

**a. Stop AND disable your service so nothing auto-restarts it:**

```bash
sudo systemctl stop <your-service>
sudo systemctl disable <your-service>   # the last fresh relaunch FAILED because a
                                        # node auto-restarted and re-served the old chain
pgrep -af torus-node                    # must print NOTHING — confirm it's really gone
```

**b. Back up + wipe your chain data (KEEP your keystore):**

```bash
mv <your-data-dir> <your-data-dir>.bak-fresh-genesis-$(date +%s)
# your keystore/passphrase files are separate — do NOT touch them
```

**c. Tell me "down + wiped". Wait for my "all three down + wiped" confirmation
before starting.** This is the make-or-break step.

---

## 3. Start on the new genesis (still in the window)

Because your data dir is now empty, you **must** pass `--genesis` on this first
boot (a resume never needed it):

```bash
target/release/torus-node \
  --genesis testnet/genesis.json \
  --data-dir <your-data-dir> \
  --keystore <your-keystore> --passphrase-file <your-passphrase-file> \
  --retention-blocks 100000 \
  --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24,/ip4/84.32.108.220/udp/30333/quic-v1/p2p/12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze
# do NOT pass --native-gossip=false
```

If you launch via systemd, make sure the unit's `ExecStart` includes
`--genesis testnet/genesis.json` **for this first boot**, then re-enable it:
`sudo systemctl enable <your-service>`. (Once block 1 is committed, the
`--genesis` flag is ignored on later restarts — harmless to leave in.)

Keep your flags from before: `--retention-blocks 100000` (without it the DB grows
forever), both of our nodes as peers (above), and **not** `--native-gossip=false`.

---

## 4. Verify after start

```bash
curl -s localhost:9090/metrics | grep -c torus_state_root_compute_seconds   # >0 = new binary
```

The decisive check that the wipe worked: **your committed height starts near 0
and climbs.** If you see the old height (~500k+), your node re-synced the OLD
chain — stop, wipe again, and we hunt down whichever old peer is still serving
it. The chain resumes on its own once all three fresh nodes are up.

Ping me when it's up — I'll confirm all 10 markets are live from our side and
re-measure block speed.
