#!/usr/bin/env python3
"""WAL-budget preflight/provenance; never opens or modifies a database."""
import os
from pathlib import Path
import re
import sys

FLAG = "TORUS_ROCKSDB_MAX_TOTAL_WAL_MB"
MIB = 1 << 20
MAX_MIB = ((1 << 64) - 1) // MIB


def parse_mib(raw):
    value = "0" if raw is None else raw.strip()
    if not re.fullmatch(r"[0-9]+", value, flags=re.ASCII):
        raise ValueError(f"{FLAG} must be whole MiB (ASCII digits)")
    mib = int(value)
    if mib > MAX_MIB:
        raise ValueError(f"{FLAG} overflows u64 bytes")
    return mib


def require_fresh_data(raw, data_path):
    mib = parse_mib(raw)
    # lexists also rejects dangling symlinks. Do not inspect, open or clean up
    # any existing data directory as part of this experiment.
    if mib and os.path.lexists(data_path):
        raise ValueError("nonzero WAL-budget experiments require a new DATA_ROOT/data path")
    return mib


def provenance(node_env):
    raw = node_env.get(FLAG)
    try:
        mib = parse_mib(raw)
        return {"recorded": FLAG in node_env, "requested_mib": raw,
                "effective_max_total_wal_size_bytes": mib * MIB,
                "mode": "explicit_flush_trigger" if mib else "rocksdb_automatic",
                "valid": True}
    except (ValueError, TypeError):
        return {"recorded": FLAG in node_env, "requested_mib": raw,
                "effective_max_total_wal_size_bytes": None, "mode": "invalid", "valid": False}


if __name__ == "__main__":
    try:
        print(require_fresh_data(sys.argv[1], Path(sys.argv[2])))
    except (ValueError, IndexError) as exc:
        print(f"FATAL: {exc}", file=sys.stderr)
        sys.exit(2)
