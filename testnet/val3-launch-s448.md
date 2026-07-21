# val3 launch instructions — 4-validator fresh genesis

Copy-paste for the val3 operator. **Supersedes `docs/val3-upgrade-instructions-s392.md`
entirely** — that doc targets a dead era (10 markets, `testnet/genesis.json`, and it
lists a peer we have since dropped).

You are back in the genesis validator set. Same keystore, same key, same peer id.

- consensus pubkey `0x99f813745e8347609dda017afbbe4a963921ed32df40e1d215c56bb9ddb3a59f`
- libp2p peer id  `12D3KooWLBPwqPS6WAeXvkPvfY1iac58F286hSTyUBnZSxcyxtdL`
- stake **2M** (the other three are 4M — deliberate, not a mistake)

**Do not generate a new key.** The node prints its pubkey at startup — check it
matches the line above before you leave it running.

---

## The set you're joining

| node | stake | reachability |
|---|---|---|
| seed `95.111.231.121` | 4M | listens :30333/udp |
| 18c `13.140.140.138` | 4M | listens :30333/udp |
| val2 `103.167.235.250` | 4M | listens :30333/udp |
| **you (val3)** | **2M** | **NAT — you dial out** |

Total power 14M, quorum `(14M*2/3)+1` = **9.3334M**.

**You are not on the critical path for launch.** seed+18c+val2 carry 12M, which
clears quorum without you, so the chain starts whether or not you're up. Join when
you're ready. Once you're in, the set tolerates any ONE node dropping.

Because you dial out, **nobody lists you in their `--p2p-peers`** — you reach them.

---

## 1. Build

```bash
cd <your torus-hyperbft checkout>
git fetch origin && git checkout perf/re-proof5 && git pull --ff-only origin perf/re-proof5
cargo build --release -p torus-node -p bench-throughput
```

Build this branch exactly. A binary from another branch can seed different genesis
state and fork off at block 1.

## 2. Build the genesis and VERIFY the hash

The genesis nodes boot is **generated**, not committed — it carries ~100k bulk bench
accounts and is too big for git. Everyone runs the same generator and must land on
the same bytes:

```bash
./testnet/gen-weighted-genesis.sh
sha256sum testnet/genesis-weighted-full.json
# MUST equal:
# 2c0d9cb52cd10c4996f2e4f43b29d54b97016e3ed91bb3984ce2725c66beb4ab
```

**If it doesn't match, stop and tell us.** A different hash means a different state
root, and your node will not be able to join.

Expect ~28MB, 100 markets, 4 validators. Run with the **default `NATIVE_AVAIL`** —
overriding it changes the hash.

> Note: `testnet/genesis.json` no longer exists. If any older instructions tell you
> to pass it, they are stale — the file was removed precisely because two genesis
> files produced two incompatible chains.

## 3. Fresh data dir — your old one is mis-keyed

Your existing data dir belongs to a previous chain and **cannot** be reused. Move it
aside; keep your keystore and passphrase, which live separately.

```bash
sudo systemctl stop <your-service>
sudo systemctl disable <your-service>   # nothing may auto-restart mid-window
pgrep -af torus-node                    # must print NOTHING

mv <your-data-dir> <your-data-dir>.bak-$(date +%s)
# do NOT touch your keystore / passphrase files
```

## 4. Launch

```bash
./target/release/torus-node \
  --genesis testnet/genesis-weighted-full.json \
  --data-dir <your-data-dir> \
  --keystore <your-keystore> --passphrase-file <your-passphrase-file> \
  --retention-blocks 100000 \
  --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24,/ip4/103.167.235.250/udp/30333/quic-v1/p2p/12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu,/ip4/13.140.140.138/udp/30333/quic-v1/p2p/12D3KooW9tETg7AgyXYvPzaQTFWi98g8msMMwx9GkezrDFPMQw7H \
  --rpc-addr 127.0.0.1:8545 \
  --metrics-addr 127.0.0.1:9090 \
  --log-level info,hotstuff_rs=warn
```

- Keep `--retention-blocks 100000` — without it the DB grows forever.
- Do **not** pass `--native-gossip=false`.
- **The peer list changed:** `84.32.108.220` (val1) was dropped from the set. If your
  old service file still lists it, remove it.
- RPC and metrics stay on loopback.
- If you relaunch via systemd, keep `--genesis` in `ExecStart` for the first boot,
  then `sudo systemctl enable <your-service>`. After block 1 commits the flag is
  ignored on later restarts — harmless to leave in.

## 5. Verify

```bash
curl -s localhost:9090/metrics | grep torus_block_height
curl -s -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"torus_getMarkets","params":[]}' \
  http://127.0.0.1:8545 | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["result"]),"markets")'
```

Expect **100 markets** and a height that **starts near 0 and climbs**.

If you see a large height instead (hundreds of thousands), your node re-synced an
OLD chain — stop, wipe again, and tell us so we can find whichever stale peer is
still serving it. That has bitten this fleet before.

Ping us when you're up and we'll confirm you're voting from our side.
