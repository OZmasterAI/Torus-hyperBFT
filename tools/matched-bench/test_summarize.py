#!/usr/bin/env python3
"""test_summarize.py — fixture test for summarize.py's exec-phase table.

Runs the real summarize.py over a synthetic two-sample sampler.csv whose
counters are chosen so every derived number is exact, and asserts:

  * the classic engine/flush sub-breakdown still resolves;
  * the r6 engine-untimed sub-timers (phase1_actions / settle_pass_a /
    settle_pass_b / cache_flush / post_engine_tail / engine_untimed) appear
    under phases.engine as ms-per-native-block;
  * the r7 flush split (state_write_build / state_write_db) appears under
    phases.flush AT THE SAME TIME as the r6 engine sub-timers — the two
    landed on separate branches and share one SUB table, so a restack that
    drops either half must fail here;
  * a PRE-r6 sampler.csv (columns absent) still summarizes, with the new
    sub-timers reported as 0.0 rather than crashing.

Usage: python3 tools/matched-bench/test_summarize.py
"""

import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SUMMARIZE = os.path.join(HERE, "summarize.py")

# Per-native-block seconds we encode into the fixture (10 native blocks over
# a 10 s window ⇒ these * 10 are the _sum deltas, and * 1000 are the ms/blk
# summarize.py must report back).
BLOCKS = 10
SPAN = 10
PER_BLK = {
    "exec_block_seconds": 1.100,
    "exec_evm_seconds": 0.010,
    "exec_verify_seconds": 0.020,
    "exec_replay_guard_seconds": 0.030,
    "exec_load_books_seconds": 0.040,
    "exec_engine_seconds": 0.670,
    "exec_save_books_seconds": 0.050,
    "exec_flush_seconds": 0.200,
    "exec_body_persist_seconds": 0.005,
    "exec_phase_margin_seconds": 0.070,
    "exec_phase_match_seconds": 0.080,
    "exec_phase_settle_seconds": 0.090,
    "exec_root_seconds": 0.120,
    "exec_state_write_seconds": 0.070,
    # r7 flush split: build + db == state_write
    "exec_state_write_build_seconds": 0.040,
    "exec_state_write_db_seconds": 0.030,
    "exec_evm_resync_seconds": 0.010,
    # r6 sub-timers
    "exec_phase1_actions_seconds": 0.015,
    "exec_settle_pass_a_seconds": 0.035,
    "exec_settle_pass_b_seconds": 0.045,
    "exec_cache_flush_seconds": 0.008,
    "exec_post_engine_tail_seconds": 0.025,
    "exec_engine_untimed_seconds": 0.390,
}
COUNTERS = {
    "blocks_committed_total": 10,
    "block_height": 10,
    "orders_placed_accepted_total": 20000,
    "orders_matched_total": 15000,
    "orders_resting_total": 100,
    "native_actions_processed_total": 20000,
}


def write_fixture(out, include_r6):
    cols = []
    vals_a = []
    vals_b = []
    for k, v in COUNTERS.items():
        cols.append("torus_" + k)
        vals_a.append(0.0)
        vals_b.append(float(v))
    for k, per in PER_BLK.items():
        if not include_r6 and k in (
            "exec_phase1_actions_seconds",
            "exec_settle_pass_a_seconds",
            "exec_settle_pass_b_seconds",
            "exec_cache_flush_seconds",
            "exec_post_engine_tail_seconds",
            "exec_engine_untimed_seconds",
        ):
            continue
        cols += ["torus_" + k + "_sum", "torus_" + k + "_count"]
        vals_a += [0.0, 0.0]
        vals_b += [per * BLOCKS, float(BLOCKS)]

    with open(os.path.join(out, "sampler.csv"), "w") as f:
        f.write("ts,node," + ",".join(cols) + "\n")
        for node in ("val0", "val1", "val2"):
            f.write("1000,%s,%s\n" % (node, ",".join("%r" % v for v in vals_a)))
            f.write(
                "%d,%s,%s\n" % (1000 + SPAN, node, ",".join("%r" % v for v in vals_b))
            )
    open(os.path.join(out, "agreement.jsonl"), "w").close()


