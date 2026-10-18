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
  * bl3 worker-aware phase accounting: when a flush WORKER is present the flush
    ms belong to W, not to the exec thread's block wall, so flush is reported as
    an OFF-CHAIN line and left OUT of the per-block phase sum — otherwise
    residual_untimed goes NEGATIVE (-212 ms on bl2 on-10m-r2) and the phase
    percentages sum past 100 %;
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


# Item 6 step 1: end_resident and its two subs (per native block), as an
# older binary (before Phase 4 step 1) emits them.
END_RESIDENT = {
    "exec_end_resident_seconds": 0.030,
    "exec_end_resident_rows_seconds": 0.012,
    "exec_end_resident_positions_seconds": 0.010,
}
# Phase 4 step 1 renamed the positions sub to users (it also follows balance
# and cvlm rows); a new binary emits only the users series.
END_RESIDENT_USERS = {
    "exec_end_resident_seconds": 0.030,
    "exec_end_resident_rows_seconds": 0.012,
    "exec_end_resident_users_seconds": 0.010,
}


# Item 6 step 2: the exec thread's wait at the end_resident worker's join.
END_RESIDENT_WAIT = {"exec_end_resident_wait_seconds": 0.008}
# Its buckets over the window (10 joins: 5 at or below 4 ms, all at or below
# 16 ms => p50 = 4 ms, p90 = 8 + 8 * (9 - 5) / 5 = 14.4 ms).
WAIT_BUCKETS = [("0.004", 5), ("0.008", 5), ("0.016", 10), ("+Inf", 10)]


# Item 6 Phase 3 (p3s2g 47.1): the exec thread's checkpoint timers, per block
# (settle_wait 6 ms, upkeep 9 ms of which the hand-off wait is 2 ms).
CHECKPOINT = {
    "exec_checkpoint_settle_wait_seconds": 0.006,
    "exec_checkpoint_upkeep_seconds": 0.009,
    "exec_checkpoint_wait_seconds": 0.002,
}
# Their buckets (10 observations each: settle 5 at or below 4 ms, all at or
# below 8 ms => p50 = 4, p99 = 4 + 4 * (9.9 - 5) / 5 = 7.92; upkeep all in
# (8, 16] => p50 = 12, p99 = 15.92).
CKPT_BUCKETS = {
    "torus_exec_checkpoint_settle_wait_seconds_bucket": [
        ("0.004", 5),
        ("0.008", 10),
        ("0.016", 10),
        ("+Inf", 10),
    ],
    "torus_exec_checkpoint_upkeep_seconds_bucket": [
        ("0.004", 0),
        ("0.008", 0),
        ("0.016", 10),
        ("+Inf", 10),
    ],
    # p3 merge wait: the deferred join, 8 at or below 0.5 ms, 2 in (20, 25] ms
    # => p50 = 0.5 * 5 / 8 = 0.3125, p99 = 20 + 5 * (9.9 - 8) / 2 = 24.75
    # (summarize rounds to 0.1 ms: 0.3, 24.8).
    "torus_exec_checkpoint_deferred_join_seconds_bucket": [
        ("0.0005", 8),
        ("0.02", 8),
        ("0.025", 10),
        ("+Inf", 10),
    ],
}


# Q6 step 0: the flush worker's checkpoint DB write and its RocksDB perf-context
# stage split, CKPT_DB_N writes over the window; per write (s): wall 0.600 =
# WAL 0.150 + memtable 0.300 + delay 0 + pre/post 0.050 + residual 0.100;
# mutex 0.020, condition wait 0.005 (signals, not stages).
CKPT_DB = {
    "write": 0.600,
    "wal": 0.150,
    "memtable": 0.300,
    "delay": 0.0,
    "pre_post": 0.050,
    "mutex": 0.020,
    "cond_wait": 0.005,
    "residual": 0.100,
}
CKPT_DB_N = 4


