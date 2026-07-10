# S442 seed-side verification checklist

Run from the **seed box** (`95.111.231.121`) after the four nodes launch on the new
genesis. Seed RPC = `127.0.0.1:8545`, seed metrics = `127.0.0.1:9090`.

---

## 1. Block-speed check (metrics :9090)

Expected steady state after the think-dev livelock fix: **~15–20 blk/s**. (S435
launched ~17 then crawled to ~1.8 — that crawl is exactly what this relaunch fixes, so
a sustained 15–20 is the pass condition.)

```bash
# committed height now vs ~10s later → blocks/s
h1=$(curl -s localhost:9090/metrics | grep -m1 '^torus_committed_height' | awk '{print $2}')
sleep 10
h2=$(curl -s localhost:9090/metrics | grep -m1 '^torus_committed_height' | awk '{print $2}')
echo "blk/s ≈ $(( (h2 - h1) / 10 ))"
```

*(Exact metric name — `torus_committed_height` vs a `_total` counter — is
`[OPERATOR-CONFIRM]` against the merged binary's `/metrics` output; grep the live
endpoint once to confirm the label.)*

Also sanity-check the mesh and that the node is up:

```bash
curl -s localhost:9090/metrics | grep -c torus_native_actions_processed_total   # >0 = up
curl -s localhost:9090/metrics | grep -iE 'peers|connected'                     # expect 3 peers
```

---

## 2. Per-height hash cross-check (fork tripwire)

Point `devnet/t12-fork-check.py` at all four live RPCs. It walks committed heights and
compares block hashes across nodes; **any divergence → `exit(70)`**.

```bash
python3 devnet/t12-fork-check.py \
  --rpc http://127.0.0.1:8545 \
  --rpc http://84.32.108.220:<VAL1-RPC-PORT> \
  --rpc http://<VAL3-RPC-HOST>:<VAL3-RPC-PORT> \
  --rpc http://103.167.235.250:28545
echo "exit=$?"     # 0 = all four agree at every height; 70 = FORK — stop and investigate
```

Tripwire expectations:
- **exit 0** — all four nodes report identical block hashes at every checked height.
  This is the only acceptable state.
- **exit 70** — at least one node diverged. Treat as a fork: identify the odd node
  out, stop it, confirm it isn't serving a stale/old chain, and re-wipe if needed.
  Do **not** let a divergent node keep voting.
- Run it once right after launch (heights 0..N), then on a loop (e.g. every few
  minutes) through the first 24h.

`[OPERATOR-CONFIRM]`:
- `<VAL1-RPC-PORT>`, `<VAL3-RPC-HOST>`, `<VAL3-RPC-PORT>` — from the roster in
  `relaunch-runbook.md`; seed `8545` and friend2 `28545` are known.

---

## 3. What to watch in the first 24h

1. **Block speed holds 15–20 blk/s** — does not crawl toward ~2. A slow decay is the
   old livelock signature; if it reappears, capture seed logs and flag it — the
   think-dev fix should prevent it.
2. **Fork tripwire stays green** (`exit 0`) on every run.
3. **4/4 mesh stays up.** val1 flapped on ~20-min cadence last run (short drops, clean
   reconnects) — watch whether it recurs; minor unless it starts dropping votes.
4. **No node re-syncs the old chain.** Every node's height climbs from 0; nobody jumps
   to an old height. The block-format change makes the old chain unjoinable, but
   confirm anyway.
5. **Leader rotation is healthy** — no single validator eating a disproportionate share
   of NewView/timeout messages (that was val3's livelock signature; leader-for-view =
   `validators_sorted_by_pubkey_ASC[view % 4]`).
6. **All markets live + all four validators voting** — confirm before re-running bench
   grids.

Log the first clean fork-check `exit=0` and the first sustained 15–20 blk/s reading as
the "relaunch healthy" milestone, then notify the operators.
