#!/usr/bin/env bash
# resummarize.sh <result-dir> — re-run summarize.py on an existing cell dir using
# the provenance stored in its summary.json (after a summarize.py change).
set -euo pipefail
D=$(cd "$1" && pwd); T=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
j() { jq -r "$@" "$D/summary.json"; }
# EVM_LOAD_RATE cells: summarize.py rewrites summary.json without .evm_load;
# it is re-derived below from the retained evm-load-* artifacts.
EVM_LOAD=$(jq -c '.evm_load // empty' "$D/summary.json")
# STATE_HASH_ATTEST cells: likewise without .state_attest (and its REJECT); a cell whose scan wrote no
# state-attest.json must stay rejected, so its old .state_attest also triggers the re-derive below.
STATE_ATTEST=$(jq -c '.state_attest // empty' "$D/summary.json")
# ... nor .genesis.evm_block_interval (run-cell.sh records it after summarize.py).
EVM_N=$(jq -r '.genesis.evm_block_interval // empty' "$D/summary.json")
# ... nor .genesis.bulk_count (BULK_COUNT, bench queue 8f; run-cell.sh records it after summarize.py).
BULK_N=$(jq -r '.genesis.bulk_count // empty' "$D/summary.json")
python3 "$T/summarize.py" --out "$D" --label "$(j .label)" --worktree "$(j .worktree)" --commit "$(j .commit)" \
  --dirty "$(j .dirty_files)" --markets "$(j .cell.markets)" --dur "$(j .cell.duration_s)" --rate "$(j .cell.rate_total)" \
  --senders "$(j .cell.senders)" --t-bench0 "$(j .timing.t_bench0)" --t-bench1 "$(j .timing.t_bench1)" \
  --t-drain "$(j .timing.t_drain)" --drained "$([ "$(j '.timing.drained_reported // .timing.drained')" = true ] && echo 1 || echo 0)" \
  --bench-rc "$(j .timing.bench_rc)" --idle-blks "$(j .idle_blk_s)" --md5-node "$(j .binaries.torus_node_md5)" \
  --md5-bench "$(j .binaries.bench_throughput_md5)" --genesis-md5 "$(j .genesis.md5)" --genesis-markets "$(j .genesis.markets)" \
  --genesis-accounts "$(j .genesis.native_balances)" --node-env "$(jq -c .cell.node_env "$D/summary.json")" \
  --env-digests "$(j '.cell.env_digests_per_node|join(" ")')" --extra-env "$(j .cell.extra_env)" \
  --bench-cmd "$(j .cell.bench_cmd)" --pids "$(j '.cell.node_pids|join(" ")')" \
  --evicted "$(j '.ingest.mempool_nonce_expired_evictions_per_node|map(tostring)|join(" ")')" \
  --bench-submitted "$(j .ingest.bench_submitted_actions)" \
  --block-cap "$(j '.cell.block_cap // ""')" --dissem "$(j '.dissemination.raw // ""')" \
  --markets-per-sender "$(j '.cell.markets_per_sender // ""')" \
  --max-in-flight "$(j '.cell.max_in_flight // ""')" \
  --in-flight-tail "$(j '.cell.in_flight_tail // ""')" \
  --open-order-budget "$(j '.cell.open_order_budget // ""')" \
  --liq-thin "$(j '.cell.liq_thin // ""')" \
  --liq-thin-avail "$(j '.cell.liq_thin_avail // ""')" \
  --drain-timeout "$(j '.timing.drain_timeout_s // ""')" \
  --digest-quiescent "$(j 'if .agreement.state_digest_quiescent == false then "0" else "1" end')" \
  --digest-secs "$(j '(.agreement.state_digest_seconds_per_node // [])|map(tostring)|join(" ")')" \
  --digest-heights "$(j '(.agreement.state_digest_heights // [])|map(tostring)|join(" ")')"
if [ -n "$EVM_N" ]; then
    tmp="$D/summary.json.evm-n.tmp"
    jq --argjson n "$EVM_N" '.genesis.evm_block_interval = $n' "$D/summary.json" > "$tmp" && mv -f "$tmp" "$D/summary.json"
fi
if [ -n "$BULK_N" ]; then
    tmp="$D/summary.json.bulk-count.tmp"
    jq --argjson n "$BULK_N" '.genesis.bulk_count = $n' "$D/summary.json" > "$tmp" && mv -f "$tmp" "$D/summary.json"
fi
# PRELOAD cells: .preload (and its REJECT fold) is re-derived from preload-cell.json + preload.json.
if [ -f "$D/preload-cell.json" ]; then
    python3 "$T/preload.py" summary --dir "$D" >/dev/null
fi
if [ -n "$EVM_LOAD" ]; then
    python3 "$T/evm_load.py" summary --dir "$D" --rate "$(jq -r '.rate // ""' <<<"$EVM_LOAD")" \
        --senders "$(jq -r '.senders // 0' <<<"$EVM_LOAD")" --rc "$(jq -r '.rc // ""' <<<"$EVM_LOAD")"
fi
# STATE_HASH_ATTEST cells: .state_attest (and its REJECT fold) is re-derived from state-attest.json
# (no file but an old .state_attest: summary writes ok=false and keeps the cell rejected).
if [ -f "$D/state-attest.json" ] || [ -n "$STATE_ATTEST" ]; then
    python3 -I "$T/state_attest.py" summary --dir "$D" >/dev/null
fi
