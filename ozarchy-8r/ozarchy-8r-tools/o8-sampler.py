#!/usr/bin/env python3
"""ozarchy-q9 per-node 60 s data-dir sampler (bench-queue step 9, Items Q9).

Runs beside a campaign (its own systemd --user unit). Every --interval s, for each
running torus-node (found by its --data-dir=<root>/data/val<i> argument) it writes one
TSV row to <out_dir>/<cell label>.tsv. The cell label is argv[2] of the running
run-cell.sh (no run-cell.sh -> label "nocell"). Columns:

  ts label node pid height
  cdb_bytes state_bytes total_bytes      du -sb consensus-db, data dir minus consensus-db, data dir
  cdb_sst_n cdb_sst_bytes cdb_wal_bytes  *.sst / *.log in consensus-db
  st_sst_n st_sst_bytes st_wal_bytes     *.sst / *.log in the data dir root (the state DB)
  fds fds_deleted                        ls /proc/PID/fd | wc -l ; ls -l /proc/PID/fd | grep -c deleted
  pend_state pend_cdb                    sum of torus_rocksdb_pending_compaction_bytes{cf} per DB
  pend_state_all pend_cdb_all            torus_rocksdb[_consensus]_pending_compaction_bytes_all
  l0_state_max l0_cdb_max                torus_rocksdb[_consensus]_l0_files_max
  l0_native_pending_cdb l0_block_bodies  per-CF L0 gauges
  pend_native_pending_cdb pend_block_bodies
  gc_deleted gc_s_sum gc_s_count         torus_da_bodies_gc_deleted(_total), torus_da_gc_seconds_{sum,count} (absent = NA)
  rej_margin matched db_size_bytes       torus_orders_rejected_margin_total, torus_orders_matched_total, torus_db_size_bytes
  log_cdb_ts cf_native_pending_bytes cf_native_shards_bytes   last Compaction Stats dump in consensus-db/LOG (Sum row Size)
  log_st_ts cf_block_bodies_bytes cf_block_headers_bytes cf_native_pending_st_bytes   last dump in the state LOG

Stops when --done-file exists (checked every 5 s) or after --max-hours.
"""

import argparse
import os
import re
import subprocess
import sys
import time
import urllib.request

COLS = (
    "ts label node pid height cdb_bytes state_bytes total_bytes cdb_sst_n cdb_sst_bytes cdb_wal_bytes "
    "st_sst_n st_sst_bytes st_wal_bytes fds fds_deleted pend_state pend_cdb pend_state_all pend_cdb_all "
    "l0_state_max l0_cdb_max l0_native_pending_cdb l0_block_bodies pend_native_pending_cdb pend_block_bodies "
    "gc_deleted gc_s_sum gc_s_count rej_margin matched db_size_bytes "
    "log_cdb_ts cf_native_pending_bytes cf_native_shards_bytes "
    "log_st_ts cf_block_bodies_bytes cf_block_headers_bytes cf_native_pending_st_bytes"
).split()

UNITS = {"B": 1, "KB": 1024, "MB": 1024**2, "GB": 1024**3, "TB": 1024**4}
SUM_RE = re.compile(r"^\s*Sum\s+\d+/\d+\s+([0-9.]+)\s+(B|KB|MB|GB|TB)\b")
HDR_RE = re.compile(r"^\*\* Compaction Stats \[([^\]]+)\] \*\*")
TS_RE = re.compile(r"^(\d{4}/\d\d/\d\d-\d\d:\d\d:\d\d)\.\d+ .*DUMPING STATS")


def nodes(data_root):
    """{idx: pid} of running torus-node processes with --data-dir=<data_root>/data/val<i>."""
    out = {}
    pat = re.compile(r"--data-dir=?(\S*)")
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/cmdline", "rb") as f:
                argv = f.read().split(b"\0")
        except OSError:
            continue
        if not argv or not argv[0].endswith(b"torus-node"):
            continue
        args = [a.decode(errors="replace") for a in argv]
        for k, a in enumerate(args):
            dd = None
            if a.startswith("--data-dir="):
                dd = a.split("=", 1)[1]
            elif a == "--data-dir" and k + 1 < len(args):
                dd = args[k + 1]
            if dd:
                m = re.fullmatch(
                    re.escape(os.path.join(data_root, "data")) + r"/val(\d+)/?", dd
                )
                if m:
                    out[int(m.group(1))] = int(d)
    return out


