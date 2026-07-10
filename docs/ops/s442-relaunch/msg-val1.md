# Message to val1 operator (copy-paste)

---

Hey — coordinated fresh-genesis relaunch again. New binary, fresh chain, same key.
Steps 1–4 now; hold at step 5 until I say go.

**Note: I have SSH to your box (`-p 58331`), so if it's easier I can just run all of
this for you — say the word and I'll drive it and only ping you to confirm. Otherwise,
the copy-paste is below.**

1. **Stop your node and keep it down** (disable auto-restart):
   ```bash
   sudo systemctl stop <your-service> && sudo systemctl disable <your-service>
   pgrep -af torus-node        # must print nothing
   ```

2. **Rename (do NOT delete) your data dir — keep your keystore:**
   ```bash
   mv <your-data-dir> <your-data-dir>.bak-<DATE>
   # keystore + passphrase files stay put — same key, same peer id 12D3KooWK5QY…
   ```

3. **Get the new build:**
   ```bash
   cd <your-torus-hyperbft-repo>
   git fetch origin && git checkout <COMMIT>
   git log --oneline -1        # must show <COMMIT>
   cargo build --release
   ```
   *(Or `sha256sum torus-node` must equal `<SHA>` if I hand you a prebuilt binary.)*

4. **Ping me "down + wiped + built"** (or just tell me to do it via SSH). Then wait for
   my "all four down — GO."

5. **On GO, launch (peers = seed + friend2; val3 dials you):**
   ```bash
   target/release/torus-node \
     --genesis testnet/genesis.json \
     --data-dir <your-data-dir> \
     --keystore <your-keystore> --passphrase-file <your-passphrase-file> \
     --retention-blocks 100000 \
     --p2p-listen /ip4/84.32.108.220/udp/30333/quic-v1 \
     --p2p-peers /ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24,/ip4/103.167.235.250/udp/30333/quic-v1/p2p/12D3KooWFSJjbJhn6H92v7FbPGWXoCWLQFhH4koG56mS7Ps7KGPu
   # do NOT pass --native-gossip=false
   ```
   Then ping me — I'll confirm your height is climbing from 0 and the mesh is 4/4.

One heads-up: last run your node network-flapped every ~20 min (short drops, clean
reconnects). Minor, but if it recurs on the new chain let me know and we'll dig in.

*(Placeholders `<your-service>`, `<your-data-dir>`, `<your-keystore>`,
`<your-passphrase-file>`, `<DATE>`, `<COMMIT>`, `<SHA>` — fill your paths / the pinned
build. RPC/metrics ports unchanged from your current unit.)*
