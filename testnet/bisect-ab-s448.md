# Bisect A/B runbook — S448 val2 flood-collapse (throughput ceiling: regression vs. flood-shape)

> **HISTORICAL — superseded, do not follow verbatim.** This documents a completed
> investigation pinned to commits `6728150` (A) and `5ff2974` (B) and to the era whose
> genesis sha256 was `858639c5…`. Those values are correct *for what this runbook
> describes* and are deliberately left unchanged.
>
> The current testnet is a different era: 4 validators (seed 4M / 18c 4M / val2 4M /
> val3 2M), genesis sha256 `624c8a67269dca117da54de30175b7b903fb5ab068d90692b8408fcdbd64dc61`,
> built from branch `perf/re-proof5`. For a live run use `val2-launch-s448.md` and
> `seed-relaunch-cmd-s448.txt`; reuse only the *method* below, never its constants.

**Goal:** determine whether the native-order flood collapse val2 saw on S448 is a **code
regression** (something between the 207k S444 run and current lowered the ceiling) or just a
**harder flood-shape** (same ceiling, pushed past it).

**Method:** two coordinated 3-box TESTNET runs, identical grid, fresh genesis each.
Devnet is excluded (localhost gossip never saturates the send queue; devnet is CPU-bound
~65-70k; devnet gave false collapse signals before — S419 harness artifacts).

| Run | Commit | Notes |
|-----|--------|-------|
| **A** | `6728150` (S444) | the 207k anchor, **pre-F-2** (no `783f4d0` action-hash change) |
| **B** | `5ff2974` (S448) | current, the one that collapses |

**Why all 3 + fresh genesis each:** `783f4d0` (F-2, signature-bound action hash) is IN B, NOT in A
→ A and B are **consensus-incompatible**, cannot share a chain. Each run needs its own fresh genesis,
and all three validators must be on the **same** binary for a given run (single-variable).

---

## Pre-checks (DONE 2026-07-11)
- Genesis matches all 3 boxes: `858639c5abe079a9b7a202578b5857ba7c0ef23af44c6de608f1bcd8d2eaea15`
- `6728150` (A) and `5ff2974` (B) present locally on seed **and** 18c (18c fetched)
- 18c disk 207G free; seed box 8c/23G SHARED (build competes w/ framework — build in a quiet window)
- F-2 ancestry confirmed: `783f4d0` IN 5ff2974 & d532720, NOT in 6728150

## Identical grid (BOTH runs — val2's collapse grid, single-variable)
Run on val2's box against his RPC:
```
MARKETS_LIST="1 2 5 10" BATCH_LIST="400 1000" SENDERS_LIST="100" SIGN_LIST="session" \
  ./testnet/bench-native-orders-grid.sh    # SENDER_OFFSET=60 RPC=:28545 METRICS=:29090
```

## Capture per run (all 3 boxes)
- peak + sustained orders/s, inclusion % (`included ≈ submitted`?)
- `missing_rej` / MISSING-body pull counts, and whether pulls RECOVER
- `Send Queue full` line density (val2)
- whether committed blocks hit `actions=0`
- push each box's flood-window log (gzip) to the `torus-hyperbft-data` repo `node-logs/`

## Verdict
- **A holds (~207k) & B collapses** → REAL regression → bisect the 3 commits between them
  (`783f4d0` F-2, `f31acf0` native-da pull-buffer, `e969c84` docs).
- **Both collapse the same** → flood-shape, NOT a regression → go straight to the pre-spread
  backpressure fix (per-peer send caps / max_transmit / byte-budget the proposal to what's
  already pre-spread).

---

## Per-box commands

