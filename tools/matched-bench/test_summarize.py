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
  * bl1 exec-chain-sub-100-attribution: the ruler columns (chain_ms,
    pipelined_ms, handoff_wait_ms, native/empty blk_s, fills_per_native_block,
    engine_ms_per_1k_fills, gap_to_100ms, commit_interval p50/p95 and the
    save_books drain/write split) resolve on BOTH binary shapes — SERIAL (no
    flush worker: the chain is the native-block share of block, the worker
    series is absent, the hand-off wait is identically 0) and PIPELINED (a
    worker series exists, flush is OFF the chain, the hand-off wait is real) —
    and that the identity block tells the two apart instead of mixing them;
  * a PRE-r6 sampler.csv (columns absent) still summarizes, with the new
    sub-timers reported as 0.0 rather than crashing;
  * a PRE-bl1 sampler.csv (no chain series at all) reports every ruler column
    as None — NOT 0.0, which would read as a 1080 ms -> 0 ms breakthrough;

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
# The window also carries 4 EMPTY blocks, so `exec_block_seconds_count` is 14
# while every phase count (and the chain count) is 10 — that gap is exactly
# what `chain_ms` exists to take out of the headline number.
BLOCKS = 10
EMPTY_BLOCKS = 4
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
    # bl1 save_books split: drain (pass 1, pinned to the exec thread because it
    # reads live book levels) + write (pass 2, the only movable half).
    "exec_save_books_drain_seconds": 0.030,
    "exec_save_books_write_seconds": 0.018,
}
R6_KEYS = (
    "exec_phase1_actions_seconds",
    "exec_settle_pass_a_seconds",
    "exec_settle_pass_b_seconds",
    "exec_cache_flush_seconds",
    "exec_post_engine_tail_seconds",
    "exec_engine_untimed_seconds",
)
# bl1 ruler series. SERIAL keeps every stage on the exec thread: the chain is
# the native-block share of the block wall (1.080 against a 1.100 block mean —
# the 20 ms/native-block difference is the four empty blocks) and E never waits
# on a worker. PIPELINED has flush off the chain: the chain drops to 0.850, a
# worker series appears, and the hand-off wait is non-zero.
BL1_SERIAL = {"exec_chain_seconds": 1.080, "exec_handoff_wait_seconds": 0.000}
BL1_PIPELINED = {
    "exec_chain_seconds": 0.850,
    "exec_handoff_wait_seconds": 0.030,
    "flush_worker_seconds": 0.260,
}
COUNTERS = {
    "blocks_committed_total": 10,
    "block_height": 10,
    "orders_placed_accepted_total": 20000,
    "orders_matched_total": 15000,
    "orders_resting_total": 100,
    "native_actions_processed_total": 20000,
}
# Cumulative commit_interval buckets over the window: 100 commits, 50 at or
# below 0.2 s and the rest at or below 0.4 s => p50 = 200 ms exactly (a bucket
# edge) and p95 = 380 ms by the standard in-bucket interpolation.
COMMIT_BUCKETS = [("0.1", 0), ("0.2", 50), ("0.4", 100), ("+Inf", 100)]