def write_fixture(
    out,
    include_r6=True,
    bl1="serial",
    end_resident=False,
    end_resident_worker=False,
    checkpoint=False,
    checkpoint_db=False,
):
    """bl1: 'serial' | 'pipelined' | 'none' (a pre-bl1 node binary).
    end_resident: the binary times end_resident (item 6 step 1); "users":
    a Phase 4 step 1 binary (users sub in place of positions);
    end_resident_worker: it runs on a worker, with a join wait (step 2)."""
    extra = {"serial": BL1_SERIAL, "pipelined": BL1_PIPELINED, "none": {}}[bl1]
    per_blk = dict(PER_BLK)
    if end_resident == "users":
        per_blk.update(END_RESIDENT_USERS)
    elif end_resident:
        per_blk.update(END_RESIDENT)
    if end_resident_worker:
        per_blk.update(END_RESIDENT_WAIT)
    if checkpoint:
        per_blk.update(CHECKPOINT)
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
    if checkpoint_db:
        for k, per in CKPT_DB.items():
            cols += [
                "torus_exec_checkpoint_db_%s_seconds_sum" % k,
                "torus_exec_checkpoint_db_%s_seconds_count" % k,
            ]
            vals_a += [0.0, 0.0]
            vals_b += [per * CKPT_DB_N, float(CKPT_DB_N)]

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
                if checkpoint:
                    for metric, bks in CKPT_BUCKETS.items():
                        for ts, scale in ((1000, 0), (1000 + SPAN, 1)):
                            for le, cum in bks:
                                f.write(
                                    "%d,%s,%s,%s,%d\n"
                                    % (ts, node, metric, le, cum * scale)
                                )
                if end_resident_worker:
                    for ts, scale in ((1000, 0), (1000 + SPAN, 1)):
                        for le, cum in WAIT_BUCKETS:
                            f.write(
                                "%d,%s,torus_exec_end_resident_wait_seconds_bucket,%s,%d\n"
                                % (ts, node, le, cum * scale)
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


def check_phase_accounting(p0, worker):
    """bl3: the per-block phase table must ACCOUNT for the block wall, whichever
    thread ran flush.

    `block_ms` is the exec thread's own timer. With a worker attached, flush is
    observed on W and is NOT part of that wall, so summing it into the per-block
    breakdown drives residual_untimed negative and the percentages past 100 %.
    """
    ph = p0["phases"]
    fl = ph["flush"]
    assert fl["off_chain"] is worker, (
        "flush.off_chain must mirror worker_present (worker=%r): %r" % (worker, fl)
    )
    # The flush ms themselves are unchanged either way — only where they are
    # counted moves.
    close(fl["ms"], 200.0, "flush ms/blk (worker=%r)" % worker)
    close(fl["state_write_ms"], 70.0, "state_write ms (worker=%r)" % worker)

    on_chain = [
        k for k, v in ph.items() if k != "residual_untimed" and not v.get("off_chain")
    ]
    assert ("flush" in on_chain) is not worker, "on-chain set: %r" % on_chain
    acc = sum(ph[k]["ms"] for k in on_chain)
    res = ph["residual_untimed"]["ms"]
    close(
        acc + res, p0["block_ms"], "phases + residual == block_ms (worker=%r)" % worker
    )
    assert res >= 0.0, "residual_untimed must never be negative: %r" % res

    pcts = [ph[k]["pct_of_block"] for k in on_chain] + [
        ph["residual_untimed"]["pct_of_block"]
    ]
    assert all(x is not None for x in pcts), "on-chain phases must keep pct_of_block"
    assert sum(pcts) <= 100.5, "phase pct sum must not exceed 100 %%: %r" % sum(pcts)
    if worker:
        assert fl["pct_of_block"] is None, (
            "an off-chain phase has no share of the exec block wall: %r" % fl
        )
        # ...but its share of WALL time is still meaningful (that is W's load).
        assert fl["pct_of_wall"] is not None


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
        # Serial: flush IS exec-thread work, so it stays inside the per-block
        # accounting and the residual keeps its pre-bl3 value exactly.
        check_phase_accounting(p0, worker=False)
        close(p0["phases"]["residual_untimed"]["ms"], 75.0, "residual (serial)")
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
        # bl3: flush ran on W, so the per-block table must not spend it twice.
        # 870 block - 825 on-chain phases = 45 ms genuinely untimed on E.
        check_phase_accounting(p0, worker=True)
        close(p0["phases"]["residual_untimed"]["ms"], 45.0, "residual (pipelined)")
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


# ------------------------------------------------- s58: load-windowed view timers
# consensus_by_node has always been computed from the metrics-before/after
# scrapes, which bracket idle + load + drain. Idle views are fast and numerous
# (the ramp alone commits ~100 near-empty blocks), so every per-view stage mean
# is diluted toward the idle value and an A/B reads as a no-op: across the s55
# fast/slow pair view_duration differed 16 % while matched/s differed 2.54x.
# These fixtures pin the LOAD-window slice, which needs the torus_view_* series
# in sampler.csv.
VIEW_IDLE = {"views": 100, "arrival_s": 1.0, "duration_s": 2.0}  # 10 ms / 20 ms
VIEW_LOAD = {"views": 20, "arrival_s": 4.0, "duration_s": 6.0}  # 200 ms / 300 ms


def write_view_fixture(out, with_view_cols=True):
    """Three samples: idle [900,1000], load [1000,1010].

    Whole-run arrival mean is 5.0 s / 120 views = 41.67 ms; the LOAD mean is
    4.0 s / 20 views = 200.00 ms. A summarizer that reports the whole-run
    number cannot tell the two apart.
    """
    cols = [
        "torus_blocks_committed_total",
        "torus_exec_block_seconds_count",
        "torus_exec_engine_seconds_count",
        "torus_consensus_view",
    ]
    idle = [50.0, 50.0, 50.0, float(VIEW_IDLE["views"])]
    load0 = list(idle)
    load1 = [70.0, 70.0, 70.0, float(VIEW_IDLE["views"] + VIEW_LOAD["views"])]
    if with_view_cols:
        for short, ikey in (
            ("view_proposal_arrival", "arrival_s"),
            ("view_duration", "duration_s"),
        ):
            cols += ["torus_%s_seconds_sum" % short, "torus_%s_seconds_count" % short]
            idle += [0.0, 0.0]
            load0 += [VIEW_IDLE[ikey], float(VIEW_IDLE["views"])]
            load1 += [
                VIEW_IDLE[ikey] + VIEW_LOAD[ikey],
                float(VIEW_IDLE["views"] + VIEW_LOAD["views"]),
            ]
    with open(os.path.join(out, "sampler.csv"), "w") as f:
        f.write("ts,node," + ",".join(cols) + "\n")
        for node in ("val0", "val1", "val2"):
            for ts, vals in ((900, idle), (1000, load0), (1010, load1)):
                f.write("%d,%s,%s\n" % (ts, node, ",".join("%r" % v for v in vals)))
    open(os.path.join(out, "agreement.jsonl"), "w").close()


def check_view_window():
    with tempfile.TemporaryDirectory() as out:
        write_view_fixture(out, with_view_cols=True)
        s = run(out)
        c = s["consensus_by_node"]["val0"]
        assert "load" in c, "consensus_by_node must carry a `load` window sibling"
        ld = c["load"]
        assert ld is not None, (
            "load window must resolve when the view series is sampled"
        )
        assert ld["view_proposal_arrival_ms"] == 200.00, (
            "load-window arrival must be 200.00 ms (4.0 s / 20 views), got %r -- "
            "a whole-run mean would read 41.67" % ld["view_proposal_arrival_ms"]
        )
        assert ld["view_duration_ms"] == 300.00, (
            "load-window view_duration must be 300.00 ms, got %r"
            % ld["view_duration_ms"]
        )
        assert ld["views"] == 20, "load window saw %r views, expected 20" % ld["views"]

    # A sampler predating the torus_view_* columns must report None, not 0.0:
    # a 0 ms arrival would read as the wait having been eliminated.
    with tempfile.TemporaryDirectory() as out:
        write_view_fixture(out, with_view_cols=False)
        s = run(out)
        c = s["consensus_by_node"]["val0"]
        assert c.get("load") is None, (
            "pre-s58 sampler must report consensus load window as None, got %r"
            % c.get("load")
        )


def check_view_hists_sampled():
    """Every VIEW_HISTS series must be in run-cell.sh WIDE_COLS, else the LOAD
    window silently reports None for it. s68 adds the block_build phases."""
    import ast
    import re

    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "summarize.py")) as f:
        src = f.read()
    hists = ast.literal_eval(
        re.search(r"^VIEW_HISTS = (\[.*?\])", src, re.S | re.M).group(1)
    )
    with open(os.path.join(here, "run-cell.sh")) as f:
        wide = set(re.search(r'^WIDE_COLS="([^"]+)"', f.read(), re.M).group(1).split())
    for name in ("torus_view_entry_slack_seconds", "torus_view_timeout_after_seconds"):
        assert name in hists, "VIEW_HISTS is missing %s" % name
    counters = ast.literal_eval(
        re.search(r"^VIEW_COUNTERS = (\[.*?\])", src, re.S | re.M).group(1)
    )
    missing_counters = [c for c in counters if c not in wide]
    assert not missing_counters, (
        "VIEW_COUNTERS not sampled in WIDE_COLS: %r" % missing_counters
    )
    for c in (
        "torus_view_entered_past_deadline_total",
        "torus_view_timeout_no_proposal_total",
        "torus_view_timeout_no_vote_total",
        "torus_view_timeout_after_vote_total",
        "torus_view_timeout_leader_total",
        "torus_view_proposals_uncertified_total",
    ):
        assert c in counters, "VIEW_COUNTERS is missing %s" % c
    for name in ("torus_view_vote_gather_seconds", "torus_view_qc_to_advance_seconds"):
        assert name in hists, "VIEW_HISTS is missing %s (s70 proposal->QC split)" % name
    for phase in ("parent", "select", "mirror", "attest", "encode"):
        assert "torus_block_build_%s_seconds" % phase in hists, (
            "VIEW_HISTS is missing torus_block_build_%s_seconds" % phase
        )
    missing = [h + s for h in hists for s in ("_sum", "_count") if h + s not in wide]
    assert not missing, "VIEW_HISTS series not sampled in WIDE_COLS: %r" % missing