### SEED (this box, 95.111.231.121) — build + relaunch
```bash
# 1. stop current
systemctl --user stop torus-seed-validator

# 2. build target binary (COMMIT = 6728150 for A, 5ff2974 for B)
cd /home/crab/projects/Torus-hyperBFT
git stash -u                       # park untracked bench/log noise if needed
git checkout <COMMIT>
cargo build --release -p torus-node -p bench-throughput
cp target/release/torus-node /home/crab/torus-seed-live/torus-node

# 3. FRESH genesis for this run (regen with THIS binary; A and B genesis hashes DIFFER — expected)
#    all 3 boxes must end up with the SAME genesis sha for the run — verify before launch.
./testnet/gen-weighted-genesis.sh              # (confirm this script exists at <COMMIT>; A pre-dates some tooling)
sha256sum testnet/genesis-weighted-full.json
cp testnet/genesis-weighted-full.json /home/crab/torus-seed-live/genesis.json

# 4. WIPE chain state for a fresh chain (MOVE, don't delete — lets us restore current B chain)
mv /home/crab/projects/Torus-hyperBFT/testnet/data /home/crab/projects/Torus-hyperBFT/testnet/data.bisect-bak-$(date +%s)

# 5. relaunch (change the log filename per run: seed-bisect-A.log / seed-bisect-B.log)
systemd-run --user --unit=torus-seed-validator \
  --working-directory=/home/crab/projects/Torus-hyperBFT \
  -p MemoryMax=14G -p AllowedCPUs=0-5 -p Restart=always -p RestartSec=10 \
  -p StandardOutput=append:/home/crab/torus-seed-live/seed-bisect-<A|B>.log \
  -p StandardError=append:/home/crab/torus-seed-live/seed-bisect-<A|B>.log \
  /home/crab/torus-seed-live/torus-node \
    --genesis /home/crab/torus-seed-live/genesis.json \
    --data-dir /home/crab/projects/Torus-hyperBFT/testnet/data \
    --keystore /home/crab/projects/Torus-hyperBFT/seed.keystore \
    --passphrase-file /home/crab/projects/Torus-hyperBFT/seed.passphrase \
    --retention-blocks 100000 \
    --p2p-listen /ip4/0.0.0.0/udp/30333/quic-v1 \
    --p2p-peers /ip4/103.167.235.250/udp/30333/quic-v1/p2p/12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu,/ip4/13.140.140.138/udp/30333/quic-v1/p2p/12D3KooW9tETg7AgyXYvPzaQTFWi98g8msMMwx9GkezrDFPMQw7H \
    --rpc-addr 127.0.0.1:8545 --metrics-addr 127.0.0.1:9090 \
    --log-level info,hotstuff_rs=warn,torus_node=error,torus_genesis=warn
```

### 18c (val4, 13.140.140.138) — build + relaunch
```bash
ssh 18c
cd /home/18c/torus-hyperbft
git fetch origin think-dev
git checkout <COMMIT>                          # 6728150 (A) or 5ff2974 (B)
cargo build --release -p torus-node -p bench-throughput
# fresh genesis: copy the SAME genesis-weighted-full.json the seed produced (verify sha matches)
mv data data.bisect-bak-$(date +%s)            # fresh chain
./target/release/torus-node \
  --genesis /home/18c/torus-hyperbft/testnet/genesis.json \
  --keystore /home/18c/.torus-hbft/validator.keystore \
  --passphrase-file /home/18c/.torus-hbft/validator.passphrase \
  --data-dir /home/18c/torus-hyperbft/data \
  --p2p-listen /ip4/13.140.140.138/udp/30333/quic-v1 \
  --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24,/ip4/103.167.235.250/udp/30333/quic-v1/p2p/12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu \
  --rpc-addr 127.0.0.1:8555 --metrics-addr 127.0.0.1:9090 \
  --retention-blocks 100000 \
  --log-level info,hotstuff_rs=warn,torus_node=error \
  >> /home/18c/torus-bisect-<A|B>.log 2>&1 &
```
> NOTE: 18c has Anvil on :8545 — torus RPC stays on **:8555** (leave as-is).

### val2 (frend2, 103.167.235.250) — his launch doc (`testnet/val2-launch-s448.md`), swap COMMIT + fresh genesis + fresh data, then run the grid above.

---

## Coordination / GO checklist
1. All 3: `git checkout <COMMIT>` + build complete
2. Genesis regenerated with `<COMMIT>` binary, **sha identical on all 3** (A and B differ from each other — fine)
3. `30333/udp` open on val2; keystores confirmed
4. Chain data moved aside on all 3 (fresh chain)
5. Coordinated start (all 3 together)
6. val2 runs identical grid; all 3 capture + push logs
7. Repeat for the other commit

## Notes on ordering ("B first")
- We are **on B-code now**, but the *current chain* is height ~291k with history — NOT a fresh-genesis B.
- Cheapest path: the current chain already IS a B data point (val2's collapse + our seed/18c
  flood-window logs are captured). Option to skip a fresh B and go straight to a fresh **A** run,
  then compare A-fresh vs the B-evidence-in-hand.
- Cleaner path: fresh genesis for BOTH so height/history is not a confound. Recommended if the
  A run is ambiguous.