def write_fixture(out, include_r6=True, bl1="serial"):
    """bl1: 'serial' | 'pipelined' | 'none' (a pre-bl1 node binary)."""
    extra = {"serial": BL1_SERIAL, "pipelined": BL1_PIPELINED, "none": {}}[bl1]
    per_blk = dict(PER_BLK)
    if bl1 == "none":
        for k in ("exec_save_books_drain_seconds", "exec_save_books_write_seconds"):
            per_blk.pop(k)
    if bl1 == "pipelined":
        # The exec thread no longer runs flush, so its block wall shrinks by
        # the same amount the worker now carries.
        per_blk["exec_block_seconds"] = 0.870
    per_blk.update(extra)

    cols = []
    vals_a = []
    vals_b = []
    for k, v in COUNTERS.items():
        cols.append("torus_" + k)
        vals_a.append(0.0)
        vals_b.append(float(v))
    if bl1 != "none":
        cols += ["torus_exec_native_blocks_total", "torus_flush_worker_depth"]
        vals_a += [0.0, 0.0]
        vals_b += [float(BLOCKS), 0.0]
    for k, per in per_blk.items():
        if not include_r6 and k in R6_KEYS:
            continue
        # Only `exec_block_seconds` observes on empty blocks too; every phase
        # (and the chain) observes once per NATIVE block.
        count = BLOCKS + EMPTY_BLOCKS if k == "exec_block_seconds" else BLOCKS
        cols += ["torus_" + k + "_sum", "torus_" + k + "_count"]
        vals_a += [0.0, 0.0]
        vals_b += [per * BLOCKS, float(count)]

    with open(os.path.join(out, "sampler.csv"), "w") as f:
        f.write("ts,node," + ",".join(cols) + "\n")
        for node in ("val0", "val1", "val2"):
            f.write("1000,%s,%s\n" % (node, ",".join("%r" % v for v in vals_a)))
            f.write(
                "%d,%s,%s\n" % (1000 + SPAN, node, ",".join("%r" % v for v in vals_b))
            )
    open(os.path.join(out, "agreement.jsonl"), "w").close()

    if bl1 != "none":
        with open(os.path.join(out, "buckets.csv"), "w") as f:
            f.write("ts,node,metric,le,count\n")
            for node in ("val0", "val1", "val2"):
                for le, _cum in COMMIT_BUCKETS:
                    f.write(
                        "1000,%s,torus_commit_interval_seconds_bucket,%s,0\n"
                        % (node, le)
                    )
                for le, cum in COMMIT_BUCKETS:
                    f.write(
                        "%d,%s,torus_commit_interval_seconds_bucket,%s,%d\n"
                        % (1000 + SPAN, node, le, cum)
                    )


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


def check_common_ruler(p0, s):
    """bl1 ruler columns that do NOT depend on which binary produced the cell."""
    # save_books pass split: both halves present, and they cannot exceed the
    # save_books total they decompose.
    sb = p0["phases"]["save_books"]
    close(sb["ms"], 50.0, "save_books ms/blk")
    close(sb["save_books_drain_ms"], 30.0, "save_books drain ms")
    close(sb["save_books_write_ms"], 18.0, "save_books write ms")
    assert p0["chain_identity"]["save_split_covers_save_books"], (
        "drain + write must fit inside save_books: %r" % sb
    )

    # Cadence split — 10 native + 4 empty blocks over a 10 s window.
    close(p0["native_blk_s"], 1.0, "native_blk_s")
    close(p0["empty_blk_s"], 0.4, "empty_blk_s")
    close(p0["native_blocks_counter"], 10.0, "exec_native_blocks_total delta")

    # The MANDATORY denominator: 15000 fills over 10 native blocks. Without it
    # a thinner block reads as a chain win (engine ms scales with fills).
    close(p0["fills_per_native_block"], 1500.0, "fills_per_native_block")
    close(p0["engine_ms_per_1k_fills"], 446.67, "engine_ms_per_1k_fills")

    # Cadence percentiles out of the histogram buckets (the mean hides these).
    close(p0["commit_interval_ms_p50"], 200.0, "commit_interval_ms_p50")
    close(p0["commit_interval_ms_p95"], 380.0, "commit_interval_ms_p95")

    # The headline must carry the chain NEXT TO its denominator, or a report
    # can quote a "chain win" that is only a thinner block.
    h = s["headline"]
    close(h["chain_ms"], p0["chain_ms"], "headline chain_ms")
    close(h["fills_per_native_block"], 1500.0, "headline fills_per_native_block")
    close(h["gap_to_100ms"], p0["chain_ms"] - 100.0, "headline gap_to_100ms")


