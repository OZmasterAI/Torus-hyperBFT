#!/usr/bin/env python3
"""Q6 step 0: soak-sampler.py's optional fast Dirty / Writeback sampler.

SOAK_DIRTY_HZ=<n> (n >= 2) adds host-dirty-<n>hz.tsv (ts, Dirty_kB, Writeback_kB from /proc/meminfo, n rows per
second) beside the unchanged 1 Hz files; unset, 1 or invalid: no extra file. Runs the real sampler against three
short-lived stand-in node processes and closed metrics ports (scrapes are recorded as ERR, never fatal).
"""

import os
import socket
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SAMPLER = os.path.join(HERE, "soak-sampler.py")
HOST_1HZ_PREFIX = "ts\tDirty_kB\tWriteback_kB\tMemAvailable_kB\tCached_kB\tval0_VmRSS"


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def run_sampler(out, hz, life_s=3):
    """Start 3 stand-in nodes living `life_s` s, announce their pids in run.log, run the sampler until they exit."""
    # Orphaned (not our children), so they are reaped on exit and leave no zombie /proc entry behind, as the harness's
    # nodes do.
    pids = [
        subprocess.run(["sh", "-c", "sleep %d >/dev/null 2>&1 & echo $!" % life_s], capture_output=True, text=True,
                       check=True).stdout.strip()
        for _ in range(3)
    ]
    with open(os.path.join(out, "run.log"), "w") as f:
        f.write("[00:00:00] node pids: %s\n" % " ".join(pids))
    env = dict(os.environ)
    env.pop("SOAK_DIRTY_HZ", None)
    if hz is not None:
        env["SOAK_DIRTY_HZ"] = hz
    r = subprocess.run(
        [sys.executable, "-I", SAMPLER, out] + [str(free_port()) for _ in range(3)],
        env=env,
        capture_output=True,
        text=True,
        timeout=life_s + 30,
    )
    return r


class SoakDirtyHzTest(unittest.TestCase):
    def test_dirty_10hz_file(self):
        with tempfile.TemporaryDirectory() as out:
            r = run_sampler(out, "10")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            with open(os.path.join(out, "host-dirty-10hz.tsv")) as f:
                rows = f.read().splitlines()
            self.assertEqual(rows[0], "ts\tDirty_kB\tWriteback_kB")
            body = [ln.split("\t") for ln in rows[1:]]
            self.assertGreaterEqual(len(body), 15, rows)  # ~3 s at 10 Hz
            for f3 in body:
                self.assertEqual(len(f3), 3, f3)
                float(f3[0])
                int(f3[1])
                int(f3[2])
            ts = [float(f3[0]) for f3 in body]
            gaps = sorted(b - a for a, b in zip(ts, ts[1:]))
            self.assertTrue(0.05 <= gaps[len(gaps) // 2] <= 0.15, gaps)
            # the 1 Hz files are unchanged
            with open(os.path.join(out, "host-1hz.tsv")) as f:
                host = f.read().splitlines()
            self.assertTrue(host[0].startswith(HOST_1HZ_PREFIX), host[0])
            self.assertLess(len(host) - 1, len(body) / 3, (len(host), len(body)))

    def test_off_by_default_and_on_bad_values(self):
        for hz in (None, "1", "0", "ten", "-5"):
            with tempfile.TemporaryDirectory() as out:
                r = run_sampler(out, hz, life_s=1)
                self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
                extra = [n for n in os.listdir(out) if n.startswith("host-dirty-")]
                self.assertEqual(extra, [], hz)
                self.assertTrue(os.path.exists(os.path.join(out, "host-1hz.tsv")), hz)


if __name__ == "__main__":
    unittest.main()
