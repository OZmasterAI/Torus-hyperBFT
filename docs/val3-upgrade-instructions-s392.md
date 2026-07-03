# val3 upgrade instructions — S395 (code tip 69f0a99)

Copy-paste for the val3 operator (3rd validator, self-hosted). Supersedes the
S392/b96f058 instructions (never run) and the earlier fa7791d ones — this
replaces them entirely. Sent 2026-07-03.

---

New build ready on a new branch: `sprint/blockspeed-orders-s395`. Why this
one matters: with three equal-stake validators the chain can only cross an
epoch boundary (every 100th view) when **all three** of us vote — so right
now it's parked, waiting for you. Our two nodes are already running this
exact code. The moment you're up on it, the chain resumes on its own.

Your node can stay up while you build.

**1. Update + rebuild:**

```bash
cd <your-torus-hyperbft-repo>
git fetch origin sprint/blockspeed-orders-s395
git checkout sprint/blockspeed-orders-s395
git log --oneline -8 | grep 69f0a99   # must match — last CODE commit (anything above it is docs-only)
cargo build --release
```

Since b96f058 this adds: a pacemaker fix so the chain un-parks after view
jumps (this is the one that matters for the current halt), per-block hot-path
cuts, and order-book persistence fixes. All wire-compatible, no config
migration, defaults unchanged.

**2. Do NOT touch your keys or data directory.** Same keystore, same data
dir. Your libp2p peer id is derived from your validator key, and our dials to
you currently fail with "Unexpected peer ID" — a clean restart on this build
with your existing keystore is exactly what should fix that. If you
regenerate anything we're worse off.

**3. Set the DA threshold env var** (you never confirmed this one — it
matters for block dissemination):

- systemd: `sudo systemctl edit <your-service>` and add:

  ```ini
  [Service]
  Environment=TORUS_HASH_ONLY_PUSH_THRESHOLD=6000000
  ```

- or if you start it by hand, prefix the command:

  ```bash
  TORUS_HASH_ONLY_PUSH_THRESHOLD=6000000 ./target/release/torus-node ...
  ```

**4. Check your start flags** (still missing last time):

- `--retention-blocks 100000` — without it your disk grows forever
- add **both** of our nodes as peers (right now you only reach the seed —
  that's why your node pulls every block body instead of getting action
  gossip):

  ```
  --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24,/ip4/84.32.108.220/udp/30333/quic-v1/p2p/12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze
  ```

- make sure you're NOT passing `--native-gossip=false`

**5. Restart into the new binary as soon as the build is done** — no waiting
for a "go" this time; we're already up and the chain is waiting on you. Just
ping me right before you restart so I can watch it come back.

**6. Send me these after restart** (so we can confirm the peer-id fix and the
gossip gap from our side):

- the first ~30 lines of your node's startup log (they include your peer id
  and listen addresses)
- your public IP (and confirm UDP 30333 is open inbound on your firewall)
- `curl -s localhost:9090/metrics | grep torus_native_gossip`
- `mtr -rz -c 20 95.111.231.121` and `mtr -rz -c 20 84.32.108.220`
  (or `ping -c 20` each if no mtr)
