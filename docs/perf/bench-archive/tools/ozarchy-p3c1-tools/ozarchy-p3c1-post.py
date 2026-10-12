#!/usr/bin/env python3
"""ozarchy-p3c1 per-cell post-processing (run with python3 -I).

Usage: ozarchy-p3c1-post.py CELL_DIR ARM

Writes CELL_DIR/p3c1-post.json and prints one summary line per section.
  ckpt:    Phase 3 step 1 checkpoint metrics, from the harness's metrics-before/after-val*.txt and the
           campaign's 2 s time series (ckpt-samples.txt). Control arms do not export them: absence is fine there.
           torus_exec_checkpoint_layer_shared must stay 0 in every sample: any nonzero value -> HARD FINDING line.
  rejects: every rejection / failure / drop count the bench and node already expose, so step 1 and control can be
           compared: summary.json ingest + funnel + oracle fields, before/after deltas of every node counter whose
           name matches reject|fail|drop|nonce|evict|refus|error|invalid|insufficient, node-log WARN/ERROR line
           categories, and bench.log error-like lines.
Exit 0 always unless a file is unreadable in a way that makes the cell unusable (rc 2); the campaign decides.
"""

import gzip
import json
import re
import sys
from pathlib import Path

CK = re.compile(r"^torus_(exec_checkpoint|flush_worker_checkpoint)")
REJ = re.compile(
    r"(reject|fail|drop|nonce|evict|refus|error|invalid|insufficient|stale|expired)",
    re.I,
)
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def parse(path):
    vals = {}
    try:
        text = Path(path).read_text()
    except OSError:
        return None
    for line in text.splitlines():
        f = line.split()
        if len(f) < 2 or f[0].startswith("#"):
            continue
        try:
            vals[f[0]] = float(f[1])
        except ValueError:
            pass
    return vals


def ckpt_series(cell):
    """ckpt-samples.txt: 'T <epoch> val<i> ok|ERR' header then metric lines, per scrape."""
    p = Path(cell) / "ckpt-samples.txt"
    out = {
        "samples": 0,
        "errors": 0,
        "shared_nonzero": [],
        "max": {},
        "first_ts": None,
        "last_ts": None,
    }
    if not p.exists():
        return out
    cur = None
    for line in p.read_text().splitlines():
        f = line.split()
        if not f:
            continue
        if f[0] == "T":
            ts, node, st = float(f[1]), f[2], f[3]
            cur = (ts, node)
            if st == "ok":
                out["samples"] += 1
                out["first_ts"] = out["first_ts"] or ts
                out["last_ts"] = ts
            else:
                out["errors"] += 1
            continue
        if cur is None or len(f) < 2:
            continue
        try:
            v = float(f[1])
        except ValueError:
            continue
        name = f[0]
        if "_bucket" in name:
            continue
        key = f"{cur[1]}:{name}"
        if key not in out["max"] or v > out["max"][key]:
            out["max"][key] = v
        if name.startswith("torus_exec_checkpoint_layer_shared") and v != 0:
            out["shared_nonzero"].append(
                {"ts": cur[0], "node": cur[1], "metric": name, "value": v}
            )
    return out


def logcats(path):
    cats = {}
    try:
        with gzip.open(path, "rt", errors="replace") as fh:
            for line in fh:
                line = ANSI.sub("", line)
                m = re.search(r"\b(WARN|ERROR)\b\s+(\S+?):?\s+(.*)", line)
                if not m:
                    continue
                msg = re.sub(r"0x[0-9a-fA-F]+|[0-9]+", "N", m.group(3))[:90]
                k = f"{m.group(1)} {m.group(2)} {msg}"
                cats[k] = cats.get(k, 0) + 1
    except OSError:
        return None
    return dict(sorted(cats.items(), key=lambda kv: -kv[1]))