def main():
    with tempfile.TemporaryDirectory() as out:
        write_fixture(out, include_r6=True, bl1="serial")
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

        # ---- bl1 exec-chain ruler on a SERIAL binary ----
        p0 = s["phase_by_node"]["val0"]
        check_common_ruler(p0, s)
        close(p0["block_ms"], 1100.0, "block_ms (serial)")
        close(p0["chain_ms"], 1080.0, "chain_ms (serial)")
        # No worker on this binary: the worker series is ABSENT (count 0), so
        # pipelined_ms is a true 0.0 and the hand-off wait is 0 by construction.
        assert p0["worker_present"] is False, "serial fixture must not look pipelined"
        close(p0["pipelined_ms"], 0.0, "pipelined_ms (serial)")
        close(p0["handoff_wait_ms"], 0.0, "handoff_wait_ms (serial)")
        close(p0["flush_worker_depth_max"], 0.0, "flush_worker_depth_max (serial)")
        # chain + the empty-block share == block, exactly.
        close(p0["empty_block_ms"], 20.0, "empty_block_ms (serial)")
        close(
            p0["chain_ms"] + p0["empty_block_ms"],
            p0["block_ms"],
            "chain + empty == block (serial)",
        )
        close(p0["gap_to_100ms"], 980.0, "gap_to_100ms (serial)")

        ci = p0["chain_identity"]
        # On a serial binary flush is ON the exec thread, so it is part of what
        # the chain must cover. Losing that would let a pipelined binary be
        # scored as serial (or vice versa) with nothing failing.
        assert "flush" in ci["e_phases"], "serial: flush must be on the chain: %r" % ci
        close(ci["e_phase_sum_ms"], 1010.0, "e_phase_sum_ms (serial)")
        assert ci["chain_covers_e_phases"], "serial: chain must cover E phases: %r" % ci
        assert ci["chain_le_block"], "serial: chain must not exceed block: %r" % ci

    # ------------------------------------------------------- PIPELINED binary
    with tempfile.TemporaryDirectory() as out:
        write_fixture(out, include_r6=True, bl1="pipelined")
        s = run(out)
        p0 = s["phase_by_node"]["val0"]
        check_common_ruler(p0, s)
        close(p0["block_ms"], 870.0, "block_ms (pipelined)")
        close(p0["chain_ms"], 850.0, "chain_ms (pipelined)")
        assert p0["worker_present"] is True, "pipelined fixture must look pipelined"
        close(p0["pipelined_ms"], 260.0, "pipelined_ms")
        close(p0["handoff_wait_ms"], 30.0, "handoff_wait_ms")
        close(p0["empty_block_ms"], 20.0, "empty_block_ms (pipelined)")
        close(p0["gap_to_100ms"], 750.0, "gap_to_100ms (pipelined)")

        ci = p0["chain_identity"]
        # Flush ran on the WORKER, so it must NOT count as chain work —
        # otherwise the identity would demand the chain cover 200 ms it never
        # spent, and the whole point of the split is lost.
        assert "flush" not in ci["e_phases"], (
            "pipelined: flush is off the chain: %r" % ci
        )
        close(ci["e_phase_sum_ms"], 810.0, "e_phase_sum_ms (pipelined)")
        assert ci["chain_covers_e_phases"], (
            "pipelined: chain must still cover what stayed on E: %r" % ci
        )
        assert ci["chain_le_block"], "pipelined: chain must not exceed block: %r" % ci
        # The design's acceptance for a pipelined binary: the chain is at least
        # engine + verify (the two stages that can never be pipelined).
        eng_ms = p0["phases"]["engine"]["ms"]
        ver_ms = p0["phases"]["verify"]["ms"]
        assert p0["chain_ms"] >= eng_ms + ver_ms, (
            "pipelined chain (%r) must be >= engine + verify (%r + %r)"
            % (p0["chain_ms"], eng_ms, ver_ms)
        )

    with tempfile.TemporaryDirectory() as out:
        write_fixture(out, include_r6=False, bl1="serial")
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

    # -------------------------------------------------------- PRE-bl1 binary
    with tempfile.TemporaryDirectory() as out:
        write_fixture(out, include_r6=True, bl1="none")
        s = run(out)
        p0 = s["phase_by_node"]["val0"]
        # ABSENT series must read as None, never 0.0: a 0 ms chain would be
        # reported as the campaign's goal reached rather than as a stale binary.
        for k in (
            "chain_ms",
            "pipelined_ms",
            "handoff_wait_ms",
            "empty_block_ms",
            "gap_to_100ms",
            "commit_interval_ms_p50",
            "commit_interval_ms_p95",
        ):
            assert p0[k] is None, "pre-bl1 binary must report %s as None, got %r" % (
                k,
                p0[k],
            )
        assert s["headline"]["chain_ms"] is None, (
            "pre-bl1 headline chain_ms must be None"
        )
        # The save split degrades to 0.0 (a phase sub-timer, same convention as
        # the pre-r6 columns) instead of crashing the summarizer.
        sb = p0["phases"]["save_books"]
        assert sb["save_books_drain_ms"] == 0.0 and sb["save_books_write_ms"] == 0.0, (
            "pre-bl1 save split must be 0.0, got %r" % sb
        )

    check_window_series()
    print("test_summarize.py: OK")


