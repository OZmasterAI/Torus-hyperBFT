#!/usr/bin/env bash
# ozarchy-p3next: build control (m) and step 2 (g2) one after the other into fresh target dirs, then stage
#   ozarchy-p3next-stage/{b,m-run,g2-run,m-je,g2-je}: bench = ozarchy-p3rss bench 05d44313 (8efcb07a, reused, the load generator of
#   sections 48-49); <key>-je = jemalloc no-prof wrapper (LD_PRELOAD only, defaults).
# Usage: build-stage.sh CTL_SHA S2_SHA   (40 hex each; worktrees wt/p3next-ctl-<8>, wt/p3next-s2-<8> are created detached if absent)
set -u
CTL=${1:?ctl sha}; S2=${2:?step 2 sha}
R=/home/oz/bench-results-matched; B=$R/ozarchy-p3next-tools/ozarchy-p3next-build.sh; ST=$R/ozarchy-p3next-stage; M=$R/ozarchy-p3next-build/md5s.txt
REPO=/home/oz/projects/torus-economy/Torus-hyperBFT
fin() { echo "exit=$1" > $R/ozarchy-p3next-build/all.done; exit "$1"; }
WC=/home/oz/projects/wt/p3next-ctl-${CTL:0:8}; WS=/home/oz/projects/wt/p3next-s2-${S2:0:8}
[ -d "$WC" ] || git -C "$REPO" worktree add --detach "$WC" "$CTL" || fin 3
[ -d "$WS" ] || git -C "$REPO" worktree add --detach "$WS" "$S2" || fin 3
$B m "$CTL" "$WC" /home/oz/.cargo-target-p3next-m-${CTL:0:8} || fin 1
$B g2 "$S2" "$WS" /home/oz/.cargo-target-p3next-g2-${S2:0:8} || fin 1
PS=$R/ozarchy-p3rss-stage
[ "$(md5sum < $PS/b/release/bench-throughput | cut -c1-32)" = 05d44313c54313208207b9297e202384 ] || fin 2
mkdir -p $ST/b/release
cp -p --reflink=auto $PS/b/release/bench-throughput $ST/b/release/bench-throughput
for k in m g2; do
    mkdir -p $ST/$k-run/release $ST/$k-je/release
    cp -p --reflink=auto $ST/$k/release/torus-node $ST/$k-run/release/torus-node
    cp -p --reflink=auto $ST/b/release/bench-throughput $ST/$k-run/release/bench-throughput
    cp -p --reflink=auto $ST/b/release/bench-throughput $ST/$k-je/release/bench-throughput
    cat > $ST/$k-je/release/torus-node <<W
#!/bin/bash
# ozarchy-p3next jemalloc wrapper: system /usr/lib/libjemalloc.so.2 (5.3.1) preloaded with its DEFAULTS (no MALLOC_CONF, no
# /etc/malloc.conf, profiling off), then exec the real node (same pid, /proc/<pid>/exe = the real binary, comm torus-node).
export LD_PRELOAD=/usr/lib/libjemalloc.so.2
unset MALLOC_CONF
exec $ST/$k-run/release/torus-node "\$@"
W
    chmod +x $ST/$k-je/release/torus-node
done
echo "b 8efcb07a0a70e21395bf10b5816f1a6cbc9edfe4 05d44313c54313208207b9297e202384 (reused from ozarchy-p3rss-stage/b)" >> $M
echo "merge-base(step 2 $S2, origin/main) = $(git -C "$REPO" merge-base "$S2" origin/main); merge-base(step 2, control) = $(git -C "$REPO" merge-base "$S2" "$CTL")"
md5sum $ST/*-run/release/* | sed "s#$ST/##"
fin 0
