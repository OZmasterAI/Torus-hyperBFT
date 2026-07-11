# val2 launch instructions (friend2 / big-server) — S448 fresh genesis

You are being re-seated as a genesis validator. New chain, **3 equal validators**
(seed 4M · 18c 4M · **val2 4M**), chain_id **7778**. Your node also runs the
**archive** node + **indexer** + **explorer** (you have the strongest box).

Your identity (must match your keystore):
- pubkey  `0x537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c`
- peer-id `12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu`
- listen  `103.167.235.250 :30333/udp` (QUIC)

---

## ⚠️ Two things that WILL break the chain if wrong — read first

1. **Keystore must be the val2 key** (pubkey `537f7618…`). If you generate a new
   key it won't match genesis and you are NOT a validator. Use your existing val2
   keystore. (Verify at startup — see step 4.)

2. **30333/udp must be reachable inbound** (firewall + any NAT port-forward).
   This set requires **all 3 validators live** — quorum is 8,000,001 and any two
   4M nodes reach only 8,000,000, so if your node is unreachable the **whole
   chain halts**. Your node was firewalled/DROP before — confirm it's open now.

---

## 1. Sync code + build (branch `think-dev`, commit `5ff2974`)

```bash
cd <your torus-hyperbft checkout>
git fetch origin
git checkout think-dev
git pull --ff-only origin think-dev      # HEAD should be 5ff2974 or later
cargo build --release                    # builds torus-node, bench-throughput, torus-explorer
```
(This branch also includes the S447 native-DA body-starvation fix — you get it for free.)

## 2. Build the genesis deterministically and VERIFY the hash

The full genesis (100k funded accounts) is regenerated locally — do NOT hand-copy it.

```bash
./testnet/gen-weighted-genesis.sh
sha256sum testnet/genesis-weighted-full.json
```
The sha256 **must equal**:
```
858639c5abe079a9b7a202578b5857ba7c0ef23af44c6de608f1bcd8d2eaea15
```
If it differs, STOP and tell me — do not launch with a mismatched genesis.

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
- `--archive` = keep ALL history (no pruning). It is mutually exclusive with
  `--retention-blocks` — do **not** pass `--retention-blocks`.
- RPC bound to loopback `127.0.0.1:8545` on purpose (the explorer/indexer run on
  this same box and read it locally; the browser never hits it directly).
- Peers are seed (`95.111.231.121`) + 18c (`13.140.140.138`). val1 is dropped.

## 4. Confirm you actually joined

In the node log at startup, check the line that prints:
```
pubkey=537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c
```
and your peer-id `12D3KooWFSJjbJhn6…`. If the pubkey is anything else → wrong
keystore, stop. Then confirm the height is climbing (it advances only once all
3 nodes are up and peered — this is a coordinated start).

---

## 5. Indexer — `torus-explorer` (Rust, this repo)

Persistent SQLite indexer that backfills from genesis and serves a REST API.

```bash
./target/release/torus-explorer \
  --rpc-url http://127.0.0.1:8545 \
  --ws-url  ws://127.0.0.1:8545 \
  --db-path ./explorer.db \
  --listen  0.0.0.0:3001
```
- API on `:3001` (`/api/blocks`, `/api/txs/{hash}`, `/api/validators`,
  `/api/stats`, …). Backfills genesis→head, then follows new heads over WS.
- Since it's an archive node, the indexer can serve full history.

## 6. Explorer frontend — `torus-HBFT-explorer` (Next.js, separate repo)

```bash
git clone git@github.com:OZmasterAI/torus-HBFT-explorer.git   # branch: master
cd torus-HBFT-explorer
cp .env.example .env.local
```
Edit `.env.local`:
```
RPC_URL=http://127.0.0.1:8545
NEXT_PUBLIC_RPC_URL=http://127.0.0.1:8545
NEXT_PUBLIC_WS_URL=ws://127.0.0.1:8545
NEXT_PUBLIC_MOCK_DATA=false
# Leave blank to use the built-in Next.js API routes (query node RPC directly),
# OR point at the Rust indexer from step 5 to offload historical queries:
NEXT_PUBLIC_EXPLORER_API_URL=http://127.0.0.1:3001
```
Then:
```bash
pnpm install
pnpm build
pnpm start            # serves the UI on :3000
```
The frontend and the Rust indexer expose the **same `/api/*` routes**, so the
Rust indexer is a drop-in — set the URL to use it, or leave blank and the
frontend indexes off the node RPC on its own.

---

## Exposing publicly (optional)
- Validator RPC stays loopback (`127.0.0.1:8545`) — don't expose it.
- Expose only the **frontend `:3000`** (and optionally indexer API `:3001`) via
  your reverse proxy / firewall.

## Coordination
The chain is a coordinated fresh start — it produces blocks only once **seed +
18c + val2 are all up and peered**. Ping me when your node is built, genesis hash
verified, and 30333/udp confirmed open, and we'll bring all three up together.
