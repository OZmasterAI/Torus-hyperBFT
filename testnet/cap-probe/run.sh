#!/usr/bin/env bash
# cap-probe/run.sh — cap-RAISED, MULTI-PRODUCER native-DA path-attribution probe.
#
# WHY: the s367 mesh probe ran at cap=100 (bodies small, safe regime) from ONE
# producer and found neither DA path binds — so it never reproduced the cap=1000
# wedge condition it was meant to explain. This variant fixes both gaps:
#   1. Drives load from EVERY validator at once (multi-producer, disjoint sender keys).
#   2. Assumes the node(s) run a binary REBUILT at a raised cap (see set-cap.sh),
#      and snapshots native-DA / gossip counters on EVERY node so analyze.py can
#      attribute the binding path: recovery-pull (PATH 2 -> erasure/recovery fix helps)
#      vs gossip pre-spread (PATH 1 -> Option-A erasure won't help; needs ingress dispersal).
#
# This script ONLY generates load + scrapes counters/logs. It never builds, restarts,
# or deploys. Bring the target up yourself first.
#
# PREREQ: bench sender keys funded/registered on the target chain (offsets 0..N*SENDERS_PER).
#
# Usage (devnet defaults):
#   docker compose -f devnet/docker-compose.yml -f testnet/cap-probe/devnet-metrics.override.yml up -d
#   CAP_LABEL=cap1000 ./testnet/cap-probe/run.sh
#   python3 testnet/cap-probe/analyze.py out-cap1000
set -u
ROOT="$(git rev-parse --show-toplevel)"
# Resolve via cargo metadata: a redirected [build] target-dir means a successful
# `cargo build --release` leaves target/release EMPTY. See testnet/lib/cargo-bin.sh.
. "$ROOT/testnet/lib/cargo-bin.sh"
BIN="${BIN:-$(cargo_bin bench-throughput "$ROOT")}"

# ---- target config (override via env). Defaults = local docker devnet. ----
RPCS="${RPCS:-http://127.0.0.1:8645 http://127.0.0.1:8546 http://127.0.0.1:8547 http://127.0.0.1:8548}"
METRICS="${METRICS:-v0=http://127.0.0.1:9091 v1=http://127.0.0.1:9092 v2=http://127.0.0.1:9093 v3=http://127.0.0.1:9094 rpc=http://127.0.0.1:9095}"
# log source: "docker:<svc,svc,...>"  OR  "file:/abs/path/node.log"  OR  "none"
LOGS="${LOGS:-docker:validator-0,validator-1,validator-2,validator-3,rpc-node}"
COMPOSE="${COMPOSE:--f $ROOT/devnet/docker-compose.yml -f $ROOT/testnet/cap-probe/devnet-metrics.override.yml}"

CAP_LABEL="${CAP_LABEL:-capUNKNOWN}"   # state the cap the binary was built at (output-dir label only)
BS_LADDER="${BS_LADDER:-400 600 800 1000}"   # batch size grows body BYTES; cap governs actions/block
RATE="${RATE:-30}"; DURATION="${DURATION:-20}"; SENDERS_PER="${SENDERS_PER:-7}"
SUBMIT_BATCH="${SUBMIT_BATCH:-10}"; PRESIGN="${PRESIGN:-120}"; COOLDOWN="${COOLDOWN:-8}"

OUT="$ROOT/testnet/cap-probe/out-${CAP_LABEL}"
mkdir -p "$OUT"
FIRST_RPC="${RPCS%% *}"
[ -x "$BIN" ] || { echo "bench binary missing: $BIN (build it first)"; exit 1; }

height() { curl -s --max-time 3 -X POST "$1" -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' 2>/dev/null \
  | grep -o '0x[0-9a-f]*' | head -1; }
hdec() { local h; h="$(height "$1")"; [ -n "$h" ] && printf '%d' "$h" || echo "NA"; }

snap_all() { # $1=phase(before|after) $2=tag
  for m in $METRICS; do local name="${m%%=*}" url="${m#*=}"
    curl -s --max-time 3 "$url/metrics" 2>/dev/null \
      | grep -E '^torus_(native_da_|native_gossip_|gossip_messages_|peers_)' \
      > "$OUT/m-${2}-${name}-${1}.txt" || echo "# SCRAPE FAILED $url" > "$OUT/m-${2}-${name}-${1}.txt"
  done
}

MARKERS='broadcast pre-proposal|native-da request: serving|native-da response: queued|OUTBOUND FAILURE|INBOUND FAILURE|oversized native action|dropped from pre-spread|body fetch exhausted'
capture_logs() { # $1=tag $2=since_iso
  case "$LOGS" in
    docker:*) local svcs="${LOGS#docker:}"; svcs="${svcs//,/ }"
      for s in $svcs; do
        docker compose $COMPOSE logs --no-log-prefix --since "$2" "$s" 2>/dev/null \
          | grep -iE "$MARKERS" > "$OUT/log-${1}-${s}.txt" 2>/dev/null || : ;
      done ;;
    file:*) local f="${LOGS#file:}"
      tail -c "+$(( ${LOG_OFF:-0} + 1 ))" "$f" 2>/dev/null | grep -iE "$MARKERS" > "$OUT/log-${1}-node.txt" || : ;;
  esac
}

run_rung() {
  local bs="$1" tag="bs${bs}-r${RATE}" idx=0 pids=()
  local ts0 h0; ts0="$(date -u +%Y-%m-%dT%H:%M:%S)"; h0="$(hdec "$FIRST_RPC")"
  [ "${LOGS:0:5}" = "file:" ] && LOG_OFF="$(stat -c%s "${LOGS#file:}" 2>/dev/null || echo 0)"
  echo "######## $tag  start=$(date +%T)  chain_h=$h0 ########"
  snap_all before "$tag"
  for rpc in $RPCS; do
    local off=$(( idx * SENDERS_PER ))
    "$BIN" consensus --rpc-urls "$rpc" --senders "$SENDERS_PER" --sender-offset "$off" \
      --batch-size "$bs" --submit-batch "$SUBMIT_BATCH" --duration "$DURATION" \
      --format bin --pre-sign "$PRESIGN" --rate "$RATE" \
      > "$OUT/bench-${tag}-p${idx}.txt" 2>&1 &
    pids+=($!); idx=$((idx+1))
  done
  for p in "${pids[@]}"; do wait "$p"; done
  snap_all after "$tag"
  capture_logs "$tag" "$ts0"
  local h1; h1="$(hdec "$FIRST_RPC")"; local dh="NA"
  [ "$h0" != "NA" ] && [ "$h1" != "NA" ] && dh=$(( h1 - h0 ))
  echo "$tag bs=$bs producers=$idx h0=$h0 h1=$h1 dh=$dh dur=${DURATION}s" | tee -a "$OUT/summary.txt"
  [ "$dh" != "NA" ] && [ "$dh" -lt 3 ] && echo "  !! LOW HEIGHT ADVANCE ($dh blocks in ${DURATION}s) — possible WEDGE" | tee -a "$OUT/summary.txt"
  sleep "$COOLDOWN"
}

echo "[cap-probe] $CAP_LABEL  producers=$(echo $RPCS | wc -w)  ladder='$BS_LADDER'  out=$OUT"
: > "$OUT/summary.txt"
for bs in $BS_LADDER; do run_rung "$bs"; done
echo "[cap-probe] DONE $(date +%T)  chain_h=$(hdec "$FIRST_RPC")"
echo "analyze: python3 testnet/cap-probe/analyze.py out-${CAP_LABEL}"
