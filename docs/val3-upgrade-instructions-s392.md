# val3 upgrade instructions — S403 (code tip c949c7b)

Copy-paste for the val3 operator (3rd validator, self-hosted). Supersedes the
S395/69f0a99 instructions — this replaces them entirely. Sent 2026-07-05.

---

New build ready: branch `sprint/blockspeed-orders-s395`, commit `c949c7b`
(also pushed to `fix/hotstuff-idle-cpu-spin` if that's what your clone
tracks). Why it matters: block time has degraded from ~270ms to ~450ms and we
traced it to your validator's leg — every quorum certificate waits on your
vote, and proposals are slow whenever you lead. Our two nodes are already on
this exact code. Your upgrade + a clean restart should give the whole chain
its speed back.

Your node can stay up while you build.

**1. Update + rebuild:**

```bash
cd <your-torus-hyperbft-repo>
git fetch origin sprint/blockspeed-orders-s395
git checkout sprint/blockspeed-orders-s395
git pull --ff-only origin sprint/blockspeed-orders-s395
git log --oneline -1   # must print: c949c7b Reapply "fix(network): refuse dials to non-global addresses..."
cargo build --release
```

Since 69f0a99 this adds: a dial filter that stops wasted dials to dead/private
addresses, kademlia address-book hygiene, and telemetry. All wire-compatible,
no config migration, defaults unchanged.

**2. Do NOT touch your keys or data directory.** Same keystore, same data
dir. Your libp2p peer id is derived from your validator key — if you
regenerate anything we're worse off.

**3. Set the DA threshold env var** (skip if already done last time):

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

- `--retention-blocks 100000` — without it your database grows forever and
  your node gets slower with height (this is likely a big part of the current
  slowdown)
- add **both** of our nodes as peers:

  ```
  --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24,/ip4/84.32.108.220/udp/30333/quic-v1/p2p/12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze
  ```

- make sure you're NOT passing `--native-gossip=false`

**5. Restart — IMPORTANT, do NOT use `systemctl restart` or a quick
kill+start.** A fast in-place restart races the dying connection and can
leave your node connected but gossip-mute, which stalls the whole chain
(this has bitten us three times). Instead:

```bash
sudo systemctl stop <your-service>     # or however you normally stop it
sleep 20
sudo systemctl start <your-service>    # or your normal start command
```

If the process ignores the stop and hangs (no new log lines), `kill -9` it —
then still wait 20 seconds before starting.

**6. Verify after restart:**

```bash
curl -s localhost:9090/metrics | grep -c torus_view_duration_seconds   # >0 = new binary
# log should show heights climbing past ~1,065,000 within a minute
# if you see NoPeersSubscribedToTopic spam: stop, wait 20s, start again
```

Ping me when it's up — I'll verify from our side and re-measure block speed.
