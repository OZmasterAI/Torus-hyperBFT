"""Bench-archive rows for ozarchy-p3cloid (INDEX rows, cells.md rows, DELETABLE block) -> analyst/archive-*.txt (read-only on results).
Copy of ozarchy-p3cloid-run/analyst/a-archive.py (A/B shape, 60 s smoke cell)."""
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = R + "ozarchy-p3cloid-run/analyst/"
SEC = "cloid cost A/B (campaign ozarchy-p3cloid, 2026-10-13; results doc section TBD)"
ARM = {
    "c0": ("arm c0 (main ef812b02, CLOID_SHARE=0, glibc)", "CLOID_SHARE=0; glibc; in-flight tail commit"),
    "c100": ("arm c100 (main ef812b02, CLOID_SHARE=100, glibc)", "CLOID_SHARE=100; glibc; in-flight tail commit"),
}
CELLS = [("ozarchy-p3cloid-smoke-300m-s0-c100", "c100", "smoke 60 s, excluded"), ("ozarchy-p3cloid-300m-w0", "c0", "warm-up 60 s, excluded")]
for r, order in enumerate((("c0", "c100"), ("c100", "c0"), ("c0", "c100"), ("c100", "c0")), 1):
    CELLS += [(f"ozarchy-p3cloid-300m-r{r}-{a}", a, "throughput 120 s") for a in order]
TIERB = ("val0.log.gz", "val1.log.gz", "val2.log.gz", "buckets.csv", "tasks.txt", "sampler.csv")
EXTRA = [
    ("ozarchy-p3cloid-run", "p3cloid campaign + smoke logs, done markers, analyst/ (a-cells.py, a-arms.py, a-paired.py, a-archive.py, cells.json, cells.txt, arms.txt, paired.txt, cloids.txt)"),
]


def size(p):
    return int(subprocess.check_output(["du", "-sb", "--apparent-size", p]).split()[0])


def h(n):
    if n >= 1 << 30:
        return "%.2f GiB" % (n / (1 << 30))
    if n >= 1 << 20:
        return "%.1f MiB" % (n / (1 << 20))
    return "%d KiB" % round(n / 1024)


idx, cel, tierB, dirs = [], [], [], []
tierA = []
for t, a, what in CELLS:
    d = t
    dirs.append(d)
    s = json.load(open(R + d + "/summary.json"))
    md5 = s["binaries"]["torus_node_md5"][:8]
    env_md5 = open(R + d + "/node-environ-trie.txt").readline().split("exe_md5=")[1][:8]
    commit = "%s @ %s (node %s%s)" % (os.path.basename(s["worktree"]), s["commit"][:8], env_md5, "; wrapper md5 " + md5 if md5 != env_md5 else "")
    if os.path.exists(R + d + "/perf.data"):
        tierA.append(R + d + "/perf.data")
    for f in sorted(os.listdir(R + d)):
        if f in TIERB:
            tierB.append(R + d + "/" + f)
    sb = sum(os.path.getsize(p) for p in tierB if p.startswith(R + d + "/"))
    idx.append("| `%s` | %s | 2026-10-13 | %s | cloid cost A/B (CLOID_SHARE 100 vs 0, one binary main ef812b02, glibc), tw, Classic, standard shape at 300 markets, shared NVMe, commit tail; %s; %s | %s | R | %s | %s |"
               % (d, h(size(R + d)), commit, ARM[a][0], what, SEC, h(os.path.getsize(R + d + "/perf.data")) if os.path.exists(R + d + "/perf.data") else "-", h(sb)))
    hd = s["headline"]
    c = s["cell"]
    env = "6 knobs empty; book rows empty (Classic); trade history off + WAL 2048 MiB; exec pipeline on (default); " + ARM[a][1] + ("; smoke" if t.endswith("-s0-c100") else "; warm-up" if t.endswith("-w0") else "")
    cel.append("| `%s` | %s | %s | %s @ %s | %s | %d | %d | %d | %s | {:,.1f} | {:,.1f} | {:,.1f} | {:,.1f} | %.1f | %.1f | %.2f | %.2f | %s | %s | %s |".format(
        hd["matched_s_avg"], hd["matched_s_first120"], hd["matched_s_best60"], hd["placed_s_avg"])
        % (d, s["status"] if s["validity"]["verdict"] == "ACCEPT" else "INVALID", s["generated_at"][:16].replace("T", " "),
           os.path.basename(s["worktree"]), s["commit"][:8], env_md5, c["markets"], c["duration_s"], c["block_cap"], env,
           hd["blk_s_avg"], hd["txs_per_block_avg"], hd["chain_ms"], hd["engine_ms_per_1k_fills"], hd.get("agreement_verdict"),
           s["liveness"]["verdict"], s["validity"]["verdict"]))
for d, what in EXTRA:
    dirs.append(d)
    sa = sum(os.path.getsize(p) for p in tierA if p.startswith(R + d + "/"))
    idx.append("| `%s` | %s | 2026-10-13 | ef812b02 (node 6f363582), bench ef812b02 (9fda6c84) | %s | %s | R | %s | - |"
               % (d, h(size(R + d)), what, SEC, h(sa) if sa else "-"))
SA = sum(os.path.getsize(p) for p in tierA)
SB = sum(os.path.getsize(p) for p in tierB)
open(O + "archive-index-p3cloid.txt", "w").write("\n".join(sorted(idx)) + "\n")
open(O + "archive-cells-p3cloid.txt", "w").write("\n".join(sorted(cel)) + "\n")
blk = []  # no Tier A: the binaries are in ozarchy-p3cloid-stage (cl-run)
blk += ["Tier B, %s, %d files:" % (h(SB), len(tierB)), "", "```", "rm -- \\"] + ["  '%s' \\" % p for p in tierB]
blk[-1] = blk[-1][:-2]
blk += ["```"]
open(O + "archive-deletable-p3cloid.txt", "w").write("\n".join(blk) + "\n")
tot = sum(size(R + d) for d in dirs)
print("TierA", SA, len(tierA), h(SA), "TierB", SB, len(tierB), h(SB), "A+B", SA + SB, h(SA + SB), "dirs", len(dirs), tot, h(tot))
