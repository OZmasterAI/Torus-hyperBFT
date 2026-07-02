#!/usr/bin/env bash
# set-cap.sh — flip the COMPILE-TIME native block caps for a cap-raised probe.
#
# NATIVE_TOTAL_BLOCK_CAP (actions/block) and NATIVE_BLOCK_BYTES_CAP (body bytes/block)
# are `const` in crates/torus-mempool/src/rate_limit.rs (used at app.rs:1312-1314).
# There is NO runtime/genesis/CLI knob — raising the cap REQUIRES a rebuild. This
# script only patches the source (reversibly). It does NOT build or restart anything.
#
#   ./set-cap.sh --show                 # print current values
#   ./set-cap.sh 1000                   # action cap -> 1000, bytes cap unchanged (6MB edge)
#   ./set-cap.sh 1000 24000000          # action cap -> 1000, bytes cap -> 24MB (probe PAST the edge)
#   ./set-cap.sh --revert               # restore the pre-patch backup
#
# After patching, rebuild the target you will probe:
#   local single node : cargo build --release -p torus-node -p bench-throughput
#   docker devnet     : docker compose -f devnet/docker-compose.yml build
# ...then relaunch that node/devnet YOURSELF (this script never restarts anything).
set -euo pipefail
ROOT="$(git rev-parse --show-toplevel)"
F="$ROOT/crates/torus-mempool/src/rate_limit.rs"
BAK="$F.probebak"
A='NATIVE_TOTAL_BLOCK_CAP'
B='NATIVE_BLOCK_BYTES_CAP'

show() { grep -nE "^pub const ($A|$B): usize = " "$F"; }

case "${1:-}" in
  --show|"") echo "current caps ($F):"; show; exit 0 ;;
  --revert)
    [ -f "$BAK" ] || { echo "no backup at $BAK — nothing to revert (or use: git checkout -- $F)"; exit 1; }
    mv "$BAK" "$F"; echo "reverted from backup:"; show; exit 0 ;;
esac

CAP="$1"
[[ "$CAP" =~ ^[0-9]+$ ]] || { echo "action cap must be an integer, got '$CAP'"; exit 1; }
[ -f "$BAK" ] || cp "$F" "$BAK"   # keep the FIRST (pristine) backup, don't overwrite on re-run

sed -i -E "s/^pub const $A: usize = .*;/pub const $A: usize = $CAP;/" "$F"
if [ "${2:-}" != "" ]; then
  BYTES="$2"; [[ "$BYTES" =~ ^[0-9_]+$ ]] || { echo "bytes cap must be an integer"; exit 1; }
  sed -i -E "s/^pub const $B: usize = .*;/pub const $B: usize = $BYTES;/" "$F"
fi

echo "patched (backup at $BAK):"; show
echo
echo "NEXT: rebuild, then relaunch the target yourself. Revert with: $0 --revert"