# s77 order latency: 100 exec observations, 50 at or below 1.28 s and the
# rest at or below 2.56 s => p50 = 1280 ms (bucket edge), p90 = 2304 ms and
# p99 = 2534.4 ms by in-bucket interpolation.
ORDER_AGE_EXEC_BUCKETS = [("0.64", 0), ("1.28", 50), ("2.56", 100), ("+Inf", 100)]
ORDER_AGE_STAGES = ("admit", "commit", "exec", "durable", "fills_visible")


def check_order_age():
    """Per-stage order-age percentiles land in phase_by_node[*].order_age_ms
    over [t_bench0, t_drain]; a stage without samples is None, and a cell
    without buckets.csv (older harness) still summarizes with all None."""
    with tempfile.TemporaryDirectory() as out:
        write_fixture(out)
        with open(os.path.join(out, "buckets.csv"), "a") as f:
            for node in ("val0", "val1", "val2"):
                for le, cum in ORDER_AGE_EXEC_BUCKETS:
                    f.write(
                        "1000,%s,torus_order_age_exec_seconds_bucket,%s,0\n"
                        % (node, le)
                    )
                    f.write(
                        "%d,%s,torus_order_age_exec_seconds_bucket,%s,%d\n"
                        % (1000 + SPAN, node, le, cum)
                    )
        age = run(out)["phase_by_node"]["val0"]["order_age_ms"]
        assert set(age) == set(ORDER_AGE_STAGES), age
        assert age["exec"] == {"n": 100, "p50": 1280.0, "p90": 2304.0, "p99": 2534.4}, (
            age
        )
        for stage in ("admit", "commit", "durable", "fills_visible"):
            assert age[stage] is None, (stage, age)
    with tempfile.TemporaryDirectory() as out:
        write_fixture(out, bl1="none")
        age = run(out)["phase_by_node"]["val0"]["order_age_ms"]
        assert all(v is None for v in age.values()), age


def check_order_age_buckets_sampled():
    """run-cell.sh must sample every stage's bucket series."""
    with open(os.path.join(HERE, "run-cell.sh")) as f:
        line = next(l for l in f if l.startswith("BUCKET_METRICS="))
    for stage in ORDER_AGE_STAGES:
        name = "torus_order_age_%s_seconds_bucket" % stage
        assert name in line, "BUCKET_METRICS is missing %s" % name


def main_s58():
    """s58 additions, invoked from __main__ after main()."""
    check_view_window()
    check_view_hists_sampled()
    check_order_age()
    check_order_age_buckets_sampled()
    print("test_summarize.py: OK")


def main_end_resident():
    """Item 6 step 1: a binary that times end_resident reports it as an
    exec-thread phase (with its two subs) and residual_untimed drops by it;
    an older binary reports 0.0 and keeps its residual."""
    with tempfile.TemporaryDirectory() as out:
        write_fixture(out, include_r6=True, bl1="pipelined", end_resident=True)
        p0 = run(out)["phase_by_node"]["val0"]
        er = p0["phases"]["end_resident"]
        close(er["ms"], 30.0, "end_resident ms/blk")
        close(er["end_resident_rows_ms"], 12.0, "end_resident_rows_ms")
        close(er["end_resident_positions_ms"], 10.0, "end_resident_positions_ms")
        close(
            er["end_resident_users_ms"], 0.0, "end_resident_users_ms on an older binary"
        )
        assert er["off_chain"] is False, "end_resident runs on the exec thread"
        check_phase_accounting(p0, worker=True)
        close(
            p0["phases"]["residual_untimed"]["ms"],
            15.0,
            "residual (pipelined, end_resident timed)",
        )
        ci = p0["chain_identity"]
        assert "end_resident" in ci["e_phases"], ci
        close(ci["e_phase_sum_ms"], 840.0, "e_phase_sum_ms (end_resident timed)")
    with tempfile.TemporaryDirectory() as out:
        write_fixture(out, include_r6=True, bl1="pipelined")
        p0 = run(out)["phase_by_node"]["val0"]
        close(
            p0["phases"]["end_resident"]["ms"], 0.0, "end_resident on an older binary"
        )
        close(p0["phases"]["residual_untimed"]["ms"], 45.0, "residual (older binary)")
        assert "end_resident" in p0["chain_identity"]["e_phases"]
        close(p0["phases"]["end_resident_wait"]["ms"], 0.0, "no wait before step 2")
        assert p0["end_resident_wait_ms_p50"] is None


def main_end_resident_users():
    """Item 6 Phase 4 step 1: a new binary emits the users sub
    (`exec_end_resident_users_seconds`) and no positions sub; summarize reads
    users, positions reads 0.0, and end_resident / residual are unchanged."""
    with tempfile.TemporaryDirectory() as out:
        write_fixture(out, include_r6=True, bl1="pipelined", end_resident="users")
        p0 = run(out)["phase_by_node"]["val0"]
        er = p0["phases"]["end_resident"]
        close(er["ms"], 30.0, "end_resident ms/blk (users binary)")
        close(er["end_resident_rows_ms"], 12.0, "end_resident_rows_ms (users binary)")
        close(er["end_resident_users_ms"], 10.0, "end_resident_users_ms")
        close(
            er["end_resident_positions_ms"],
            0.0,
            "end_resident_positions_ms on a users binary",
        )
        check_phase_accounting(p0, worker=True)
        close(p0["phases"]["residual_untimed"]["ms"], 15.0, "residual (users binary)")
    print("test_summarize.py end_resident users: OK")


