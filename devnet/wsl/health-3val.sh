#!/usr/bin/env bash
# health-3val.sh — verify the WSL 3-val devnet is healthy at idle.
# Scrapes each node's /metrics twice, WINDOW seconds apart, and reports:
#   - idle block rate  = d(torus_blocks_committed_total)/dt   (NODE counter only)
#   - height agreement = torus_block_height per node
#   - mesh             = torus_peers_connected / torus_consensus_mesh_peers
#   - error spam       = ERROR line count in each node log
# NOTE: block rate is scraped straight from each node's Prometheus counter — it is
# NEVER inferred from batch size x anything.
set -uo pipefail
cd "$(dirname "$0")"
source ./env.sh
WINDOW="${WINDOW:-30}"

scrape() { # $1=port $2=metric -> summed value as a bare integer (0 if absent/down)
    local v
    v=$(curl -s -m 3 "http://localhost:$1/metrics" 2>/dev/null \
        | awk -v m="$2" '$1==m || index($1, m"{")==1 {s+=$2} END{printf "%.0f", s+0}')
    # guard: strip anything non-numeric so downstream integer tests never choke
    printf '%s' "${v//[!0-9]/}" | grep -qE '^[0-9]+$' && printf '%s' "${v//[!0-9]/}" || printf '0'
}

declare -A H0 C0
ports=($METRICS_PORTS)
labels=(val0 val1 val2)

echo "=== scrape #1 (t=0) ==="
for i in 0 1 2; do
    p=${ports[$i]}
    H0[$i]=$(scrape "$p" torus_block_height)
    C0[$i]=$(scrape "$p" torus_blocks_committed_total)
    pc=$(scrape "$p" torus_peers_connected)
    mp=$(scrape "$p" torus_consensus_mesh_peers)
    printf "  %-5s height=%-6s committed=%-6s peers_connected=%-3s mesh_peers=%-3s\n" \
        "${labels[$i]}" "${H0[$i]}" "${C0[$i]}" "$pc" "$mp"
done

echo "sleeping ${WINDOW}s..."
sleep "$WINDOW"

echo "=== scrape #2 (t=${WINDOW}s) ==="
ok=1
for i in 0 1 2; do
    p=${ports[$i]}
    h1=$(scrape "$p" torus_block_height)
    c1=$(scrape "$p" torus_blocks_committed_total)
    pc=$(scrape "$p" torus_peers_connected)
    dh=$(( ${h1:-0} - ${H0[$i]:-0} ))
    dc=$(( ${c1:-0} - ${C0[$i]:-0} ))
    rate=$(awk -v d="$dc" -v w="$WINDOW" 'BEGIN{printf "%.3f", d/w}')
    printf "  %-5s height=%-6s committed=%-6s d_committed=%-4s idle_blk/s=%-6s peers_connected=%-3s\n" \
        "${labels[$i]}" "$h1" "$c1" "$dc" "$rate" "$pc"
    [ "${pc:-0}" -ge 2 ] || { echo "    WARN ${labels[$i]}: only $pc peers connected (<2)"; ok=0; }
    [ "$dc" -ge 1 ]      || { echo "    WARN ${labels[$i]}: committed NOT advancing (dc=$dc)"; ok=0; }
done

echo "=== error scan (node logs) ==="
for i in 0 1 2; do
    log="$RUN_DIR/val$i.log"
    [ -f "$log" ] || continue
    n=$(grep -c -E ' ERROR | panicked' "$log" 2>/dev/null); n=${n:-0}
    printf "  %-5s ERROR/panic lines=%s\n" "${labels[$i]}" "$n"
    [ "$n" -eq 0 ] || echo "    (last error) $(grep -E ' ERROR | panicked' "$log" | tail -1)"
done

echo
if [ "$ok" = 1 ]; then echo "HEALTH: OK (all nodes committing, mesh formed)"; else echo "HEALTH: DEGRADED (see WARN above)"; fi
