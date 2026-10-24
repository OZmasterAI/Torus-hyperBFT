#!/usr/bin/env bash
# check.sh — validate the testnet genesis drafts against main's genesis loader
# (the torus-node binary) and boot each one as a local devnet.
#
#   TORUS_NODE_BIN=/path/to/torus-node tools/genesis-draft/check.sh
#
# 1. shape:   each draft names the owner's decisions (testnet/testnet-genesis-draft.md):
#             explicit activations {running_state_hash: 1, book_window_commit: 1} and no
#             other feature, Classic books, no reputation leader selection, evm.block_interval
#             11, evm.chain_id == chain_id; dead-leader K = 0 at 3 validators, K = 3 / cap 6
#             at 4 and 5; the variants differ only in validators and K.
# 2. refuse:  the node refuses every draft as checked in, and (on the 5-validator draft,
#             which has every placeholder) each placeholder left alone while all the others
#             are filled with dummy values (tools/genesis-draft/fill-dummy.py).
# 3. boot:    each draft filled with dummy devnet values starts N local validators (keys
#             01..0N, attesting with their account keys) and all reach TARGET committed blocks.
#             Every node started is killed on exit.
#
# Env: TORUS_NODE_BIN (default $CARGO_TARGET_DIR/release/torus-node), VARIANTS (default
# "3 4 5"), TARGET (committed blocks per node, default 5), BOOT_TIMEOUT (s, default 180),
# WORK (scratch dir, default a new mktemp -d), SKIP_REFUSE=1 (skip step 2), ATTEST_HEIGHT (default
# 250; 0 = skip): also wait until every node is at this height and require each node to have
# submitted >= 1 state hash attestation, with no hash mismatch and no hash-unverified node.
# Localhost only; ports p2p 30461.., rpc 8661.., metrics 9171.. (clear of devnet/wsl).
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN="${TORUS_NODE_BIN:-${CARGO_TARGET_DIR:-$REPO/target}/release/torus-node}"
VARIANTS="${VARIANTS:-3 4 5}"
TARGET="${TARGET:-5}"
BOOT_TIMEOUT="${BOOT_TIMEOUT:-300}"
ATTEST_HEIGHT="${ATTEST_HEIGHT:-250}"
WORK="${WORK:-$(mktemp -d /tmp/genesis-draft-check.XXXXXX)}"
FILL="$REPO/tools/genesis-draft/fill-dummy.py"
DRAFT() { printf '%s' "$REPO/testnet/testnet-genesis-draft-${1}val.json"; }
[ -x "$BIN" ] || { echo "FATAL: no torus-node at $BIN (set TORUS_NODE_BIN)" >&2; exit 1; }
command -v jq >/dev/null || { echo "FATAL: jq not installed" >&2; exit 1; }
mkdir -p "$WORK"

# Devnet consensus seeds 01..05 (devnet/wsl/env.sh KEY0..) and the libp2p ids of 01 / 02.
KEYS=(0100000000000000000000000000000000000000000000000000000000000000
      0200000000000000000000000000000000000000000000000000000000000000
      0300000000000000000000000000000000000000000000000000000000000000
      0400000000000000000000000000000000000000000000000000000000000000
      0500000000000000000000000000000000000000000000000000000000000000)
PID0=12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X
PID1=12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3
# Validator ACCOUNT keys (attestation), matching fill-dummy.py ACCOUNT_ADDRESSES. INSECURE.
ATTEST=($(jq -r '.validators | sort_by(.index) | .[].private_key' "$REPO/devnet/wsl/keys-4val/bench-validator-keys.json")
        0x0505050505050505050505050505050505050505050505050505050505050505)
[ "${#ATTEST[@]}" = 5 ] || { echo "FATAL: expected 4 bench validator account keys" >&2; exit 1; }

PIDS=()
cleanup() {
    local p
    for p in "${PIDS[@]+"${PIDS[@]}"}"; do kill "$p" 2>/dev/null || true; done
    for p in "${PIDS[@]+"${PIDS[@]}"}"; do
        for _ in $(seq 1 50); do kill -0 "$p" 2>/dev/null || break; sleep 0.2; done
        kill -9 "$p" 2>/dev/null || true
    done
    PIDS=()
}
trap cleanup EXIT

