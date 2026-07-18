# p3 Throughput Ladder — Results & Findings

**Date:** 2026-07-18
**Network:** live 4-validator testnet · genesis anchor `8d7953a1` (4-val, 3-3-3-1 stake)
**Branches tested:** `perf/p3-throughput` → `perf/deep-book-storage` → `feat/precompile-0800-topn-gas`
**Method:** node-counter-sourced (`testnet/p3-testnet/measure-leg.sh`, `RUN_BENCH=0`), load from `bench-throughput`

Raw per-leg data: `testnet/p3-testnet/results/<label>/{summary.json,sampler.csv,counters-*.json}`

---

## TL;DR

- **Fill throughput is network-bound, not compute-bound.** The match engine never reached its raw ceiling; the **dissemination layer** (gossip + block-body DA) capped it first.
- **`--flow cross` measures resting, not fills** (~1.6% conversion). The honest fill numbers come from **`--flow match`**.
- **deep-book** sustains a fill ceiling of **~2k matched/s** (node) / **~6.2k orders/s** (bench), but **wedges below ~0.35 taker-ratio** via gossipsub Send-Queue saturation.
- **topn (precompile-0800)** **regresses the block-body DA path**: at the same 0.5 match load deep-book handles cleanly, topn body-starves — **5× fewer fills, 6× slower blocks, wedge.**

---

## Setup

| validator | stake | role |
|---|---|---|
| seed (`95.111.231.121`) | 3M | public |
| c18 (`13.140.140.138`) | 3M | public / measuring node |
| val2 (`103.167.235.250`) | 3M | public / load generator (60 senders) |
| home (WSL2, NAT) | 1M | dial-out |

Quorum = >2/3 of 10M = **>6.667M**. Single-fault tolerant.

---

## Methodology findings

1. **The bench tool's own "orders/s" is misleading.** Node counters `torus_orders_placed` / `torus_orders_matched` / `torus_native_actions_processed` are the truth, and they are consensus-deterministic → **one measuring node captures the whole chain** (never sum across boxes).
2. **`--flow cross` does not cross.** Despite a tight ±50 price band, only ~1.6% of orders fill (below even the rest-flow floor) — it measures cheap **resting** throughput. Under a 60-sender flood, orders pile onto the book as resting depth faster than the touch is consumed.
3. **`--flow match` is required for fill measurement.** Deterministic maker/taker geometry (takers priced beyond the whole maker grid → cross by construction). `--taker-ratio` sets the maker:taker split.

---

## Results

### Cross flow (resting throughput — cheap book inserts, ~98% never trade)

| branch | placed/s (sust) | placed/s (peak-1s) | matched/s | conversion |
|---|---|---|---|---|
| p3 | 39,142 | 99,018 | 630 | 1.61% |
| deep-book | 34,982 | 101,049 | 560 | 1.60% |

Filling is **~20× more expensive per order** than resting — which is why the honest fill ceiling (below) is far under the flashy ~39k "placed."

### Match flow (real fills — node-sourced matched/s)

**deep-book taker-ratio sweep:**

| taker-ratio | bench orders/s | node matched/s (sust) | block time | state |
|---|---|---|---|---|
| 0.5 | 5,836 | 2,054 | ~1 s | ✅ clean |
| 0.4 | 6,235 | 1,676 | ~1 s | ✅ clean (rode edge) |
| 0.3 | — | — | ~29 s | ❌ **WEDGED** (Send-Q 20–23) |
| 0.25 | — | — | — | ❌ **WEDGED** (+ safety violation) |

→ **Gossip-saturation cliff at ~0.35 taker.** Clean fill ceiling **~6.2k orders/s** (peak at 0.4).

**topn (precompile-0800) at the same 0.5:**

| metric | topn 0.5 | deep-book 0.5 |
|---|---|---|
| matched/s | **421** | 2,054 (**~5×**) |
| exec/s | 14 | 51 |
| block time | **~6.6 s** | ~1 s (**~6×**) |
| body-fetch-exhausted / no-body | **2,551** | — |
| Send-Queue-full | 0 | 0 |

→ topn **body-starves and wedges** at a load deep-book sustains cleanly.

---

## Three headline findings

### 1. Cheap resting ≠ real throughput
The ~39k "placed/s" is ~98% resting orders that never trade. The honest fill ceiling is **~2k matched/s**. A chain that "places 39k/s" but fills 630/s is doing 630/s of actual trading.

### 2. deep-book — gossip-saturation cliff (~0.35 taker)
Deeper maker book → bigger match-heavy block bodies + heavier vote traffic → **libp2p gossipsub Send-Queue saturation** (`Send Queue full, could not send Publish`) → dropped attestations → incomplete QCs → chain wedge. Clean at 0.4/0.5, wedged at 0.3/0.25. At 0.25 it produced a **consensus safety violation** (a node finalized a block past real quorum, forking 1 block ahead).

### 3. topn — block-body DA regression
Distinct failure mode: `Send-Queue-full = 0`, but 2,551 **body-fetch-exhausted / no-body / CompactBlock-fallback** events. The `precompile_provider` + `torus-state backend` changes degrade block-body availability/dissemination under fill load. **Actionable regression for the branch owner.**

**Both branches fail differently under match load:** deep-book at the *gossip* layer (mitigated by staying ≥0.35 taker), topn at the *DA/body* layer (wedges even at the safe 0.5).

---

## Ops lessons

- **conntrack UDP idle-timeout (120s)** silently killed inter-validator QUIC links every ~2 min → chain halts (the block-2488 halt). Fix: `sudo sysctl -w net.netfilter.nf_conntrack_udp_timeout_stream=3600` on **every** box. ⚠️ Runtime-only — make persistent via `/etc/sysctl.d/` to survive reboot.
- **Recovery playbook:** load-induced wedges do **not** self-heal, and single-node restart-from-DB does not recover a chain-wide wedge (and can leave a divergent 1-ahead node). **Coordinated fresh wipe + seed-first relaunch** is the reliable reset.
- **cargo gotcha:** killing `rustc` mid-build corrupts the incremental fingerprint DB (`Finished` printed but no binary produced). Fix with a full `cargo clean`, or scp a known-good binary between identical x86_64 boxes (used c18 ← seed).

---

## Open follow-ups

- [ ] **p3 `--flow match 0.5`** never measured — needed to complete the 3-branch fill ladder.
- [ ] **topn DA regression** — hand to the branch owner with the numbers above.
- [ ] Make the **conntrack fix persistent** on all 4 boxes.
- [ ] One-time full **`cargo clean` on c18** to restore local builds.
- [ ] Investigate whether the gossip Send-Queue depth / block-body DA path can be tuned (erasure-coded body dissemination is already a design note in-tree).
