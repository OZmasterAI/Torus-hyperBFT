#!/usr/bin/env python3
"""Run ONE matched-bench cell for a campaign (called by ab-driver.sh; README.md).

Waits for an idle host, checks ports and free disk, writes
<campaign>/<label>.manifest.json (args, load, harness commit, binary sha256s),
then runs <worktree>/tools/matched-bench/run-cell.sh with a clean env:
TARGET_DIR=<artifacts>, DATA_ROOT=<campaign>/<label> (the cell's devnet RocksDB,
pruned later by prune-cell.sh), RESULTS_ROOT=<results-root> (results in
<results-root>/<label>), plus the allowlisted --runner-env knobs.
"""

import argparse, hashlib, json, os, pathlib, socket, subprocess, time

RUNNER_ENV_KEYS = {
    "BAND",
    "CROSS_FRACTION",
    "CANCEL_FRACTION",
    "MPS",
    "RATE_SCHEDULE",
    "DEPTH_OBSERVER",
    "OPEN_ORDER_BUDGET",
    "BATCH",
    "NODE_CPUS",
    "BENCH_CPUS",
    "BENCH_RPCS",
    "CONC",
    "SENDERS",
    "STATE_HASH_ACTIVATION",
    "ORACLE_FEED",
    "ORACLE_PRICE",
    "ORACLE_INTERVAL_MS",
    "ORACLE_WALK_BP",
    "ORACLE_FEED_DRAIN",
}
p = argparse.ArgumentParser(
    description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
)
p.add_argument("label")
p.add_argument(
    "--artifacts",
    required=True,
    help="dir with release/torus-node and release/bench-throughput (+ optional manifest.json provenance)",
)
p.add_argument("--markets", default=10, type=int)
p.add_argument("--duration", default=120, type=int)
p.add_argument("--rate", default=76000, type=int)
p.add_argument("--extra-env", default="", help="node env, space-separated KEY=VAL")
p.add_argument("--cap", default=200, type=int)
p.add_argument(
    "--worktree",
    default=os.environ.get(
        "HARNESS_WT", os.path.expanduser("~/projects/wt/harness")
    ),
    help="harness worktree (default $HARNESS_WT, else ~/projects/wt/harness)",
)
p.add_argument(
    "--campaign-dir",
    default=os.environ.get("CAMPAIGN_DIR"),
    help="campaign dir: manifest, run log and the devnet DB <campaign>/<label> go here (default $CAMPAIGN_DIR; required)",
)
p.add_argument(
    "--results-root",
    default=os.environ.get(
        "RESULTS_ROOT", os.path.expanduser("~/bench-results-matched")
    ),
    help="results go to <results-root>/<label> (default $RESULTS_ROOT, else ~/bench-results-matched)",
)
p.add_argument("--crash-at")  # s75: int or comma list (multi-crash)
p.add_argument(
    "--no-idle-wait",
    action="store_true",
    help="s87: skip the load<1.5/fstrim idle wait (UNSCORED warm-up only; recorded in the manifest)",
)
p.add_argument(
    "--runner-env",
    action="append",
    default=[],
    metavar="NAME=VALUE",
    help="Recorded workload knob, one of: " + ", ".join(sorted(RUNNER_ENV_KEYS)),
)
a = p.parse_args()
if not a.campaign_dir:
    p.error("--campaign-dir (or CAMPAIGN_DIR) is required")
runner_env = {}
for item in a.runner_env:
    key, sep, value = item.partition("=")
    if not sep or key not in RUNNER_ENV_KEYS:
        p.error("Unsupported --runner-env assignment: " + item)
    if key in runner_env:
        p.error("Duplicate --runner-env key: " + key)
    runner_env[key] = value
root = pathlib.Path(a.campaign_dir).resolve()
results_root = pathlib.Path(a.results_root).resolve()
wt = pathlib.Path(a.worktree)
results = results_root / a.label
data = root / a.label
if results.exists() or data.exists():
    raise SystemExit("Refusing existing label")


def maintenance_active():
    return (
        subprocess.run(
            ["pgrep", "-x", "fstrim"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        ).returncode
        == 0
    )


if a.no_idle_wait:
    print(
        "WARNING: --no-idle-wait: idle-host wait skipped (unscored warm-up), load",
        os.getloadavg(),
        flush=True,
    )
while not a.no_idle_wait and (os.getloadavg()[0] >= 1.5 or maintenance_active()):
    print(
        "Waiting for idle host:",
        os.getloadavg(),
        "fstrim=",
        maintenance_active(),
        flush=True,
    )
    time.sleep(20)
for port in [8645, 8646, 8647, 9161, 9162, 9163]:
    with socket.socket() as sock:
        if sock.connect_ex(("127.0.0.1", port)) == 0:
            raise SystemExit(f"Port {port} occupied")
artifacts = pathlib.Path(a.artifacts)
required_gib = max(14, 6 + a.duration * 0.2)
if __import__("shutil").disk_usage(root).free < required_gib * 1024**3:
    raise SystemExit(
        f"Insufficient free space: need {required_gib}GiB for requested duration; retention authorization pending"
    )
manifest = {
    "idle_wait_skipped": a.no_idle_wait,
    "driver_sha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
    "args": vars(a),
    "start_load": os.getloadavg(),
    "start_ts": time.time(),
    "runner_commit": subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=wt, text=True
    ).strip(),
    "binaries": {
        name: hashlib.sha256((artifacts / "release" / name).read_bytes()).hexdigest()
        for name in ["torus-node", "bench-throughput"]
    },
}
(root / (a.label + ".manifest.json")).write_text(json.dumps(manifest, indent=2) + "\n")
env = {
    k: os.environ[k]
    for k in ["HOME", "USER", "LOGNAME", "PATH", "LANG", "TZ"]
    if k in os.environ
}
env.update(
    TARGET_DIR=str(artifacts),
    DATA_ROOT=str(data),
    RESULTS_ROOT=str(results_root),
    BLOCK_CAP=str(a.cap),
)
env.update(runner_env)
if a.crash_at is not None:
    env.update(
        CRASH_KILL_AT_S=str(a.crash_at), CRASH_REQUIRE_REPLAY="1", KILL_NODE="val1"
    )
print("Starting", a.label, flush=True)
provenance = artifacts / "manifest.json"
if provenance.exists():
    (root / (a.label + ".artifact-provenance.json")).write_text(provenance.read_text())
with (root / (a.label + ".log")).open("x") as output:
    result = subprocess.run(
        [
            str(wt / "tools/matched-bench/run-cell.sh"),
            str(wt),
            a.label,
            str(a.markets),
            str(a.duration),
            str(a.rate),
            a.extra_env,
        ],
        env=env,
        stdout=output,
        stderr=subprocess.STDOUT,
    )
manifest.update(end_ts=time.time(), runner_exit=result.returncode)
(root / (a.label + ".manifest.json")).write_text(json.dumps(manifest, indent=2) + "\n")
if (results / "summary.json").exists():
    s = json.loads((results / "summary.json").read_text())
    print(
        json.dumps(
            {k: s[k] for k in ["status", "headline", "validity", "timing"] if k in s}
        ),
        flush=True,
    )
raise SystemExit(result.returncode)
