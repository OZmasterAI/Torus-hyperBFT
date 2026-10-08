#!/usr/bin/env python3
"""liq_stress.py <cell-dir> — row 76 liquidation-stress report for one cell.

Reads what run-cell.sh leaves in a LIQ_THIN / ORACLE_SHOCK_BP cell:
  summary.json                      cell.liq_thin(_avail), oracle_feed.shock_bp/_round
  metrics-{before,after}-val<i>.txt torus_liquidations_triggered_total,
                                    torus_liquidator_vault_deficit, and the
                                    per-class / scanned / acted counters
  sampler.csv                       1 Hz per node: block height, exec lag
                                    (torus_exec_queue_depth), post-engine tail
                                    timer, and (stress cells only) the
                                    liquidation counters, vault deficit, step
                                    timer (_sum/_count), pending and deferred
  vault-val<i>.json                 torus_getLiquidatorVault at the digest
  oracle-feed.log                   "[oracle-feed] shock round R at <ms> ms"

Writes <cell-dir>/liq-stress.json and prints the same JSON. Anything the cell
cannot show is listed under "missing", never guessed. The step timer, the
per-class counters and the pending gauge come from feat/liq-telemetry; a
binary without them falls back to the lower bounds of
torus_liquidations_triggered_total (and says so).
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
STEP_SUM = "torus_liquidation_step_seconds_sum"
STEP_CNT = "torus_liquidation_step_seconds_count"
PENDING = "torus_liquidation_pending"
DEFERRED = "torus_liquidation_deferred"
CLASS = {
    "stage1": "torus_liquidations_stage1_total",
    "backstop": "torus_liquidations_backstop_total",
    "adl": "torus_liquidations_adl_total",
    "scanned": "torus_liquidation_scanned_total",
    "acted": "torus_liquidation_acted_total",
}
SHOCK_RE = re.compile(r"\[oracle-feed\] shock round (\d+) at (\d+) ms")

NO_TIMER = (
    "liquidation step timer: no torus_liquidation_step_seconds in sampler.csv / "
    "metrics-after-*.txt (a binary without feat/liq-telemetry, or a non-stress cell). "
    "post_engine_tail_ms_per_block (torus_exec_post_engine_tail_seconds) is an UPPER "
    "BOUND that also covers drain_core_writer, process_governance, distribute_fees and "
    "process_epoch_boundary."
)
NO_PENDING = (
    "no liquidatable account remains: no torus_liquidation_pending telemetry "
    "(a binary without feat/liq-telemetry, or a non-stress cell). "
    "blocks_shock_to_last_liquidation is the last 1 Hz sample where "
    "torus_liquidations_triggered_total rose, a LOWER BOUND."
)
ALWAYS_MISSING = [
    "ADL counterparties: each node logs one info line per ADL'd (account, market) "
    "('liquidation: ADL' with counterparty count, total size, price) and each "
    "counterparty close at debug ('liquidation: ADL close') in its own log "
    "(RUN_DIR/val<i>.log, not in the cell dir); this tool does not parse node logs. "
    "torus_liquidations_adl_total counts ADL runs (accounts and the vault), not closes.",
]
NOTES = [
    "torus_liquidations_triggered_total counts accounts ACTED ON per block: an account "
    "that stays under maintenance margin over several blocks counts once per block "
    "(likewise the per-class counters).",
    "torus_liquidation_pending (set after each step) = accounts holding a pending row "
    "(acted on and still under maintenance margin, carried over until rescanned; the "
    "vault while ADL-able) UNION the scan-window candidates the 64-per-block act budget "
    "left unclassified (torus_liquidation_deferred; may include healthy accounts): an "
    "upper bound. Liquidatable accounts the round-robin has not reached yet and that "
    "no budget cut deferred are not counted.",
    "torus_liquidations_adl_total includes the liquidator vault's own ADL, which "
    "torus_liquidation_acted_total (the budgeted actions) does not.",
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
    if a is None or b is None or any(k not in x for k in (TAIL_SUM, TAIL_CNT) for x in (a, b)):  # ozarchy patch: sampler has _sum only
        return None
    dn = b[TAIL_CNT] - a[TAIL_CNT]
    return round((b[TAIL_SUM] - a[TAIL_SUM]) * 1000 / dn, 4) if dn > 0 else None


def step_ms(a, b):
    """Mean liquidation step ms per native block between two sampler rows."""
    if a is None or b is None or STEP_SUM not in a or STEP_SUM not in b:
        return None
    dn = b[STEP_CNT] - a[STEP_CNT]
    return round((b[STEP_SUM] - a[STEP_SUM]) * 1000 / dn, 4) if dn > 0 else None


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
        "liquidations_by_class": {},
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
        cls = {}
        for k, name in CLASS.items():
            cb = metric(os.path.join(d, "metrics-before-%s.txt" % n), name)
            ca = metric(os.path.join(d, "metrics-after-%s.txt" % n), name)
            cls[k] = num(ca - cb) if ca is not None and cb is not None else None
        r["liquidations_by_class"][n] = cls
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
    # sample_metrics.py writes "0" for a metric the node does not export: the
    # telemetry columns count only if the node's after-snapshot has the timer.
    tel = any(
        metric(os.path.join(d, "metrics-after-%s.txt" % n), STEP_CNT) is not None
        for n in NODES
    )
    if not tel:
        header = [c for c in header if c not in (STEP_SUM, STEP_CNT, PENDING, DEFERRED)]
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

    # Pending gauge (val0): from the shock (or the first sample) on, the peak,
    # the first sample back at 0 after a pending > 0, the last sample.
    has_pending = PENDING in header
    r["pending"] = {"peak": None, "zero": None, "last": None}
    r["pending_timeline"] = []
    r["blocks_shock_to_pending_zero"] = None
    zero_ts = None
    if has_pending and v0:
        after = [x for x in v0 if PENDING in x and (shock_ts is None or x["ts"] >= shock_ts)]
        pt = lambda x: {"ts": num(x["ts"]), "height": num(x.get(HEIGHT)), "pending": num(x[PENDING])}  # noqa: E731
        first_up = next((i for i, x in enumerate(after) if x[PENDING] > 0), None)
        if after:
            r["pending"]["last"] = pt(after[-1])
        if first_up is not None:
            r["pending"]["peak"] = pt(max(after, key=lambda x: x[PENDING]))
            z = next((x for x in after[first_up:] if x[PENDING] == 0), None)
            if z is not None:
                zero_ts = z["ts"]
                r["pending"]["zero"] = point(z)
                if r["shock_height"] is not None and z.get(HEIGHT) is not None:
                    r["blocks_shock_to_pending_zero"] = num(z[HEIGHT]) - r["shock_height"]
            else:
                missing.append(
                    "pending never returned to 0 by the last sample (%s): the drain ended "
                    "with liquidation work left" % (r["pending"]["last"],)
                )
            # Timeline: the sample before the first pending > 0 .. the zero sample
            # (or the end), each with the step ms per block since the previous sample.
            i0 = v0.index(after[first_up])
            end = next((i for i, x in enumerate(v0) if x["ts"] == zero_ts), len(v0) - 1)
            for i in range(max(i0 - 1, 0), end + 1):
                x = v0[i]
                r["pending_timeline"].append(
                    {
                        "ts": num(x["ts"]),
                        "height": num(x.get(HEIGHT)),
                        "pending": num(x.get(PENDING)),
                        "deferred": num(x.get(DEFERRED)),
                        "step_ms": step_ms(v0[i - 1], x) if i > 0 else None,
                    }
                )

    # Window: shock (or first liquidation) .. pending back at 0 (else the last
    # liquidation; else the end of sampling).
    w_start = shock_ts if shock_ts is not None else (rises[0]["ts"] if rises else None)
    w_end = zero_ts if zero_ts is not None else (rises[-1]["ts"] if rises else None)
    r["window"] = {"start_ts": num(w_start), "end_ts": num(w_end)}
    r["post_engine_tail_ms_per_block"], r["exec_lag"] = {}, {}
    r["liquidation_step_ms_per_block"] = {}
    for n in NODES:
        s = (rows or {}).get(n) or []
        if not s:
            r["post_engine_tail_ms_per_block"][n] = r["exec_lag"][n] = None
            r["liquidation_step_ms_per_block"][n] = None
            continue
        end = w_end if w_end is not None else s[-1]["ts"]
        base_rows = [x for x in s if w_start is None or x["ts"] < w_start]
        win_rows = [x for x in s if w_start is not None and w_start <= x["ts"] <= end]
        base_last = base_rows[-1] if base_rows else None
        r["post_engine_tail_ms_per_block"][n] = {
            "baseline": tail_ms(s[0], base_last),
            "window": tail_ms(base_last, win_rows[-1]) if win_rows else None,
        }
        if STEP_SUM in header:
            span = ([base_last] if base_last else []) + win_rows
            per = [step_ms(a, b) for a, b in zip(span, span[1:])]
            per = [v for v in per if v is not None]
            r["liquidation_step_ms_per_block"][n] = {
                "baseline": step_ms(s[0], base_last),
                "window": step_ms(base_last, win_rows[-1]) if win_rows else None,
                "window_max": max(per) if per else None,
            }
        else:
            r["liquidation_step_ms_per_block"][n] = None
        lag = lambda rs: [num(x[LAG]) for x in rs if LAG in x]  # noqa: E731
        bl, wl = lag(base_rows), lag(win_rows)
        r["exec_lag"][n] = {
            "baseline_max": max(bl) if bl else None,
            "window_max": max(wl) if wl else None,
            "window_p50": statistics.median_low(wl) if wl else None,
        }
    if rows is None:
        missing.append("sampler.csv missing")
    if STEP_SUM not in header:
        missing.append(NO_TIMER)
    if not has_pending:
        missing.append(NO_PENDING)
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
