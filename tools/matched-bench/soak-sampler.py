#!/usr/bin/env python3
"""ozarchy-p4soak10 1 Hz sidecar (run with python3 -I): soak-sampler.py OUT_DIR PORT0 PORT1 PORT2 [PORT3 ...]

Waits for the harness's "] node pids: " line in OUT_DIR/run.log (up to 900 s), then once per second (aligned to the
wall-clock second) until every node pid is gone (or SIGTERM):
  * metrics-1hz.txt.gz: for each node, "T <epoch> val<i> ok <scrape_s>|ERR" and the /metrics lines without comments and
    without histogram buckets, except the buckets with le >= 0.128 of the latency families in KEEP_BUCKETS (slow-tail
    counts: count - bucket(le=0.512) = events over 0.512 s);
  * host-1hz.tsv: epoch, /proc/meminfo Dirty / Writeback / MemAvailable / Cached kB, and per node VmRSS / RssAnon /
    RssFile kB, voluntary / nonvoluntary context switches, utime+stime ticks (/proc/<pid>/stat).
"""

import gzip
import os
import re
import signal
import sys
import time
import urllib.request

KEEP_BUCKETS = (
    "torus_rpc_submit_admit_seconds_bucket",
    "torus_rpc_submit_verify_seconds_bucket",
    "torus_rpc_submit_permit_wait_seconds_bucket",
    "torus_rpc_submit_admit_insert_seconds_bucket",
    "torus_order_age_admit_seconds_bucket",
    "torus_commit_interval_seconds_bucket",
    "torus_exec_block_seconds_bucket",
    "torus_view_duration_seconds_bucket",
    "torus_flush_worker_checkpoint_seconds_bucket",
)
LE = re.compile(r'le="([^"]+)"')
STOP = False


def on_term(_s, _f):
    global STOP
    STOP = True


def keep(line):
    if not line or line[0] == "#":
        return False
    name = line.split("{", 1)[0].split(" ", 1)[0]
    if not name.endswith("_bucket"):
        return True
    if name not in KEEP_BUCKETS:
        return False
    m = LE.search(line)
    if not m:
        return False
    le = m.group(1)
    return le == "+Inf" or float(le) >= 0.128


def meminfo():
    d = {}
    with open("/proc/meminfo") as fh:
        for ln in fh:
            k, v = ln.split(":", 1)
            if k in ("Dirty", "Writeback", "MemAvailable", "Cached"):
                d[k] = v.split()[0]
    return d


def proc(pid):
    out = {}
    try:
        with open("/proc/%s/status" % pid) as fh:
            for ln in fh:
                k = ln.split(":", 1)[0]
                if k in (
                    "VmRSS",
                    "RssAnon",
                    "RssFile",
                    "voluntary_ctxt_switches",
                    "nonvoluntary_ctxt_switches",
                ):
                    out[k] = ln.split()[1]
        with open("/proc/%s/stat" % pid) as fh:
            f = fh.read().rsplit(")", 1)[1].split()
            out["cpu_ticks"] = str(int(f[11]) + int(f[12]))
    except (OSError, IndexError, ValueError):
        return None
    return out


def main():
    o, ports = sys.argv[1], sys.argv[2:]  # one metrics port per node (4 with N_VALIDATORS=4)
    signal.signal(signal.SIGTERM, on_term)
    t0 = time.time()
    pids = None
    while not STOP and time.time() - t0 < 900:
        try:
            for ln in open(os.path.join(o, "run.log"), errors="replace"):
                if "] node pids: " in ln:
                    pids = ln.split("] node pids: ", 1)[1].split()
                    break
        except OSError:
            pass
        if pids:
            break
        time.sleep(1)
    if not pids:
        return
    pk = (
        "VmRSS",
        "RssAnon",
        "RssFile",
        "voluntary_ctxt_switches",
        "nonvoluntary_ctxt_switches",
        "cpu_ticks",
    )
    with (
        gzip.open(os.path.join(o, "metrics-1hz.txt.gz"), "wt", compresslevel=1) as mz,
        open(os.path.join(o, "host-1hz.tsv"), "w") as hf,
    ):
        hf.write("ts\tDirty_kB\tWriteback_kB\tMemAvailable_kB\tCached_kB")
        for i in range(len(pids)):  # one column group per node pid, as each row
            hf.write("".join("\tval%d_%s" % (i, k) for k in pk))
        hf.write("\n")
        n = 0
        while not STOP:
            now = time.time()
            time.sleep(max(0.0, 1.0 - (now % 1.0)))
            ts = time.time()
            mi = meminfo()
            row = [
                "%.3f" % ts,
                mi.get("Dirty", ""),
                mi.get("Writeback", ""),
                mi.get("MemAvailable", ""),
                mi.get("Cached", ""),
            ]
            alive = 0
            for p in pids:
                d = proc(p)
                if d:
                    alive += 1
                row.extend((d or {}).get(k, "") for k in pk)
            hf.write("\t".join(row) + "\n")
            for i, port in enumerate(ports):
                s = time.time()
                try:
                    body = urllib.request.urlopen(
                        "http://127.0.0.1:%s/metrics" % port, timeout=0.9
                    ).read()
                    txt = body.decode("utf-8", "replace")
                    mz.write("T %.3f val%d ok %.4f\n" % (s, i, time.time() - s))
                    mz.write("\n".join(ln for ln in txt.split("\n") if keep(ln)))
                    mz.write("\n")
                except Exception as e:  # noqa: BLE001 - a failed scrape is recorded, never fatal
                    mz.write("T %.3f val%d ERR %s\n" % (s, i, type(e).__name__))
            n += 1
            if n % 10 == 0:
                hf.flush()
            if alive == 0:
                break


if __name__ == "__main__":
    main()