node_argv() { # idx genesis datadir -> NODE_ARGV
    local i=$1
    local peers="/ip4/127.0.0.1/udp/30461/quic-v1/p2p/$PID0"
    [ "$i" = 0 ] && peers="/ip4/127.0.0.1/udp/30462/quic-v1/p2p/$PID1"
    [ "$i" -ge 2 ] && peers="$peers,/ip4/127.0.0.1/udp/30462/quic-v1/p2p/$PID1"
    NODE_ARGV=(--genesis="$2" --data-dir="$3" --validator-key="${KEYS[$i]}"
        --p2p-listen="/ip4/127.0.0.1/udp/$((30461 + i))/quic-v1" --p2p-private-addrs
        --p2p-peers="$peers" --rpc-addr="127.0.0.1:$((8661 + i))"
        --metrics-addr="127.0.0.1:$((9171 + i))" --log-level=info)
}

# prefix match: a counter `x` is exposed as `x_total`.
metric_prefix() { # port prefix -> integer (sum of every series whose name starts with prefix)
    { curl -s -m 3 "http://127.0.0.1:$1/metrics" 2>/dev/null || true; } \
        | awk -v m="$2" '$1 !~ /^#/ && index($1, m)==1 && $1 !~ /_created/ {s+=$2} END{printf "%.0f", s+0}'
}

metric() { # port name -> integer (0 if absent or the node is not up yet)
    { curl -s -m 3 "http://127.0.0.1:$1/metrics" 2>/dev/null || true; } \
        | awk -v m="$2" '$1==m || index($1, m"{")==1 {s+=$2} END{printf "%.0f", s+0}'
}

fail=0
echo "binary: $BIN (md5 $(md5sum "$BIN" | cut -c1-32)); repo HEAD $(git -C "$REPO" rev-parse --short HEAD)"
echo "work:   $WORK"

# 1. Shape -------------------------------------------------------------------------------
echo "=== 1. shape ==="
for n in $VARIANTS; do
    want_k=3; [ "$n" = 3 ] && want_k=0
    if jq -e --argjson n "$n" --argjson k "$want_k" '
        (.validators | length) == $n
        and .consensus.activations == {running_state_hash: 1, book_window_commit: 1}
        and (.consensus | has("state_hash_activation_height") or has("book_window_commit") | not)
        and .consensus.book_mode == "classic"
        and .consensus.reputation_leader_selection == false
        and .consensus.leader_miss_threshold == $k
        and .consensus.leader_exclusion_cap == 6
        and .evm.block_interval == 11
        and .evm.chain_id == .chain_id' "$(DRAFT "$n")" >/dev/null; then
        echo "  ${n}val: ok (validators $n, K $want_k, activations hash + L6.1 from block 1)"
    else
        echo "  ${n}val: FAIL shape"; fail=1
    fi
done
strip='del(.validators, .consensus.leader_miss_threshold)'
for n in $VARIANTS; do
    if [ "$(jq -S "$strip" "$(DRAFT "$n")")" != "$(jq -S "$strip" "$(DRAFT 4)")" ]; then
        echo "  ${n}val: FAIL differs from 4val outside validators / leader_miss_threshold"; fail=1
    fi
done

# 2. Refusal -----------------------------------------------------------------------------
# run_refused <label> <genesis>: the node must exit non-zero on its own (not by timeout).
run_refused() {
    local label=$1 gen=$2 dd="$WORK/refuse/$1" log="$WORK/refuse/$1.log" rc=0
    mkdir -p "$dd"
    node_argv 0 "$gen" "$dd"
    timeout -s KILL 60 "$BIN" "${NODE_ARGV[@]}" >"$log" 2>&1 || rc=$?
    local why
    why=$(grep -m1 -E 'refusing|Error|error|panicked' "$log" | sed -E 's/\x1b\[[0-9;]*m//g' | cut -c1-200)
    if [ "$rc" != 0 ] && [ "$rc" != 137 ]; then
        printf '  %-28s refused (exit %s): %s\n' "$label" "$rc" "$why"
    else
        printf '  %-28s NOT REFUSED (exit %s)\n' "$label" "$rc"; fail=1
    fi
}
if [ "${SKIP_REFUSE:-0}" != 1 ]; then
    echo "=== 2. refusal ==="
    for n in $VARIANTS; do run_refused "draft-${n}val-as-checked-in" "$(DRAFT "$n")"; done
    for key in $(python3 -I "$FILL" --list "$(DRAFT 5)"); do
        python3 -I "$FILL" "$(DRAFT 5)" "$WORK/leave-$key.json" --leave "$key"
        run_refused "only FILL:$key" "$WORK/leave-$key.json"
    done
