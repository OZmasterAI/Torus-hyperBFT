# Bench-archive rows for ozarchy-p3t and ozarchy-p23 (INDEX rows, cells.md sections, DELETABLE blocks) -> /tmp/p3t-analyst/archive-*.txt
# (read-only on results). Derived from ozarchy-p3p-tools/a-archive.py.
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = "/tmp/p3t-analyst/"
DOC = "`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`"
SEC = {
    "p3t": DOC
    + " section '43. Phase 3 prototype vs control with the disk taken out: shared NVMe vs tmpfs upper bound (campaign ozarchy-p3t, 2026-10-11)'",
    "p23": DOC
    + " section '44. P2-3 worker pool sizing: thread spawns, spawn/join cost and what a pool would save (campaign ozarchy-p23, 2026-10-11)'",
}


def size(p):
    return int(subprocess.check_output(["du", "-sb", "--apparent-size", p]).split()[0])


def h(n):
    if n >= 1 << 30:
        return "%.2f GiB" % (n / (1 << 30))
    if n >= 1 << 20:
        return "%.1f MiB" % (n / (1 << 20))
    return "%d KiB" % round(n / 1024)


ARM = {
    "cd": ("arm cd (control, shared NVMe)", "p3 off; data dirs on the shared NVMe"),
    "ad": (
        "arm ad (`TORUS_P3_PROTO=all`, shared NVMe)",
        "p3 all; data dirs on the shared NVMe",
    ),
    "cm": (
        "arm cm (control, data dirs on tmpfs: upper bound)",
        "p3 off; data dirs on tmpfs (/dev/shm)",
    ),
    "am": (
        "arm am (`TORUS_P3_PROTO=all`, data dirs on tmpfs: upper bound)",
        "p3 all; data dirs on tmpfs (/dev/shm)",
    ),
    "m": ("main 4e7fa146, node defaults", "node defaults"),
}
CAMP = {
    "p3t": dict(
        cells=[("w0", "cd", "warm-up 60 s, excluded")]
        + [
            (f"r{r}-{a}", a, "throughput 120 s")
            for r, order in enumerate(
                (
                    ("cd", "ad", "am", "cm"),
                    ("ad", "cm", "cd", "am"),
                    ("cm", "am", "ad", "cd"),
                    ("am", "cd", "cm", "ad"),
                ),
                1,
            )
            for a in order
        ],
        what="Phase 3 prototype vs control, disk isolation (tmpfs = upper bound), tw, Classic, standard shape at 300 markets, RocksDB LOG saved",
        branch=", branch proto/p3-coalesce @ 682a8df5",
        tierB=(
            "val0.log.gz",
            "val1.log.gz",
            "val2.log.gz",
            "buckets.csv",
            "tasks.txt",
            "sampler.csv",
        ),
        extra=[
            (
                "ozarchy-p3t-run",
                "p3t campaign log, done marker, analyst/ (a-cells.py, a-arms.py, cells.json, arms.txt)",
            ),
            (
                "ozarchy-p3t-tools",
                "campaign driver ozarchy-p3t-campaign.sh (node and harness reused from ozarchy-p3p-stage / ozarchy-p3p-tools)",
            ),
        ],
        stageA=[],
        tag="682a8df5 (6e40fa3c)",
    ),
    "p23": dict(
        cells=[("w0", "m", "warm-up 60 s, excluded")]
        + [
            (
                f"{k}{i}",
                "m",
                "perf cycles:u on val0, 45 s from bench + 35 s (prof-sidecar), 120 s"
                if k == "p"
                else "unprofiled throughput reference, 120 s",
            )
            for i in range(1, 5)
            for k in ("p", "u")
        ],
        what="P2-3 worker pool sizing profile on main 4e7fa146, tw, Classic, standard shape at 300 markets",
        branch=", main @ 4e7fa146",
        tierB=(
            "val0.log.gz",
            "val1.log.gz",
            "val2.log.gz",
            "buckets.csv",
            "sampler.csv",
        ),
        extra=[
            (
                "ozarchy-p23-build",
                "build log and md5s of node 169567f2 + bench 74cef98f from main 4e7fa146 (md5s.txt)",
            ),
            (
                "ozarchy-p23-run",
                "p23 campaign log, done marker, analyst/ (p23-spawn.py/.txt, p23-exited.py/.txt, spawnbench-out.txt)",
            ),
            (
                "ozarchy-p23-stage",
                "staged node 169567f2 and bench 74cef98f (`n/release/`, main 4e7fa146, rebuildable)",
            ),
            (
                "ozarchy-p23-tools",
                "campaign driver, build script, spawnbench.rs + compiled spawnbench, spawnbench-out.txt",
            ),
        ],
        stageA=[
            "ozarchy-p23-stage/n/release/torus-node",
            "ozarchy-p23-stage/n/release/bench-throughput",
        ],
        tag="4e7fa146 (169567f2)",
    ),
}
for camp, C in CAMP.items():
    idx = []
    cel = []
    tierA = [R + p for p in C["stageA"]]
    tierB = []
    dirs = []
    for t, a, what in C["cells"]:
        d = f"ozarchy-{camp}-300m-" + t
        dirs.append(d)
        s = json.load(open(R + d + "/summary.json"))
        md5 = s["binaries"]["torus_node_md5"][:8]
        commit = "%s @ %s (node %s%s)" % (
            os.path.basename(s["worktree"]),
            s["commit"][:8],
            md5,
            C["branch"],
        )
        if os.path.exists(R + d + "/perf.data"):
            tierA.append(R + d + "/perf.data")
        for f in sorted(os.listdir(R + d)):
            if f in C["tierB"]:
                tierB.append(R + d + "/" + f)
        sb = sum(os.path.getsize(p) for p in tierB if p.startswith(R + d + "/"))
        sa = sum(os.path.getsize(p) for p in tierA if p.startswith(R + d + "/"))
        idx.append(
            "| `%s` | %s | 2026-10-11 | %s | %s; %s; %s | %s | R | %s | %s |"
            % (
                d,
                h(size(R + d)),
                commit,
                C["what"],
                ARM[a][0],
                what,
                SEC[camp],
                h(sa) if sa else "-",
                h(sb),
            )
        )
        hd = s["headline"]
        c = s["cell"]
        env = (
            "6 knobs empty; book rows empty (Classic); trade history off + WAL 2048 MiB; "
            + ARM[a][1]
            + ("; warm-up" if t == "w0" else "")
            + ("; perf on val0" if t.startswith("p") else "")
        )
        cel.append(
            "| `%s` | %s | %s | %s @ %s | %s | %d | %d | %d | %s | {:,.1f} | {:,.1f} | {:,.1f} | {:,.1f} | %.1f | %.1f | %.2f | %.2f | %s | %s | %s |".format(
                hd["matched_s_avg"],
                hd["matched_s_first120"],
                hd["matched_s_best60"],
                hd["placed_s_avg"],
            )
            % (
                d,
                s["status"] if s["validity"]["verdict"] == "ACCEPT" else "INVALID",
                s["generated_at"][:16].replace("T", " "),
                os.path.basename(s["worktree"]),
                s["commit"][:8],
                md5,
                c["markets"],
                c["duration_s"],
                c["block_cap"],
                env,
                hd["blk_s_avg"],
                hd["txs_per_block_avg"],
                hd["chain_ms"],
                hd["engine_ms_per_1k_fills"],
                hd.get("agreement_verdict"),
                s["liveness"]["verdict"],
                s["validity"]["verdict"],
            )
        )
    for d, what in C["extra"]:
        dirs.append(d)
        sa = sum(os.path.getsize(p) for p in tierA if p.startswith(R + d + "/"))
        idx.append(
            "| `%s` | %s | 2026-10-11 | %s | %s | %s | R | %s | - |"
            % (d, h(size(R + d)), C["tag"], what, SEC[camp], h(sa) if sa else "-")
        )
    SA = sum(os.path.getsize(p) for p in tierA)
    SB = sum(os.path.getsize(p) for p in tierB)
    open(O + f"archive-index-{camp}.txt", "w").write("\n".join(sorted(idx)) + "\n")
    open(O + f"archive-cells-{camp}.txt", "w").write("\n".join(sorted(cel)) + "\n")
    blk = []
    if tierA:
        blk += [
            "Tier A, %s, %d files:" % (h(SA), len(tierA)),
            "",
            "```",
            "rm -- \\",
        ] + ["  '%s' \\" % p for p in tierA]
        blk[-1] = blk[-1][:-2]
        blk += ["```", ""]
    blk += ["Tier B, %s, %d files:" % (h(SB), len(tierB)), "", "```", "rm -- \\"] + [
        "  '%s' \\" % p for p in tierB
    ]
    blk[-1] = blk[-1][:-2]
    blk += ["```"]
    open(O + f"archive-deletable-{camp}.txt", "w").write("\n".join(blk) + "\n")
    tot = sum(size(R + d) for d in dirs)
    print(
        camp,
        "TierA",
        SA,
        len(tierA),
        h(SA),
        "TierB",
        SB,
        len(tierB),
        h(SB),
        "A+B",
        SA + SB,
        h(SA + SB),
        "dirs",
        len(dirs),
        tot,
        h(tot),
        "first dirs",
        dirs[:3],
    )
