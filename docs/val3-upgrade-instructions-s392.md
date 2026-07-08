# val3 upgrade instructions — S433 (code tip 928b700)

Copy-paste for the val3 operator (3rd validator, self-hosted). Supersedes the
S420/6e03294 instructions — this replaces them entirely. Prepared 2026-07-08.

---

New build ready: branch `integration/bs4a-livelock-s428`, commit `928b700`
(pushed to origin). **Unlike last time, the chain is NOT halted — it's live
and producing blocks right now.** So please **build now, but do NOT restart
your node yet.** All three nodes move to this exact commit *together* in a
short coordinated window; running the new binary against the old fleet (or
vice-versa) is not supported. We'll pick the restart window with you once
you're actively at your machine — ping me and we'll do our side in lockstep.

What's in it since your last upgrade (6e03294) — all consensus robustness,
no config or key changes:

- **Tip-fork livelock fix (S426/S428)** — a node that missed a QC'd block at
  the tip could pin the commit frontier forever. Now healed via by-hash
  justify recovery + gap-tolerant block-sync serving. This is the big one.
- **Off-thread DA body recovery (BS-4a/4b)** — failed views recover missing
  native-DA block bodies on a background worker instead of blocking consensus,
  with one event-driven mid-budget re-fetch. Faster, non-stalling recovery.
- **Header-first body-starvation heal (S432)** — a validator that received a
  header but not its body now *proactively fetches* the missing body instead
  of stalling behind it.
- **Wrong-set PC discard (S430)** — locally-collected phase-certificates from
  the wrong validator set are discarded during validator-set transitions
  (safety hardening; inert at our current fixed set, correct for the future).
- Telemetry: new `torus_state_root_compute_seconds` metric at all root-compute
  sites; native-DA recovery counters.

No key or config migration. No env-var changes this time. You can build now,
well before the restart window.

**1. Update + rebuild (safe to do now, node keeps running):**

```bash
cd <your-torus-hyperbft-repo>
git fetch origin integration/bs4a-livelock-s428
git checkout 928b700   # build this EXACT code commit so all three of us are on identical code.
                       # (the branch tip has a docs-only commit above 928b700 that changes no code;
                       #  checking out the commit directly keeps your `git log -1` matching this doc.)
git log --oneline -1   # must print: 928b700 test(hotstuff): fix stale justify_block_livelock split-precondition (S433)
cargo build --release
```

Building does not touch your running node — it just produces the new binary
at `target/release/torus-node`, ready for the restart window.

**2. Do NOT touch your keys or data directory.** Same keystore, same data
dir. Your libp2p peer id is derived from your validator key — if you
regenerate anything we're worse off.

**3. Check your start flags** (same as last time — keep these):

- `--retention-blocks 100000` — without it your database grows forever and
  your node gets slower with height
- keep **both** of our nodes as peers:

  ```
  --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24,/ip4/84.32.108.220/udp/30333/quic-v1/p2p/12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze
  ```

- make sure you're NOT passing `--native-gossip=false`

**4. Restart protocol — ONLY during the coordinated window, and do NOT use
`systemctl restart` or a quick kill+start.** A fast in-place restart races the
dying connection and can leave your node connected but gossip-mute, which
stalls the whole chain (this has bitten us; the watchdog helps, but don't lean
on it):

```bash
sudo systemctl stop <your-service>     # or however you normally stop it
sleep 20
sudo systemctl start <your-service>    # or your normal start command
```

If the process ignores the stop and hangs (no new log lines), `kill -9` it —
then still wait 20 seconds before starting.

Because the chain is live, the restart window is a brief deliberate halt: when
you're ready, ping me, we stop our two nodes, you stop yours, everyone starts
on `928b700`, and the chain resumes on its own once all three are up. Timing
within the window is relaxed — a node on the new binary just idles until the
other two join.

**5. Verify after restart:**

```bash
curl -s localhost:9090/metrics | grep -c torus_state_root_compute_seconds   # >0 = new binary
# once all three nodes are up, your committed height should climb steadily
# if you see NoPeersSubscribedToTopic spam: stop, wait 20s, start again
```

Ping me when it's up — I'll verify from our side and re-measure block speed.
