#!/usr/bin/env python3
"""s70 proposal->QC split: gap_attr joins the per-view `view_close` trace lines
of val0-2 (plus vote admission lines) into critical-path segments."""

import os
import tempfile
import unittest

from gap_attr import view_join

ANSI = "\x1b[2m2026-09-27T12:57:46.950046Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mtorus_node\x1b[0m\x1b[2m:\x1b[0m "


def close(
    view,
    start,
    end,
    propose="-",
    rx="-",
    vote="-",
    timeout="-",
    pc_view="-",
    first="-",
    pc="-",
):
    return (
        ANSI + f"body_fetch_diag view_close: view={view} start_us={start} end_us={end} "
        f"propose_us={propose} proposal_rx_us={rx} vote_us={vote} timeout_us={timeout} "
        f"pc_view={pc_view} first_vote_rx_us={first} pc_us={pc}\n"
    )


def vote_admission(view, unix_us, peer):
    return (
        ANSI
        + f"body_fetch_diag admission: pid=1 seq=0 mono_us=0 unix_us={unix_us} kind=vote "
        f"peer={peer} view={view} hash=AAAA admitted=true queue_depth_before=0 queue_depth_after=1\n"
    )


# View 10: val0 leads, val1 collects. View 11: val1 leads and times out (no PC).
LOGS = {
    "val0": [
        close(10, 1_000_000, 1_110_000, propose=1_050_000, vote=1_052_000),
        close(11, 1_110_000, 2_400_000, rx=1_120_000, vote=1_125_000),
    ],
    "val1": [
        vote_admission(10, 1_053_000, "v0"),
        vote_admission(10, 1_072_000, "v2"),
        "unrelated line\n",
        close(
            10,
            1_010_000,
            1_100_000,
            rx=1_060_000,
            vote=1_062_000,
            pc_view=10,
            first=1_055_000,
            pc=1_080_000,
        ),
        close(11, 1_100_000, 2_390_000, propose=1_115_000, timeout=2_380_000),
    ],
    "val2": [
        close(10, 1_005_000, 1_108_000, rx=1_065_000, vote=1_070_000),
        close(11, 1_108_000, 2_395_000, rx=1_122_000, vote=1_126_000),
    ],
}


def write_run(root):
    run = os.path.join(root, "run")
    os.makedirs(run)
    for node, lines in LOGS.items():
        with open(os.path.join(run, node + ".log"), "w") as f:
            f.writelines(lines)
    return root


class ViewJoinTest(unittest.TestCase):
    def test_segments_for_a_certified_view(self):
        with tempfile.TemporaryDirectory() as d:
            j = view_join(write_run(d))
        self.assertEqual(j["joined"], 1)
        self.assertEqual(j["no_pc"], 1)
        seg = {k: v["p50"] for k, v in j["segments"].items()}
        self.assertEqual(
            seg,
            {
                "propose": 50.0,  # L start -> L propose
                "header_rx_last": 15.0,  # L propose -> last follower header
                "vote_sent_last": 20.0,  # L propose -> last vote sent (any node)
                "vote_to_pc": 10.0,  # last vote sent -> N collects the PC
                "vote_queue": 8.0,  # last vote admitted at N -> PC
                "vote_gather": 25.0,  # N first vote received -> PC
                "pc_to_advance": 20.0,  # N PC -> N enters the next view
                "advance_to_leader": 10.0,  # N enters next view -> L enters it
                "cycle": 100.0,  # L start -> N enters the next view
            },
        )
        self.assertEqual(j["last_voter"], {"follower": 1})

    def test_window_filters_by_leader_view_start(self):
        with tempfile.TemporaryDirectory() as d:
            j = view_join(write_run(d), window=(2, 3))
        self.assertEqual(j["joined"], 0)

    def test_missing_logs_return_none(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertIsNone(view_join(d))


if __name__ == "__main__":
    unittest.main()
