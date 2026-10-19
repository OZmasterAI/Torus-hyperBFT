# ozarchy-8r raw data (archy, s68)

Text-only per-cell data of bench queue step 8r (perf doc section 103): perf/rh4-gc-worker a0181b8b, tools frozen from
main d1efbfa6, standard load, 300 markets, 300 s cells, warm-up then A B C C B A twice (n = 4 per arm).
A = no retention; B = TORUS_RETENTION_BLOCKS=1000 (worker GC); C = TORUS_RETENTION_BLOCKS=1000 + TORUS_DA_GC_INLINE=1.

- `ozarchy-8r/ozarchy-8r-300m-<cell>/`: one dir per cell (summary.json, run.log, bench.log, metrics before / after /
  1 Hz, RocksDB LOG dumps, samplers, node logs `val*.log.gz`, state digests). Files over 1 MB are gzipped (`*.gz`).
- `ozarchy-8r/ozarchy-8r-run/`: campaign log, cells index, smokes, analysis (`analyst/r8.txt` + script).
- `ozarchy-8r/ozarchy-8r-tools/`: the frozen spec, checks, driver and harness.
- `ozarchy-8r/stage-md5s.txt`: binary md5s.

Excluded: node DBs, binaries, genesis files (genesis md5 is in each cell's summary.json / run.log).
