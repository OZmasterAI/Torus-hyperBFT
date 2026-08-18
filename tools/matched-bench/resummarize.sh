#!/usr/bin/env bash
# resummarize.sh <result-dir> — re-run summarize.py on an existing cell dir using
# the provenance stored in its summary.json (after a summarize.py change).
set -euo pipefail
D=$(cd "$1" && pwd); T=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
j() { jq -r "$@" "$D/summary.json"; }
python3 "$T/summarize.py" --out "$D" --label "$(j .label)" --worktree "$(j .worktree)" --commit "$(j .commit)" \
  --dirty "$(j .dirty_files)" --markets "$(j .cell.markets)" --dur "$(j .cell.duration_s)" --rate "$(j .cell.rate_total)" \
  --senders "$(j .cell.senders)" --t-bench0 "$(j .timing.t_bench0)" --t-bench1 "$(j .timing.t_bench1)" \
  --t-drain "$(j .timing.t_drain)" --drained "$([ "$(j .timing.drained)" = true ] && echo 1 || echo 0)" \
  --bench-rc "$(j .timing.bench_rc)" --idle-blks "$(j .idle_blk_s)" --md5-node "$(j .binaries.torus_node_md5)" \
  --md5-bench "$(j .binaries.bench_throughput_md5)" --genesis-md5 "$(j .genesis.md5)" --genesis-markets "$(j .genesis.markets)" \
  --genesis-accounts "$(j .genesis.native_balances)" --node-env "$(jq -c .cell.node_env "$D/summary.json")" \
  --env-digests "$(j '.cell.env_digests_per_node|join(" ")')" --extra-env "$(j .cell.extra_env)" \
  --bench-cmd "$(j .cell.bench_cmd)" --pids "$(j '.cell.node_pids|join(" ")')" \
  --evicted "$(j '.ingest.mempool_nonce_expired_evictions_per_node|map(tostring)|join(" ")')" \
  --bench-submitted "$(j .ingest.bench_submitted_actions)"
