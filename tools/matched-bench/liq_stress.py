#!/usr/bin/env python3
"""liq_stress.py <cell-dir> — row 76 liquidation-stress report for one cell.

Reads what run-cell.sh leaves in a LIQ_THIN / ORACLE_SHOCK_BP cell:
  summary.json                      cell.liq_thin(_avail), oracle_feed.shock_bp/_round
  metrics-{before,after}-val<i>.txt torus_liquidations_triggered_total,
                                    torus_liquidator_vault_deficit
  sampler.csv                       1 Hz per node: block height, exec lag
                                    (torus_exec_queue_depth), post-engine tail
                                    timer, and (stress cells only) the
                                    liquidation counter and vault deficit
  vault-val<i>.json                 torus_getLiquidatorVault at the digest
  oracle-feed.log                   "[oracle-feed] shock round R at <ms> ms"

Writes <cell-dir>/liq-stress.json and prints the same JSON. Anything the cell
cannot show is listed under "missing" (with what would be needed), never
guessed: there is no liquidation-step timer, no pending-account gauge and no
ADL record in the node today.
"""

import csv
import json
import os
import re
import statistics
import sys

NODES = ("val0", "val1", "val2")
LIQ = "torus_liquidations_triggered_total"
DEFICIT = "torus_liquidator_vault_deficit"
HEIGHT = "torus_block_height"
LAG = "torus_exec_queue_depth"
TAIL_SUM = "torus_exec_post_engine_tail_seconds_sum"
TAIL_CNT = "torus_exec_post_engine_tail_seconds_count"
SHOCK_RE = re.compile(r"\[oracle-feed\] shock round (\d+) at (\d+) ms")

ALWAYS_MISSING = [
    "liquidation step timer: nothing times NativeExecutor::run_liquidations alone "
    "(crates/torus-consensus/src/app.rs, inside the post-engine tail). "
    "post_engine_tail_ms_per_block (torus_exec_post_engine_tail_seconds) is an UPPER "
    "BOUND that also covers drain_core_writer, process_governance, distribute_fees and "
    "process_epoch_boundary. Needed: a histogram around run_liquidations "
    "(e.g. torus_exec_liquidation_seconds) or a per-block log line with its ms.",
    "no liquidatable account remains: the step's pending/cooldown/cursor rows "
    "(CF_NATIVE_LIQUIDATION, NativeExecutor::liquidation_due) are not exported. "
    "blocks_shock_to_last_liquidation is the last 1 Hz sample where "
    "torus_liquidations_triggered_total rose, a LOWER BOUND. Needed: a gauge of pending "
    "accounts, or a per-block line in liquidation_pass with height, scanned, acted, "
    "stage1/backstop/adl counts and pending.",
    "ADL counterparties: liq::adl_close transfers positions with no trade record, log "
    "line or metric, so ADL'd senders outside the thin set cannot be listed from this "
    "cell. Needed: a log line or counter per ADL close (counterparty, market, size), "
    "or a torus_getPosition snapshot per non-thin sender and market before the shock "
    "and after the drain (the bench only grows positions, so any shrink is ADL).",
]
NOTES = [
    "torus_liquidations_triggered_total counts accounts ACTED ON per block: an account "
    "that stays under maintenance margin over several blocks counts once per block.",
]


def load_json(path):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def metric(path, name):
    """Last value of `name` in a Prometheus text file; None if absent."""
    val = None
    try:
        with open(path) as f:
            for line in f:
                fields = line.split()
                if len(fields) >= 2 and fields[0] == name:
                    val = float(fields[1])
    except OSError:
        return None
    return val


def num(v):
    return int(v) if v is not None and float(v).is_integer() else v


def read_sampler(path):
    """{node: [row dict with float values, ts-sorted]}, valid scrapes only."""
    rows, header = {n: [] for n in NODES}, []
    try:
        with open(path, newline="") as f:
            r = csv.DictReader(f)
            header = r.fieldnames or []
            for row in r:
                if row.get("scrape_valid", "1") != "1" or row.get("node") not in rows:
                    continue
                try:
                    rows[row["node"]].append(
                        {k: float(v) for k, v in row.items() if k != "node" and v != ""}
                    )
                except ValueError:
                    continue
    except OSError:
        return None, []
    for n in rows:
        rows[n].sort(key=lambda x: x["ts"])
    return rows, header


def tail_ms(a, b):
    """Mean post-engine tail ms per native block between two sampler rows."""
    if a is None or b is None or TAIL_SUM not in a or TAIL_SUM not in b:
        return None
    dn = b[TAIL_CNT] - a[TAIL_CNT]
    return round((b[TAIL_SUM] - a[TAIL_SUM]) * 1000 / dn, 4) if dn > 0 else None


