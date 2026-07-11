# val2 launch instructions (friend2 / big-server) — S448 fresh genesis

You are being re-seated as a genesis validator on a fresh chain. **3 equal
validators** (seed 4M · 18c 4M · **val2 4M**), chain_id **7778**. You run the
**validator in archive mode**. (The indexer + block explorer are hosted on our
18c box — you do **not** need to run those.)

Your identity (your **existing** val2 keystore — same key as before):
- pubkey  `0x537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c`
- peer-id `12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu`
- listen  `103.167.235.250 :30333/udp` (QUIC)

---

## ⚠️ Read first — either of these breaks the whole chain

**1. Port 30333/udp MUST be reachable inbound.** This is the big one. The set
requires **all 3 validators live** — quorum is 8,000,001 and any two 4M nodes
reach only 8,000,000, so if your node is unreachable the **entire chain halts**.
Your node was firewalled/DROP before, so please confirm it's open now:

```bash
# host firewall (ufw example) — allow the QUIC/UDP port
sudo ufw allow 30333/udp
sudo ufw status | grep 30333

# iptables example (if not using ufw)
sudo iptables -A INPUT -p udp --dport 30333 -j ACCEPT
```
If your box is behind a router/NAT, also **port-forward UDP 30333 → this host**.
(We'll confirm the peer link lights up from the seed/18c side when you start.)

**2. Use your existing val2 keystore** (the one for pubkey `537f7618…`). The node
prints its pubkey at startup — sanity-check it matches (see step 4). Don't run
`keygen`; a new key would not be in genesis.

---

## 1. Sync code + build (branch `think-dev`)

```bash
cd <your torus-hyperbft checkout>
git fetch origin
git checkout think-dev
git pull --ff-only origin think-dev
cargo build --release -p torus-node          # also pulls in the S447 native-DA fix
```

## 2. Build the genesis deterministically and VERIFY the hash

The full genesis (100k funded accounts) is regenerated locally — do NOT hand-copy it.

```bash
cargo build --release -p bench-throughput    # gen-genesis needs this binary
./testnet/gen-weighted-genesis.sh
sha256sum testnet/genesis-weighted-full.json
```
The sha256 **must equal**:
```
858639c5abe079a9b7a202578b5857ba7c0ef23af44c6de608f1bcd8d2eaea15
```
If it differs, STOP and tell us — do not launch with a mismatched genesis.

## 3. Launch the validator (ARCHIVE mode)

Replace the two keystore paths with your actual val2 keystore/passphrase.

```bash
./target/release/torus-node \
  --genesis testnet/genesis-weighted-full.json \
  --keystore /path/to/val2.keystore \
  --passphrase-file /path/to/val2.passphrase \
  --data-dir ./data \
  --archive \
  --p2p-listen /ip4/0.0.0.0/udp/30333/quic-v1 \
  --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24,/ip4/13.140.140.138/udp/30333/quic-v1/p2p/12D3KooW9tETg7AgyXYvPzaQTFWi98g8msMMwx9GkezrDFPMQw7H \
  --rpc-addr 127.0.0.1:8545 \
  --metrics-addr 127.0.0.1:9090 \
  --log-level info,hotstuff_rs=warn
```
Notes:
- `--archive` = keep ALL history (no pruning). Do **not** also pass
  `--retention-blocks` (mutually exclusive). This makes you the full-history node.
- Peers are seed (`95.111.231.121`) + 18c (`13.140.140.138`). val1 is dropped.
- RPC on loopback is fine — nothing external needs it on your box.
- Consider running it under systemd / `nohup` so it survives your SSH session.

## 4. Confirm you actually joined

In the node log at startup, check for:
```
pubkey=537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c
```
and your peer-id `12D3KooWFSJjbJhn6…`. If the pubkey differs → wrong keystore,
stop. Then confirm the block height is climbing.

---

## Coordination
The chain is a coordinated fresh start — it only produces blocks once **seed +
18c + val2 are all up and peered**. Ping us when:
1. your node is built,
2. the genesis sha matches `858639c5…`, and
3. 30333/udp is confirmed open,

and we'll bring all three up together.