def run(out):
    cmd = [
        sys.executable,
        SUMMARIZE,
        "--out",
        out,
        "--label",
        "fixture",
        "--t-bench0",
        "1000",
        "--t-bench1",
        str(1000 + SPAN),
        "--t-drain",
        str(1000 + SPAN),
        "--markets",
        "10",
        "--dur",
        "10",
        "--rate",
        "1000",
        "--senders",
        "1",
        "--bench-rc",
        "0",
    ]
    r = subprocess.run(cmd, capture_output=True, text=True)
    assert r.returncode == 0, "summarize.py failed:\n%s\n%s" % (r.stdout, r.stderr)
    with open(os.path.join(out, "summary.json")) as f:
        return json.load(f)


def close(a, b, what):
    assert a is not None and abs(a - b) < 0.05, "%s: got %r, want %r" % (what, a, b)


def main():
    with tempfile.TemporaryDirectory() as out:
        write_fixture(out, include_r6=True)
        s = run(out)
        eng = s["phase_by_node"]["val0"]["phases"]["engine"]

        close(eng["ms"], 670.0, "engine ms/blk")
        # classic sub-breakdown still there
        close(eng["phase_margin_ms"], 70.0, "phase_margin_ms")
        close(eng["phase_match_ms"], 80.0, "phase_match_ms")
        close(eng["phase_settle_ms"], 90.0, "phase_settle_ms")
        # r6 sub-timers
        close(eng["phase1_actions_ms"], 15.0, "phase1_actions_ms")
        close(eng["settle_pass_a_ms"], 35.0, "settle_pass_a_ms")
        close(eng["settle_pass_b_ms"], 45.0, "settle_pass_b_ms")
        close(eng["cache_flush_ms"], 8.0, "cache_flush_ms")
        close(eng["post_engine_tail_ms"], 25.0, "post_engine_tail_ms")
        close(eng["engine_untimed_ms"], 390.0, "engine_untimed_ms")

        # the engine-internal accounting closes: phase1+margin+match+settle+
        # tail+untimed == engine
        acc = (
            eng["phase1_actions_ms"]
            + eng["phase_margin_ms"]
            + eng["phase_match_ms"]
            + eng["phase_settle_ms"]
            + eng["post_engine_tail_ms"]
            + eng["engine_untimed_ms"]
        )
        close(acc, eng["ms"], "engine sub-timer sum")

        # r7 flush split survives alongside the r6 engine sub-timers
        fl = s["phase_by_node"]["val0"]["phases"]["flush"]
        close(fl["ms"], 200.0, "flush ms/blk")
        close(fl["state_write_ms"], 70.0, "state_write_ms")
        close(fl["state_write_build_ms"], 40.0, "state_write_build_ms")
        close(fl["state_write_db_ms"], 30.0, "state_write_db_ms")
        close(
            fl["state_write_build_ms"] + fl["state_write_db_ms"],
            fl["state_write_ms"],
            "state_write build+db == state_write",
        )

        # late/early windows carry the new keys too
        late = s["phase_by_node"]["val0"].get("late_60s")
        if late is not None:
            assert "engine_untimed" in late, "late_60s must carry engine_untimed"
            assert "state_write_build" in late, "late_60s must carry state_write_build"

    with tempfile.TemporaryDirectory() as out:
        write_fixture(out, include_r6=False)
        s = run(out)
        eng = s["phase_by_node"]["val0"]["phases"]["engine"]
        close(eng["ms"], 670.0, "engine ms/blk (pre-r6)")
        for k in (
            "phase1_actions_ms",
            "settle_pass_a_ms",
            "settle_pass_b_ms",
            "cache_flush_ms",
            "post_engine_tail_ms",
            "engine_untimed_ms",
        ):
            assert eng[k] == 0.0, "pre-r6 binary must report %s == 0, got %r" % (
                k,
                eng[k],
            )
        # the r7 flush split is independent of the r6 columns
        fl = s["phase_by_node"]["val0"]["phases"]["flush"]
        close(fl["state_write_build_ms"], 40.0, "state_write_build_ms (pre-r6)")
        close(fl["state_write_db_ms"], 30.0, "state_write_db_ms (pre-r6)")

    print("test_summarize.py: OK")


if __name__ == "__main__":
    main()