# ---------------------------------------------------------------------------
# bl4 phase1-actions-drift-attribution
#
# The per-cell phase table averages the WHOLE bench window, which is why the
# same binary reports phase1_actions 121-125 ms on a 120 s cell and 375-394 ms
# on a 300 s cell: phase1 grows inside the run, and a 120 s cell simply stops
# before the expensive windows. `windows_60s` publishes the series the average
# hides, and `phase1_drift` states the verdict mechanically so no report has to
# eyeball it.
#
# The fixture below is a 5-window run whose phase1 span per block grows
# 20 -> 40 -> 80 -> 160 -> 320 ms while the ACTION count per block is pinned at
# 200 and the cancelled-order count doubles each window: that is the shape the
# real cells have, and it must come out as "grows_with_run_length" with a FLAT
# per-cancelled-order cost (pure volume). A second fixture holds phase1 flat to
# prove the verdict can also say "flat".
# ---------------------------------------------------------------------------

WIN_SPAN = 60
WIN_BLOCKS = 50


def write_window_fixture(out, phase1_ms_by_win, cancelled_by_win, resting_by_win):
    """Cumulative sampler.csv with one sample per 60 s window boundary."""
    nwin = len(phase1_ms_by_win)
    per_blk_flat = {
        "exec_block_seconds": 1.000,
        "exec_engine_seconds": 0.600,
        "exec_verify_seconds": 0.020,
        "exec_load_books_seconds": 0.010,
        "exec_save_books_seconds": 0.100,
        "exec_flush_seconds": 0.250,
        "exec_evm_seconds": 0.0,
        "exec_replay_guard_seconds": 0.0,
        "exec_body_persist_seconds": 0.0,
        "exec_phase_margin_seconds": 0.030,
        "exec_phase_match_seconds": 0.090,
        "exec_phase_settle_seconds": 0.190,
        "exec_chain_seconds": 1.000,
        "exec_handoff_wait_seconds": 0.0,
    }
    span_cols = list(per_blk_flat) + ["exec_phase1_actions_seconds"]
    plain_cols = [
        "blocks_committed_total",
        "block_height",
        "orders_placed_accepted_total",
        "orders_matched_total",
        "orders_resting_total",
        "native_actions_processed_total",
        "exec_native_blocks_total",
        "exec_resting_orders",
        "exec_phase1_actions_processed_total",
        "exec_phase1_orders_cancelled_total",
        "flush_worker_depth",
    ]
    cols = ["torus_" + c for c in plain_cols]
    for k in span_cols:
        cols += ["torus_" + k + "_sum", "torus_" + k + "_count"]

    lines = []
    cum = {c: 0.0 for c in plain_cols}
    cum_span = {k: 0.0 for k in span_cols}
    cum_n = 0.0
    for w in range(nwin + 1):
        row = dict(cum)
        # `resting_orders_end` is a GAUGE read at the window's END sample, so
        # row w carries the depth window w-1 finished at; the opening row is
        # the pre-run depth.
        row["exec_resting_orders"] = float(resting_by_win[w - 1] if w else 0)
        vals = [row[c] for c in plain_cols]
        for k in span_cols:
            vals += [cum_span[k], cum_n]
        lines.append((1000 + w * WIN_SPAN, vals))
        if w == nwin:
            break
        # advance one window
        cum_n += WIN_BLOCKS
        for k, per in per_blk_flat.items():
            cum_span[k] += per * WIN_BLOCKS
        cum_span["exec_phase1_actions_seconds"] += (
            phase1_ms_by_win[w] / 1000.0 * WIN_BLOCKS
        )
        cum["blocks_committed_total"] += WIN_BLOCKS
        cum["block_height"] += WIN_BLOCKS
        cum["exec_native_blocks_total"] += WIN_BLOCKS
        cum["orders_placed_accepted_total"] += 1000 * WIN_BLOCKS
        cum["orders_matched_total"] += 800 * WIN_BLOCKS
        cum["orders_resting_total"] += 100 * WIN_BLOCKS
        cum["native_actions_processed_total"] += 200 * WIN_BLOCKS
        cum["exec_phase1_actions_processed_total"] += 200 * WIN_BLOCKS
        cum["exec_phase1_orders_cancelled_total"] += cancelled_by_win[w] * WIN_BLOCKS

    with open(os.path.join(out, "sampler.csv"), "w") as f:
        f.write("ts,node," + ",".join(cols) + "\n")
        for node in ("val0", "val1", "val2"):
            for ts, vals in lines:
                f.write("%d,%s,%s\n" % (ts, node, ",".join("%r" % v for v in vals)))
    open(os.path.join(out, "agreement.jsonl"), "w").close()
    return 1000, 1000 + nwin * WIN_SPAN


