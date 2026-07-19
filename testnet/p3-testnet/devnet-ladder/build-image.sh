#!/usr/bin/env bash
# Build torus-node for the CURRENT branch and bake it into a per-branch image tag.
#
# ============================ WHY THIS EXISTS ==============================
# THE STALE-BINARY TRAP (cost a full A/B round to discover, 2026-07-19):
#
#   ~/.cargo/config.toml may set `target-dir` to a SHARED location (e.g.
#   /home/<user>/.cargo-target). When it does, cargo NEVER writes to
#   <repo>/target/. But devnet/Dockerfile does `COPY target/release/torus-node`,
#   relative to the repo build context — where stale hand-copied binaries can sit
#   (a `target/release/` holding ONLY loose binaries, with no deps/ or
#   .fingerprint/ dir, was never a cargo output dir).
#
#   Building the image naively then bakes the STALE binary — and, worse, the SAME
#   stale binary for EVERY branch. The resulting A/B looks clean and measures
#   nothing: a guaranteed false null.
#
#   Symptom: `cargo build` prints "Finished" and compiles crates, yet
#   <repo>/target/release/torus-node keeps its old mtime and md5.
#
# This script stages the freshly built binary into the build context and then
# VERIFIES, by md5, what actually landed INSIDE the image. Per-branch tags mean a
# mis-tagged run cannot silently reuse another branch's image.
# ===========================================================================
#
# Usage: ./build-image.sh <tag-suffix>          e.g. ./build-image.sh p3
# Env:   CARGO_OUT   override the cargo output binary path (auto-detected)
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
SUFFIX=${1:?usage: build-image.sh <tag-suffix>}
IMAGE="torus-devnet-node:ladder-$SUFFIX"

cd "$REPO"
BRANCH=$(git rev-parse --abbrev-ref HEAD)
COMMIT=$(git rev-parse --short HEAD)
echo "=== building $BRANCH ($COMMIT) -> $IMAGE ==="

nice -n 5 cargo build --release -p torus-node

# Resolve where cargo ACTUALLY wrote, honouring a shared target-dir.
if [ -z "${CARGO_OUT:-}" ]; then
    TARGET_DIR=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
        | python3 -c 'import sys,json;print(json.load(sys.stdin)["target_directory"])' 2>/dev/null || echo "$REPO/target")
    CARGO_OUT="$TARGET_DIR/release/torus-node"
fi
[ -f "$CARGO_OUT" ] || { echo "FATAL: cargo produced no binary at $CARGO_OUT"; exit 1; }
echo "cargo output: $CARGO_OUT"

install -Dm755 "$CARGO_OUT" "$REPO/target/release/torus-node"
STAGED_MD5=$(md5sum "$REPO/target/release/torus-node" | cut -d' ' -f1)
echo "staged md5=$STAGED_MD5"

sudo -n docker build -f devnet/Dockerfile -t "$IMAGE" "$REPO"

# The only check that matters: what is actually in the image.
IMG_MD5=$(sudo -n docker run --rm --entrypoint md5sum "$IMAGE" /usr/local/bin/torus-node | cut -d' ' -f1)
[ "$IMG_MD5" = "$STAGED_MD5" ] || { echo "FATAL: image binary ($IMG_MD5) != staged binary ($STAGED_MD5)"; exit 1; }

echo "OK: $IMAGE  branch=$BRANCH commit=$COMMIT md5=$IMG_MD5"
echo "$IMAGE $BRANCH $COMMIT $IMG_MD5" >> "$HERE/image-manifest.txt"