def cell_label():
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/cmdline", "rb") as f:
                argv = [a.decode(errors="replace") for a in f.read().split(b"\0") if a]
        except OSError:
            continue
        for k, a in enumerate(argv):
            if a.endswith("run-cell.sh") and len(argv) > k + 2:
                return argv[k + 2]
    return "nocell"


def du(path):
    try:
        r = subprocess.run(
            ["du", "-sb", path], capture_output=True, text=True, timeout=50
        )
        return int(r.stdout.split()[0])
    except Exception:
        return None


def files(path):
    sst_n = sst_b = wal_b = 0
    try:
        with os.scandir(path) as it:
            for e in it:
                try:
                    if e.name.endswith(".sst"):
                        sst_n += 1
                        sst_b += e.stat(follow_symlinks=False).st_size
                    elif e.name.endswith(".log"):
                        wal_b += e.stat(follow_symlinks=False).st_size
                except OSError:
                    pass
    except OSError:
        return None, None, None
    return sst_n, sst_b, wal_b


def fds(pid):
    base = f"/proc/{pid}/fd"
    try:
        names = os.listdir(base)
    except OSError:
        return None, None
    dele = 0
    for n in names:
        try:
            if os.readlink(os.path.join(base, n)).endswith(" (deleted)"):
                dele += 1
        except OSError:
            pass
    return len(names), dele


def scrape(port):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/metrics", timeout=5) as r:
            text = r.read().decode(errors="replace")
    except Exception:
        return None
    m = {}
    for line in text.splitlines():
        if not line or line[0] == "#":
            continue
        try:
            k, v = line.rsplit(" ", 1)
            m[k] = float(v)
        except ValueError:
            continue
    return m


def last_dump(log_path, want, tail_bytes=6 << 20):
    """(dump timestamp, {cf: Sum Size bytes}) of the last complete DUMPING STATS in a RocksDB LOG."""
    try:
        size = os.path.getsize(log_path)
        with open(log_path, "rb") as f:
            f.seek(max(0, size - tail_bytes))
            lines = f.read().decode(errors="replace").splitlines()
    except OSError:
        return None, {}
    starts = [i for i, l in enumerate(lines) if TS_RE.match(l)]
    for s in reversed(starts):
        ts = TS_RE.match(lines[s]).group(1)
        got = {}
        cf = None
        for l in lines[s + 1 :]:
            if TS_RE.match(l):
                break
            h = HDR_RE.match(l)
            if h:
                cf = h.group(1)
                continue
            sm = SUM_RE.match(l)
            if sm and cf in want and cf not in got:
                got[cf] = int(float(sm.group(1)) * UNITS[sm.group(2)])
        if got:
            return ts, got
    return None, {}


def g(m, k):
    if m is None or k not in m:
        return "NA"
    v = m[k]
    return str(int(v)) if v == int(v) else repr(v)


def cf_sum(m, family, db_consensus):
    if m is None:
        return "NA"
    tot, seen = 0.0, False
    for k, v in m.items():
        if not k.startswith(family + "{"):
            continue
        is_c = 'db="consensus"' in k
        if is_c == db_consensus:
            tot += v
            seen = True
    return str(int(tot)) if seen else "NA"


def first(m, keys):
    for k in keys:
        if m is not None and k in m:
            return g(m, k)
    return "NA"