def main_end_resident_worker():
    """Item 6 step 2: end_resident on a worker is an OFF-chain phase (its ms
    still reported, no share of the exec block); the exec thread's join wait
    is the on-chain phase in its place, in residual accounting and the chain
    identity; its p50 / p90 come from the wait buckets."""
    with tempfile.TemporaryDirectory() as out:
        write_fixture(
            out,
            include_r6=True,
            bl1="pipelined",
            end_resident=True,
            end_resident_worker=True,
        )
        p0 = run(out)["phase_by_node"]["val0"]
        er = p0["phases"]["end_resident"]
        close(er["ms"], 30.0, "end_resident ms/blk (worker)")
        close(er["end_resident_rows_ms"], 12.0, "end_resident_rows_ms (worker)")
        assert er["off_chain"] is True, er
        assert er["pct_of_block"] is None, er
        assert "end_resident" in p0["off_chain_phases"], p0["off_chain_phases"]
        w = p0["phases"]["end_resident_wait"]
        close(w["ms"], 8.0, "end_resident_wait ms/blk")
        assert w["off_chain"] is False, w
        check_phase_accounting(p0, worker=True)
        # 45 untimed before step 1, 15 with end_resident on the thread; now
        # only the 8 ms wait is on the thread: 45 - 8.
        close(
            p0["phases"]["residual_untimed"]["ms"],
            37.0,
            "residual (end_resident on a worker)",
        )
        ci = p0["chain_identity"]
        assert (
            "end_resident_wait" in ci["e_phases"]
            and "end_resident" not in ci["e_phases"]
        ), ci
        close(ci["e_phase_sum_ms"], 818.0, "e_phase_sum_ms (840 - 30 + 8)")
        close(p0["end_resident_wait_ms_p50"], 4.0, "wait p50")
        close(p0["end_resident_wait_ms_p90"], 14.4, "wait p90")


def main_checkpoint_timers():
    """Item 6 Phase 3 (p3s2g 47.1): a binary that times the exec thread's
    checkpoint work reports checkpoint_settle_wait and checkpoint_upkeep (with
    its hand-off wait sub) as exec-thread phases, residual_untimed drops by
    their sum, per-observation p50 / p99 come from their buckets, the chain
    identity is unchanged; an older binary reports 0.0 / None; run-cell.sh
    samples the sums, counts and buckets."""
    with tempfile.TemporaryDirectory() as out:
        write_fixture(
            out,
            include_r6=True,
            bl1="pipelined",
            end_resident=True,
            end_resident_worker=True,
            checkpoint=True,
        )
        p0 = run(out)["phase_by_node"]["val0"]
        sw = p0["phases"]["checkpoint_settle_wait"]
        up = p0["phases"]["checkpoint_upkeep"]
        close(sw["ms"], 6.0, "checkpoint_settle_wait ms/blk")
        close(up["ms"], 9.0, "checkpoint_upkeep ms/blk")
        close(up["checkpoint_wait_ms"], 2.0, "checkpoint_wait (sub of upkeep)")
        assert sw["off_chain"] is False and up["off_chain"] is False, (sw, up)
        check_phase_accounting(p0, worker=True)
        # 37 ms untimed with end_resident on a worker (main_end_resident_worker); now 37 - 6 - 9.
        close(
            p0["phases"]["residual_untimed"]["ms"], 22.0, "residual (checkpoint timed)"
        )
        ci = p0["chain_identity"]
        assert (
            "checkpoint_upkeep" not in ci["e_phases"]
            and "checkpoint_settle_wait" not in ci["e_phases"]
        ), ci
        close(ci["e_phase_sum_ms"], 818.0, "e_phase_sum_ms unchanged")
        close(p0["checkpoint_settle_wait_ms_p50"], 4.0, "settle wait p50")
        close(p0["checkpoint_settle_wait_ms_p99"], 7.92, "settle wait p99")
        close(p0["checkpoint_upkeep_ms_p50"], 12.0, "upkeep p50")
        close(p0["checkpoint_upkeep_ms_p99"], 15.92, "upkeep p99")
        close(p0["checkpoint_deferred_join_ms_p50"], 0.3, "deferred join p50")
        close(p0["checkpoint_deferred_join_ms_p99"], 24.8, "deferred join p99")
        assert "checkpoint_deferred_join" not in p0["phases"], (
            "the join is inside upkeep, not a phase"
        )
    with tempfile.TemporaryDirectory() as out:
        write_fixture(
            out,
            include_r6=True,
            bl1="pipelined",
            end_resident=True,
            end_resident_worker=True,
        )
        p0 = run(out)["phase_by_node"]["val0"]
        close(p0["phases"]["checkpoint_upkeep"]["ms"], 0.0, "upkeep on an older binary")
        close(p0["phases"]["residual_untimed"]["ms"], 37.0, "residual (older binary)")
        assert (
            p0["checkpoint_upkeep_ms_p50"] is None
            and p0["checkpoint_settle_wait_ms_p99"] is None
        )
        assert p0["checkpoint_deferred_join_ms_p99"] is None
    with open(os.path.join(HERE, "run-cell.sh")) as f:
        src = f.read()
    wide = next(l for l in src.splitlines() if l.startswith("WIDE_COLS="))
    bkt = next(l for l in src.splitlines() if l.startswith("BUCKET_METRICS="))
    for name in (
        "checkpoint_settle_wait",
        "checkpoint_upkeep",
        "checkpoint_wait",
        "checkpoint_deferred_join",
    ):
        for suf in ("_sum", "_count"):
            assert ("torus_exec_%s_seconds%s" % (name, suf)) in wide.replace(
                '"', " "
            ).split(), (name, suf)
    for name in (
        "checkpoint_settle_wait",
        "checkpoint_upkeep",
        "checkpoint_deferred_join",
    ):
        assert ("torus_exec_%s_seconds_bucket" % name) in bkt.replace(
            '"', " "
        ).split(), name
    print("test_summarize.py checkpoint timers: OK")


def test_checkpoint_timers():
    main_checkpoint_timers()


