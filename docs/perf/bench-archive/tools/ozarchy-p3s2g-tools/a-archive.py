# Bench-archive rows for ozarchy-p3s2g (INDEX rows, cells.md section, DELETABLE block) -> ozarchy-p3s2g-run/analyst/archive-*.txt
# (read-only on results). Derived from ozarchy-p3c1-run/analyst/a-archive.py.
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = "/home/oz/bench-results-matched/ozarchy-p3s2g-run/analyst/"
DOC = "`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`"
SEC = {"p3s2g": DOC + " section '47. Phase 3 step 2 (ingress view) vs main, both with the bench in-flight tail on committed bodies (campaign ozarchy-p3s2g, 2026-10-12)'"}


def size(p):
    return int(subprocess.check_output(["du", "-sb", "--apparent-size", p]).split()[0])


def h(n):
    if n >= 1 << 30:
        return "%.2f GiB" % (n / (1 << 30))
    if n >= 1 << 20:
        return "%.1f MiB" % (n / (1 << 20))
    return "%d KiB" % round(n / 1024)


ARM = {
    "cd": ("arm cd (control main c97f56f9, shared NVMe)", "checkpoint interval unset; in-flight tail commit; data dirs on the shared NVMe"),
    "sd": ("arm sd (step 2 2296be67, `TORUS_CHECKPOINT_INTERVAL_MS=15000`, shared NVMe)", "checkpoint interval 15 s; in-flight tail commit; data dirs on the shared NVMe"),
}
CAMP = {
    "p3s2g": dict(
        cells=[("w0", "cd", "warm-up 60 s, excluded")]
        + [(f"r{r}-{a}", a, "throughput 120 s" + ("; agreement DISAGREE (mark-only, digest timing; section 47.3), throughput kept" if (r, a) == (3, "cd") else ""))
           for r, order in enumerate((("cd", "sd"), ("sd", "cd"), ("cd", "sd"), ("sd", "cd")), 1)
           for a in order],
        what="Phase 3 step 2 (ingress view) vs main, bench in-flight tail on committed bodies (IN_FLIGHT_TAIL=commit, bench 86d2b2bb), tw, Classic, standard shape at 300 markets, shared NVMe, RocksDB LOG saved, 2 s checkpoint metric samples",
        branch="",
        tierB=("val0.log.gz", "val1.log.gz", "val2.log.gz", "buckets.csv", "tasks.txt", "sampler.csv"),
        extra=[
            ("ozarchy-p3s2g-build", "build logs and md5s.txt: control c97f56f9 (node f35fab7d), step 2 2296be67 (node 1c02893b), bench 86d2b2bb (3af99b73)"),
            ("ozarchy-p3s2g-run", "p3s2g campaign and smoke logs, done markers, analyst/ (a-p3s2g.py, a-arms.py, a-extra.py, a-archive.py, outputs)"),
            ("ozarchy-p3s2g-stage", "staged binaries: b/ (bench 86d2b2bb), c/ (node c97f56f9), s/ (node 2296be67), c-run/ and s-run/ (node + bench copies); rebuildable"),
            ("ozarchy-p3s2g-tools", "campaign driver, build script, post-processing (ozarchy-p3c1-post.py copy)"),
            ("ozarchy-p3s2g-smoke-300m-s0", "prep smoke cell (sd, 60 s; not analysed)"),
        ],
        stageA=["ozarchy-p3s2g-stage/b/release/bench-throughput", "ozarchy-p3s2g-stage/c/release/torus-node", "ozarchy-p3s2g-stage/s/release/torus-node",
                "ozarchy-p3s2g-stage/c-run/release/torus-node", "ozarchy-p3s2g-stage/c-run/release/bench-throughput",
                "ozarchy-p3s2g-stage/s-run/release/torus-node", "ozarchy-p3s2g-stage/s-run/release/bench-throughput"],
        tag="c97f56f9 (f35fab7d), 2296be67 (1c02893b), bench 86d2b2bb (3af99b73)",
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
            "| `%s` | %s | 2026-10-12 | %s | %s; %s; %s | %s | R | %s | %s |"
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
            "| `%s` | %s | 2026-10-12 | %s | %s | %s | R | %s | - |"
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
