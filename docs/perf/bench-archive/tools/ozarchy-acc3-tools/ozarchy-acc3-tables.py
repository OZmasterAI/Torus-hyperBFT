#!/usr/bin/env python3
# ozarchy-acc3-tables.py  (run as: python3 -I ozarchy-acc3-tables.py). READ-ONLY on the cells; writes
# ozarchy-acc3-run/handoff-tables.txt: numbers only (no ranking, no verdict). Arrows mark the better direction per column.
#   CPU-s/1M fills   = (summary proc_cpu_by_node.<val>.whole_run user_ms + sys_ms)/1000 / (fills/1e6), mean of val0-2
#   write MB/s       = storage write_bytes (io-ticks.txt, 3 node pids, t_bench0..t_bench1) / bench seconds
#   wchar MB/s       = logical writes (io-ticks.txt wchar), same window
#   SST live GB      = table_file_creation minus table_file_deletion bytes in each validator's RocksDB LOG (whole DB life)
#   growth GB/day    = live SST at bench end / bench seconds * 86400 (fresh DB per cell)
#   append CF        = cf_native_trades + cf_native_user_trades + cf_native_pending (the screen's append-only set)
#   trade rows       = lsm rows of cf_native_trades + cf_native_user_trades (data rows, range tombstones excluded)
#   warm-up          = twd-warm-rs (excluded from every measured column): restart facts only, no verdict
import hashlib
import json
import os
import re
import statistics

R = "/home/oz/bench-results-matched"
RUN = R + "/ozarchy-acc3-run"
PFX = "ozarchy-acc3-300m-"
ARMS = {
    "b": ["b-r1", "b-r2", "b-r3", "b-r4"],
    "tw": ["tw-r1", "tw-r2", "tw-r3", "tw-r4"],
    "twd": ["twd-r1", "twd-r2", "twd-r3", "twd-r4"],
}
WARM = "warm-twd-rs"
APPEND = ("cf_native_trades", "cf_native_user_trades", "cf_native_pending")
TRADE = ("cf_native_trades", "cf_native_user_trades")
GB, MB = 1e9, 1e6
VALS = ("val0", "val1", "val2")


def read_text(path):
    with open(path, errors="replace") as f:
        return f.read()


def jload(path):
    with open(path) as f:
        return json.load(f)


def mean(xs):
    return sum(xs) / len(xs)


def sd(xs):
    return statistics.stdev(xs) if len(xs) > 1 else None


def f(x, nd=1):
    return "-" if x is None else ("%." + str(nd) + "f") % x


def pct(x, base):
    return "-" if not base or x is None else "%+.2f%%" % (100.0 * (x - base) / base)


def parse_log(path):
    """created: file_number -> (cf, bytes) for table files with bytes > 0; deleted: set of file numbers."""
    created, deleted = {}, set()
    for line in open(path, errors="replace"):
        i = line.find("EVENT_LOG_v1 ")
        if i < 0 or (
            "table_file_creation" not in line and "table_file_deletion" not in line
        ):
            continue
        try:
            d = json.loads(line[i + len("EVENT_LOG_v1 ") :])
        except ValueError:
            continue
        if (
            "table_file_creation" in line
            and d.get("cf_name") is not None
            and d.get("file_number")
        ):
            size = int(d.get("file_size", 0))
            if size > 0:
                created[d["file_number"]] = (d["cf_name"], size)
        elif "table_file_deletion" in line and d.get("file_number"):
            deleted.add(d["file_number"])
    return created, deleted


def log_stats(cell, bench_s):
    acc = {"created": [], "live": [], "append_created": [], "append_live": []}
    for v in VALS:
        created, deleted = parse_log(os.path.join(cell, "rocksdb-LOG-" + v + ".txt"))
        gone = [created[n] for n in deleted if n in created]
        acc["created"].append(sum(sz for _, sz in created.values()) / GB)
        acc["live"].append(
            (sum(sz for _, sz in created.values()) - sum(sz for _, sz in gone)) / GB
        )
        app = [sz for cf, sz in created.values() if cf in APPEND]
        app_gone = [sz for cf, sz in gone if cf in APPEND]
        acc["append_created"].append(sum(app) / GB)
        acc["append_live"].append((sum(app) - sum(app_gone)) / GB)
    return {
        "created_GB": mean(acc["created"]),
        "live_GB": mean(acc["live"]),
        "growth_GB_day": mean(acc["live"]) / bench_s * 86400,
        "append_live_GB": mean(acc["append_live"]),
        "append_growth_GB_day": mean(acc["append_live"]) / bench_s * 86400,
    }


