# Bench-archive rows for ozarchy-p3c1 (INDEX rows, cells.md section, DELETABLE block) -> ozarchy-p3c1-run/analyst/archive-*.txt
# (read-only on results). Derived from ozarchy-p3t-tools/a-archive.py.
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = "/home/oz/bench-results-matched/ozarchy-p3c1-run/analyst/"
DOC = "`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`"
SEC = {"p3c1": DOC + " section '45. Phase 3 step 1 (checkpoint interval 15 s) vs control on shared NVMe and tmpfs (campaign ozarchy-p3c1, 2026-10-11)'"}


def size(p):
    return int(subprocess.check_output(["du", "-sb", "--apparent-size", p]).split()[0])


def h(n):
    if n >= 1 << 30:
        return "%.2f GiB" % (n / (1 << 30))
    if n >= 1 << 20:
        return "%.1f MiB" % (n / (1 << 20))
    return "%d KiB" % round(n / 1024)


ARM = {
    "cd": ("arm cd (control 2725e321, shared NVMe)", "checkpoint interval unset; data dirs on the shared NVMe"),
    "sd": ("arm sd (step 1 7c178637, `TORUS_CHECKPOINT_INTERVAL_MS=15000`, shared NVMe)", "checkpoint interval 15 s; data dirs on the shared NVMe"),
    "cm": ("arm cm (control 2725e321, data dirs on tmpfs: upper bound)", "checkpoint interval unset; data dirs on tmpfs (/dev/shm)"),
    "sm": ("arm sm (step 1 7c178637, `TORUS_CHECKPOINT_INTERVAL_MS=15000`, data dirs on tmpfs: upper bound)", "checkpoint interval 15 s; data dirs on tmpfs (/dev/shm)"),
}
CAMP = {
    "p3c1": dict(
        cells=[("w0", "cd", "warm-up 60 s, excluded")]
        + [(f"r{r}-{a}", a, "throughput 120 s")
           for r, order in enumerate((("cd", "sd", "sm", "cm"), ("sd", "cm", "cd", "sm"), ("cm", "sm", "sd", "cd")), 1)
           for a in order]
        + [("r4-sm", "sm", "throughput 120 s, EXCLUDED: harness worktree deleted mid-cell (20:48), false val0 DEATH at teardown; data valid but replaced by r4b-sm")]
        + [("w4", "cd", "warm-up 60 s before the round 4 rerun, excluded")]
        + [(f"r4b-{a}", a, "throughput 120 s (round 4 rerun)") for a in ("sm", "cd", "cm", "sd")],
        what="Phase 3 step 1 (checkpoint interval) vs control, shared NVMe and tmpfs (upper bound), tw, Classic, standard shape at 300 markets, RocksDB LOG saved, 2 s checkpoint metric samples",
        branch="",
        tierB=("val0.log.gz", "val1.log.gz", "val2.log.gz", "buckets.csv", "tasks.txt", "sampler.csv"),
        extra=[
            ("ozarchy-p3c1-build", "build logs and md5s.txt: control 2725e321 (node 18fb490d, bench c4559781), step 1 7c178637 (node c161a337), unused c9ed0ba7 build (29c14cbf)"),
            ("ozarchy-p3c1-run", "p3c1 campaign log, done marker, analyst/ (a-cells.py, a-arms.py, a-ckpt.py, a-archive.py, outputs)"),
            ("ozarchy-p3c1-stage", "staged nodes: c/ (2725e321), s/ (7c178637), s-run/ (step 1 node + control bench), c-c9ed0ba7/ (unused); rebuildable"),
            ("ozarchy-p3c1-tools", "campaign driver, build script, post-processing (ozarchy-p3c1-post.py)"),
            ("ozarchy-p3c1-smoke-300m-s0", "prep smoke cell (sd, 60 s; not analysed)"),
            ("ozarchy-p3c1-smoke", "smoke driver log and done marker"),
            ("ozarchy-p3c1-r4-cd-stub-never-ran", "empty r4-cd dir of the first attempt (detach.sh missing; never ran)"),
        ],
        stageA=["ozarchy-p3c1-stage/c/release/torus-node", "ozarchy-p3c1-stage/c/release/bench-throughput",
                "ozarchy-p3c1-stage/s/release/torus-node", "ozarchy-p3c1-stage/s/release/bench-throughput",
                "ozarchy-p3c1-stage/s-run/release/torus-node", "ozarchy-p3c1-stage/s-run/release/bench-throughput",
                "ozarchy-p3c1-stage/c-c9ed0ba7/release/torus-node", "ozarchy-p3c1-stage/c-c9ed0ba7/release/bench-throughput"],
        tag="2725e321 (18fb490d), 7c178637 (c161a337)",
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
            "6 knobs empty; book rows empty (Classic); trade history off + WAL 2048 MiB; exec pipeline on (default); "
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