def test_checkpoint_db_write_split():
    """Q6 step 0: summarize.py reports the checkpoint DB write and its stage
    split per write over the phase window (None on an older binary), and
    run-cell.sh samples every series' sum and count plus the write's start /
    end gauges (any WIDE_COLS line)."""
    with tempfile.TemporaryDirectory() as out:
        write_fixture(
            out,
            include_r6=True,
            bl1="pipelined",
            end_resident=True,
            end_resident_worker=True,
            checkpoint=True,
            checkpoint_db=True,
        )
        p0 = run(out)["phase_by_node"]["val0"]
        assert p0["checkpoint_db_writes"] == CKPT_DB_N, p0.get("checkpoint_db_writes")
        for k, per in CKPT_DB.items():
            close(
                p0["checkpoint_db_%s_ms_per_call" % k], per * 1000, "checkpoint_db " + k
            )
    with tempfile.TemporaryDirectory() as out:
        write_fixture(
            out,
            include_r6=True,
            bl1="pipelined",
            end_resident=True,
            end_resident_worker=True,
            checkpoint=True,
        )
        p0 = run(out)["phase_by_node"]["val0"]
        assert p0["checkpoint_db_writes"] == 0, p0.get("checkpoint_db_writes")
        for k in CKPT_DB:
            assert p0["checkpoint_db_%s_ms_per_call" % k] is None, k
    with open(os.path.join(HERE, "run-cell.sh")) as f:
        src = f.read()
    wide = []
    for line in src.splitlines():
        line = line.strip()
        if line.startswith("WIDE_COLS="):
            wide += line.split("=", 1)[1].replace('"', " ").split()
    for k in CKPT_DB:
        for suf in ("_sum", "_count"):
            assert ("torus_exec_checkpoint_db_%s_seconds%s" % (k, suf)) in wide, (
                k,
                suf,
            )
    for g in (
        "torus_exec_checkpoint_db_write_start_ms",
        "torus_exec_checkpoint_db_write_end_ms",
    ):
        assert g in wide, g


# Q6 step 0, second bullet: the consensus thread's two state-DB writes per
# commit (commit-manifest put, commit-persist WriteBatch write) and RocksDB's
# stage split of each, per call (s). Inside a checkpoint DB-write window of the
# node's own W the write queues (pre/post holds the WAL-group join wait);
# outside it is a plain small write. wall = wal + memtable + delay + pre_post +
# residual; mutex / cond_wait are overlapping signals. The persist write costs
# COMMIT_DB_PERSIST_X times the manifest put.
COMMIT_DB_IN = {
    "write": 0.030,
    "wal": 0.002,
    "memtable": 0.001,
    "delay": 0.0,
    "pre_post": 0.020,
    "mutex": 0.004,
    "cond_wait": 0.003,
    "residual": 0.007,
}
COMMIT_DB_OUT = {
    "write": 0.0004,
    "wal": 0.0002,
    "memtable": 0.0001,
    "delay": 0.0,
    "pre_post": 0.00005,
    "mutex": 0.0,
    "cond_wait": 0.0,
    "residual": 0.00005,
}
COMMIT_DB_PERSIST_X = 2.0
COMMIT_DB_PER_S = 4
# Each node's checkpoint DB-write spans (unix ms). val0's touch the 1 s sample
# intervals ending at 1004, 1005 and 1009; val1's the one ending at 1007.
COMMIT_DB_SPANS = {
    "val0": [(1003200, 1004600), (1008100, 1008300)],
    "val1": [(1006500, 1006900)],
    "val2": [],
}
COMMIT_DB_STAGES = [
    "wal",
    "memtable",
    "delay",
    "pre_post",
    "mutex",
    "cond_wait",
    "residual",
]


def add_commit_db_rows(out):
    """Rewrite write_fixture's two-row sampler.csv as 1 Hz rows (ts 1000 ..
    1000 + SPAN, every existing column linear in between) and add the
    consensus-thread write columns (cumulative, per interval in / out of the
    node's own checkpoint spans) and the checkpoint start / end gauges (the
    last span that ended before the sample)."""
    path = os.path.join(out, "sampler.csv")
    with open(path) as f:
        lines = f.read().splitlines()
    header = lines[0].split(",")
    two = {}
    for line in lines[1:]:
        f = line.split(",")
        two.setdefault(f[1], []).append([float(x) for x in f[2:]])
    walls = {
        "commit_manifest": "torus_commit_manifest_seconds",
        "commit_persist": "torus_commit_persist_write_seconds",
    }
    new_cols = [
        "torus_exec_checkpoint_db_write_start_ms",
        "torus_exec_checkpoint_db_write_end_ms",
    ]
    for w, wall in walls.items():
        new_cols += [wall + "_sum", wall + "_count"]
        for k in COMMIT_DB_STAGES:
            new_cols += [
                "torus_%s_db_%s_seconds_sum" % (w, k),
                "torus_%s_db_%s_seconds_count" % (w, k),
            ]
    with open(path, "w") as f:
        f.write(",".join(header + new_cols) + "\n")
        for node in ("val0", "val1", "val2"):
            a, b = two[node]
            spans = COMMIT_DB_SPANS[node]
            acc = {c: 0.0 for c in new_cols[2:]}
            for i in range(SPAN + 1):
                ts = 1000 + i
                if i > 0:
                    inside = any(
                        s / 1000.0 <= ts and e / 1000.0 > ts - 1 for s, e in spans
                    )
                    per = COMMIT_DB_IN if inside else COMMIT_DB_OUT
                    for w, wall in walls.items():
                        x = COMMIT_DB_PERSIST_X if w == "commit_persist" else 1.0
                        acc[wall + "_sum"] += per["write"] * x * COMMIT_DB_PER_S
                        acc[wall + "_count"] += COMMIT_DB_PER_S
                        for k in COMMIT_DB_STAGES:
                            acc["torus_%s_db_%s_seconds_sum" % (w, k)] += (
                                per[k] * x * COMMIT_DB_PER_S
                            )
                            acc["torus_%s_db_%s_seconds_count" % (w, k)] += (
                                COMMIT_DB_PER_S
                            )
                done = [sp for sp in spans if sp[1] / 1000.0 < ts]
                gauges = list(done[-1]) if done else [0, 0]
                base = [x + (y - x) * i / SPAN for x, y in zip(a, b)]
                vals = (
                    base + [float(g) for g in gauges] + [acc[c] for c in new_cols[2:]]
                )
                f.write("%d,%s,%s\n" % (ts, node, ",".join("%r" % v for v in vals)))