def io_rates(cell, t0, t1):
    """io-ticks.txt rows: ts pid rchar wchar read_bytes write_bytes cancelled_write_bytes (1 Hz per node pid)."""
    rows = {}
    for line in read_text(os.path.join(cell, "io-ticks.txt")).splitlines():
        p = line.split()
        if len(p) >= 6:
            rows.setdefault(p[1], []).append((float(p[0]), int(p[3]), int(p[5])))
    wb = wc = 0.0
    n = 0
    for rs in rows.values():
        inside = [r for r in rs if t0 <= r[0] <= t1]
        if len(inside) < 2:
            continue
        a, b = inside[0], inside[-1]
        wb += b[2] - a[2]
        wc += b[1] - a[1]
        n += 1
    if n != 3:
        return None
    dur = t1 - t0
    return {"write_MBps": wb / dur / MB, "wchar_MBps": wc / dur / MB}


def trade_rows(cell):
    per_val = []
    for v in VALS:
        rows = 0
        for line in read_text(os.path.join(cell, "lsm-" + v + ".txt")).splitlines():
            m = re.match(r"cf=(\S+) files=(\d+) bytes=(\d+) rows=(\d+)", line)
            if m and m.group(1) in TRADE:
                rows += int(m.group(4))
        per_val.append(rows)
    return mean(per_val)


def data_du(cell):
    tot, sst, wal = [], [], []
    for line in read_text(os.path.join(cell, "data-du.txt")).splitlines():
        p = line.split()
        if len(p) >= 8 and p[0] in VALS:
            tot.append(int(p[2]) / GB)
            sst.append(int(p[4]) / GB)
            wal.append(int(p[-1]) / GB)
    return {"total_GB": mean(tot), "sst_GB": mean(sst), "wal_GB": mean(wal)}


def node_md5(cell):
    return ",".join(
        sorted(
            set(
                re.findall(
                    r"exe_md5=(\w+)",
                    read_text(os.path.join(cell, "node-environ-trie.txt")),
                )
            )
        )
    )


def checks_ok(tag):
    return read_text(RUN + "/campaign.log").count("CHECK OK " + PFX + tag + " ")


def cell_row(tag):
    cell = os.path.join(R, PFX + tag)
    rc = read_text(os.path.join(R, PFX + tag + ".cell.rc")).strip()
    s = jload(os.path.join(cell, "summary.json"))
    h = s["headline"]
    t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
    cpu_v = []
    for v in VALS:
        w = s["proc_cpu_by_node"][v]["whole_run"]
        cpu_v.append((w["user_ms"] + w["sys_ms"]) / 1000.0 / (w["fills"] / 1e6))
    return {
        "tag": tag,
        "rc": rc,
        "agree": h.get("agreement_verdict"),
        "liveness": h.get("liveness_verdict"),
        "validity": s["validity"]["verdict"],
        "stale": (s.get("oracle_feed") or {}).get("stale_marks_at_bench_end"),
        "md5": node_md5(cell),
        "matched": h["matched_s_avg"],
        "placed": h["placed_s_avg"],
        "blk": h["blk_s_avg"],
        "wall": s["timing"]["bench_wall_s"],
        "cpu": mean(cpu_v),
        "cpu_v": cpu_v,
        "io": io_rates(cell, t0, t1),
        "log": log_stats(cell, t1 - t0),
        "du": data_du(cell),
        "trade_rows": trade_rows(cell),
        "checks": checks_ok(tag),
    }


def restart_cols(m):
    return "%s | %s | %s | %s | %s | %s | %s | %s | %s | %s" % (
        f(m.get("down_s"), 2),
        f(m.get("db_open_s"), 2),
        f(m.get("restart_to_db_open_s"), 2),
        m.get("wal_files_replayed"),
        f((m.get("wal_bytes_replayed") or 0) / MB, 0),
        f((m.get("wal_bytes_at_kill") or 0) / MB, 0),
        m.get("rewind_blocks"),
        f(m.get("restart_to_first_commit_s"), 2),
        f(m.get("max_commit_gap_s"), 2),
        "yes" if m.get("wal_replayed_missing_from_sample") else "no",
    )


