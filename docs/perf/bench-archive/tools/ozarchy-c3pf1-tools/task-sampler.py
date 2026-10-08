#!/usr/bin/env python3
"""task-sampler.py <cell_out_dir>: 1 Hz per-thread utime/stime (ticks) of val0 -> tasks.txt.
Lines "ts tid comm utime stime", plus "ts PROC - utime stime" for the whole process (includes exited threads).
Starts at the harness "bench:" line, stops when summary.json appears or val0 exits."""

import os, sys, time

out = sys.argv[1]
t0 = time.time()
while True:
    try:
        if "] bench: " in open(out + "/run.log").read():
            break
    except FileNotFoundError:
        pass
    if time.time() - t0 > 900:
        sys.exit(1)
    time.sleep(1)
pid = open("/home/oz/torus-wsl-devnet/run/pids").readline().strip()


def parse(s):
    i, j = s.index("("), s.rindex(")")
    f = s[j + 2 :].split()
    return s[i + 1 : j].replace(" ", "_"), f[11], f[12]


with open(out + "/tasks.txt", "a") as o:
    for _ in range(1500):
        if os.path.exists(out + "/summary.json"):
            break
        ts = int(time.time())
        try:
            _, u, s = parse(open(f"/proc/{pid}/stat").read())
        except (FileNotFoundError, ProcessLookupError):
            break
        lines = [f"{ts} PROC - {u} {s}"]
        try:
            tids = os.listdir(f"/proc/{pid}/task")
        except FileNotFoundError:
            break
        for t in tids:
            try:
                c, u, s = parse(open(f"/proc/{pid}/task/{t}/stat").read())
                lines.append(f"{ts} {t} {c} {u} {s}")
            except (FileNotFoundError, ProcessLookupError):
                pass
        o.write("\n".join(lines) + "\n")
        o.flush()
        time.sleep(max(0, ts + 1 - time.time()))
