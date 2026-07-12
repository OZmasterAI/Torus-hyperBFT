#!/usr/bin/env bash
# S459 wedge + unwedge proof on the devnet.
#
# DEFAULT MODE (injection): run a healthy devnet, stop it mid-flight, delete the
# SAME interior uncommitted block from every node's consensus tree
# (torus-wedge-inject) — the exact storage shape a real body loss leaves — then
# restart into a genuine wedge (committed height frozen, tree/views racing,
# "REFUSING to commit across a hole" in logs) and prove `torus-unwedge`
# recovers it end-to-end.
#
# ORGANIC=1: the S458 formation recipe (no-gossip + hash-only + tiny action +
# restart-all). NOT reproducible on current binaries — the S444+ healing layers
# (peer pull via commit manifest, boot-parked hole heal) recover tiny bodies
# (runs 2/3). Kept for future formation forensics.
#
# The tools run INSIDE containers (root) because compose volumes are root-owned
# (run4 lesson: host-side open → EACCES). Volume names align with NODES.
#
# Run ON 18c from the isolated worktree, NEVER the live checkout (ETXTBSY
# lesson, see ab-d2l-s458.sh fuser guard).
# Usage:
#   PROVE_UNWEDGE=1 SKIP_BUILD=1 bash devnet/wedge-repro-s459.sh
set -euo pipefail
cd "$(dirname "$0")"
REPO=$(cd .. && pwd)

OUT=wedge-repro-s459
mkdir -p "$OUT"
ABSOUT=$(cd "$OUT" && pwd)

