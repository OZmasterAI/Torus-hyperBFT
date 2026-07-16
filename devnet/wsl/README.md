# WSL bare-metal 3-validator devnet (perf/funnel-truth)

Local-only, **no docker**, 3 `torus-node` processes on localhost. Built for the
funnel-truth mission (establish why ~398/400 orders die inside `execute_batch`).
Uses the **weighted 100k-account** genesis (bench-reproducible funded senders,
10 markets) with the consensus validator set swapped to the 3 deterministic
devnet keys we hold the private keys for.

## Topology

| node | validator key (ed25519 seed)      | RPC (host) | p2p/udp | metrics | data dir                         |
|------|-----------------------------------|------------|---------|---------|----------------------------------|
| val0 | `0x01..` (32B)                    | 8645       | 30401   | 9161    | `~/torus-wsl-devnet/data/val0`   |
| val1 | `0x02..`                          | 8646       | 30402   | 9162    | `~/torus-wsl-devnet/data/val1`   |
| val2 | `0x03..`                          | 8647       | 30403   | 9163    | `~/torus-wsl-devnet/data/val2`   |

Dial edges: val0↔val1 (known peer ids), val2→{val0,val1}. Ports deliberately
avoid the live-testnet convention (8545/30333/9090).

Consensus: hotstuff, 3 equal-stake validators (power 1e6 each), epoch_length 100,
timeout_base 500ms. `permanent_stakes` (hardhat governance voters) carry **no**
consensus power — the validator set is built solely from `.validators`.

## Relaunch recipe (verbatim, from a fresh WSL shell)

```bash
source "$HOME/.cargo/env"
cd ~/torus-hyperbft
git checkout perf/funnel-truth

# 1. build (skip if target/release/{torus-node,bench-throughput} exist)
cargo build --release -p torus-node -p bench-throughput

# 2. genesis (regenerates the git-ignored 100k-account + 3-val genesis)
./devnet/wsl/gen-3val-genesis.sh

# 3. launch 3 validators (CLEAN=1 wipes data dirs for a fresh genesis init)
CLEAN=1 ./devnet/wsl/launch-3val.sh

# 4. verify idle health (scrapes each node's Prometheus counters twice, 30s apart)
./devnet/wsl/health-3val.sh

# 5. stop (data dirs are preserved unless you CLEAN on next launch)
./devnet/wsl/stop-3val.sh
```

`WINDOW=30` (health scrape gap) and `DATA_ROOT=$HOME/torus-wsl-devnet` are
overridable via env. The node runtime env (`TORUS_HASH_ONLY_PUSH_THRESHOLD=6000000`,
`TORUS_NATIVE_TOTAL_BLOCK_CAP=100`) is set in `env.sh` to match the S415/S458
devnet body-push tuning so a later bench (A3) runs against the same config.

## Notes for the funnel bench (A3)

- Funded bench senders live at bench account index ≥ 60 (`gen-accounts --offset 60`).
- Genesis has **10 markets** → `bench --markets` must be ≤ 10.
- Throughput MUST be read from node Prometheus counters
  (`torus_blocks_committed_total`, `torus_native_actions_processed`, …), never
  computed as included-actions × batch-size.