def warm_facts():
    """Warm-up (recovery check, excluded): restart metrics, post-restart commits and ERROR/panic lines of val1, state digests."""
    cell = os.path.join(R, PFX + WARM)
    rc = read_text(os.path.join(R, PFX + WARM + ".cell.rc")).strip()
    s = jload(os.path.join(cell, "summary.json"))
    h = s["headline"]
    m = jload(os.path.join(cell, "restart-metrics.json"))
    tail = read_text(os.path.join(cell, "crash-restart-tail.log")).splitlines()
    commits = [l for l in tail if "on_committed_block: sending to execution pipeline" in l]
    heights = [int(x) for x in re.findall(r"height=(\d+)", " ".join(commits[-1:]))]
    errs = [l for l in tail if " ERROR " in l]
    panics = [l for l in tail if "panicked" in l]
    gate = [
        l.strip()[:220]
        for l in read_text(os.path.join(cell, "run.log")).splitlines()
        if "crash gate" in l or "VALIDITY" in l
    ]
    digs = []
    for v in VALS:
        with open(os.path.join(cell, "state-digest-" + v + ".txt"), "rb") as fh:
            digs.append(hashlib.sha256(fh.read()).hexdigest()[:12])
    return {
        "rc": rc,
        "agree": h.get("agreement_verdict"),
        "liveness": h.get("liveness_verdict"),
        "validity": s["validity"]["verdict"],
        "stale": (s.get("oracle_feed") or {}).get("stale_marks_at_bench_end"),
        "md5": node_md5(cell),
        "m": m,
        "post_commits": len(commits),
        "last_height": heights[0] if heights else None,
        "errors": [l.strip()[:220] for l in errs],
        "panics": len(panics),
        "gate": gate,
        "digests": digs,
        "digests_equal": len(set(digs)) == 1,
    }


