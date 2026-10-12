#!/usr/bin/env bash
# build b (bench 8efcb07a), m (node 8efcb07a), p (node efcd1548) one after the other; writes ozarchy-p3rss-build/all.done
B=/home/oz/bench-results-matched/ozarchy-p3rss-tools/ozarchy-p3rss-build.sh
M=8efcb07a0a70e21395bf10b5816f1a6cbc9edfe4; P=efcd15481891dd29c9ec87c01ac679b897c25421
$B b $M /home/oz/projects/wt/p3rss-8efcb07a /home/oz/.cargo-target-p3rss-b && \
$B m $M /home/oz/projects/wt/p3rss-8efcb07a /home/oz/.cargo-target-p3rss-m && \
$B p $P /home/oz/projects/wt/p23b-efcd1548 /home/oz/.cargo-target-p23b-p
echo "exit=$?" > /home/oz/bench-results-matched/ozarchy-p3rss-build/all.done
