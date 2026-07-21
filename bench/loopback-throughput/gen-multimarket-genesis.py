#!/usr/bin/env python3
# Build an N-market genesis from a base genesis by cloning the market template with fresh market_ids.
# Usage: gen-multimarket-genesis.py <base_genesis.json> <N> <out.json>
import json, sys, hashlib
base, N, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
g = json.load(open(base)); tmpl = dict(g["markets"][0])
g["markets"] = [{**tmpl, "market_id": i, "base_asset": f"S{i}", "quote_asset": "USD"} for i in range(1, N + 1)]
b = json.dumps(g, separators=(",", ":")).encode(); open(out, "wb").write(b)
print(f"markets={N} sha256={hashlib.sha256(b).hexdigest()[:16]} -> {out}")