RPCS=(http://localhost:8645 http://localhost:8546 http://localhost:8547 http://localhost:8548)
# first 4 align with RPCS; rpc-node has a block tree too (surgery covers it)
NODES=(devnet-validator-0-1 devnet-validator-1-1 devnet-validator-2-1 devnet-validator-3-1 devnet-rpc-node-1)
VOLS=(devnet_validator-0-data devnet_validator-1-data devnet_validator-2-data devnet_validator-3-data devnet_rpc-node-data)

if [ "${SKIP_BUILD:-0}" != 1 ]; then
    (cd "$REPO" && nice -n 10 cargo build --release -p torus-node -p bench-throughput)
fi
CT=${CARGO_TARGET_DIR:-$REPO/target}
if [ "$CT" != "$REPO/target" ]; then
    mkdir -p "$REPO/target/release"
    if fuser -s "$REPO/target/release/torus-node" 2>/dev/null; then
        echo "FATAL: $REPO/target/release/torus-node is being EXECUTED (live validator?)." >&2
        exit 1
    fi
    for f in torus-node bench-throughput torus-unwedge torus-wedge-inject; do
        [ -x "$CT/release/$f" ] && cp "$CT/release/$f" "$REPO/target/release/"
    done
fi

trap 'docker compose down -v >/dev/null 2>&1 || true' EXIT

height() {
    curl -s -m 2 -X POST -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "$1" | python3 -c 'import sys,json; print(int(json.load(sys.stdin)["result"],16))' 2>/dev/null || echo -1
}

# Run an offline tool against a node's ROOT-OWNED compose volume by mounting it
# into a throwaway container. $1=volume $2=binary, rest = tool args.
tool() {
    local vol=$1 bin=$2; shift 2
    docker run --rm \
        -v "$vol:/data" \
        -v "$REPO/target/release/$bin:/tool:ro" \
        -v "$ABSOUT:/out" \
        --entrypoint /tool \
        torus-devnet-node:local "$@"
}

echo "== phase 1: up (healthy defaults) =="
docker compose up -d --force-recreate
sleep 15
for r in "${RPCS[@]}"; do echo "  $r h=$(height "$r")"; done

if [ "${ORGANIC:-0}" = 1 ]; then
    echo "== phase 2 (ORGANIC): single tiny action, then restart-all =="
    docker run --rm --network host --entrypoint /bench \
        -v "$REPO/target/release/bench-throughput:/bench:ro" \
        torus-devnet-node:local \
        consensus --rpc-urls "${RPCS[0]}" --batch-size 1 --senders 1 \
        --duration 3 --rate 1 --pre-sign 3 \
        --sign-mode session --markets 1 \
        --format bin 2>&1 | tee "$OUT/tiny-action.log" || true
    sleep 15
    for n in "${NODES[@]}"; do docker restart -t 0 "$n"; done
    sleep 30
else
    echo "== phase 2: stop mid-flight and inject a shared hole =="
    VICTIM=""
    for attempt in 1 2 3; do
        docker compose stop >/dev/null 2>&1
        if VICTIM=$(tool "${VOLS[0]}" torus-wedge-inject --data-dir /data 2>"$OUT/inject-pick.err"); then
            echo "  victim block: $VICTIM (picked on ${NODES[0]})"
            OK_ALL=1
            for i in 1 2 3 4; do
                if OUT_H=$(tool "${VOLS[$i]}" torus-wedge-inject --data-dir /data --hash "$VICTIM" 2>&1); then
                    echo "  ${NODES[$i]}: injected"
                else
                    echo "  ${NODES[$i]}: inject FAILED ($OUT_H) — retrying cycle"
                    OK_ALL=0; break
                fi
            done
            [ "$OK_ALL" = 1 ] && break
        else
            echo "  attempt $attempt: pick failed ($(cat "$OUT/inject-pick.err")) — retrying"
        fi
        VICTIM=""
        docker compose start >/dev/null 2>&1
        sleep 8
    done
    [ -n "$VICTIM" ] || { echo "FATAL: could not inject a shared hole in 3 attempts"; exit 1; }

    echo "== phase 3: restart into the wedge =="
    docker compose start
    sleep 30
fi

echo "== phase 4: verdict =="
H1=(); for r in "${RPCS[@]}"; do H1+=("$(height "$r")"); done
sleep 30
H2=(); for r in "${RPCS[@]}"; do H2+=("$(height "$r")"); done
FROZEN=1
for i in "${!RPCS[@]}"; do
    echo "  ${NODES[$i]}: h ${H1[$i]} -> ${H2[$i]}"
    [ "${H1[$i]}" != "${H2[$i]}" ] && FROZEN=0
done
HOLE=0
for n in "${NODES[@]}"; do
    if docker logs "$n" 2>&1 | grep -q "REFUSING to commit across a hole"; then
        echo "  $n: HOLE SIGNATURE present"
        HOLE=1
    fi
    docker logs "$n" >"$OUT/$n.log" 2>&1
done
if [ "$FROZEN" = 1 ] && [ "$HOLE" = 1 ]; then
    echo "WEDGE FORMED (frozen heights + commit-hole signature)"
else
    echo "WEDGE NOT FORMED (frozen=$FROZEN hole=$HOLE)"
    [ "${PROVE_UNWEDGE:-0}" = 1 ] && exit 1
fi

if [ "${PROVE_UNWEDGE:-0}" = 1 ]; then
    echo "== phase 5: unwedge proof =="
    docker compose stop
    PCFILE="$OUT/recovery-pc.bin"
    rm -f "$PCFILE"
    for i in "${!NODES[@]}"; do
        echo "  --- ${NODES[$i]} inspect ---"
        tool "${VOLS[$i]}" torus-unwedge --data-dir /data --inspect --json \
            | tee "$OUT/${NODES[$i]}-inspect.json"
        if [ ! -s "$PCFILE" ]; then
            tool "${VOLS[$i]}" torus-unwedge --data-dir /data --export-pc /out/recovery-pc.bin || true
        fi
    done
    [ -s "$PCFILE" ] || { echo "FATAL: no recovery PC harvestable on any node"; exit 1; }
    for i in "${!NODES[@]}"; do
        echo "  --- ${NODES[$i]} apply ---"
        tool "${VOLS[$i]}" torus-unwedge --data-dir /data --apply --pc-file /out/recovery-pc.bin --yes \
            | tee "$OUT/${NODES[$i]}-apply.log"
    done
    docker compose start
    sleep 30
    H3=(); for r in "${RPCS[@]}"; do H3+=("$(height "$r")"); done
    sleep 30
    OK=1
    for i in "${!RPCS[@]}"; do
        H4=$(height "${RPCS[$i]}")
        echo "  ${NODES[$i]}: h ${H3[$i]} -> $H4"
        [ "$H4" -le "${H3[$i]}" ] && OK=0
    done
    if [ "$OK" = 1 ]; then echo "UNWEDGE PROVEN: commits advancing on all nodes"; else echo "UNWEDGE FAILED: heights not advancing"; exit 1; fi
fi
