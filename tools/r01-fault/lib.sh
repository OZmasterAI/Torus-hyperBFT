#!/usr/bin/env bash
# lib.sh — shared helpers for the R01 fault cases (sourced by run-case.sh / ro-case.sh).
# 4-validator localhost devnet (devnet/genesis.json, keys 01..04), validator v3 is the
# faulted node: its data dir lives on the loop fs /mnt/r01, everything else under
# /home/oz/r01-fault. Ports = the t15 harness block (28645.., 32433.., 29091..).
# Requires before sourcing: CASE (name).

WT=/home/oz/projects/wt/r01-fault
NODE_BIN=/home/oz/.cargo-target-r01-fault/release/torus-node
BENCH_BIN=/home/oz/.cargo-target-r01-fault/release/bench-throughput
RES=/home/oz/r01-fault/results/$CASE
RUN=/home/oz/r01-fault/run/$CASE
FDIR=/mnt/r01/$CASE
FAULT_IDX=3

RPC_PORTS=(28645 28646 28647 28648)
P2P_PORTS=(32433 32434 32435 32436)
MET_PORTS=(29091 29092 29093 29094)
KEYS=(
    0100000000000000000000000000000000000000000000000000000000000000
    0200000000000000000000000000000000000000000000000000000000000000
    0300000000000000000000000000000000000000000000000000000000000000
    0400000000000000000000000000000000000000000000000000000000000000
)
PID_V0=12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X
PID_V1=12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3
ADDR_V0="/ip4/127.0.0.1/udp/${P2P_PORTS[0]}/quic-v1/p2p/$PID_V0"
ADDR_V1="/ip4/127.0.0.1/udp/${P2P_PORTS[1]}/quic-v1/p2p/$PID_V1"

# Bench devnet posture (devnet/wsl/env.sh): anti-spam off, node-local only.
export TORUS_INGRESS_MIN_COLLATERAL=0 TORUS_ADDR_RATE_LIMIT=0 TORUS_RPC_IP_WEIGHT_PER_MIN=0

log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$RES/events.log" >&2; }

datadir() { if [ "$1" = "$FAULT_IDX" ]; then echo "$FDIR/v$1"; else echo "$RUN/v$1/data"; fi; }
nodelog() { echo "$RES/v$1.log"; }

# make_genesis <epoch_length>: devnet 4-val genesis + running state hash from height 1.
make_genesis() {
    jq --argjson e "$1" '.consensus.epoch_length = $e | .consensus.state_hash_activation_height = 1' \
        "$WT/devnet/genesis.json" > "$RUN/genesis.json"
}

start_node() { # $1 = index; appends to the node log; sets PIDS[$1]
    local i=$1 peers
    case "$i" in 0) peers="$ADDR_V1" ;; 1) peers="$ADDR_V0" ;; *) peers="$ADDR_V0,$ADDR_V1" ;; esac
    mkdir -p "$(datadir "$i")"
    echo "===== start v$i $(date -Is) TORUS_EXEC_PIPELINE=${TORUS_EXEC_PIPELINE-unset} =====" >> "$(nodelog "$i")"
    "$NODE_BIN" \
        --genesis "$RUN/genesis.json" \
        --data-dir "$(datadir "$i")" \
        --validator-key "${KEYS[$i]}" \
        --p2p-listen "/ip4/127.0.0.1/udp/${P2P_PORTS[$i]}/quic-v1" \
        --p2p-private-addrs \
        --p2p-peers "$peers" \
        --rpc-addr "127.0.0.1:${RPC_PORTS[$i]}" \
        --metrics-addr "127.0.0.1:${MET_PORTS[$i]}" \
        --archive \
        --log-level info \
        >> "$(nodelog "$i")" 2>&1 &
    PIDS[$i]=$!
    echo "${PIDS[$i]}" > "$RUN/v$i.pid"
    log "started v$i pid ${PIDS[$i]} data $(datadir "$i")"
}

rpc() { # $1 port, $2 method, $3 params-json -> raw json (empty on failure)
    curl -s -m 3 -X POST -H 'Content-Type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"$2\",\"params\":${3:-[]},\"id\":1}" \
        "http://127.0.0.1:$1" 2>/dev/null
}