def analyze(d):
    summary = load_json(os.path.join(d, "summary.json")) or {}
    cell, feed = summary.get("cell") or {}, summary.get("oracle_feed") or {}
    missing = []
    r = {
        "cell_dir": os.path.abspath(d),
        "liq_thin": cell.get("liq_thin"),
        "liq_thin_avail": cell.get("liq_thin_avail"),
        "shock_bp": feed.get("shock_bp"),
        "shock_round": feed.get("shock_round"),
        "liquidations": {},
        "vault_deficit_after": {},
        "vault": {},
    }
    for i, n in enumerate(NODES):
        b = metric(os.path.join(d, "metrics-before-%s.txt" % n), LIQ)
        a = metric(os.path.join(d, "metrics-after-%s.txt" % n), LIQ)
        r["liquidations"][n] = {
            "before": num(b),
            "after": num(a),
            "delta": num(a - b) if a is not None and b is not None else None,
        }
        if a is None:
            missing.append(
                "metrics-after-%s.txt has no %s (binary without the liquidation step?)"
                % (n, LIQ)
            )
        r["vault_deficit_after"][n] = metric(
            os.path.join(d, "metrics-after-%s.txt" % n), DEFICIT
        )
        v = load_json(os.path.join(d, "vault-%s.json" % n))
        r["vault"][n] = v
        if v is None:
            missing.append(
                "vault-%s.json (torus_getLiquidatorVault; written only with LIQ_THIN>0 or ORACLE_SHOCK_BP>0)"
                % n
            )

    r["shock"] = None
    try:
        with open(os.path.join(d, "oracle-feed.log")) as f:
            for line in f:
                m = SHOCK_RE.search(line)
                if m:
                    r["shock"] = {"round": int(m.group(1)), "unix_ms": int(m.group(2))}
                    break
    except OSError:
        pass
    if r["shock"] is None:
        missing.append(
            "no '[oracle-feed] shock round' line in oracle-feed.log (no ORACLE_SHOCK_BP, or an older feed)"
        )

    rows, header = read_sampler(os.path.join(d, "sampler.csv"))
    r["per_sample"] = LIQ in header
    if not r["per_sample"]:
        missing.append(
            "sampler.csv has no %s column (cell run without LIQ_THIN/ORACLE_SHOCK_BP): "
            "no per-sample liquidation timeline" % LIQ
        )
    v0 = (rows or {}).get("val0") or []

    def height_at(ts):
        return num(next((x[HEIGHT] for x in v0 if x["ts"] >= ts and HEIGHT in x), None))

    r["shock_height"] = None
    shock_ts = None
    if r["shock"] and v0:
        shock_ts = next(
            (x["ts"] for x in v0 if x["ts"] * 1000 >= r["shock"]["unix_ms"]), None
        )
        r["shock_height"] = height_at(shock_ts) if shock_ts is not None else None
    rises = [
        x
        for p, x in zip(v0, v0[1:])
        if r["per_sample"] and x.get(LIQ, 0) > p.get(LIQ, 0)
    ]
    point = lambda x: {"ts": num(x["ts"]), "height": num(x.get(HEIGHT))}  # noqa: E731
    r["first_liquidation"] = point(rises[0]) if rises else None
    r["last_liquidation"] = point(rises[-1]) if rises else None
    r["blocks_shock_to_last_liquidation"] = (
        r["last_liquidation"]["height"] - r["shock_height"]
        if rises and r["shock_height"] is not None
        else None
    )

    # Window: shock (or first liquidation) .. last liquidation (or end of sampling).
    w_start = shock_ts if shock_ts is not None else (rises[0]["ts"] if rises else None)
    w_end = rises[-1]["ts"] if rises else None
    r["window"] = {"start_ts": num(w_start), "end_ts": num(w_end)}
    r["post_engine_tail_ms_per_block"], r["exec_lag"] = {}, {}
    for n in NODES:
        s = (rows or {}).get(n) or []
        if not s:
            r["post_engine_tail_ms_per_block"][n] = r["exec_lag"][n] = None
            continue
        end = w_end if w_end is not None else s[-1]["ts"]
        base_rows = [x for x in s if w_start is None or x["ts"] < w_start]
        win_rows = [x for x in s if w_start is not None and w_start <= x["ts"] <= end]
        base_last = base_rows[-1] if base_rows else None
        r["post_engine_tail_ms_per_block"][n] = {
            "baseline": tail_ms(s[0], base_last),
            "window": tail_ms(base_last, win_rows[-1]) if win_rows else None,
        }
        lag = lambda rs: [num(x[LAG]) for x in rs if LAG in x]  # noqa: E731
        bl, wl = lag(base_rows), lag(win_rows)
        r["exec_lag"][n] = {
            "baseline_max": max(bl) if bl else None,
            "window_max": max(wl) if wl else None,
            "window_p50": statistics.median_low(wl) if wl else None,
        }
    if rows is None:
        missing.append("sampler.csv missing")
    r["missing"] = missing + ALWAYS_MISSING
    r["notes"] = NOTES
    return r


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    r = analyze(sys.argv[1])
    out = json.dumps(r, indent=1, sort_keys=True)
    with open(os.path.join(sys.argv[1], "liq-stress.json"), "w") as f:
        f.write(out + "\n")
    print(out)


if __name__ == "__main__":
    main()
