#!/bin/bash
# ozarchy-p3rss heap-profile wrapper: jemalloc (system /usr/lib/libjemalloc.so.2, 5.3.1, prof enabled) preloaded with sampled
# heap profiling (1 sample per 512 KiB allocated; a dump every 2^lg_prof_interval bytes allocated), then exec the real node
# (same pid, /proc/<pid>/exe = the real binary, comm torus-node). P3RSS_HEAP_PREFIX comes from the campaign (cell dir).
export LD_PRELOAD=/usr/lib/libjemalloc.so.2
export MALLOC_CONF="prof:true,prof_active:true,lg_prof_sample:19,lg_prof_interval:${P3RSS_LG_PROF_INTERVAL:-33},prof_prefix:${P3RSS_HEAP_PREFIX:?}"
exec /home/oz/bench-results-matched/ozarchy-p3rss-stage/s2-run/release/torus-node "$@"
