#!/usr/bin/env bash
set -u
D=/home/oz/bench-results-matched/ubench-adl-dirty-check
export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes" CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
for x in "A /home/oz/projects/wt/adl-budget /home/oz/.cargo-target-adl-budget" "B /home/oz/projects/wt/adl-dirty-check /home/oz/.cargo-target-adl-dirty-check"; do
  set -- $x
  echo "== build $1 $(date +%T)"
  (cd "$2" && CARGO_TARGET_DIR="$3" cargo test -p torus-bridge --release --test ubench_adl --no-run --message-format=json-render-diagnostics > "$D/build-$1.json") || { echo "build $1 FAILED"; exit 1; }
  exe=$(python3 -I -c 'import json,sys
for l in open(sys.argv[1]):
    try: j=json.loads(l)
    except Exception: continue
    if j.get("reason")=="compiler-artifact" and j.get("executable") and j["target"]["name"]=="ubench_adl": print(j["executable"])' "$D/build-$1.json" | tail -1)
  echo "   $1 exe=$exe"
  cp -p "$exe" "$D/bin/$1/ubench_adl"
done
echo "== end $(date +%T)"