def main():
    out = [
        "ozarchy-acc3 handoff tables (numbers only; read-only extraction by ozarchy-acc3-tables.py)",
        "Arrows: ↑ higher is better, ↓ lower is better. Throughput cells: 120 s, 300 markets, Classic, fresh data dir per cell. Warm-up excluded.",
        "",
        "== A. Per throughput cell",
        "cell | rc | agree | liveness | validity | oracle stale | node md5 | checks OK | matched/s ↑ | placed/s ↑ | blk/s ↑ | bench s",
    ]
    rows = {}
    for arm in ARMS:
        for tag in ARMS[arm]:
            r = cell_row(tag)
            rows[tag] = r
            out.append(
                "%s | %s | %s | %s | %s | %s | %s | %d | %s | %s | %s | %s"
                % (
                    tag,
                    r["rc"],
                    r["agree"],
                    r["liveness"],
                    r["validity"],
                    r["stale"],
                    r["md5"],
                    r["checks"],
                    f(r["matched"]),
                    f(r["placed"]),
                    f(r["blk"]),
                    r["wall"],
                )
            )
    out += [
        "",
        "== B. Per throughput cell: node CPU, disk, trade rows",
        "cell | CPU-s/1M fills ↓ mean (val0/val1/val2) | write MB/s ↓ (sum val0-2) | wchar MB/s ↓ (sum val0-2) | SST created GB ↓ (per validator) | "
        "SST live end GB ↓ (per validator) | SST growth GB/day ↓ (per validator) | append-CF live GB ↓ | append-CF growth GB/day ↓ | "
        "data end GB ↓ total (sst / wal) | trade rows ↑",
    ]
    for tag, r in rows.items():
        io = r["io"] or {}
        out.append(
            "%s | %s (%s / %s / %s) | %s | %s | %s | %s | %s | %s | %s | %s (%s / %s) | %s"
            % (
                tag,
                f(r["cpu"], 2),
                f(r["cpu_v"][0], 2),
                f(r["cpu_v"][1], 2),
                f(r["cpu_v"][2], 2),
                f(io.get("write_MBps")),
                f(io.get("wchar_MBps")),
                f(r["log"]["created_GB"], 2),
                f(r["log"]["live_GB"], 2),
                f(r["log"]["growth_GB_day"], 1),
                f(r["log"]["append_live_GB"], 2),
                f(r["log"]["append_growth_GB_day"], 1),
                f(r["du"]["total_GB"], 2),
                f(r["du"]["sst_GB"], 2),
                f(r["du"]["wal_GB"], 2),
                f(r["trade_rows"], 0),
            )
        )
    out += [
        "",
        "== C. Per arm (4 cells each). Δ vs b = arm mean / b mean - 1; twd vs tw = twd mean / tw mean - 1",
        "arm | matched/s ↑ mean | sd | r1 | r2 | r3 | r4 | Δ vs b | CPU-s/1M ↓ mean | sd | Δ vs b | "
        "write MB/s ↓ mean (sum val0-2) | Δ vs b | data end GB ↓ (per validator) | Δ vs b | growth GB/day ↓ (per validator) | append growth GB/day ↓ | trade rows ↑ mean (per validator)",
    ]
    stats = {}
    raw = {}
    for arm in ARMS:
        rs = [rows[t] for t in ARMS[arm]]
        m = [r["matched"] for r in rs]
        c = [r["cpu"] for r in rs]
        w = [r["io"]["write_MBps"] for r in rs if r["io"]]
        du = [r["du"]["total_GB"] for r in rs]
        g = [r["log"]["growth_GB_day"] for r in rs]
        ag = [r["log"]["append_growth_GB_day"] for r in rs]
        raw[arm] = {"m": m, "c": c, "w": w, "du": du, "g": g, "ag": ag}
        stats[arm] = {"m": mean(m), "c": mean(c), "w": mean(w), "du": mean(du), "g": mean(g), "ag": mean(ag)}
    base = stats["b"]
    for arm in ARMS:
        st = stats[arm]
        m = raw[arm]["m"]
        c = raw[arm]["c"]
        out.append(
            "%s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s"
            % (
                arm,
                f(st["m"]),
                f(sd(m)),
                f(m[0]),
                f(m[1]),
                f(m[2]),
                f(m[3]),
                pct(st["m"], base["m"]),
                f(st["c"], 2),
                f(sd(c), 2),
                pct(st["c"], base["c"]),
                f(st["w"]),
                pct(st["w"], base["w"]),
                f(st["du"], 2),
                pct(st["du"], base["du"]),
                f(st["g"]),
                f(st["ag"]),
                f(mean([rows[t]["trade_rows"] for t in ARMS[arm]]), 0),
            )
        )
    t, d = stats["tw"], stats["twd"]
    cols = ["twd vs tw", "-", "-", "-", "-", "-", "-", pct(d["m"], t["m"]), "-", "-", pct(d["c"], t["c"]),
            "-", pct(d["w"], t["w"]), "-", pct(d["du"], t["du"]), pct(d["g"], t["g"]), pct(d["ag"], t["ag"]), "-"]
    out.append(" | ".join(cols))
    out += [
        "",
        "== D. Warm-up recovery check (twd-warm-rs: 120 s cell, SIGKILL val1 at bench+40 s, restart on the same data dir; excluded from A-C)",
    ]
    wf = warm_facts()
    m = wf["m"]
    out += [
        "warm %s | agree=%s | liveness=%s | validity=%s | oracle stale=%s | node md5=%s" % (wf["rc"], wf["agree"], wf["liveness"], wf["validity"], wf["stale"], wf["md5"]),
        "restart: " + restart_cols(m),
        "  columns: down s | DB open s | restart->DB open s | WAL files replayed | WAL MB replayed | WAL MB at kill | rewind blocks | restart->first commit s | max commit gap s | WAL file missing from 1 s sample",
        "val1 post-restart commits (on_committed_block lines in crash-restart-tail.log): %d, last height %s" % (wf["post_commits"], wf["last_height"]),
        "val1 post-restart panic/'panicked' lines: %d; ERROR lines: %d" % (wf["panics"], len(wf["errors"])),
    ]
    out += ["  ERROR: " + e for e in wf["errors"][:5]]
    out += ["crash gate line: " + g for g in wf["gate"]]
    out += [
        "state digests (sha256 first 12 of state-digest-val0/1/2.txt): %s | all equal: %s" % (" ".join(wf["digests"]), "yes" if wf["digests_equal"] else "no"),
    ]
    out += [
        "",
        "Notes:",
        "- write MB/s and wchar MB/s: io-ticks.txt over the bench window, summed over the 3 node pids (per validator = /3).",
        "- SST created/live: RocksDB LOG table_file_creation minus table_file_deletion, per validator, whole DB life (fresh DB per cell).",
        "- growth GB/day: the 120 s bench window extrapolated to 24 h at bench load. A load-test rate, not a production rate.",
        "- trade rows: data rows only (range tombstones excluded). tw and twd read 0 by design; each shows only the startup tombstone file. b is the positive control.",
        "- every measured cell is run-cell.sh with CLEAN=1 (launch-3val.sh wipes DATA_ROOT/data before the cell).",
        "- warm-up crash gate: the harness regex 'panic / fail-stop after the restart' matches the INFO config line 'running state hash fail-stop ... on=false'; not a failure.",
    ]
    with open(RUN + "/handoff-tables.txt", "w") as fh:
        fh.write("\n".join(out) + "\n")
    print("wrote " + RUN + "/handoff-tables.txt (%d lines)" % len(out))


if __name__ == "__main__":
    main()
