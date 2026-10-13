"""Bench-archive rows for ozarchy-p3gate (INDEX rows, cells.md rows, DELETABLE block) -> analyst/archive-*.txt (read-only on results).
Copy of ozarchy-p3cloid-run/analyst/a-archive.py; two binaries (s2 c88d3073 node 4e0d8b84 / bench ae96e0c1, m de1a97c1 node f1081574 /
bench 42365459), harness = s2 worktree for every cell (summary 'commit' is the harness worktree; the node is identified by its md5)."""
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = R + "ozarchy-p3gate-run/analyst/"
SEC = "final Phase 3 gate (campaign ozarchy-p3gate, 2026-10-13; results doc section TBD)"
ARM = {
    "s2": ("arm s2 (Phase 3 c88d3073 = step 1 + step 2 + merge-wait on main de1a97c1, je compiled in, TORUS_CHECKPOINT_INTERVAL_MS=15000)", "TORUS_CHECKPOINT_INTERVAL_MS=15000; je compiled in; CLOID_SHARE=0; in-flight tail commit"),
    "m": ("arm m (main de1a97c1, je compiled in, knob unset)", "checkpoint knob unset; je compiled in; CLOID_SHARE=0; in-flight tail commit"),
}
P = "ozarchy-p3gate-300m-"
CELLS = [("ozarchy-p3gate-smoke-300m-s0-s2-TERM", "s2", "smoke 90 s SIGTERM val1 at 40 s, excluded"), ("ozarchy-p3gate-smoke-300m-s0-m", "m", "smoke 60 s, excluded"),
         (P + "w0", "s2", "warm-up 60 s, excluded")]
for r, order in enumerate((("s2", "m"), ("m", "s2"), ("s2", "m"), ("m", "s2")), 1):
    CELLS += [(f"{P}r{r}-{a}", a, "throughput 120 s") for a in order]
CELLS += [(P + "k1-m", "m", "soak 600 s"), (P + "k1-s2", "s2", "soak 600 s"),
          (P + "t1-s2-TERM", "s2", "restart 300 s, SIGTERM val1 at bench+180 s"), (P + "t1-m-TERM", "m", "restart 300 s, SIGTERM val1 at bench+180 s")]
TIERB = ("val0.log.gz", "val1.log.gz", "val2.log.gz", "buckets.csv", "tasks.txt", "sampler.csv")
EXTRA = [
    ("ozarchy-p3gate-run", "p3gate campaign + smoke logs, done markers, analyst/ (a-cells.py, a-arms.py, a-paired.py, a-series.py, a-term.py, a-archive.py, cells*.json, cells.txt, arms*.txt, paired.txt, series.*, fig-rss/fig-node svg+png, a-term.txt, ckpt-timers.txt)"),
    ("ozarchy-p3gate-tools", "campaign driver ozarchy-p3gate-campaign.sh, build script, matched-bench tools copy (c88d3073 + p3term crash-kill.sh)"),
    ("ozarchy-p3gate-build", "build logs + md5s.txt (s2/bs2/m/bm)"),
    ("ozarchy-p3gate-stage", "staged binaries s2-run, m-run (torus-node + bench-throughput)"),
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
    idx.append("| `%s` | %s | 2026-10-13 | %s | final Phase 3 gate (s2 c88d3073 vs main de1a97c1, both je compiled in, CLOID_SHARE=0), tw, Classic, standard shape at 300 markets, shared NVMe, commit tail; %s; %s | %s | R | %s | %s |"
               % (d, h(size(R + d)), commit, ARM[a][0], what, SEC, h(os.path.getsize(R + d + "/perf.data")) if os.path.exists(R + d + "/perf.data") else "-", h(sb)))
    hd = s["headline"]
    c = s["cell"]
    env = "6 knobs empty; book rows empty (Classic); trade history off + WAL 2048 MiB; exec pipeline on (default); " + ARM[a][1] + ("; smoke" if "-smoke-" in t else "; warm-up" if t.endswith("-w0") else "")
    cel.append("| `%s` | %s | %s | %s @ %s | %s | %d | %d | %d | %s | {:,.1f} | {:,.1f} | {:,.1f} | {:,.1f} | %.1f | %.1f | %.2f | %.2f | %s | %s | %s |".format(
        hd["matched_s_avg"], hd["matched_s_first120"], hd["matched_s_best60"], hd["placed_s_avg"])
        % (d, s["status"] if s["validity"]["verdict"] == "ACCEPT" else "INVALID", s["generated_at"][:16].replace("T", " "),
           os.path.basename(s["worktree"]), s["commit"][:8], env_md5, c["markets"], c["duration_s"], c["block_cap"], env,
           hd["blk_s_avg"], hd["txs_per_block_avg"], hd["chain_ms"], hd["engine_ms_per_1k_fills"], hd.get("agreement_verdict"),
           s["liveness"]["verdict"], s["validity"]["verdict"]))
for d, what in EXTRA:
    dirs.append(d)
    sa = sum(os.path.getsize(p) for p in tierA if p.startswith(R + d + "/"))
    idx.append("| `%s` | %s | 2026-10-13 | s2 c88d3073 (node 4e0d8b84, bench ae96e0c1), m de1a97c1 (node f1081574, bench 42365459) | %s | %s | R | %s | - |"
               % (d, h(size(R + d)), what, SEC, h(sa) if sa else "-"))
SA = sum(os.path.getsize(p) for p in tierA)
SB = sum(os.path.getsize(p) for p in tierB)
open(O + "archive-index-p3gate.txt", "w").write("\n".join(sorted(idx)) + "\n")
open(O + "archive-cells-p3gate.txt", "w").write("\n".join(sorted(cel)) + "\n")
blk = []  # no Tier A: the binaries are in ozarchy-p3gate-stage (s2-run, m-run)
blk += ["Tier B, %s, %d files:" % (h(SB), len(tierB)), "", "```", "rm -- \\"] + ["  '%s' \\" % p for p in tierB]
blk[-1] = blk[-1][:-2]
blk += ["```"]
open(O + "archive-deletable-p3gate.txt", "w").write("\n".join(blk) + "\n")
tot = sum(size(R + d) for d in dirs)
print("TierA", SA, len(tierA), h(SA), "TierB", SB, len(tierB), h(SB), "A+B", SA + SB, h(SA + SB), "dirs", len(dirs), tot, h(tot))
