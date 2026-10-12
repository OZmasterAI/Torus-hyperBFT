"""Bench-archive rows for ozarchy-p3rss (INDEX rows, cells.md rows, DELETABLE block) -> analyst/archive-*.txt (read-only on results).
Derived from ozarchy-p3s2g-run/analyst/a-archive.py."""
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = R + "ozarchy-p23b-run/analyst/"
SEC = "`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section '50. P2-3 persistent exec worker pool vs its merge-base, commit-tail mode (campaign ozarchy-p23b, 2026-10-12)'"
ARM = {
    "k": ("arm k (control c97f56f9, tail commit)", "in-flight tail commit"),
    "p": ("arm p (P2-3 pool efcd1548, tail commit)", "in-flight tail commit"),
    "pk": ("arm pk (control c97f56f9 + perf on val0)", "in-flight tail commit; perf on val0"),
    "pp": ("arm pp (pool efcd1548 + perf on val0)", "in-flight tail commit; perf on val0"),
}
CELLS = [("w0", "k", "warm-up 60 s, excluded")]
for r, order in enumerate((("k", "p"), ("p", "k"), ("k", "p"), ("p", "k")), 1):
    CELLS += [(f"r{r}-{a}", a, "throughput 120 s") for a in order]
CELLS += [("x1-pp", "pp", "perf 120 s"), ("x1-pk", "pk", "perf 120 s"), ("x2-pk", "pk", "perf 120 s"), ("x2-pp", "pp", "perf 120 s")]
TIERB = ("val0.log.gz", "val1.log.gz", "val2.log.gz", "buckets.csv", "tasks.txt", "sampler.csv")
EXTRA = [
    ("ozarchy-p23b-run", "p23b campaign log (+ the held preflight log), done marker, analyst/ (a-cells.py, a-arms.py, p23-spawn.py, p23-exited.py, forks.txt, outputs)"),
    ("ozarchy-p23b-smoke-300m-s0-pp", "prep smoke cell (pool + perf, 60 s; 1 fork in 45 s, flush worker)"),
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
    d = "ozarchy-p23b-300m-" + t
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
    idx.append("| `%s` | %s | 2026-10-12 | %s | P2-3 worker pool vs merge-base, tw, Classic, standard shape at 300 markets, shared NVMe, commit tail; %s; %s | %s | R | %s | %s |"
               % (d, h(size(R + d)), commit, ARM[a][0], what, SEC, h(os.path.getsize(R + d + "/perf.data")) if os.path.exists(R + d + "/perf.data") else "-", h(sb)))
    hd = s["headline"]
    c = s["cell"]
    env = "6 knobs empty; book rows empty (Classic); trade history off + WAL 2048 MiB; exec pipeline on (default); " + ARM[a][1] + ("; warm-up" if t == "w0" else "")
    cel.append("| `%s` | %s | %s | %s @ %s | %s | %d | %d | %d | %s | {:,.1f} | {:,.1f} | {:,.1f} | {:,.1f} | %.1f | %.1f | %.2f | %.2f | %s | %s | %s |".format(
        hd["matched_s_avg"], hd["matched_s_first120"], hd["matched_s_best60"], hd["placed_s_avg"])
        % (d, s["status"] if s["validity"]["verdict"] == "ACCEPT" else "INVALID", s["generated_at"][:16].replace("T", " "),
           os.path.basename(s["worktree"]), s["commit"][:8], env_md5, c["markets"], c["duration_s"], c["block_cap"], env,
           hd["blk_s_avg"], hd["txs_per_block_avg"], hd["chain_ms"], hd["engine_ms_per_1k_fills"], hd.get("agreement_verdict"),
           s["liveness"]["verdict"], s["validity"]["verdict"]))
for d, what in EXTRA:
    dirs.append(d)
    sa = sum(os.path.getsize(p) for p in tierA if p.startswith(R + d + "/"))
    idx.append("| `%s` | %s | 2026-10-12 | c97f56f9 (f35fab7d), efcd1548 (dacdeec8), bench 05d44313 | %s | %s | R | %s | - |"
               % (d, h(size(R + d)), what, SEC, h(sa) if sa else "-"))
SA = sum(os.path.getsize(p) for p in tierA)
SB = sum(os.path.getsize(p) for p in tierB)
open(O + "archive-index-p23b.txt", "w").write("\n".join(sorted(idx)) + "\n")
open(O + "archive-cells-p23b.txt", "w").write("\n".join(sorted(cel)) + "\n")
blk = ["Tier A, %s, %d files (the 4 perf.data files; their fork counts are kept in ozarchy-p23b-run/analyst/forks.txt and p23-spawn-pk.txt):" % (h(SA), len(tierA)), "", "```", "rm -- \\"]
blk += ["  '%s' \\" % p for p in tierA]
blk[-1] = blk[-1][:-2]
blk += ["```", "", "Tier B, %s, %d files:" % (h(SB), len(tierB)), "", "```", "rm -- \\"] + ["  '%s' \\" % p for p in tierB]
blk[-1] = blk[-1][:-2]
blk += ["```"]
open(O + "archive-deletable-p23b.txt", "w").write("\n".join(blk) + "\n")
tot = sum(size(R + d) for d in dirs)
print("TierA", SA, len(tierA), h(SA), "TierB", SB, len(tierB), h(SB), "A+B", SA + SB, h(SA + SB), "dirs", len(dirs), tot, h(tot))
