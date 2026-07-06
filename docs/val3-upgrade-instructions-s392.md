# val3 upgrade instructions — S420 (code tip 6e03294)

Copy-paste for the val3 operator (3rd validator, self-hosted). Supersedes the
S403/c949c7b instructions — this replaces them entirely. Prepared 2026-07-06.

---

New build ready: branch `sprint/blockspeed-orders-s395`, commit `6e03294`
(pushed to origin). The chain is currently **halted** at height 1,114,439 —
we stopped it deliberately for this upgrade, so nothing is waiting on you
being fast, but the network can't restart without you (3-of-3 quorum). All
three nodes move to this exact commit together; running the old binary
against the new fleet is not supported.

What's in it since your last upgrade (c949c7b):

- **Root-cause fix for the block-time slowdown** — leader selection cost grew
  with chain height (this was the ~270ms → ~450ms drag). Proposals no longer
  slow down as the chain grows.
- **Mesh watchdog + explicit validator peering** — detects and heals the
  gossip-mute wedge that used to stall the chain after restarts. This only
  works when all validators run it, which is why we upgrade in lockstep.
- **Exec/throughput work** — batched order placement, background trade-history
  writes, balance caching. Also new message-size gates with saner defaults.
- Telemetry and an RPC safety fix.

No key or config migration. You can build now, before the restart window.

**1. Update + rebuild:**

```bash
cd <your-torus-hyperbft-repo>
git fetch origin sprint/blockspeed-orders-s395
git checkout sprint/blockspeed-orders-s395
git pull --ff-only origin sprint/blockspeed-orders-s395
git log --oneline -1   # must print: 6e03294 chore(test): nextest config — bound the suite, cap hanging integ tests
cargo build --release
```

**2. Do NOT touch your keys or data directory.** Same keystore, same data
dir. Your libp2p peer id is derived from your validator key — if you
regenerate anything we're worse off.

**3. REMOVE the DA threshold env var** (we previously asked you to set it —
that advice is withdrawn; measurements showed the compiled default is faster
and the new build ignores oversized values anyway):

- systemd: `sudo systemctl edit <your-service>` and delete the
  `Environment=TORUS_HASH_ONLY_PUSH_THRESHOLD=6000000` line (leave the
  `[Service]` section empty or remove the override), then
  `sudo systemctl daemon-reload`
- or if you start it by hand, just drop the
  `TORUS_HASH_ONLY_PUSH_THRESHOLD=6000000` prefix from your command

**4. Check your start flags** (same as last time — keep these):

- `--retention-blocks 100000` — without it your database grows forever and
  your node gets slower with height
- keep **both** of our nodes as peers:

  ```
  --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24,/ip4/84.32.108.220/udp/30333/quic-v1/p2p/12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze
  ```

- make sure you're NOT passing `--native-gossip=false`

**5. Restart protocol — IMPORTANT, do NOT use `systemctl restart` or a quick
kill+start.** A fast in-place restart races the dying connection and can
leave your node connected but gossip-mute, which stalls the whole chain
(this has bitten us three times; the new watchdog helps, but don't lean on
it):

```bash
sudo systemctl stop <your-service>     # or however you normally stop it
sleep 20
sudo systemctl start <your-service>    # or your normal start command
```

If the process ignores the stop and hangs (no new log lines), `kill -9` it —
then still wait 20 seconds before starting.

Since the chain is halted, timing doesn't need to be exact: bring your node
up on the new binary whenever you're ready and it will idle until all three
of us are up, then the chain resumes on its own. Ping me when you start it
and we'll bring up our side.

**6. Verify after restart:**

```bash
curl -s localhost:9090/metrics | grep -c torus_view_duration_seconds   # >0 = new binary
# once all three nodes are up, heights should climb past 1,114,439 within a minute
# if you see NoPeersSubscribedToTopic spam: stop, wait 20s, start again
```

Ping me when it's up — I'll verify from our side and re-measure block speed.
