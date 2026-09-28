# s73: bad-minute bursts (per-minute cycle 900-1250 ms vs ~650-800)

No node code changed. Cells: s72-bst-20260928 (15), s70-ladv-20260927 (4),
s73-quiet-20260928 (2 scored), s73-xfs-20260928 (2 scored). Scripts:
`~/bench-results-matched/s73-analysis/` (README.txt lists them).

## 1. What a bad bin looks like (19 cells, 552 bins of 10 s, 71 bad)

Bad = val0 mean view length > 1.25x the cell median. Every view phase grows,
so it is a host-wide slowdown, not one code path:

| phase | normal ms | bad ms |
|---|---|---|
| leader build (start -> propose) | 330 | 468 |
| deliver (propose -> last voter rx) | 58 | 128 |
| vote (rx -> vote sent) | 80 | 136 |
| collect (last vote -> PC) | 91 | 143 |
| advance (PC -> next view start) | 103 | 137 |

The only host signal that tracks bad bins after removing each cell's linear
time trend is sda latency: w_await 16 -> 31 ms (19/19 cells), fsync await
4.6 -> 10 ms, r_await 0.2 -> 1.4 ms, while the nodes write less (wkB/s lower).
Ruled out: RocksDB write stalls (counters 0), L0/flush/compaction and dirty
pages (they only follow the within-run drift), algo-thread CPU run-queue wait,
WAL fsync (`TORUS_SYNC_WAL_ON_COMMIT` unset, no `set_sync`). The algo thread is
in D state only ~80 ms per 10 s, far below the ~3 s of extra view time per bad
bin, so how disk latency reaches consensus is still not pinned down.

## 2. Wall-clock periodicity

Pooled by second of the wall-clock minute (all cells): views/s 1.07 at
:00-:04 vs ~1.45 late in the minute (-26%), w_await 33 vs 13 ms. At 5-minute
boundaries: 0.94 views/s, w_await 50 ms, 14/18 bins bad. The dip lines up
with the wall clock (max/min 1.88 of ingest rate by 6 s bucket) and not with
node start (1.16), and node starts are spread over the minute. Ingest
(`torus_native_gossip_published_actions_total`) drops to 57% in :00-:09
(21/21 cells) and view timeouts double; the load is closed-loop (CONC=256), so
these follow the slowdown.

A 5-minute boundary adds ~7.5% to its bench minute and explains the worst
minute in 5/19 cells (srvd-r3 min 3, 1.47x). The rest of the "bad minutes" is
the within-run drift (later minutes slower) plus scattered disk-latency bins.

## 3. The source is outside the VM

- Quiet host A/B (s73-quiet, launchpad services + watchdog cron + idle Claude
  sessions stopped): the dip stays (0.96 views/s at :00-:04 vs 1.30-1.46).
- Kernel trace of every fsync/fdatasync/sync call on the host, idle, 6 min:
  nothing syncs on the minute (surrealkv ~0.7/s evenly, TORUSd twice).
- Idle host, O_DIRECT 4 KiB random reads every 100 ms for 5 min: 2.56 ms mean
  (p90 4.4, max 47) at :00-:04 vs 1.44-1.77 ms later in the minute, decaying
  over the minute like the views/s sawtooth.
- The box is a KVM VPS on a QEMU virtual disk; CPU steal is 0 in every cell.

Most likely other tenants' minute-aligned jobs on the shared storage. Not
fixable from inside the VM.

## 4. Hourly cron at :17 (fixed)

The provider image installed `/etc/cron.hourly/free` (`echo 1 >
/proc/sys/vm/drop_caches`) and `/etc/cron.hourly/fstrim` (`fstrim /`), run by
root at :17 every hour. They hit s72 srv-r3/srv-r4, s72-dpf-off-r1,
s70-qcsplit-qs-r2 and s73-quiet r1. drop_caches is removed and fstrim moved to
04:17 nightly (`/etc/cron.d/s73-fstrim-nightly`); originals in
`/root/s73-cron-backup/`.

## 5. Separate XFS filesystem for node data (not adopted)

Node DBs on XFS in a loop-mounted image (direct I/O), same binary and env:

| | ext4 (s73-quiet r1, r2) | XFS (s73-xfs x-r1, x-r2) |
|---|---|---|
| matched/s | 63.5k, 63.1k | 73.2k, 59.3k |
| views/s :00-:04 vs late minute | 0.96 vs ~1.3 | 1.13 vs ~1.5 |
| 190 s foreign fsync probe (5x 4 KiB/s on root ext4) | +9% view length vs rest of cell (r3) | -13% (no harm) |

The minute dip is the same on both. Throughput difference is inside cell
noise (+-6k); x-r2 ran with the root fs nearly full because the image was not
trimmed between cells. s73-quiet r3 (42.8k) is excluded: an unplanned fsync
probe ran over its first half.

## 6. Open

- Mechanism from disk latency to view length (algo D-state is too small).
- CPU: ~95% busy, run queue ~40 on 18 vCPUs during cells; the algo thread
  waits for a CPU about as long as it runs. A CPU-pinning A/B is the next
  lever inside our control.
- RocksDB block cache (256 MiB/node, `torus-state/src/db.rs:268`): add
  hit/miss metrics before changing the size.
- Bench: keep arms interleaved (the minute dip hits all arms alike); the
  quiet-host gate only checks load at cell start.