def sample(args, label, pids):
    rows = []
    ts = time.time()
    for i in sorted(pids):
        pid = pids[i]
        dd = os.path.join(args.data_root, "data", f"val{i}")
        cdb = os.path.join(dd, "consensus-db")
        tot = du(dd)
        c = du(cdb)
        st = tot - c if tot is not None and c is not None else None
        cn, cb, cw = files(cdb)
        sn, sb, sw = files(dd)
        nf, nd = fds(pid)
        m = scrape(args.metrics_base + i)
        lc_ts, lc = last_dump(
            os.path.join(cdb, "LOG"), {"cf_native_pending", "cf_native_shards"}
        )
        ls_ts, lsv = last_dump(
            os.path.join(dd, "LOG"),
            {"cf_block_bodies", "cf_block_headers", "cf_native_pending"},
        )
        vals = [
            f"{ts:.1f}",
            label,
            f"val{i}",
            str(pid),
            g(m, "torus_block_height"),
            str(c),
            str(st),
            str(tot),
            str(cn),
            str(cb),
            str(cw),
            str(sn),
            str(sb),
            str(sw),
            str(nf),
            str(nd),
            cf_sum(m, "torus_rocksdb_pending_compaction_bytes", False),
            cf_sum(m, "torus_rocksdb_pending_compaction_bytes", True),
            g(m, "torus_rocksdb_pending_compaction_bytes_all"),
            g(m, "torus_rocksdb_consensus_pending_compaction_bytes_all"),
            g(m, "torus_rocksdb_l0_files_max"),
            g(m, "torus_rocksdb_consensus_l0_files_max"),
            g(m, 'torus_rocksdb_l0_files{cf="cf_native_pending",db="consensus"}'),
            g(m, 'torus_rocksdb_l0_files{cf="cf_block_bodies"}'),
            g(
                m,
                'torus_rocksdb_pending_compaction_bytes{cf="cf_native_pending",db="consensus"}',
            ),
            g(m, 'torus_rocksdb_pending_compaction_bytes{cf="cf_block_bodies"}'),
            first(
                m, ["torus_da_bodies_gc_deleted_total", "torus_da_bodies_gc_deleted"]
            ),
            g(m, "torus_da_gc_seconds_sum"),
            g(m, "torus_da_gc_seconds_count"),
            g(m, "torus_orders_rejected_margin_total"),
            g(m, "torus_orders_matched_total"),
            g(m, "torus_db_size_bytes"),
            lc_ts or "NA",
            str(lc.get("cf_native_pending", "NA")),
            str(lc.get("cf_native_shards", "NA")),
            ls_ts or "NA",
            str(lsv.get("cf_block_bodies", "NA")),
            str(lsv.get("cf_block_headers", "NA")),
            str(lsv.get("cf_native_pending", "NA")),
        ]
        rows.append("\t".join(v if v != "None" else "NA" for v in vals))
    path = os.path.join(args.out_dir, f"{label}.tsv")
    new = not os.path.exists(path)
    with open(path, "a") as f:
        if new:
            f.write("\t".join(COLS) + "\n")
        f.write("\n".join(rows) + "\n")
    return time.time() - ts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--done-file", required=True)
    ap.add_argument("--data-root", default=os.path.expanduser("~/torus-wsl-devnet"))
    ap.add_argument("--metrics-base", type=int, default=9161)
    ap.add_argument("--interval", type=float, default=60.0)
    ap.add_argument("--max-hours", type=float, default=8.0)
    ap.add_argument("--once", action="store_true")
    args = ap.parse_args()
    os.makedirs(args.out_dir, exist_ok=True)
    t_end = time.time() + args.max_hours * 3600
    last_pids, next_t = None, 0.0
    print(
        f"q9-sampler start pid={os.getpid()} out={args.out_dir} done={args.done_file} interval={args.interval}",
        flush=True,
    )
    while time.time() < t_end and not os.path.exists(args.done_file):
        pids = nodes(args.data_root)
        now = time.time()
        if pids:
            if pids != last_pids or now >= next_t:
                label = cell_label()
                took = sample(args, label, pids)
                if pids != last_pids:
                    with open(os.path.join(args.out_dir, f"{label}.cmdline"), "a") as f:
                        for i in sorted(pids):
                            try:
                                with open(f"/proc/{pids[i]}/cmdline", "rb") as c:
                                    cl = (
                                        c.read()
                                        .replace(b"\0", b" ")
                                        .decode(errors="replace")
                                        .strip()
                                    )
                            except OSError:
                                cl = "GONE"
                            f.write(f"{now:.1f} val{i} pid={pids[i]} {cl}\n")
                    print(
                        f"{time.strftime('%H:%M:%S')} new node set {pids} label={label} (sample {took:.1f}s)",
                        flush=True,
                    )
                last_pids, next_t = pids, now + args.interval
                if args.once:
                    return 0
        else:
            last_pids = None
        time.sleep(5)
    print(f"q9-sampler end {time.strftime('%H:%M:%S')}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
