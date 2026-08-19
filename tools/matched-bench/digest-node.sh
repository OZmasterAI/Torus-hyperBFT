#!/usr/bin/env bash
# digest-node.sh — ONE validator's RPC state digest, fanned out but ORDER-STABLE.
#
#   digest-node.sh <rpc-url> <markets> <accounts-file> <out-file> [PAR=8] [TIMEOUT=60]
#
# Writes the digest STREAM to <out-file> and prints "<sha256> <wall_seconds>".
#
# The stream is byte-identical to the pre-r6 serial loop in run-cell.sh:
#
#     market 1  torus_getOrderBook          (jq -cS of .result // .error)
#     market 1  torus_getOpenInterest
#     ...
#     market N  torus_getOpenInterest
#     account 1 torus_getBalances
#     ...
#
# WHY THIS FILE EXISTS: at 300 markets the serial loop is 650 `curl -m 10` round
# trips and takes ~4 min per node, so val0's digest was taken ~8 min before
# val2's. If anything at all still moved in between, three honest validators
# hash differently and a good candidate reads as a fork (r6-base-300m-r1).
# Per-market and per-account calls therefore run PAR-way parallel here, and
# run-cell.sh runs all three NODES concurrently, so the whole fleet is digested
# inside one short window. Determinism is preserved by writing each answer to a
# zero-padded part file and concatenating in glob order — never in completion
# order (which is what a naive `xargs -P ... | sha256sum` would give you; the
# unit test in test_harness.py answers high market ids first on purpose).
set -uo pipefail

[ $# -ge 4 ] || { sed -n '2,6p' "$0" >&2; exit 2; }
URL=$1
MARKETS=$2
ACCOUNTS=$3
OUTFILE=$4
PAR=${5:-8}
TIMEOUT=${6:-60}

[[ "$MARKETS" =~ ^[0-9]+$ && "$PAR" =~ ^[0-9]+$ && "$PAR" -ge 1 ]] || {
    echo "digest-node.sh: bad MARKETS/PAR" >&2; exit 2; }
[ -r "$ACCOUNTS" ] || { echo "digest-node.sh: cannot read accounts file $ACCOUNTS" >&2; exit 2; }
command -v jq >/dev/null || { echo "digest-node.sh: need jq" >&2; exit 2; }

export DIGEST_TIMEOUT=$TIMEOUT

# One RPC -> one compacted, key-SORTED JSON line. A dead/slow endpoint yields a
# stable sentinel instead of an empty line, so a truncated digest can never be
# mistaken for a matching one.
digest_call() { # $1=url $2=method $3=params-json
    local r
    r=$(curl -s -m "$DIGEST_TIMEOUT" -H 'content-type: application/json' "$1" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":$3}") || r=""
    if [ -z "$r" ]; then printf '"RPC_EMPTY"\n'; return; fi
    printf '%s' "$r" | jq -cS '.result // .error' 2>/dev/null || printf '"RPC_BADJSON"\n'
}
export -f digest_call

digest_market() { # $1=url $2=partdir $3=market-id
    local hx; hx=$(printf '0x%x' "$3")
    {
        digest_call "$1" torus_getOrderBook "[\"$hx\"]"
        digest_call "$1" torus_getOpenInterest "[\"$hx\"]"
    } > "$2/m$(printf '%08d' "$3")"
}
export -f digest_market

digest_account() { # $1=url $2=partdir $3=ordinal $4=address
    digest_call "$1" torus_getBalances "[\"$4\"]" > "$2/z$(printf '%08d' "$3")"
}
export -f digest_account

PARTS=$(mktemp -d "${TMPDIR:-/tmp}/digest-parts-XXXXXX") || exit 1
trap 'rm -rf "$PARTS"' EXIT

T0=$(date +%s.%N)
if [ "$MARKETS" -gt 0 ]; then
    seq 1 "$MARKETS" | xargs -r -P "$PAR" -I{} \
        bash -c 'digest_market "$0" "$1" "$2"' "$URL" "$PARTS" {}
fi
# `nl` numbers the accounts so the balance parts keep the file's own order.
grep -v '^[[:space:]]*$' "$ACCOUNTS" | nl -ba -w1 -s' ' \
    | xargs -r -P "$PAR" -n2 bash -c 'digest_account "$0" "$1" "$2" "$3"' "$URL" "$PARTS"
T1=$(date +%s.%N)

# Glob order == id order (zero padded), markets before accounts. NEVER completion order.
: > "$OUTFILE"
shopt -s nullglob
for f in "$PARTS"/m* "$PARTS"/z*; do cat "$f" >> "$OUTFILE"; done
shopt -u nullglob

SHA=$(sha256sum "$OUTFILE" | cut -d' ' -f1)
printf '%s %s\n' "$SHA" "$(awk -v a="$T0" -v b="$T1" 'BEGIN{printf "%.1f", b-a}')"