def test_commit_db_write_split_in_vs_out():
    """Q6 step 0, second bullet: summarize.py reports the consensus thread's
    two state-DB writes and their RocksDB stage split per call over the phase
    window, and split by whether each 1 s sample interval touches a checkpoint
    DB-write span of the node's own W (`own`) or of any node (`any`). None on
    an older binary; run-cell.sh samples every series' sum and count."""

    def side(calls, per_in, per_out, n_in, x):
        # n_in of `calls` calls at per_in cost, the rest at per_out.
        return {
            k: (per_in[k] * n_in + per_out[k] * (calls - n_in)) * x / calls * 1000
            for k in per_in
        }

    with tempfile.TemporaryDirectory() as out:
        write_fixture(
            out,
            include_r6=True,
            bl1="pipelined",
            end_resident=True,
            end_resident_worker=True,
            checkpoint=True,
        )
        add_commit_db_rows(out)
        p0 = run(out)["phase_by_node"]["val0"]
        total = SPAN * COMMIT_DB_PER_S
        n_in = 3 * COMMIT_DB_PER_S
        for w, x in (("commit_manifest", 1.0), ("commit_persist", COMMIT_DB_PERSIST_X)):
            whole = side(total, COMMIT_DB_IN, COMMIT_DB_OUT, n_in, x)
            for k in COMMIT_DB_STAGES:
                close(
                    p0["%s_db_%s_ms_per_call" % (w, k)],
                    whole[k],
                    "%s whole-window %s" % (w, k),
                )
        close(
            p0["commit_persist_write_ms_per_call"],
            side(total, COMMIT_DB_IN, COMMIT_DB_OUT, n_in, COMMIT_DB_PERSIST_X)[
                "write"
            ],
            "persist wall",
        )
        split = p0["commit_db_split"]
        own, anyn = split["own"], split["any"]
        assert own["checkpoint_db_writes"] == 2, own
        assert (own["in_s"], own["out_s"]) == (3, 7), own
        assert anyn["checkpoint_db_writes"] == 3, anyn
        assert (anyn["in_s"], anyn["out_s"]) == (4, 6), anyn
        for w, x in (("commit_manifest", 1.0), ("commit_persist", COMMIT_DB_PERSIST_X)):
            assert (
                own[w]["in"]["calls"] == n_in and own[w]["out"]["calls"] == total - n_in
            ), own[w]
            for k in ["write"] + COMMIT_DB_STAGES:
                close(
                    own[w]["in"][k + "_ms_per_call"],
                    COMMIT_DB_IN[k] * x * 1000,
                    "%s own in %s" % (w, k),
                )
                close(
                    own[w]["out"][k + "_ms_per_call"],
                    COMMIT_DB_OUT[k] * x * 1000,
                    "%s own out %s" % (w, k),
                )
            # val1's span adds one interval of out-cost calls to val0's `any` in side.
            a_in = side(n_in + COMMIT_DB_PER_S, COMMIT_DB_IN, COMMIT_DB_OUT, n_in, x)
            assert anyn[w]["in"]["calls"] == n_in + COMMIT_DB_PER_S, anyn[w]
            for k in ["write"] + COMMIT_DB_STAGES:
                close(
                    anyn[w]["in"][k + "_ms_per_call"], a_in[k], "%s any in %s" % (w, k)
                )
                close(
                    anyn[w]["out"][k + "_ms_per_call"],
                    COMMIT_DB_OUT[k] * x * 1000,
                    "%s any out %s" % (w, k),
                )
    with tempfile.TemporaryDirectory() as out:
        write_fixture(
            out,
            include_r6=True,
            bl1="pipelined",
            end_resident=True,
            end_resident_worker=True,
            checkpoint=True,
        )
        p0 = run(out)["phase_by_node"]["val0"]
        for w in ("commit_manifest", "commit_persist"):
            for k in COMMIT_DB_STAGES:
                assert p0["%s_db_%s_ms_per_call" % (w, k)] is None, (w, k)
            assert p0["commit_db_split"]["own"][w]["in"]["calls"] == 0
            assert p0["commit_db_split"]["own"][w]["in"]["write_ms_per_call"] is None
        assert p0["commit_db_split"]["own"]["checkpoint_db_writes"] == 0
    with open(os.path.join(HERE, "run-cell.sh")) as f:
        src = f.read()
    wide = []
    for line in src.splitlines():
        line = line.strip()
        if line.startswith("WIDE_COLS="):
            wide += line.split("=", 1)[1].replace('"', " ").split()
    for w in ("commit_manifest", "commit_persist"):
        for k in COMMIT_DB_STAGES:
            for suf in ("_sum", "_count"):
                assert ("torus_%s_db_%s_seconds%s" % (w, k, suf)) in wide, (w, k, suf)
    for c in (
        "torus_commit_manifest_seconds_sum",
        "torus_commit_manifest_seconds_count",
        "torus_commit_persist_write_seconds_sum",
        "torus_commit_persist_write_seconds_count",
    ):
        assert c in wide, c


def fail_scrape(out, node, ts, gauges=(0, 0)):
    """Add a scrape_valid column (1 everywhere) to sampler.csv and turn `node`'s
    row at `ts` into a failed scrape: sample_metrics.py writes every counter as
    0 and scrape_valid=0. `gauges` overrides the checkpoint start / end gauges of
    that row (an invalid row's gauges must not be trusted either)."""
    path = os.path.join(out, "sampler.csv")
    with open(path) as f:
        lines = f.read().splitlines()
    header = lines[0].split(",")
    gs = header.index("torus_exec_checkpoint_db_write_start_ms")
    ge = header.index("torus_exec_checkpoint_db_write_end_ms")
    with open(path, "w") as f:
        f.write(",".join(header + ["scrape_valid"]) + "\n")
        for line in lines[1:]:
            v = line.split(",")
            if v[1] == node and int(float(v[0])) == ts:
                v = v[:2] + ["0.0"] * (len(v) - 2)
                v[gs], v[ge] = str(float(gauges[0])), str(float(gauges[1]))
                f.write(",".join(v + ["0"]) + "\n")
            else:
                f.write(",".join(v + ["1"]) + "\n")


