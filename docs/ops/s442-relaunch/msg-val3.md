# Message to val3 operator (copy-paste)

---

Hey — we're doing another coordinated fresh-genesis relaunch. New binary, fresh
chain, same key. The crawl your node kept hitting is exactly the bug this fixes (more
on that at the bottom).

Same drill as last time. Steps 1–4 you can do now; hold at step 5 until I say go.

1. **Stop your node and keep it down** (disable auto-restart so nothing revives it):
   ```bash
   sudo systemctl stop <your-service> && sudo systemctl disable <your-service>
   pgrep -af torus-node        # must print nothing
   ```

2. **Rename (do NOT delete) your data dir — keep your keystore:**
   ```bash
   mv <your-data-dir> <your-data-dir>.bak-<DATE>
   # keystore + passphrase files stay put — same key, same peer id
   ```

3. **Get the new build:**
   ```bash
   cd <your-torus-hyperbft-repo>
   git fetch origin && git checkout <COMMIT>
   git log --oneline -1        # must show <COMMIT>
   cargo build --release
   ```
   *(If I send you a prebuilt binary instead: `sha256sum torus-node` must equal
   `<SHA>`.)*

4. **Ping me "down + wiped + built."** Then wait for my "all four down — GO."

5. **On GO, launch (you dial out, NAT — pass the other three as peers):**
   ```bash
   target/release/torus-node \
     --genesis testnet/genesis.json \
     --data-dir <your-data-dir> \
     --keystore <your-keystore> --passphrase-file <your-passphrase-file> \
     --retention-blocks 100000 \
     --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24,/ip4/84.32.108.220/udp/30333/quic-v1/p2p/12D3KooWK5QYy1chfBmWpk6kfu9KTpq4rqfnRkXFuXmAc4DCPUze,/ip4/103.167.235.250/udp/30333/quic-v1/p2p/12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu
   # do NOT pass --native-gossip=false
   ```
   Then ping me — I'll confirm your height is climbing from 0 and the mesh is 4/4.

**About that crawl you kept seeing:** on the last chain your node was failing a chunk
of its leader turns and the whole chain would collapse to a ~1s-per-block cadence
whenever it did. That was a body-starvation / missing-body livelock — the leader
couldn't get block bodies moving on its turn, so the pipeline stalled. It wasn't your
box or your network; it was a real bug in the node. This new build (the think-dev
merge) is specifically the fix for that class of stall, which is the main reason we're
relaunching. You should see the chain hold a steady ~15–20 blk/s this time instead of
crawling to ~2.

*(Placeholders `<your-service>`, `<your-data-dir>`, `<your-keystore>`,
`<your-passphrase-file>`, `<DATE>`, `<COMMIT>`, `<SHA>` — fill your paths / the pinned
build.)*