def main():
    cell, arm = sys.argv[1], sys.argv[2]
    c = Path(cell)
    res = {"cell": c.name, "arm": arm, "ckpt": {}, "rejects": {}}
    # ---- checkpoint metrics, before/after per node
    for i in range(3):
        b, a = (
            parse(c / f"metrics-before-val{i}.txt"),
            parse(c / f"metrics-after-val{i}.txt"),
        )
        if a is None:
            res["ckpt"][f"val{i}"] = "metrics-after missing"
            continue
        b = b or {}
        d = {}
        for k, v in a.items():
            if CK.match(k) and "_bucket" not in k:
                d[k] = {"after": v, "delta": v - b.get(k, 0.0)}
        res["ckpt"][f"val{i}"] = d
    res["ckpt"]["series"] = ckpt_series(c)
    # ---- rejects: node counters
    for i in range(3):
        b, a = (
            parse(c / f"metrics-before-val{i}.txt") or {},
            parse(c / f"metrics-after-val{i}.txt") or {},
        )
        res["rejects"][f"node_val{i}"] = {
            k: v - b.get(k, 0.0)
            for k, v in sorted(a.items())
            if REJ.search(k)
            and "_bucket" not in k
            and not k.endswith(("_sum", "_count"))
            and not k.startswith("torus_rocksdb")
        }
    # ---- rejects: summary.json
    try:
        s = json.loads((c / "summary.json").read_text())
        ing = s.get("ingest") or {}
        inf = ing.get("in_flight") or {}
        mix = ing.get("econ_mix") or {}
        res["rejects"]["bench"] = {
            "submitted": ing.get("bench_submitted_actions"),
            "val0_actions_processed": ing.get("val0_actions_processed"),
            "sent_minus_accepted_place": (mix.get("sent_place") or 0)
            - (mix.get("place") or 0),
            "sent_minus_accepted_cancel_all": (mix.get("sent_cancel_all") or 0)
            - (mix.get("cancel_all") or 0),
            "in_flight_released_refused": inf.get("released_refused"),
            "in_flight_released_timeout": inf.get("released_timeout"),
            "in_flight_at_end": inf.get("in_flight_at_end"),
            "tail_errors": inf.get("tail_errors"),
            "tail_missed": inf.get("tail_missed"),
            "mempool_nonce_expired_evictions_per_node": ing.get(
                "mempool_nonce_expired_evictions_per_node"
            ),
            "oracle_rejected": (s.get("oracle_feed") or {}).get("rejected"),
            "oracle_stale_marks_at_bench_end": (s.get("oracle_feed") or {}).get(
                "stale_marks_at_bench_end"
            ),
            "dissemination_total_failures": (s.get("dissemination") or {}).get(
                "total_failures"
            ),
            "agreement_error_lines": (s.get("agreement") or {}).get("error_lines"),
            "agreement_panic_or_failstop_lines": (s.get("agreement") or {}).get(
                "panic_or_failstop_lines"
            ),
        }
    except (OSError, ValueError) as e:
        res["rejects"]["bench"] = f"summary.json unreadable: {e}"
    # ---- rejects: bench.log error-like lines (counted by normalised text)
    bl = {}
    try:
        for line in (c / "bench.log").read_text(errors="replace").splitlines():
            line = ANSI.sub("", line)
            if (
                re.search(
                    r"(error|reject|refus|nonce|insufficient|fail|busy)", line, re.I
                )
                and "In-flight cap" not in line
            ):
                k = re.sub(r"[0-9]+", "N", line)[:120]
                bl[k] = bl.get(k, 0) + 1
    except OSError:
        bl = None
    res["rejects"]["bench_log_lines"] = bl
    # ---- rejects: node log WARN/ERROR categories
    res["rejects"]["node_log_warn_error"] = {
        f"val{i}": logcats(c / f"val{i}.log.gz") for i in range(3)
    }
    (c / "p3c1-post.json").write_text(json.dumps(res, indent=1, sort_keys=True))

    # ---- one-line prints
    ck = []
    for i in range(3):
        d = res["ckpt"].get(f"val{i}")
        if isinstance(d, dict):
            n = d.get(
                "torus_exec_checkpoints", d.get("torus_exec_checkpoints_total", {})
            ).get("delta")
            ws = d.get("torus_exec_checkpoint_wait_seconds_sum", {}).get("delta")
            wc = d.get("torus_exec_checkpoint_wait_seconds_count", {}).get("delta")
            lb = (d.get("torus_exec_checkpoint_layer_bytes_sum", {}).get("delta"),
                  d.get("torus_exec_checkpoint_layer_bytes_count", {}).get("delta"))
            sh = d.get("torus_exec_checkpoint_layer_shared_total", d.get("torus_exec_checkpoint_layer_shared", {})).get("after")
            fs = d.get("torus_flush_worker_checkpoint_seconds_sum", {}).get("delta")
            ck.append(
                f"val{i} ckpts={n} wait_sum={ws} wait_n={wc} layer_bytes_sum/n={lb} layer_shared={sh} flush_ckpt_sum={fs} ({len(d)} series)"
            )
        else:
            ck.append(f"val{i} {d}")
    ser = res["ckpt"]["series"]
    print(f"[ckpt] {'; '.join(ck)}; samples ok={ser['samples']} err={ser['errors']}")
    if ser["shared_nonzero"]:
        print(
            f"HARD FINDING {c.name}: torus_exec_checkpoint_layer_shared nonzero in {len(ser['shared_nonzero'])} samples, "
            f"first {ser['shared_nonzero'][0]}"
        )
    for i in range(3):
        d = res["ckpt"].get(f"val{i}")
        if isinstance(d, dict):
            for k, v in d.items():
                if (
                    k.startswith("torus_exec_checkpoint_layer_shared")
                    and v["after"] != 0
                ):
                    print(
                        f"HARD FINDING {c.name}: val{i} {k} = {v['after']} at cell end"
                    )
    nz = {
        f"val{i}": {k: v for k, v in res["rejects"][f"node_val{i}"].items() if v}
        for i in range(3)
    }
    print(f"[rejects] node nonzero deltas: {json.dumps(nz, sort_keys=True)}")
    b = res["rejects"]["bench"]
    if isinstance(b, dict):
        print("[rejects] bench: " + " ".join(f"{k}={v}" for k, v in b.items()))
    else:
        print(f"[rejects] bench: {b}")
    print(
        f"[rejects] bench.log error-like line kinds: {0 if not bl else len(bl)}; node WARN/ERROR lines: "
        + " ".join(
            f"val{i}={sum((res['rejects']['node_log_warn_error'][f'val{i}'] or {}).values())}"
            for i in range(3)
        )
    )


if __name__ == "__main__":
    main()