def test_commit_db_split_skips_failed_scrape():
    """Codex P2 (Q6 step 0 re-review): a failed scrape (zeroed counters,
    scrape_valid=0) mid-series must not corrupt the split. Both intervals
    touching the invalid row contribute nothing (calls, timings, exposure); the
    next interval starts from a valid row, so accumulated history is never
    counted as one interval. val0's row at 1006 fails: intervals (1005, 1006]
    and (1006, 1007] drop out. Both are `out` for val0's own spans; for `any`
    the second is val1's `in` interval. The invalid row carries a bogus
    checkpoint span gauge pair touching (1009, 1010] that must be ignored."""
    with tempfile.TemporaryDirectory() as out:
        write_fixture(
            out,
            include_r6=True,
            bl1="pipelined",
            end_resident=True,
            end_resident_worker=True,
            checkpoint=True,
        )
        add_commit_db_rows(out)
        fail_scrape(out, "val0", 1006, gauges=(1009500, 1009700))
        p0 = run(out)["phase_by_node"]["val0"]
        split = p0["commit_db_split"]
        own, anyn = split["own"], split["any"]
        total = SPAN * COMMIT_DB_PER_S
        n_in = 3 * COMMIT_DB_PER_S
        dropped = 2 * COMMIT_DB_PER_S
        assert own["checkpoint_db_writes"] == 2, own
        assert (own["in_s"], own["out_s"]) == (3, 5), own
        assert anyn["checkpoint_db_writes"] == 3, anyn
        assert (anyn["in_s"], anyn["out_s"]) == (3, 5), anyn
        for w, x in (("commit_manifest", 1.0), ("commit_persist", COMMIT_DB_PERSIST_X)):
            for name, sp in (("own", own), ("any", anyn)):
                assert sp[w]["in"]["calls"] == n_in, (name, sp[w])
                assert sp[w]["out"]["calls"] == total - n_in - dropped, (name, sp[w])
                for k in ["write"] + COMMIT_DB_STAGES:
                    close(
                        sp[w]["in"][k + "_ms_per_call"],
                        COMMIT_DB_IN[k] * x * 1000,
                        "%s %s in %s" % (w, name, k),
                    )
                    close(
                        sp[w]["out"][k + "_ms_per_call"],
                        COMMIT_DB_OUT[k] * x * 1000,
                        "%s %s out %s" % (w, name, k),
                    )


def add_oracle_drain(out, blocks, secs):
    """ORACLE_FEED_DRAIN=1 shape: after the load window, the live feed keeps
    producing ORACLE-ONLY native blocks through the drain — every per-block
    series counts them (`secs` each), no order fills. One more sampler row per
    node at 1000 + SPAN + 20 (the drain end)."""
    path = os.path.join(out, "sampler.csv")
    with open(path) as f:
        lines = f.read().splitlines()
    cols = lines[0].split(",")
    extra = []
    for line in lines[1:]:
        row = line.split(",")
        if row[0] != str(1000 + SPAN):
            continue
        row[0] = str(1000 + SPAN + 20)
        for i, c in enumerate(cols[2:], start=2):
            v = float(row[i])
            if c.endswith("_count") or c in (
                "torus_exec_native_blocks_total",
                "torus_blocks_committed_total",
                "torus_block_height",
            ):
                v += blocks
            elif c.endswith("_sum"):
                v += blocks * secs
            row[i] = "%r" % v
        extra.append(",".join(row))
    with open(path, "w") as f:
        f.write("\n".join(lines + extra) + "\n")


def run_drain(out, stdout=False):
    """`run` with a 20 s drain after the load window."""
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
        str(1000 + SPAN + 20),
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
        s = json.load(f)
    return (s, r.stdout) if stdout else s


# The exec-chain ruler: every field below is per native block or derived from
# one, so in feed-drain mode none of it may mix the load and drain windows.
RULER = (
    "chain_ms",
    "gap_to_100ms",
    "pipelined_ms",
    "handoff_wait_ms",
    "empty_block_ms",
    "fills_per_native_block",
    "engine_ms_per_1k_fills",
    "chain_ms_p50",
    "chain_ms_p95",
    "handoff_wait_ms_p95",
    "pipelined_ms_p95",
    "native_blocks_counter",
    "block_ms",
    "phase_window_native_blocks",
    "chain_identity",
    "phases",
)


def main_feed_drain():
    """ORACLE_FEED_DRAIN=1 (drain.json carries `feed_live`): fills per native
    block and chain ms cover the LOAD window only — the 30 oracle-only native
    blocks the live feed adds to the drain would otherwise dilute fills/blk
    (15000/40 = 375 instead of 1500) and the chain mean. The bench+drain
    values are kept beside them. Default mode (no feed_live) is unchanged."""
    oracle_blocks, oracle_secs = 30, 0.005
    for bl1 in ("serial", "pipelined"):
        with tempfile.TemporaryDirectory() as ref:
            # The load window alone: what every ruler field must equal in
            # feed-drain mode.
            write_fixture(ref, bl1=bl1)
            want = run(ref)["phase_by_node"]["val0"]
        with tempfile.TemporaryDirectory() as d:
            write_fixture(d, bl1=bl1)
            add_oracle_drain(d, oracle_blocks, oracle_secs)
            with open(os.path.join(d, "drain.json"), "w") as f:
                json.dump(
                    {"drained": True, "feed_live": {"native_blocks": oracle_blocks}}, f
                )
            s, out = run_drain(d, stdout=True)
            p0 = s["phase_by_node"]["val0"]
            for k in RULER:
                assert p0[k] == want[k], (bl1, k, p0[k], want[k])
            assert p0["phase_window"].startswith("load [t_bench0, t_bench1]"), p0[
                "phase_window"
            ]
            assert p0["chain_identity"]["chain_covers_e_phases"] is True, p0[
                "chain_identity"
            ]
            # Cadence and the drain cadence are untouched by the mode.
            assert p0["drain"] is not None
            line = next(l for l in out.splitlines() if l.startswith("CHAIN val0:"))
            assert "chain_ms=%s " % want["chain_ms"] in line, line
            assert "empty_block_ms=%s " % want["empty_block_ms"] in line, line
            assert "fills/blk=%s " % want["fills_per_native_block"] in line, line
    with tempfile.TemporaryDirectory() as d:
        write_fixture(d)
        add_oracle_drain(d, oracle_blocks, oracle_secs)
        p0 = run_drain(d)["phase_by_node"]["val0"]
        # Default mode: both stay on bench+drain, exactly as before.
        nb = BLOCKS + oracle_blocks
        close(
            p0["fills_per_native_block"], 15000 / nb, "default fills/blk (bench+drain)"
        )
        bd_chain = (
            (BL1_SERIAL["exec_chain_seconds"] * BLOCKS + oracle_blocks * oracle_secs)
            / nb
            * 1000
        )
        close(p0["chain_ms"], bd_chain, "default chain_ms (bench+drain)")
        assert "bench_drain" not in p0, sorted(p0)
        assert p0["phase_window"] == "bench+drain [t_bench0, t_drain]", p0[
            "phase_window"
        ]
        default_keys = sorted(p0)

        with open(os.path.join(d, "drain.json"), "w") as f:
            json.dump(
                {"drained": True, "feed_live": {"native_blocks": oracle_blocks}}, f
            )
        s = run_drain(d)
        p0 = s["phase_by_node"]["val0"]
        close(
            p0["fills_per_native_block"], 1500.0, "feed-drain fills/blk (load window)"
        )
        close(p0["chain_ms"], 1080.0, "feed-drain chain_ms (load window)")
        close(p0["gap_to_100ms"], 980.0, "feed-drain gap_to_100ms follows chain_ms")
        assert p0["phase_window"].startswith("load [t_bench0, t_bench1]"), p0[
            "phase_window"
        ]
        bd = p0["bench_drain"]
        close(bd["fills_per_native_block"], 15000 / nb, "kept bench+drain fills/blk")
        close(bd["chain_ms"], bd_chain, "kept bench+drain chain_ms")
        close(bd["gap_to_100ms"], bd_chain - 100, "kept bench+drain gap")
        h = s["headline"]
        close(h["chain_ms"], 1080.0, "headline chain_ms")
        close(h["fills_per_native_block"], 1500.0, "headline fills/blk")
        # Same keys as default mode, plus the kept bench+drain values.
        assert sorted(set(p0) - {"bench_drain"}) == default_keys
    print("test_summarize.py feed-drain: OK")


