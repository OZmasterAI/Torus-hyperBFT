"""Empty-cadence A/B sampler (ee408ae vs s395 sprint binary).

Waits for the devnet to settle (height >= SETTLE_HEIGHT and rising), then
measures N windows of W seconds each: blocks committed, ms/block, 1-min
loadavg. Appends rows to a shared CSV so both legs land in one table.
"""

import json
import sys
import time
import urllib.request

RPC = "http://localhost:8645"
CSV = "/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/31b6729e-67b5-4947-8692-f4a692f9bd15/scratchpad/ab/ab-results.csv"
SETTLE_HEIGHT = 20
SETTLE_TIMEOUT = 180


def block_number():
    req = urllib.request.Request(
        RPC,
        data=json.dumps(
            {"jsonrpc": "2.0", "id": 1, "method": "eth_blockNumber", "params": []}
        ).encode(),
        headers={"content-type": "application/json"},
    )
    return int(json.load(urllib.request.urlopen(req, timeout=5))["result"], 16)


def load_1m():
    with open("/proc/loadavg") as handle:
        return handle.read().split()[0]


def main():
    label = sys.argv[1]
    windows = int(sys.argv[2]) if len(sys.argv) > 2 else 3
    window_secs = int(sys.argv[3]) if len(sys.argv) > 3 else 60

    deadline = time.time() + SETTLE_TIMEOUT
    last = -1
    while time.time() < deadline:
        try:
            h = block_number()
            if h >= SETTLE_HEIGHT and h > last >= 0:
                break
            last = h
        except Exception:
            pass
        time.sleep(3)
    else:
        print(f"{label}: SETTLE TIMEOUT (last height {last})")
        sys.exit(1)

    print(
        f"{label}: settled at height {block_number()}, measuring "
        f"{windows}x{window_secs}s"
    )
    rows = []
    for i in range(windows):
        h1 = block_number()
        t1 = time.time()
        time.sleep(window_secs)
        h2 = block_number()
        dt = time.time() - t1
        n = h2 - h1
        ms = round(dt * 1000.0 / n, 1) if n else 0.0
        rows.append((label, i + 1, h2, n, ms, load_1m()))
        print(
            f"{label} w{i + 1}: +{n} blocks in {dt:.1f}s = {ms} ms/blk "
            f"({n / dt:.1f} blk/s), load {load_1m()}"
        )

    with open(CSV, "a") as out:
        for r in rows:
            out.write(",".join(str(x) for x in r) + "\n")


main()