# eth_blockNumber = min(applied-height marker, committed) on that node (torus-rpc eth_head).
head_of() { # $1 index -> height or -1
    local r; r=$(rpc "${RPC_PORTS[$1]}" eth_blockNumber '[]')
    python3 -c 'import sys,json
try: print(int(json.loads(sys.argv[1])["result"],16))
except Exception: print(-1)' "$r"
}

peer_head() { # max head over v0..v2
    local i h m=-1
    for i in 0 1 2; do h=$(head_of "$i"); [ "$h" -gt "$m" ] && m=$h; done
    echo "$m"
}

state_hash() { # $1 index, $2 height -> local hash or ERR
    local r; r=$(rpc "${RPC_PORTS[$1]}" torus_getStateHash "[$2]")
    python3 -c 'import sys,json
try: print(json.loads(sys.argv[1])["result"]["localHash"])
except Exception: print("ERR")' "$r"
}

wait_head() { # $1 index, $2 target, $3 timeout s -> 0 when head >= target
    local t=0
    while [ "$t" -lt "$3" ]; do
        [ "$(head_of "$1")" -ge "$2" ] && return 0
        sleep 2; t=$((t + 2))
    done
    return 1
}

load_loop() { # native order bursts (t15 shape) until $RUN/STOP_LOAD
    local n=0
    while [ ! -f "$RUN/STOP_LOAD" ]; do
        n=$((n + 1))
        "$BENCH_BIN" consensus --rpc-urls "http://127.0.0.1:${RPC_PORTS[0]},http://127.0.0.1:${RPC_PORTS[1]},http://127.0.0.1:${RPC_PORTS[2]}" \
            --batch-size "${BS:-100}" --senders 20 --duration 45 --rate "${RATE:-3}" --pre-sign "$((45 * ${RATE:-3}))" \
            --sign-mode session --markets 4 >> "$RES/bench.log" 2>&1 || true
        [ -f "$RUN/STOP_LOAD" ] && break
        sleep 1
    done
}

stop_all() {
    touch "$RUN/STOP_LOAD"
    [ -n "${LOAD_PID:-}" ] && kill "$LOAD_PID" 2>/dev/null
    pkill -f "$BENCH_BIN consensus" 2>/dev/null
    local i p
    for i in 0 1 2 3; do
        p=$(cat "$RUN/v$i.pid" 2>/dev/null) || continue
        if kill -0 "$p" 2>/dev/null && tr '\0' ' ' < "/proc/$p/cmdline" | grep -q -- "$(datadir "$i")"; then
            kill "$p" 2>/dev/null
        fi
    done
    sleep 3
    for i in 0 1 2 3; do
        p=$(cat "$RUN/v$i.pid" 2>/dev/null) || continue
        if kill -0 "$p" 2>/dev/null && tr '\0' ' ' < "/proc/$p/cmdline" | grep -q -- "$(datadir "$i")"; then
            kill -9 "$p" 2>/dev/null
        fi
    done
    log "all nodes + load stopped"
}

# Extract the R01-relevant lines of a node log (FATAL / write errors / replay / panics).
fault_lines() { # $1 logfile
    grep -aE 'FATAL|fail-stop|crash recovery|panicked|RocksDB write failed|No space|Read-only|IO error|flush worker|applied-height' "$1" \
        | sed -E 's/\x1b\[[0-9;]*m//g' | head -${2:-60}
}

# Compare the retained running-state-hash checkpoints of v3 against v0..v2.
compare_checkpoints() { # $1 from-height, $2 to-height (multiples of 100) -> writes hash-compare.txt
    local h i out="$RES/hash-compare.txt" ok=1 ref v
    : > "$out"
    for ((h = $1; h <= $2; h += 100)); do
        ref=$(state_hash 0 "$h")
        line="h=$h"
        for i in 0 1 2 3; do
            v=$(state_hash "$i" "$h"); line+=" v$i=$v"
            [ "$v" = "$ref" ] && [ "$v" != ERR ] || ok=0
        done
        echo "$line" >> "$out"
    done
    cat "$out" >&2
    return $((1 - ok))
}
