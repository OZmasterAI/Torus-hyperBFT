#!/bin/bash
# ozarchy-p3fsoak jemalloc wrapper (same as ozarchy-p3q-stage/g2-je, ozarchy-p3soakje): system /usr/lib/libjemalloc.so.2 (5.3.1) preloaded with its
# DEFAULTS (no MALLOC_CONF, no /etc/malloc.conf, profiling off), then exec the real node (same pid, /proc/<pid>/exe = the real binary, comm torus-node).
export LD_PRELOAD=/usr/lib/libjemalloc.so.2
unset MALLOC_CONF
exec /home/oz/bench-results-matched/ozarchy-p3q-stage/cs2-run/release/torus-node "$@"
