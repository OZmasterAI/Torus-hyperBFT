# New Validator Onboarding — welcome aboard

This is the first-time setup guide for a friend joining the Torus-hyperBFT testnet as
a validator. Work top to bottom. At the end you'll send us a **short list of four
public items** — and, critically, **never any private key**. We fold those into the
genesis, then we all relaunch together on a coordinated window.

> You do **not** launch on your own. Adding a validator changes the genesis hash, so
> everyone wipes and starts together. Get set up, send us your list, then wait for the
> coordinator's "go".

---

## 0. The 30-second version

1. Build the node.
2. Run `torus-node keygen` → this makes your **consensus keystore** and prints your
   **consensus pubkey**. The keystore stays on your box, forever, secret.
3. Make a plain **Ethereum account** (any wallet) → this is your **validator address**.
   The private key / seed stays with you, secret.
4. Send us the four **public** items in [§5 The send-us list](#5-the-send-us-list).
5. Wait for the relaunch window, then start with the genesis + peer list we hand you.

---

## 1. Hardware & prerequisites

- **CPU**: multi-core (validators do EVM execution + state-root hashing on the hot path).
  Your 6-core box is fine.
- **RAM**: 8+ GB recommended under load.
- **Disk**: SSD. With `--retention-blocks` the footprint is bounded to the retention
  window; without it (archive) it grows forever. We run pruned on testnet.
- **Network**: a **public, static IP** and the ability to open **inbound UDP 30333**.
  If you're behind NAT with no inbound, tell us — we can run you dial-out like val3, but
  a reachable IP is much better for the set.
- **Build deps**: Rust (rustup, edition 2021), `libclang-dev`, `pkg-config`, `libssl-dev`.
- Keep the machine's clock synced (NTP).

### Ports

| Port  | Proto | Purpose        | Exposure |
|-------|-------|----------------|----------|
| 30333 | UDP   | P2P (libp2p/QUIC) consensus + sync | **inbound open to the world** |
| 8545  | TCP   | JSON-RPC       | localhost only (or firewalled) |
| 9090  | TCP   | Prometheus metrics | localhost / your monitoring only |

Only **UDP 30333** needs to be publicly reachable.

---

## 2. Build the node

```bash
# clone + check out the branch in one step
git clone -b feat/precompile-0800-topn-gas \
  https://github.com/OZmasterAI/Torus-hyperBFT.git Torus-hyperBFT
cd Torus-hyperBFT
git rev-parse --short HEAD                     # confirm: 3f23c7d
cargo build --release -p torus-node
```

Binary lands at `target/release/torus-node`. Build from the **head of
`feat/precompile-0800-topn-gas`** — this is the current node binary. It already contains
all of the `perf/p3-throughput` and `perf/deep-book-storage` work (topn-gas is a
descendant of both), so it's the single branch to build; you do **not** need to build the
others. If the coordinator hands you a different exact commit for a relaunch, build that
one instead — a stale binary can seed a different genesis state and fork on block 1.

---

## 3. Generate your consensus keystore (Ed25519)

This is your **validator identity on the consensus layer**. It also determines your
libp2p peer id. One command:

```bash
target/release/torus-node keygen --output validator.keystore
```

- You'll be prompted for a **passphrase** — pick a strong one and store it *separately*
  from the keystore file.
- It writes `validator.keystore` (an encrypted JSON file) and prints:

  ```
  Keystore written to: validator.keystore
  Public key: 537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c
  ```

  That hex string is your **consensus pubkey**. You can re-read it any time from the
  keystore file's `pubkey_hex` field:

  ```bash
  grep pubkey_hex validator.keystore
  ```

- **Prefix it with `0x`** when you send it to us (genesis wants
  `0x537f76…`). The keystore stores it without the prefix — that's fine.

### Keystore safety (non-negotiable)

- `chmod 600 validator.keystore` and back it up somewhere safe. **Losing it = losing
  your validator identity**; there's no recovery.
- The keystore's *decrypted* contents are your **private signing key**. Never paste the
  keystore file, its ciphertext, or your passphrase into chat, email, or an issue.
- Never use `--validator-key <hex>` in production — it puts the raw key in `ps` output
  and shell history. Always use `--keystore`.

---

## 4. Generate your validator account (EVM address)

Separately from the consensus key, you need an ordinary **Ethereum account**. This
address is what **holds your stake and receives your block rewards**, and it's what you
sign with later to update commission, claim rewards, or unjail. It is *not* derived from
the keystore — it's a normal secp256k1 wallet.

Use whatever you already trust. Two easy options:

```bash
# Foundry (recommended):
cast wallet new
#   Successfully created new keypair.
#   Address:     0x0936adb653ffd8ef521029ffbf27bc1c815a7cbb
#   Private key: 0x....   <-- KEEP SECRET, never send

# or any wallet (MetaMask, hardware wallet, etc.) — we only need the address.
```

- Save the **private key / seed phrase** offline. Treat it like the keys to the money —
  because it is.
- We only ever need the **address** (the `0x…` 20-byte value).

---

## 5. The send-us list

Send us these **four public items** — a plain text message is fine:

| # | Item | Example | Where it comes from |
|---|------|---------|---------------------|
| 1 | **Validator address** (EVM account) | `0x0936adb653ffd8ef521029ffbf27bc1c815a7cbb` | §4 — the address only |
| 2 | **Consensus pubkey** (Ed25519, `0x`-prefixed) | `0x537f761858a3b44a23a0d63e16d9e642a86c44aac98a94d39501d2115528a63c` | §3 — `keygen` output / `pubkey_hex`, add `0x` |
| 3 | **Public IP + P2P port** | `203.0.113.7:30333` | your server's public IP; port is UDP 30333 |
| 4 | **(helpful, optional) libp2p peer id** | `12D3KooWFSJj…` | printed in your node log on first start; we can also derive it from #2 |

That's it. We derive your peer's full multiaddr from #2 + #3, assign your stake weight,
and place you in genesis.

### 🚫 NEVER send — not to us, not to anyone

- ❌ Your **keystore file**, its ciphertext, or its **passphrase** (that's your Ed25519
  private key).
- ❌ Your **EVM private key** or **seed phrase** (that controls your funds).

We will **never** ask for any of these. Anyone who does is trying to steal your
validator or your stake. Everything in the send-us list is public by design; everything
in this box is secret by design. If you're ever unsure whether an item is safe to share:
if it can *sign* on your behalf, it's secret.

---

## 6. What happens next (our side)

1. We add your entry to `testnet/genesis.json`:
   ```json
   {
     "address": "<your item #1>",
     "pubkey":  "<your item #2>",
     "stake":   "1000000000000000000000000",
     "commission_bps": 500
   }
   ```
   (Stake weight per the agreed topology — you're slated for the 1M placeholder slot;
   we'll confirm the exact number.)
2. The validator set and genesis timestamp change, so the **genesis hash changes** →
   this is a **coordinated fresh-genesis relaunch**, not a resume.
3. We push the relaunch commit and give you: the exact commit hash, the final
   `genesis.json`, and the **peer list** (everyone's multiaddr except your own).

---

## 7. First launch (in the coordinated window only)

Don't start until the coordinator confirms **all nodes are down + wiped**. Then, on a
fresh/empty data dir you must pass `--genesis` on this first boot:

```bash
target/release/torus-node \
  --genesis testnet/genesis.json \
  --data-dir ./data \
  --keystore validator.keystore --passphrase-file ./passphrase \
  --retention-blocks 100000 \
  --p2p-listen /ip4/<YOUR-PUBLIC-IP>/udp/30333/quic-v1 \
  --p2p-peers <ALL PEER MULTIADDRS EXCEPT YOUR OWN — comma-separated> \
  --rpc-addr 127.0.0.1:8545 \
  --metrics-addr 127.0.0.1:9090
```

- `--passphrase-file` (a `chmod 600` file holding your passphrase) lets systemd start you
  unattended. Interactive `--keystore` alone will prompt.
- **Open inbound UDP 30333** on your public IP so the others can dial you.
- Do **not** pass `--native-gossip=false`.
- The peer multiaddrs look like:
  `/ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf…`

---

## 8. Verify you're up

```bash
# node is serving metrics:
curl -s localhost:9090/metrics | grep -c torus_native_actions_processed_total   # >0 = up

# committed height starts near 0 and CLIMBS — this proves you're on the NEW chain:
curl -s localhost:9090/metrics | grep torus_block_height
```

- If height jumps to a large old value (~130k+), you re-synced the **old** chain — stop,
  wipe your data dir again, and tell the coordinator.
- Your log prints your **peer id** on startup — grab it for send-us item #4 if you
  haven't already.
- Ping the coordinator when you're up; we confirm all validators are voting.

---

## 9. Ongoing operator checklist

- [ ] Keystore backed up; passphrase stored separately.
- [ ] EVM private key / seed stored offline.
- [ ] Inbound UDP 30333 open; RPC/metrics ports firewalled to localhost.
- [ ] `--retention-blocks` set (pruned) unless you deliberately want archive.
- [ ] Clock synced (NTP).
- [ ] Grafana dashboards imported (`monitoring/dashboards/`) — see
      [monitoring-setup.md](monitoring-setup.md).
- [ ] Watch `torus_block_height` (climbing), `torus_peers_connected` (≥3),
      `torus_consensus_view` (advancing).

Welcome to the set. See [validator-guide.md](validator-guide.md) for key rotation,
commission, jailing/unjail, and self-delegation details once you're live.