fi

# 3. Boot --------------------------------------------------------------------------------
echo "=== 3. boot (TARGET=$TARGET committed blocks per node) ==="
for n in $VARIANTS; do
    gen="$WORK/filled-${n}val.json"
    python3 -I "$FILL" "$(DRAFT "$n")" "$gen"
    run="$WORK/boot-${n}val"
    mkdir -p "$run"
    for i in $(seq 0 $((n - 1))); do
        (umask 077; printf '%s\n' "${ATTEST[$i]}" > "$run/val$i.key")
        node_argv "$i" "$gen" "$run/data$i"
        "$BIN" "${NODE_ARGV[@]}" --state-hash-attest-key="$run/val$i.key" >"$run/val$i.log" 2>&1 &
        PIDS+=($!)
    done
    deadline=$((SECONDS + BOOT_TIMEOUT)); ok=0
    while [ "$SECONDS" -lt "$deadline" ]; do
        lo=999999999
        for i in $(seq 0 $((n - 1))); do
            c=$(metric $((9171 + i)) torus_blocks_committed_total)
            [ "$c" -lt "$lo" ] && lo=$c
        done
        if [ "$lo" -ge "$TARGET" ]; then
            [ "$ATTEST_HEIGHT" = 0 ] && { ok=1; break; }
            hlo=999999999
            for i in $(seq 0 $((n - 1))); do
                h=$(metric $((9171 + i)) torus_block_height)
                [ "$h" -lt "$hlo" ] && hlo=$h
            done
            [ "$hlo" -ge "$ATTEST_HEIGHT" ] && { ok=1; break; }
        fi
        alive=0; for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && alive=$((alive + 1)); done
        [ "$alive" = "$n" ] || break
        sleep 2
    done
    for i in $(seq 0 $((n - 1))); do
        printf '  %sval node%s: committed=%s height=%s peers=%s | %s\n' "$n" "$i" \
            "$(metric $((9171 + i)) torus_blocks_committed_total)" \
            "$(metric $((9171 + i)) torus_block_height)" \
            "$(metric $((9171 + i)) torus_peers_connected)" \
            "$(grep -m1 -oE 'genesis digest stored[^"]*' "$run/val$i.log" | cut -c1-60)"
    done
    if [ "$ATTEST_HEIGHT" != 0 ]; then
        for i in $(seq 0 $((n - 1))); do
            sub=$(metric_prefix $((9171 + i)) torus_state_hash_attestations_submitted)
            mis=$(metric_prefix $((9171 + i)) torus_state_hash_mismatch)
            noq=$(metric_prefix $((9171 + i)) torus_state_hash_no_quorum)
            unv=$(metric_prefix $((9171 + i)) torus_state_hash_unverified)
            hh=$(metric_prefix $((9171 + i)) torus_state_hash_height)
            na=$(grep -c 'attestation not admitted' "$run/val$i.log" || true)
            printf '  %sval node%s: attestations_submitted=%s hash_height=%s mismatch=%s no_quorum=%s unverified=%s not_admitted_lines=%s\n' \
                "$n" "$i" "$sub" "$hh" "$mis" "$noq" "$unv" "$na"
            { [ "$sub" -ge 1 ] && [ "$mis" = 0 ] && [ "$unv" = 0 ]; } || ok=0
        done
    fi
    sed -E 's/\x1b\[[0-9;]*m//g' "$run/val0.log" \
        | grep -m4 -oE '(running state hash: [^,]{0,90}|L3.1 coalesced Classic book save (ON|OFF)[^,]{0,60}|epoch plan hand-off[^,]{0,40})' \
        | sed 's/^/    node0 log: /' || true
    if [ "$ok" = 1 ]; then
        echo "  ${n}val: BOOT OK (every node >= $TARGET committed blocks; attest check at height $ATTEST_HEIGHT: ${ATTEST_HEIGHT/#0/skipped})"
    else
        echo "  ${n}val: BOOT FAIL (see $run/val*.log)"; fail=1
    fi
    cleanup
    left=$(pgrep -f -- "--data-dir=$run/" || true)
    [ -z "$left" ] || { echo "  ${n}val: FAIL nodes left running: $left"; fail=1; }
done

[ "$fail" = 0 ] && echo "RESULT: PASS" || echo "RESULT: FAIL"
exit "$fail"
