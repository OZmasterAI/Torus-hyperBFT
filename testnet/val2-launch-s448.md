# val2 launch instructions (friend2 / big-server) — S448 fresh genesis

You run: the **validator (archive mode)** + the **indexer** + the **block explorer**,
and expose the explorer to the web (you're the archive node / strongest box, so
everything history-related lives here).

Fresh chain, **4 validators**, chain_id **7778**: seed / 18c / you at **4M** each,
plus **val3** (NAT box, dials out) at **2M**. Total power 14M.

Your identity (your **existing** val2 keystore — same key as before):
- pubkey  `0x537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c`
- listen  `103.167.235.250 :30333/udp` (QUIC)

---

## ⚠️ Two things that halt the whole chain if wrong — read first

**1. Port `30333/udp` MUST be open inbound.** Quorum is `(14M*2/3)+1` = **9.3334M**, so
the chain survives ANY ONE validator being down (worst case 10M remains) but halts the
moment a **second** one drops. Your node alone going dark no longer stops the chain — but
it removes all slack, and you are also the archive/explorer host. You were firewalled
before — confirm:
```bash
sudo ufw allow 30333/udp && sudo ufw status | grep 30333
# or iptables:
sudo iptables -A INPUT -p udp --dport 30333 -j ACCEPT
```
If behind a router/NAT, also port-forward **UDP 30333** to this box.

**2. Use your existing val2 keystore** (pubkey `537f7618…`). Don't generate a new
key. The node prints its pubkey at startup — sanity-check it matches.

---

## 1. Build (branch `perf/re-proof5`)
```bash
cd <your torus-hyperbft checkout>
git fetch origin && git checkout perf/re-proof5 && git pull --ff-only origin perf/re-proof5
cargo build --release -p torus-node -p bench-throughput -p torus-explorer
```

## 2. Regenerate the genesis and VERIFY the hash
```bash
./testnet/gen-weighted-genesis.sh
sha256sum testnet/genesis-weighted-full.json
# MUST equal:
# 2c0d9cb52cd10c4996f2e4f43b29d54b97016e3ed91bb3984ce2725c66beb4ab
```
**If it doesn't match, stop and tell us.**

Expect ~28MB, 100,060 native + 100,052 EVM accounts, and **100 markets**. Run the
script with the **default `NATIVE_AVAIL`** — overriding it changes the hash.

**On the 100 markets:** ids 1–10 are the named pairs (BTC/ETH/SOL/…), 11–100 are
synthetic `S11…S100-USD`, pre-seeded so throughput sweeps can pick any market count
without a new genesis. This is the only cheap way to get them: markets do NOT
auto-create by trading — RPC ingress rejects unknown `market_id` against
`CF_NATIVE_MARKETS`, so post-genesis registration needs a **governance proposal per
market** (S432). Registering at genesis is nearly free: that CF is **off the
consensus state root**, and matching threads spawn per *market with orders in a
block*, not per registered market. Idle markets cost nothing.

**Do not read 100 as a recommended load.** Your own finding is that matching
oversubscribes past `markets ≈ cores` (64→128 regressed 90,598 → 22,800 fills/s).
Every validator executes every block, so the ceiling is set by the WEAKEST box, not
yours: seed has 6 cores allotted (`AllowedCPUs=0-5`), 18c has 18, you have 64. Sweep
market count upward and find the cliff rather than assuming your 64 transfers.

The script finds `bench-throughput` via `cargo metadata`, so a redirected
`[build] target-dir` in `~/.cargo/config.toml` is handled automatically — you do
not need the binary at `./target/release/`. If it still can't find it, the error
tells you exactly where it looked and what to build.

## 3. Stop the old node and WIPE its data dir (keep your keystore)

**This is the make-or-break step.** `torus-node` logs `"database already
initialized, skipping genesis"` (main.rs:502) and resumes whatever chain is in the
data dir — so launching on top of your old data silently ignores the new genesis
and puts you back on the DEAD chain. With your same keys you would then serve that
old chain to the fresh nodes and re-infect them.

