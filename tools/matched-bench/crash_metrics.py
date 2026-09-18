#!/usr/bin/env python3
"""Parse a bounded pre-kill scrape without turning missing evidence into zero."""
from decimal import Decimal, InvalidOperation
import json
import sys

METRICS = {
    "block_height": "torus_block_height",
    "blocks_committed": "torus_blocks_committed_total",
    "exec_queue_depth": "torus_exec_queue_depth",
    "flush_worker_depth": "torus_flush_worker_depth",
    "matched": "torus_orders_matched_total",
}


def parse_snapshot(text, scrape_rc):
    rows = {}
    for line in text.splitlines():
        fields = line.split()
        if fields and not fields[0].startswith("#"):
            rows.setdefault(fields[0], []).append(fields[1:])
    out = {"scrape_rc": scrape_rc}
    missing = []
    for key, metric in METRICS.items():
        value = None
        found = rows.get(metric, [])
        if scrape_rc == 0 and len(found) == 1 and found[0]:
            try:
                number = Decimal(found[0][0])
                if number.is_finite() and number >= 0 and number == number.to_integral_value():
                    value = int(number)
            except InvalidOperation:
                pass
        out[key] = value
        if value is None:
            missing.append(metric)
    out["metrics_status"] = "UNKNOWN" if missing else "OK"
    out["missing_or_invalid_metrics"] = missing
    return out


if __name__ == "__main__":
    json.dump(parse_snapshot(sys.stdin.read(), int(sys.argv[1])), sys.stdout, indent=1)
