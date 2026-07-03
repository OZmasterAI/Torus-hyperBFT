# val3 upgrade instructions — S392 (b96f058)

Copy-paste for the val3 operator (3rd validator, self-hosted). Supersedes the
earlier fa7791d instructions. Sent 2026-07-03.

---

Big upgrade ready on branch `fix/hotstuff-idle-cpu-spin`. Your node can stay
up while you build — but **don't restart until I say go** (chain is halted;
we bring our two up first, then you rejoin).

**1. Update + rebuild:**

```bash
cd <your-torus-hyperbft-repo>
git fetch origin fix/hotstuff-idle-cpu-spin
git checkout fix/hotstuff-idle-cpu-spin
git pull --ff-only origin fix/hotstuff-idle-cpu-spin
git log --oneline -1        # must print: b96f058
cargo build --release
```

**2. Set the DA threshold env var** (you never confirmed this one — it
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

**3. Check your start flags** (still missing last time):

- `--retention-blocks 100000` — without it your disk grows forever
- add **both** of our nodes as peers (right now you only reach the seed —
  that's why your node pulls every block body instead of getting action
  gossip):

  ```
  --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu,/ip4/84.32.108.220/udp/30333/quic-v1/p2p/12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze
  ```

- make sure you're NOT passing `--native-gossip=false`

**4. Send me these** so we can fix the gossip gap from our side too:

- your public IP (and confirm UDP 30333 is open inbound on your firewall)
- `curl -s localhost:9090/metrics | grep torus_native_gossip`
- `mtr -rz -c 20 95.111.231.121` and `mtr -rz -c 20 84.32.108.220`
  (or `ping -c 20` each if no mtr)

**5. Wait for my "go"**, then restart into the new binary. After restart,
send me the `torus_native_gossip` metrics again once the chain is moving so
we can confirm gossip is actually flowing this time.
