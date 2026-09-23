# Sourced by every s63+ benchmark driver.
# 1) One global lock: only one benchmark driver runs at a time; others queue.
# 2) bench_wait_quiet: before each cell, wait until no node, generator, cargo
#    or rustc process exists, so builds never overlap a measured cell.
BENCH_GLOBAL_LOCK=/home/18c/bench-results-matched/.bench-global.lock
exec 8>"$BENCH_GLOBAL_LOCK"
echo "$(date +%T) waiting for global bench lock" >&2
flock 8
echo "$(date +%T) holding global bench lock (pid $$)" >&2

bench_wait_quiet() {
  while pgrep -x torus-node >/dev/null || pgrep -x bench-throughpu >/dev/null \
     || pgrep -x cargo >/dev/null || pgrep -x rustc >/dev/null; do
    sleep 30
  done
}
