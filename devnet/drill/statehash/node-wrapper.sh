#!/usr/bin/env bash
# node-wrapper.sh — used as $BIN by drill.sh so devnet/wsl/start-node.sh keeps
# its exact argv; appends --state-hash-attest-key for the validator whose
# --data-dir ends in val<N> (key file: $DRILL_KEYS/attest<N>.key, mode 0600).
set -euo pipefail
: "${NODE_BIN:?}" "${DRILL_KEYS:?}"
idx=""
for a in "$@"; do
    case "$a" in --data-dir=*val[0-9]) idx=${a##*val} ;; esac
done
[ -n "$idx" ] || { echo "node-wrapper: no --data-dir=...val<N> in argv" >&2; exit 1; }
exec "$NODE_BIN" "$@" --state-hash-attest-key="$DRILL_KEYS/attest$idx.key"