def main_phase2_step0():
    """Item 6 Phase 2 step 0.2: per cancel-all books visited / hit, per by-id
    action books probed, thread spawns per site per native block and per
    minute, exec ms per oracle-only block (node timer + buckets), and the
    nodes' user / sys CPU per native block and per 1k fills from the
    /proc/<pid>/stat snapshots (procstat.raw)."""
    step0 = {
        "exec_cancel_all_total": 40,
        "exec_cancel_all_books_visited_total": 12000,
        "exec_cancel_all_books_hit_total": 200,
        "exec_by_id_actions_total": 20,
        "exec_by_id_books_probed_total": 6000,
        "exec_thread_spawns_match": 620,
        "exec_thread_spawns_end_resident": 10,
        # P2-1 (s104): the cancel-all index size gauges at the window's end.
        "exec_cancel_all_index_entries": 900,
        "exec_cancel_all_index_traders": 300,
    }
    saved = dict(COUNTERS)
    COUNTERS.update(step0)
    try:
        with tempfile.TemporaryDirectory() as out:
            write_fixture(out, include_r6=True, bl1="serial")
            # Oracle-only blocks: 10 at 5 ms mean; 4 at or below 4 ms, all at
            # or below 8 ms => p50 = 4 + 4 * (5 - 4) / 6 ms.
            with open(os.path.join(out, "sampler.csv")) as f:
                lines = f.read().splitlines()
            lines[0] += (
                ",torus_exec_oracle_only_block_seconds_sum,torus_exec_oracle_only_block_seconds_count"
            )
            for i in range(1, len(lines)):
                lines[i] += ",0.0,0.0" if lines[i].startswith("1000,") else ",0.05,10.0"
            with open(os.path.join(out, "sampler.csv"), "w") as f:
                f.write("\n".join(lines) + "\n")
            with open(os.path.join(out, "buckets.csv"), "a") as f:
                for ts, scale in ((1000, 0), (1000 + SPAN, 1)):
                    for le, cum in (("0.004", 4), ("0.008", 10), ("+Inf", 10)):
                        f.write(
                            "%d,val0,torus_exec_oracle_only_block_seconds_bucket,%s,%d\n"
                            % (ts, le, cum * scale)
                        )
            tck = os.sysconf("SC_CLK_TCK")
            with open(os.path.join(out, "procstat.raw"), "w") as f:
                f.write("before val0 4242 1000 200\n")
                f.write("bench_end val0 4242 %d %d\n" % (1000 + 3 * tck, 200 + tck))
                f.write("after val0 4242 %d %d\n" % (1000 + 4 * tck, 200 + 2 * tck))
                f.write("before val1 4243 7 7\n")
            s = run(out)
        p0 = s["phase_by_node"]["val0"]
        ca = p0["cancel_all"]
        close(ca["cancel_alls"], 40, "cancel_alls")
        close(ca["per_native_block"], 4.0, "cancel-alls per native block")
        close(ca["books_visited_per_cancel_all"], 300.0, "books visited per cancel-all")
        close(ca["books_hit_per_cancel_all"], 5.0, "books hit per cancel-all")
        close(ca["index_entries_end"], 900, "index (trader, market) entries at the end")
        close(ca["index_traders_end"], 300, "index traders at the end")
        bi = p0["by_id"]
        close(bi["actions"], 20, "by-id actions")
        close(bi["books_probed_per_action"], 300.0, "books probed per by-id action")
        ts_ = p0["thread_spawns"]
        close(ts_["per_native_block"]["match"], 62.0, "match spawns per native block")
        close(
            ts_["per_native_block"]["end_resident"],
            1.0,
            "end_resident spawns per native block",
        )
        close(ts_["per_native_block"]["flush_digest"], 0.0, "absent site reads 0")
        close(ts_["total_per_native_block"], 63.0, "spawns per native block")
        close(ts_["total_per_minute"], 630 / SPAN * 60, "spawns per minute")
        oo = p0["oracle_only_blocks"]
        close(oo["blocks"], 10, "oracle-only blocks")
        close(oo["ms_avg"], 5.0, "oracle-only ms avg")
        close(oo["ms_p50"], 4.0 + 4.0 * 1 / 6, "oracle-only p50")
        pc = s["proc_cpu_by_node"]["val0"]
        assert pc["pid"] == 4242, pc
        ld, wr = pc["load"], pc["whole_run"]
        close(ld["user_ms"], 3000.0, "load user ms")
        close(ld["sys_ms"], 1000.0, "load sys ms")
        # load window: 10 native blocks, 15000 fills (the fixture's sampler).
        close(ld["sys_ms_per_native_block"], 100.0, "sys ms per native block")
        close(ld["sys_ms_per_1k_fills"], 1000.0 / 15, "sys ms per 1k fills")
        close(ld["user_ms_per_1k_fills"], 200.0, "user ms per 1k fills")
        close(wr["sys_ms"], 2000.0, "whole-run sys ms")
        assert wr["sys_ms_per_native_block"] is None, (
            "no metrics-before/after in the fixture"
        )
        assert s["proc_cpu_by_node"]["val1"] is None, "val1 has no bench_end snapshot"
    finally:
        COUNTERS.clear()
        COUNTERS.update(saved)
    print("test_summarize.py phase2-step0: OK")


if __name__ == "__main__":
    main()
    main_end_resident()
    main_end_resident_users()
    main_end_resident_worker()
    main_s58()
    main_feed_drain()
    main_phase2_step0()
    main_checkpoint_timers()
