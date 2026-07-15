#!/usr/bin/env bash
# Native 4-validator devnet harness for the A/B acceptance on 18c.
# ISOLATED from the live validator: loopback-only, high ports, own data dirs,
# devnet keys 01-04 + devnet genesis. Every devnet process pinned to cores 3-17.
# NOTHING here ever touches /home/18c/torus-hyperbft (live).
set -uo pipefail

ROOT=/home/18c/torus-ab
RUN=$ROOT/run
GEN=$ROOT/devnet-genesis.json
PIN="taskset -c 3-17"
V0PID=12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X
V1PID=12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3
BOOT="/ip4/127.0.0.1/udp/40333/quic-v1/p2p/$V0PID"
KEYS=(0100000000000000000000000000000000000000000000000000000000000000 \
      0200000000000000000000000000000000000000000000000000000000000000 \
      0300000000000000000000000000000000000000000000000000000000000000 \
      0400000000000000000000000000000000000000000000000000000000000000)
RPC_BASE=18555; P2P_BASE=40333; MET_BASE=19090

rpcport(){ echo $((RPC_BASE+$1)); }
metport(){ echo $((MET_BASE+$1)); }

height(){
  curl -s -m 2 -X POST -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    "http://127.0.0.1:$1" 2>/dev/null \
    | python3 -c 'import sys,json;print(int(json.load(sys.stdin)["result"],16))' 2>/dev/null || echo -1
}

launch(){ # $1=tag $2=binary $3=sync_wal(0/1) [$4=nodecount default 4]
  local tag=$1 bin=$2 sync=$3 nc=${4:-4} d i
  d=$RUN/$tag; mkdir -p "$d"
  : > "$d/pids"
  for i in $(seq 0 $((nc-1))); do
    local dd=$d/data-$i
    mkdir -p "$dd"
    local peers="--p2p-peers=$BOOT"
    [ "$i" = 0 ] && peers="--p2p-peers=/ip4/127.0.0.1/udp/40334/quic-v1/p2p/$V1PID"
    TORUS_SYNC_WAL_ON_COMMIT=$sync \
    TORUS_HASH_ONLY_PUSH_THRESHOLD=${TORUS_PUSH_THRESHOLD:-6000000} \
    TORUS_NATIVE_TOTAL_BLOCK_CAP=${TORUS_NATIVE_TOTAL_BLOCK_CAP:-100} \
    $PIN "$bin" \
      --genesis="$GEN" --data-dir="$dd" \
      --validator-key="${KEYS[$i]}" \
      --p2p-listen="/ip4/127.0.0.1/udp/$((P2P_BASE+i))/quic-v1" \
      --p2p-private-addrs \
      $peers \
      --rpc-addr="127.0.0.1:$(rpcport $i)" \
      --metrics-addr="127.0.0.1:$(metport $i)" \
      --log-level="info,hotstuff_rs=info,torus_node=warn" \
      > "$d/node-$i.log" 2>&1 &
    echo $! >> "$d/pids"
  done
  echo "launched $tag pids: $(tr '\n' ' ' < "$d/pids")"
}

launch_one(){ # $1=tag $2=binary $3=sync $4=idx  (restart a single node into existing dir)
  local tag=$1 bin=$2 sync=$3 i=$4 d=$RUN/$1
  local peers="--p2p-peers=$BOOT"
  [ "$i" = 0 ] && peers="--p2p-peers=/ip4/127.0.0.1/udp/40334/quic-v1/p2p/$V1PID"
  TORUS_SYNC_WAL_ON_COMMIT=$sync \
  TORUS_HASH_ONLY_PUSH_THRESHOLD=${TORUS_PUSH_THRESHOLD:-6000000} \
  TORUS_NATIVE_TOTAL_BLOCK_CAP=${TORUS_NATIVE_TOTAL_BLOCK_CAP:-100} \
  $PIN "$bin" \
    --genesis="$GEN" --data-dir="$d/data-$i" \
    --validator-key="${KEYS[$i]}" \
    --p2p-listen="/ip4/127.0.0.1/udp/$((P2P_BASE+i))/quic-v1" \
    --p2p-private-addrs $peers \
    --rpc-addr="127.0.0.1:$(rpcport $i)" \
    --metrics-addr="127.0.0.1:$(metport $i)" \
    --log-level="info,hotstuff_rs=info,torus_node=warn" \
    >> "$d/node-$i.log" 2>&1 &
  echo "restarted node-$i pid $!"
  echo $! >> "$d/pids"
}

wait_chain(){
  local tag=$1 i h
  for i in $(seq 1 40); do
    h=$(height $(rpcport 0))
    [ "$h" -gt 2 ] 2>/dev/null && { echo "chain up: height=$h after $((i*2))s"; return 0; }
    sleep 2
  done
  echo "FATAL: chain $tag never reached height>2"; return 1
}

snap(){
  local tag=$1 label=$2 i
  for i in 0 1 2 3; do
    curl -s -m 2 "http://127.0.0.1:$(metport $i)/metrics" > "$RUN/$tag/met-$i-$label.txt" 2>/dev/null || true
  done
}

killpid(){ kill "$1" 2>/dev/null || true; }
teardown(){
  local tag=$1 p
  if [ -f "$RUN/$tag/pids" ]; then
    while read -r p; do [ -n "$p" ] && killpid "$p"; done < "$RUN/$tag/pids"
    sleep 2
    while read -r p; do [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true; done < "$RUN/$tag/pids"
  fi
  echo "torn down $tag"
}
