# cap-probe — cap-raised, multi-producer native-DA path attribution

Resolves the question the **s367 mesh probe left open**. That probe ran at `cap=100`
(bodies stayed small) from a **single** producer and found *neither* DA path binds —
so it never reproduced the `cap=1000` wedge it was meant to explain, and the
"erasure coding is the real fix" thesis (`docs/plans/sprint5-erasure-coding.md`) still
rests only on the coarse s365 wedge, unconfirmed by direct path attribution.

This variant closes both gaps:
1. **Cap is actually raised** (compile-time — see `set-cap.sh`), so blocks can grow past the safe regime.
2. **Multi-producer + follower instrumentation**: load from every validator at once, and native-DA / gossip counters snapshotted on **every** node, so `analyze.py` names which path binds.

## The decision rule it answers

| What moves first under load | Binding path | Implication |
|---|---|---|
| `native_da_pull_requests` / `pull_failures` up, `OUTBOUND FAILURE`, `body fetch exhausted` | **PATH 2 — recovery pull** | erasure / recovery-path fix **is** the lever |
| gossip `dropped_full` / `dropped_oversized`, `dropped from pre-spread` | **PATH 1 — pre-spread** | Option-A erasure **won't help**; need ingress dispersal (Option B) / bigger budget |
| neither, blocks pinned at cap | **cap-bound** | raise cap further; DA has headroom |
| neither, low fill + low drop | **load-bound** | push harder |

## Run procedure (devnet)

```bash
# 1. raise the cap and REBUILD the devnet image (context = repo root)
./testnet/cap-probe/set-cap.sh 1000              # or: 1000 24000000  to also lift the 6MB byte cap
docker compose -f devnet/docker-compose.yml build

# 2. bring devnet up WITH follower metrics exposed on the host
docker compose -f devnet/docker-compose.yml \
               -f testnet/cap-probe/devnet-metrics.override.yml up -d

# 3. probe (label the dir with the cap you built at) + analyze
CAP_LABEL=cap1000 ./testnet/cap-probe/run.sh
python3 testnet/cap-probe/analyze.py out-cap1000

# 4. ALWAYS revert the source cap when done
./testnet/cap-probe/set-cap.sh --revert
```

Sweep the ladder by rebuilding at each cap: `100` (control) → `250` → `500` → `1000`,
re-running steps 1–3 with a distinct `CAP_LABEL` each time.

## Faithful WAN mode (the caveat that matters)

The s365 wedge was **WAN-specific** (3 boxes, real internet, 6MB bodies → `ConnectionReset`).
Devnet is a **single-box LAN** — it may *not* reproduce it (the s367 notes already warn
local held bs500 pre-fix; contention differs). **A clean devnet run is NOT proof the
testnet won't wedge.** To test faithfully, build at the raised cap, deploy to the real
validators, and point the probe at their endpoints:

```bash
RPCS="http://127.0.0.1:8545"                       # only the local validator produces
METRICS="local=http://127.0.0.1:9090"              # add remote validators' metrics if reachable
LOGS="file:testnet/node.debug.log"
CAP_LABEL=cap1000-wan ./testnet/cap-probe/run.sh
```

(Deploying a raised cap to production validators is a redeploy — do that deliberately,
not as part of this script; the script never restarts or deploys anything.)

## Prerequisites / assumptions to sanity-check on first run
- **Sender keys funded/registered** on the target chain for offsets `0 .. producers*SENDERS_PER`.
- `--sender-offset` yields disjoint keys per producer (verify: no cross-producer dup-inclusion spike).
- Metrics reachable on the host ports in `METRICS` (devnet needs the override file).
- `analyze.py` diffs the counters proven live in the s367 snapshots + adds `body fetch exhausted`
  (the actual wedge signature the old probe missed).

## Files
- `set-cap.sh` — flip/revert the compile-time `NATIVE_TOTAL_BLOCK_CAP` / `NATIVE_BLOCK_BYTES_CAP`.
- `devnet-metrics.override.yml` — expose all nodes' metrics on the host.
- `run.sh` — multi-producer load + per-node before/after counter + log capture. Load-only, never deploys.
- `analyze.py` — per-node deltas + binding-path verdict.