```bash
sudo systemctl stop <your-service>
sudo systemctl disable <your-service>   # nothing may auto-restart mid-window
pgrep -af torus-node                    # must print NOTHING

mv ./data ./data.bak-$(date +%s)        # MOVE, don't delete — lets us roll back
# do NOT touch your keystore / passphrase files, they live separately
```

Tell us "down + wiped" and **wait for the go** before starting. One node still
serving the old chain undoes the whole relaunch.

## 4. Launch the validator (ARCHIVE mode) — set your keystore paths
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
- `--archive` keeps full history — do **not** also pass `--retention-blocks`.
- RPC/metrics stay on **loopback** — never expose them.
- Confirm in the log: `pubkey=537f761858…` and height climbing.

> Don't hard-start yet — coordinated launch (step 8). Steps 5–6 you can set up now.

---

## 5. Indexer (`torus-explorer`) — bound to loopback
Backfills genesis→head from your local archive RPC into SQLite, serves `/api` locally.
```bash
./target/release/torus-explorer \
  --rpc-url http://127.0.0.1:8545 \
  --ws-url  ws://127.0.0.1:8545 \
  --db-path ./explorer.db \
  --listen  127.0.0.1:3001
```

## 6. Block explorer UI (`torus-HBFT-explorer` — you have collaborator access)
```bash
git clone https://github.com/OZmasterAI/torus-HBFT-explorer
cd torus-HBFT-explorer
cp .env.example .env.local
```
Edit `.env.local`:
```
RPC_URL=http://127.0.0.1:8545
NEXT_PUBLIC_RPC_URL=http://127.0.0.1:8545
NEXT_PUBLIC_WS_URL=ws://127.0.0.1:8545
NEXT_PUBLIC_MOCK_DATA=false
NEXT_PUBLIC_EXPLORER_API_URL=
```
Build + run (bind to loopback; the reverse proxy is the only public door):
```bash
pnpm install
pnpm approve-builds --all      # REQUIRED — compiles keccak/sharp/etc; build fails otherwise
pnpm build
pnpm start -- -H 127.0.0.1 -p 3000
```

## 7. Expose ONLY the UI to the web, safely (Caddy = automatic HTTPS)
Everything above binds to loopback. Caddy is the single public entrypoint on 443,
reverse-proxying to the UI. The node RPC (8545), metrics (9090) and indexer (3001)
stay private.

```bash
# install caddy (https://caddyserver.com/docs/install) then:
sudo tee /etc/caddy/Caddyfile >/dev/null <<'CADDY'
# sslip.io gives you instant HTTPS on an IP-derived hostname — no DNS setup.
# (Or use your own domain pointed at 103.167.235.250.)
explorer.103-167-235-250.sslip.io {
    reverse_proxy 127.0.0.1:3000
}
CADDY
sudo systemctl restart caddy

# firewall: open only web + keep the validator port
sudo ufw allow 80/tcp && sudo ufw allow 443/tcp
# DO NOT open 8545 / 9090 / 3000 / 3001 to the internet.
```
Then browse: **https://explorer.103-167-235-250.sslip.io**

Notes:
- Page data loads via the UI's server-side `/api` proxy → your local archive node,
  so full history works. Browser-side live-WS + wallet-connect are disabled by the
  loopback RPC (fine for viewing); ping us if you want those exposed too (testnet
  RPC, acceptable) and we'll add a proxied `/ws` + `/rpc`.
- For durability run the indexer + `pnpm start` + caddy under systemd so they
  survive reboots/SSH logout.

---

## 8. Coordination
It's a coordinated fresh start — blocks only flow once **seed + 18c + you** are all up
and peered. Those three carry 12M of the 14M power, which clears the 9.3334M quorum on
their own, so the chain starts without waiting on **val3** — val3 is NAT/dials-out and
joins itself whenever it comes up. Do not list val3 in `--p2p-peers`; it dials you.

Ping us once: (1) built, (2) genesis sha matches `2c0d9cb52cd10c4996f2e4f43b29d54b97016e3ed91bb3984ce2725c66beb4ab`, and
(3) 30333/udp confirmed open. We start all three together, then you bring up the
indexer + UI.