def run_window(out, t0, t1, t_bench1=None):
    cmd = [
        sys.executable,
        SUMMARIZE,
        "--out",
        out,
        "--label",
        "winfixture",
        "--t-bench0",
        str(t0),
        "--t-bench1",
        str(t1 if t_bench1 is None else t_bench1),
        "--t-drain",
        str(t1),
        "--markets",
        "10",
        "--dur",
        str(t1 - t0),
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


def check_window_series():
    # -------------------------------------------------- growing phase1 (real)
    p1 = [20.0, 40.0, 80.0, 160.0, 320.0]
    canc = [10, 20, 40, 80, 160]
    resting = [100_000, 200_000, 400_000, 800_000, 1_600_000]
    with tempfile.TemporaryDirectory() as out:
        t0, t1 = write_window_fixture(out, p1, canc, resting)
        s = run_window(out, t0, t1)
        p0 = s["phase_by_node"]["val0"]
        wins = p0.get("windows_60s")
        assert wins is not None, "summary.json must carry windows_60s"
        assert len(wins) == 5, "5 whole 60 s windows, got %d" % len(wins)

        for i, w in enumerate(wins):
            assert w["win"] == i and w["t_rel_s"] == i * 60, (
                "window index/offset: %r" % w
            )
            close(w["span_s"], 60.0, "window %d span" % i)
            close(w["native_blocks"], WIN_BLOCKS, "window %d native_blocks" % i)
            close(w["phase1_actions_ms"], p1[i], "window %d phase1_actions_ms" % i)
            # the flat phases must stay flat window to window — if they drift
            # the window arithmetic is wrong, not the node.
            close(w["engine_ms"], 600.0, "window %d engine_ms" % i)
            close(w["phase_settle_ms"], 190.0, "window %d phase_settle_ms" % i)
            close(w["block_ms"], 1000.0, "window %d block_ms" % i)
            close(w["fills_per_block"], 800.0, "window %d fills_per_block" % i)
            close(w["phase1_actions_per_block"], 200.0, "window %d p1 actions" % i)
            close(
                w["phase1_orders_cancelled_per_block"],
                canc[i],
                "window %d cancelled/blk" % i,
            )
            close(
                w["resting_orders_end"], resting[i], "window %d resting_orders_end" % i
            )
            # 20 ms over 10 cancelled orders = 2000 us each, and it stays there
            # while both halves double: FLAT unit cost, growing volume.
            close(
                w["phase1_us_per_cancelled_order"],
                2000.0,
                "window %d us/cancelled order" % i,
            )

        d = p0.get("phase1_drift")
        assert d is not None, "summary.json must carry phase1_drift"
        close(d["first_ms"], 20.0, "drift first_ms")
        close(d["last_ms"], 320.0, "drift last_ms")
        close(d["max_ms"], 320.0, "drift max_ms")
        close(d["growth_ratio"], 16.0, "drift growth_ratio")
        close(d["resting_growth_ratio"], 16.0, "drift resting_growth_ratio")
        assert d["verdict"] == "grows_with_run_length", (
            "a 16x rise across 5 windows is the drift this exists to name: %r" % d
        )
        # The whole point: a 120 s cell would have averaged windows 0-1 only.
        close(d["first120_ms"], 30.0, "drift first120_ms (windows 0-1)")
        close(d["window_avg_ms"], 124.0, "drift window_avg_ms (all 5)")
        assert d["unit_cost_verdict"] == "flat", (
            "unit cost is pinned at 2000 us/cancelled order — the growth is "
            "VOLUME (deeper books mean each cancel-all removes more): %r" % d
        )

        # A window series must never contradict the whole-window average it
        # decomposes: the block-weighted mean of the windows is that average.
        eng = p0["phases"]["engine"]
        close(eng["phase1_actions_ms"], 124.0, "cell-average phase1_actions_ms")

    # -------------------------- drain windows must not reverse the verdict
    # After the bench stops submitting, the node runs the backlog down and
    # phase1 falls back (every real 300 s cell shows this in its last window).
    # Counting that as the series' END would report the drift as reversing, so
    # the verdict is computed over LOADED windows only while windows_60s still
    # publishes the whole run.
    with tempfile.TemporaryDirectory() as out:
        t0, t1 = write_window_fixture(
            out, p1 + [15.0], canc + [5], resting + [1_600_000]
        )
        t_bench1 = t1 - WIN_SPAN  # the last window is drain
        s = run_window(out, t0, t1, t_bench1=t_bench1)
        p0 = s["phase_by_node"]["val0"]
        assert len(p0["windows_60s"]) == 6, "the drain window must still be PUBLISHED"
        assert [w["in_bench_window"] for w in p0["windows_60s"]] == [True] * 5 + [
            False
        ], "the drain window must be flagged: %r" % p0["windows_60s"]
        d = p0["phase1_drift"]
        assert d["loaded_windows"] == 5 and d["windows_incl_drain"] == 6, "%r" % d
        close(d["last_ms"], 320.0, "drift last_ms ignores the drain window")
        close(d["window_avg_ms"], 124.0, "drift window_avg_ms ignores drain")
        assert d["verdict"] == "grows_with_run_length", (
            "a 15 ms drain window must not cancel the drift: %r" % d
        )

    # -------------------------------------------------- flat phase1 (control)
    with tempfile.TemporaryDirectory() as out:
        t0, t1 = write_window_fixture(out, [50.0] * 5, [25] * 5, [500_000] * 5)
        s = run_window(out, t0, t1)
        d = s["phase_by_node"]["val0"]["phase1_drift"]
        close(d["growth_ratio"], 1.0, "flat growth_ratio")
        assert d["verdict"] == "flat", (
            "a flat series must NOT be reported as drift: %r" % d
        )

    # ------------------------------------- pre-bl4 binary: no workload counters
    # The window series still resolves (it is pure sampler arithmetic); only
    # the per-unit-cost columns go None, never 0.0.
    with tempfile.TemporaryDirectory() as out:
        t0, t1 = write_window_fixture(out, p1, [0] * 5, resting)
        s = run_window(out, t0, t1)
        p0 = s["phase_by_node"]["val0"]
        assert len(p0["windows_60s"]) == 5
        for w in p0["windows_60s"]:
            assert w["phase1_us_per_cancelled_order"] is None, (
                "absent counter must read None, not 0.0: %r" % w
            )
        assert p0["phase1_drift"]["unit_cost_verdict"] is None
        assert p0["phase1_drift"]["verdict"] == "grows_with_run_length"


if __name__ == "__main__":
    main()
