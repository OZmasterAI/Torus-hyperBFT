#!/usr/bin/env python3
"""Validate the runner's economic workload before starting any processes."""
import json
import math
import re
import shlex
import sys


def finite_number(raw):
    # Python float accepts underscores/Unicode digits that Rust/Clap reject.
    if not re.fullmatch(r"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?", raw):
        raise ValueError("expected an ASCII decimal or exponent number")
    value = float(raw)
    if not math.isfinite(value):
        raise ValueError("number must be finite")
    return value


def parse_workload(band, cross, cancel, schedule, duration):
    if not re.fullmatch(r"[0-9]+", band) or not 1 <= int(band) < 30000:
        raise ValueError("BAND must be an integer in 1..29999 (fixed mid is 30000)")
    fractions = {}
    for name, raw in (("cross_fraction", cross), ("cancel_fraction", cancel)):
        value = finite_number(raw)
        if not math.isfinite(value) or not 0 <= value <= 1:
            raise ValueError(f"{name} must be finite and within [0,1]")
        fractions[name] = value
    phases = []
    if schedule:
        if any(c.isspace() for c in schedule):
            raise ValueError("runner RATE_SCHEDULE must not contain whitespace")
        if duration <= 0:
            raise ValueError("scheduled duration must be positive")
        entries = schedule.split(",")
        if len(entries) > 64:
            raise ValueError("rate schedule exceeds 64 phases")
        for entry in entries:
            start, rate = entry.split(":")
            if not re.fullmatch(r"[0-9]+", start.strip()):
                raise ValueError("phase offset must be nonnegative integer seconds")
            start, rate = int(start), finite_number(rate.strip())
            if not math.isfinite(rate) or rate < 0:
                raise ValueError("phase rate must be finite and nonnegative")
            if start >= duration or (not phases and start != 0):
                raise ValueError("first phase must start at 0; all starts must precede duration")
            if phases and start <= phases[-1]["start_s"]:
                raise ValueError("phase offsets must strictly increase")
            if phases:
                phases[-1]["end_s"] = start
            phases.append({"start_s": start, "end_s": duration, "rate_total": rate})
    return {"econ": True, "target_margin": 1500, "band": int(band), **fractions,
            "rate_units": "actions/s", "rate_schedule": phases,
            "rate_schedule_raw": schedule or None, "rate_total_overridden": bool(phases),
            "scheduled_zero_means": "pause"}


def schedule_provenance(workload, observed, parse_errors, bench_cmd, duration):
    """Gate declared schedule evidence, without claiming achieved phase rates."""
    try:
        command = shlex.split(bench_cmd)
    except ValueError:
        command = bench_cmd.split()
    flags = [i for i, arg in enumerate(command) if arg == "--rate-schedule" or arg.startswith("--rate-schedule=")]
    required = bool(flags or observed or parse_errors or
                    isinstance(workload, dict) and (workload.get("rate_schedule_raw") or workload.get("rate_schedule")))
    if not required:
        return {"required": False, "valid": None, "problems": []}
    problems = []
    planned = []
    try:
        raw = workload["rate_schedule_raw"]
        if not isinstance(raw, str) or not raw:
            raise ValueError("missing raw schedule")
        planned = parse_workload(str(workload["band"]), str(workload["cross_fraction"]),
                                 str(workload["cancel_fraction"]), raw, duration)["rate_schedule"]
        if workload["rate_schedule"] != planned:
            problems.append("manifest phases differ from declared schedule")
        if len(flags) != 1:
            problems.append("scheduled workload requires exactly one command schedule flag")
        else:
            i = flags[0]
            command_raw = command[i].split("=", 1)[1] if "=" in command[i] else command[i + 1]
            if command_raw != raw:
                problems.append("command and manifest schedules differ")
    except (KeyError, TypeError, ValueError, IndexError):
        problems.append("missing or invalid schedule manifest/command")
    if parse_errors:
        problems.append("malformed phase records")
    if len(observed) != len(planned) or not planned:
        problems.append("phase record count differs from manifest")
    unix_base = None
    for index, (phase, record) in enumerate(zip(planned, observed)):
        try:
            if not isinstance(record, dict) or type(record["index"]) is not int or record["index"] != index:
                raise ValueError("wrong phase index")
            for key in ("start_s", "end_s", "rate_total", "observed_elapsed_s", "planned_unix_s"):
                if type(record[key]) not in (int, float) or not math.isfinite(record[key]):
                    raise ValueError("invalid numeric phase field")
            if any(record[key] != phase[key] for key in ("start_s", "end_s", "rate_total")):
                raise ValueError("phase differs from manifest")
            if not phase["start_s"] <= record["observed_elapsed_s"] < phase["end_s"]:
                raise ValueError("phase timer was observed outside its interval")
            base = record["planned_unix_s"] - phase["start_s"]
            if base <= 0 or unix_base is not None and abs(base - unix_base) > 1e-5:
                raise ValueError("inconsistent nominal clock")
            unix_base = base
        except (KeyError, TypeError, ValueError):
            problems.append(f"invalid or mismatched phase record {index}")
    return {"required": True, "valid": not problems, "problems": problems}


if __name__ == "__main__":
    try:
        band, cross, cancel, schedule, duration = sys.argv[1:]
        print(json.dumps(parse_workload(band, cross, cancel, schedule, int(duration)), allow_nan=False))
    except (ValueError, OverflowError) as error:
        sys.exit(f"invalid workload: {error}")
