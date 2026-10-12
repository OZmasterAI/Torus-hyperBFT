#!/usr/bin/env bash
# ozarchy-p3g2: build g1 (b1605417) and g2 (51a67f57) one after the other, then stage:
#   <key>-run/release/{torus-node,bench-throughput} for g1 g2 and m (main 8efcb07a node 09827336, reused from ozarchy-p3rss-stage/m-run);
#   bench = ozarchy-p3rss bench 05d44313 (8efcb07a, reused); g2-heap/release/torus-node = jemalloc prof wrapper (as p3rss).
set -u
R=/home/oz/bench-results-matched; B=$R/ozarchy-p3g2-tools/ozarchy-p3g2-build.sh; ST=$R/ozarchy-p3g2-stage; M=$R/ozarchy-p3g2-build/md5s.txt
fin() { echo "exit=$1" > $R/ozarchy-p3g2-build/all.done; exit "$1"; }
$B g1 b160541746aa3f7fc9591c83bc483c832fbb112e /home/oz/projects/wt/p3g2-b1605417 /home/oz/.cargo-target-p3g2-g1 || fin 1
$B g2 51a67f57faad37ae49f56133e816ec26dd9d816b /home/oz/projects/wt/p3g2-51a67f57 /home/oz/.cargo-target-p3g2-g2 || fin 1
PS=$R/ozarchy-p3rss-stage
[ "$(md5sum < $PS/b/release/bench-throughput | cut -c1-32)" = 05d44313c54313208207b9297e202384 ] || fin 2
[ "$(md5sum < $PS/m-run/release/torus-node | cut -c1-32)" = 098273361a2aae6b6c97f20b361d3b63 ] || fin 2
mkdir -p $ST/b/release $ST/m-run/release $ST/g1-run/release $ST/g2-run/release $ST/g2-heap/release
cp -p --reflink=auto $PS/b/release/bench-throughput $ST/b/release/bench-throughput
cp -p --reflink=auto $PS/m-run/release/torus-node $ST/m-run/release/torus-node
for k in g1 g2; do cp -p --reflink=auto $ST/$k/release/torus-node $ST/$k-run/release/torus-node; done
for k in m g1 g2; do cp -p --reflink=auto $ST/b/release/bench-throughput $ST/$k-run/release/bench-throughput; done
cp -p --reflink=auto $ST/b/release/bench-throughput $ST/g2-heap/release/bench-throughput
sed "s#/ozarchy-p3rss-stage/s2-run/#/ozarchy-p3g2-stage/g2-run/#; s#ozarchy-p3rss heap-profile wrapper#ozarchy-p3g2 heap-profile wrapper (copy of p3rss)#" $PS/s2-heap/release/torus-node > $ST/g2-heap/release/torus-node
chmod +x $ST/g2-heap/release/torus-node
echo "b 8efcb07a0a70e21395bf10b5816f1a6cbc9edfe4 05d44313c54313208207b9297e202384 (reused from ozarchy-p3rss-stage/b)" >> $M
echo "m 8efcb07a0a70e21395bf10b5816f1a6cbc9edfe4 098273361a2aae6b6c97f20b361d3b63 (reused from ozarchy-p3rss-stage/m-run)" >> $M
md5sum $ST/*-run/release/* | sed "s#$ST/##"; cat $ST/g2-heap/release/torus-node
fin 0
