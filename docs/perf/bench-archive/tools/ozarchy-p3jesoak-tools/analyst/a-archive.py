"""Bench-archive rows for ozarchy-p3jesoak (INDEX rows, cells.md rows, DELETABLE block) -> analyst/archive-*.txt (read-only on results).
Copy of ozarchy-p3fsoak-run/analyst/a-archive.py. Keeps sampler.csv and val*.log.gz of the soak cells (sources of section 57: per node expiry from each node log)."""
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = R + "ozarchy-p3jesoak-run/analyst/"
SEC = "`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section '57. jemalloc batch part B: 10 min soaks with jemalloc compiled in, Phase 3 je vs je (`e7be8460` vs `0360c840`) and main je vs glibc (`88ddc4c2` vs `deeacdb3`) (campaign ozarchy-p3jesoak, 2026-10-13)'"
ARM = {
    "jmf": ("arm jmf (test/main-fix-je 0360c840 = main + cancel-share fix de97682f + jemalloc compiled in, no checkpoint knob; RAM-gate control)", "checkpoint interval unset; jemalloc compiled in; in-flight tail commit"),
    "jcs": ("arm jcs (test/p3-cs-je e7be8460 = step 2 + fix 41a95355 + jemalloc compiled in, at 15 s)", "checkpoint interval 15000 ms; jemalloc compiled in; in-flight tail commit"),
    "jb": ("arm jb (main deeacdb3, glibc, no cancel fix)", "checkpoint interval unset; glibc; in-flight tail commit"),
    "ja": ("arm ja (feat/jemalloc-allocator 88ddc4c2 = main deeacdb3 + jemalloc compiled in, no cancel fix)", "checkpoint interval unset; jemalloc compiled in; in-flight tail commit"),
}
CELLS = [("smoke", "s0-jcs", "jcs", "smoke 60 s (checkpoint knob check), excluded"), ("main", "w0", "jmf", "warm-up 60 s, excluded"),
         ("main", "k1-jmf", "jmf", "soak 600 s"), ("main", "k1-jcs", "jcs", "soak 600 s"), ("main", "k1-jb", "jb", "soak 600 s"), ("main", "k1-ja", "ja", "soak 600 s")]
TIERB = ("buckets.csv", "tasks.txt")
TIERB_W0 = TIERB + ("val0.log.gz", "val1.log.gz", "val2.log.gz", "sampler.csv")
EXTRA = [
    ("ozarchy-p3jesoak-run", "p3jesoak campaign + smoke logs, done markers, analyst/ (a-series.py, a-archive.py, series.txt/json, fig-*.svg/png)"),
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
for kind, t, a, what in CELLS:
    d = ("ozarchy-p3jesoak-smoke-300m-" if kind == "smoke" else "ozarchy-p3jesoak-300m-") + t
    dirs.append(d)
    s = json.load(open(R + d + "/summary.json"))
    md5 = s["binaries"]["torus_node_md5"][:8]
    env_md5 = open(R + d + "/node-environ-trie.txt").readline().split("exe_md5=")[1][:8]
    commit = "%s @ %s (node %s%s)" % (os.path.basename(s["worktree"]), s["commit"][:8], env_md5, "; wrapper md5 " + md5 if md5 != env_md5 else "")
    if os.path.exists(R + d + "/perf.data"):
        tierA.append(R + d + "/perf.data")
    for f in sorted(os.listdir(R + d)):
        if f in (TIERB_W0 if t in ("w0", "s0-jcs") else TIERB):
            tierB.append(R + d + "/" + f)
    sb = sum(os.path.getsize(p) for p in tierB if p.startswith(R + d + "/"))
    idx.append("| `%s` | %s | 2026-10-13 | %s | jemalloc soak (600 s), saturated, tw, Classic, standard shape at 300 markets, shared NVMe, commit tail; %s; %s | %s | R | %s | %s |"
               % (d, h(size(R + d)), commit, ARM[a][0], what, SEC, h(os.path.getsize(R + d + "/perf.data")) if os.path.exists(R + d + "/perf.data") else "-", h(sb)))
    hd = s["headline"]
    c = s["cell"]
    env = "6 knobs empty; book rows empty (Classic); trade history off + WAL 2048 MiB; exec pipeline on (default); " + ARM[a][1] + ("; warm-up" if t == "w0" else "; smoke" if t == "s0-jcs" else "")
    cel.append("| `%s` | %s | %s | %s @ %s | %s | %d | %d | %d | %s | {:,.1f} | {:,.1f} | {:,.1f} | {:,.1f} | %.1f | %.1f | %.2f | %.2f | %s | %s | %s |".format(
        hd["matched_s_avg"], hd["matched_s_first120"], hd["matched_s_best60"], hd["placed_s_avg"])
        % (d, s["status"] if s["validity"]["verdict"] == "ACCEPT" else "INVALID", s["generated_at"][:16].replace("T", " "),
           os.path.basename(s["worktree"]), s["commit"][:8], env_md5, c["markets"], c["duration_s"], c["block_cap"], env,
           hd["blk_s_avg"], hd["txs_per_block_avg"], hd["chain_ms"], hd["engine_ms_per_1k_fills"], hd.get("agreement_verdict"),
           s["liveness"]["verdict"], s["validity"]["verdict"]))
for d, what in EXTRA:
    dirs.append(d)
    sa = sum(os.path.getsize(p) for p in tierA if p.startswith(R + d + "/"))
    idx.append("| `%s` | %s | 2026-10-13 | 0360c840 (9d6e57c1), e7be8460 (4521dd5f), deeacdb3 (e901fd1d), 88ddc4c2 (f2fbe17b), bench 05d44313 | %s | %s | R | %s | - |"
               % (d, h(size(R + d)), what, SEC, h(sa) if sa else "-"))
SA = sum(os.path.getsize(p) for p in tierA)
SB = sum(os.path.getsize(p) for p in tierB)
open(O + "archive-index-p3jesoak.txt", "w").write("\n".join(sorted(idx)) + "\n")
open(O + "archive-cells-p3jesoak.txt", "w").write("\n".join(sorted(cel)) + "\n")
assert not tierA  # Tier A: none (the binaries are in ozarchy-p3je-stage, listed under p3je)
blk = ["Tier B, %s, %d files:" % (h(SB), len(tierB)), "", "```", "rm -- \\"] + ["  '%s' \\" % p for p in tierB]
blk[-1] = blk[-1][:-2]
blk += ["```"]
open(O + "archive-deletable-p3jesoak.txt", "w").write("\n".join(blk) + "\n")
tot = sum(size(R + d) for d in dirs)
print("TierA", SA, len(tierA), h(SA), "TierB", SB, len(tierB), h(SB), "A+B", SA + SB, h(SA + SB), "dirs", len(dirs), tot, h(tot))
