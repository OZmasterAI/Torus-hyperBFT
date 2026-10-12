#!/usr/bin/env bash
# Stage the run dirs for ozarchy-p3rss / ozarchy-p23b after the builds (b, m, p) are done:
#   <key>-run/release/{torus-node,bench-throughput}: node per binary key + the one bench (8efcb07a), reflink copies.
#   Reused nodes (same build flags: frame pointers, line tables, mold; md5 checked against their build record):
#     c  2725e321 <- ozarchy-p3c1-stage/c   (p3c1 record 18fb490d)
#     s1 a44bdde6 <- ozarchy-p3r1-stage/s   (p3r1 record 7d1a0874)
#     s2 2296be67 <- ozarchy-p3s2g-stage/s  (p3s2g record 1c02893b)
#     k  c97f56f9 <- ozarchy-p3s2g-stage/c  (p3s2g record f35fab7d)
#   Built here: m 8efcb07a, p efcd1548 (ozarchy-p3rss-stage/{m,p}), b bench 8efcb07a.
#   <key>-heap/release/torus-node = wrapper (jemalloc LD_PRELOAD + prof) that execs <key>-run/release/torus-node, for c s1 s2.
set -eu
R=/home/oz/bench-results-matched; ST=$R/ozarchy-p3rss-stage; M=$R/ozarchy-p3rss-build/md5s.txt
die() { echo "STAGE FAIL: $*"; exit 1; }
BENCH=$ST/b/release/bench-throughput
[ -x "$BENCH" ] || die "no bench"
reuse() { # key sha src_node rec_md5 provenance
    local k=$1 sha=$2 src=$3 rec=$4 prov=$5 d=$ST/$1-run/release
    [ "$(md5sum < "$src" | cut -c1-32)" = "$rec" ] || die "$src md5 != $rec"
    mkdir -p "$d"; [ -e "$d/torus-node" ] || cp -p --reflink=auto "$src" "$d/torus-node"
    grep -q "^$k $sha " "$M" || echo "$k $sha $rec $(readelf -n "$d/torus-node" | awk '/Build ID/{print $3}') (reused from $prov)" >> "$M"
}
reuse c  2725e321bdc48f9ca137166f272b8eaefbd5ba3e $R/ozarchy-p3c1-stage/c/release/torus-node  18fb490d5f82cc1aaed299687a060f56 "ozarchy-p3c1-stage/c, p3c1 build 2026-10-11 19:17"
reuse s1 a44bdde6d25cd3bb80723eaed961b2ef96052eca $R/ozarchy-p3r1-stage/s/release/torus-node  7d1a08743778d315ed0c7a21eaeb8f6c "ozarchy-p3r1-stage/s, p3r1 build 2026-10-11 23:55"
reuse s2 2296be6727eb712f4c41f5150c5b834cc939b415 $R/ozarchy-p3s2g-stage/s/release/torus-node 1c02893bcf90c8bdd3b85d4b387ca53b "ozarchy-p3s2g-stage/s, p3s2g build 2026-10-12 12:22"
reuse k  c97f56f927994d88a2a00f63d147ce3cfaf0ae6b $R/ozarchy-p3s2g-stage/c/release/torus-node f35fab7d78b1654fb0fcc6146a5069e8 "ozarchy-p3s2g-stage/c, p3s2g build 2026-10-12 12:20"
for k in m p; do
    d=$ST/$k-run/release; mkdir -p "$d"
    [ -e "$d/torus-node" ] || cp -p --reflink=auto "$ST/$k/release/torus-node" "$d/torus-node"
done
for k in c s1 s2 m k p; do
    d=$ST/$k-run/release; [ -e "$d/bench-throughput" ] || cp -p --reflink=auto "$BENCH" "$d/bench-throughput"
done
for k in c s1 s2; do
    d=$ST/$k-heap/release; mkdir -p "$d"
    [ -e "$d/bench-throughput" ] || cp -p --reflink=auto "$BENCH" "$d/bench-throughput"
    cat > "$d/torus-node" <<W
#!/bin/bash
# ozarchy-p3rss heap-profile wrapper: jemalloc (system /usr/lib/libjemalloc.so.2, 5.3.1, prof enabled) preloaded with sampled
# heap profiling (1 sample per 512 KiB allocated; a dump every 2^lg_prof_interval bytes allocated), then exec the real node
# (same pid, /proc/<pid>/exe = the real binary, comm torus-node). P3RSS_HEAP_PREFIX comes from the campaign (cell dir).
export LD_PRELOAD=/usr/lib/libjemalloc.so.2
export MALLOC_CONF="prof:true,prof_active:true,lg_prof_sample:19,lg_prof_interval:\${P3RSS_LG_PROF_INTERVAL:-33},prof_prefix:\${P3RSS_HEAP_PREFIX:?}"
exec $ST/$k-run/release/torus-node "\$@"
W
    chmod +x "$d/torus-node"
done
md5sum $ST/*-run/release/* | sed "s#$ST/##"
echo "STAGE OK"
