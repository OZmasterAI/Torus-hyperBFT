# R01 read-only case: the user's steps

Node: `fix/r01-write-fail-stop` @ 01d991ab, binary `/home/oz/.cargo-target-r01-fault/release/torus-node`.
Script: `/home/oz/r01-fault/ro-case.sh <native|none> [label]` (pipeline off; v3 is the faulted
validator, data dir on `/mnt/r01/ro-<load>-<label>/v3`). It never runs sudo. Results go to
`/home/oz/r01-fault/results/ro-<load>-<label>/SUMMARY.txt`.

Run every step as `oz` in a terminal; the `sudo` lines ask for your password.
Checked on 2026-10-09: `/sys/fs/ext4/loop0/trigger_fs_error` exists (root, write-only) and the
fs is `/dev/loop0` = `/home/oz/r01-fault/r01.img`, superblock "Errors behavior: Continue",
mounted `rw,relatime`. Nothing else may be running on /mnt/r01 (no full-disk case, no
`/mnt/r01/fill`).

## 0. Free space on /mnt/r01 first (optional, recommended)

The full-disk cases left about 2.6 GiB of data dirs on /mnt/r01 (1.2 GiB free; a native RO
run writes about 0.8 GiB). Delete the old case dirs as oz before the RO run (the agent does
not do recursive deletes):
`/mnt/r01/{case1-pipeoff-native,c1-pipeoff-native,c1b-pipeoff-native-catchup,c1c-pipeoff-native-bs1,c1d-pipeoff-native-heavy,c2-pipeoff-empty,c2b-pipeoff-empty-starve,c3-pipeon-boundary-empty,c3b-pipeon-boundary-empty,c3-pipeon-boundary-native,ro-native-dryfill}`.

## 1. Before the run: make an ext4 error remount the fs read-only

```
sudo mount -o remount,errors=remount-ro /mnt/r01
findmnt -n -o OPTIONS /mnt/r01        # must show rw,...,errors=remount-ro
```

`ro-case.sh` refuses to start without `errors=remount-ro` in the mount options.

## 2. Start the run

```
/home/oz/r01-fault/ro-case.sh none 1      # empty blocks (marker-only flush path)
/home/oz/r01-fault/ro-case.sh native 1    # native orders (state + marker flush path)
```

Each run takes about 1 min to start the 4 validators and warm up, then prints

```
READY: ask user to force ro now   (echo r01 | sudo tee /sys/fs/ext4/loop0/trigger_fs_error)
```

## 3. Force the fs error (read-only) when READY is printed

Either by hand, as soon as READY appears:

```
echo r01 | sudo tee /sys/fs/ext4/loop0/trigger_fs_error
```

or armed in a second terminal right after starting step 2 (it fires the moment the script
creates the trigger file, so you do not need to watch for READY):

```
sudo bash -c 'until [ -e /home/oz/r01-fault/run/ro-TRIGGER ]; do sleep 0.05; done; echo r01 > /sys/fs/ext4/loop0/trigger_fs_error'
```

Arm it only after `ro-case.sh` has started: the script deletes a stale trigger file at start.
With `errors=remount-ro` the kernel logs the error, aborts the journal and remounts
/mnt/r01 read-only; every later RocksDB write of v3 fails with EROFS. The script then
waits up to 15 min for v3 to exit and writes exit code, failed height H, last applied
height and the first failing write to SUMMARY.txt. Check with `findmnt -n -o OPTIONS /mnt/r01`
(now `ro,...`) and `sudo dmesg | tail`.

What to expect, from the full-disk runs: the commit's fail-stop (exit 70, FATAL serial
flush line) happens only when a committed block is in v3's execution thread when RocksDB
starts refusing writes. If a consensus (hotstuff) write fails while execution is idle, the
`hotstuff-algo` thread panics at `crates/torus-consensus/src/kv_store.rs:170`, the execution
thread then only logs "shutting down" and the process stays up: SUMMARY says ZOMBIE, and
the script kills v3 itself at the end of the 15 min window (`WAIT_S=<s>` shortens it). To
make the exit-70 outcome likely, the script by default (`STARVE=1`) sets v3's
`torus-execution` thread to SCHED_IDLE on CPU 31 next to 16 busy loops from READY until the fs
goes ro (at most 60 s), and the native load is 400 orders x 10 actions/s per sender
(`BS`, `RATE` override). `STARVE=0` runs without it. One ext4 error is one attempt: a ZOMBIE
needs the step 4 recovery and a new run (another label).

Self-test already done without sudo (`DRY_FILL=1`: the fault is a full disk instead of the
ext4 error): `DRY_FILL=1 WAIT_S=90 ro-case.sh native dryfill` gave exit 70, H=591, last applied
590, boot replay "applied_height=590 gap=42", checkpoints 300..800 equal on all 4
(`/home/oz/r01-fault/results/ro-native-dryfill/SUMMARY.txt`).

## 4. Recovery, when the script prints `RECOVER:` (v3 is stopped by then)

```
sudo umount /mnt/r01
sudo e2fsck -fy /home/oz/r01-fault/r01.img
sudo mount -o loop /home/oz/r01-fault/r01.img /mnt/r01
stat -c %U:%G /mnt/r01                      # expect oz:oz
```

`chown` is not needed: the fs root inode is owned by `oz:oz` (checked: `stat` gives oz:oz 755
while mounted), and the ownership is stored in the image, so it survives umount, fsck and
mount. Only if the `stat` above shows `root:root` (e.g. fsck rebuilt the root directory):
`sudo chown oz:oz /mnt/r01`. The re-mount drops `errors=remount-ro` (back to the superblock
default, Continue); repeat step 1 before another run. The loop device may get a different
number after the re-mount; check `losetup -j /home/oz/r01-fault/r01.img`, and if it is not
loop0, the trigger path in step 3 becomes `/sys/fs/ext4/loopN/trigger_fs_error` (the script's
READY line hardcodes loop0).

The script waits up to 30 min for /mnt/r01 to be writable again, then restarts v3 with the
same argv, records its boot replay line (`crash recovery: execution gap detected, replaying
... applied_height=` = the marker after the exit, expected H-1), waits until v3 applies the
next checkpoint (every 100 blocks) and compares every retained checkpoint hash
(`torus_getStateHash`) with v0..v2. Last lines of SUMMARY.txt: `RESULT: checkpoints a..b MATCH`
or `MISMATCH`.

## 5. After the run

The script stops every node it started. Leftovers to delete by hand (recursive deletes are
not done by the agent): `/mnt/r01/ro-<load>-<label>`, `/home/oz/r01-fault/run/ro-<load>-<label>`.
