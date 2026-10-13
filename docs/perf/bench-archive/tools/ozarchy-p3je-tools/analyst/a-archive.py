"""Bench-archive rows for ozarchy-p3je (SAT + FIX + smoke; INDEX rows, cells.md rows, DELETABLE block) -> analyst/archive-*.txt
(read-only on results). Copy of ozarchy-p3iv-run/analyst/a-archive.py; arms ja (feat/jemalloc-allocator 88ddc4c2, jemalloc compiled in)
and jb (main deeacdb3, glibc). Tier A = the staged binaries in ozarchy-p3je-stage (keep until part B is written up); the four
~/.cargo-target-p3je-* dirs are listed as outside-tree candidates."""
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = R + "ozarchy-p3je-sat-run/analyst/"
SEC = "`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section '56. jemalloc compiled in: feat/jemalloc-allocator `88ddc4c2` vs main `deeacdb3` (glibc), saturated and fixed offered load, commit-tail mode (campaign ozarchy-p3je, 2026-10-13)'"
ARM = {
    "ja": ("arm ja (88ddc4c2, jemalloc compiled in)", "jemalloc compiled in (tikv-jemallocator, no LD_PRELOAD); no checkpoint knob; in-flight tail commit"),
    "jb": ("arm jb (main deeacdb3, glibc)", "glibc malloc; no checkpoint knob; in-flight tail commit"),
}
CELLS = [("ozarchy-p3je-sat-smoke-300m-s0-ja", "ja", "SAT", "smoke 60 s, excluded")]
for m, rate in (("sat", "SAT"), ("fix", "FIX")):
    CELLS.append((f"ozarchy-p3je-{m}-300m-w0", "jb", rate, "warm-up 60 s, excluded"))
    for r, order in enumerate((("jb", "ja"), ("ja", "jb"), ("jb", "ja"), ("ja", "jb")), 1):
        CELLS += [(f"ozarchy-p3je-{m}-300m-r{r}-{a}", a, rate, "throughput 120 s") for a in order]
MODE = {"SAT": "saturated (RATE 76000 actions/s offered ceiling)", "FIX": "fixed offered load RATE 907 actions/s (75% of the jb SAT submit rate)"}
TIERB = ("val0.log.gz", "val1.log.gz", "val2.log.gz", "buckets.csv", "tasks.txt", "sampler.csv")
EXTRA = [
    ("ozarchy-p3je-sat-run", "p3je SAT campaign + smoke logs, done markers, analyst/ (a-cells.py, a-arms.py, a-paired.py, a-archive.py, cells.json, arms.txt, paired.txt, cells.txt)"),
    ("ozarchy-p3je-fix-run", "p3je FIX campaign log, RATE.txt (derivation), done marker, analyst/ (same scripts; cells.json, arms.txt, paired.txt, cells.txt)"),
    ("ozarchy-p3je-tools", "p3je build scripts (ozarchy-p3je-build.sh, -build-all.sh), campaign driver ozarchy-p3je-campaign.sh (CAMP=SAT|FIX) + env.sh"),
    ("ozarchy-p3je-build", "p3je build logs of ja / jb / jcs / jmf, md5s.txt, md5s-all.txt (+ b 8efcb07a)"),
    ("ozarchy-p3je-stage", "staged nodes ja 88ddc4c2 / jb deeacdb3 / jcs e7be8460 / jmf 0360c840 (+ -run dirs with bench 05d44313, reflinks); part B runs from here"),
]


def size(p):
    return int(subprocess.check_output(["du", "-sb", "--apparent-size", p]).split()[0])


def h(n):
    if n >= 1 << 30:
        return "%.2f GiB" % (n / (1 << 30))
    if n >= 1 << 20:
        return "%.1f MiB" % (n / (1 << 20))
    return "%d KiB" % round(n / 1024)


idx, cel, tierB, dirs = [], {"SAT": [], "FIX": []}, [], []
for d, a, mode, what in CELLS:
    dirs.append(d)
    s = json.load(open(R + d + "/summary.json"))
    md5 = s["binaries"]["torus_node_md5"][:8]
    env_md5 = open(R + d + "/node-environ-trie.txt").readline().split("exe_md5=")[1][:8]
    commit = "%s @ %s (node %s)" % (os.path.basename(s["worktree"]), s["commit"][:8], env_md5)
    for f in sorted(os.listdir(R + d)):
        if f in TIERB:
            tierB.append(R + d + "/" + f)
    sb = sum(os.path.getsize(p) for p in tierB if p.startswith(R + d + "/"))
    idx.append("| `%s` | %s | 2026-10-13 | %s | jemalloc compiled-in A/B (88ddc4c2 vs main deeacdb3 glibc), %s, tw, Classic, standard shape at 300 markets, shared NVMe, commit tail; %s; %s | %s | R | - | %s |"
               % (d, h(size(R + d)), commit, MODE[mode], ARM[a][0], what, SEC, h(sb)))
    hd = s["headline"]
    c = s["cell"]
    env = "6 knobs empty; book rows empty (Classic); trade history off + WAL 2048 MiB; exec pipeline on (default); rate %d; " % c["rate_total"] + ARM[a][1] + ("; smoke" if "-s0-" in d else "; warm-up" if d.endswith("-w0") else "")
    cel[mode].append("| `%s` | %s | %s | %s @ %s | %s | %d | %d | %d | %s | {:,.1f} | {:,.1f} | {:,.1f} | {:,.1f} | %.1f | %.1f | %.2f | %.2f | %s | %s | %s |".format(
        hd["matched_s_avg"], hd["matched_s_first120"], hd["matched_s_best60"], hd["placed_s_avg"])
        % (d, s["status"] if s["validity"]["verdict"] == "ACCEPT" else "INVALID", s["generated_at"][:16].replace("T", " "),
           os.path.basename(s["worktree"]), s["commit"][:8], env_md5, c["markets"], c["duration_s"], c["block_cap"], env,
           hd["blk_s_avg"], hd["txs_per_block_avg"], hd["chain_ms"], hd["engine_ms_per_1k_fills"], hd.get("agreement_verdict"),
           s["liveness"]["verdict"], s["validity"]["verdict"]))
tierA = []
for k in ("ja", "jb", "jcs", "jmf"):
    for sub in (k, k + "-run"):
        for f in ("torus-node", "bench-throughput"):
            p = R + "ozarchy-p3je-stage/%s/release/%s" % (sub, f)
            if os.path.exists(p):
                tierA.append(p)
p = R + "ozarchy-p3je-stage/b/release/bench-throughput"
tierA.append(p)
for d, what in EXTRA:
    dirs.append(d)
    sa = sum(os.path.getsize(p) for p in tierA if p.startswith(R + d + "/"))
    idx.append("| `%s` | %s | 2026-10-13 | ja 88ddc4c2 (f2fbe17b), jb deeacdb3 (e901fd1d), jcs e7be8460 (4521dd5f), jmf 0360c840 (9d6e57c1), bench 05d44313 | %s | %s | R | %s | - |"
               % (d, h(size(R + d)), what, SEC, h(sa) if sa else "-"))
SA = sum(os.path.getsize(p) for p in tierA)
SB = sum(os.path.getsize(p) for p in tierB)
TD = ["/home/oz/.cargo-target-p3je-ja-88ddc4c2", "/home/oz/.cargo-target-p3je-jb-deeacdb3", "/home/oz/.cargo-target-p3je-jcs-e7be8460", "/home/oz/.cargo-target-p3je-jmf-0360c840"]
STD = sum(size(t) for t in TD)
open(O + "archive-index-p3je.txt", "w").write("\n".join(sorted(idx)) + "\n")
open(O + "archive-cells-p3je.txt", "w").write("SAT\n" + "\n".join(sorted(cel["SAT"])) + "\nFIX\n" + "\n".join(sorted(cel["FIX"])) + "\n")
blk = ["Tier A, %s, %d files (keep until part B, the ja / jb / jcs / jmf soaks, is written up; reflinks, frees less than the apparent size):" % (h(SA), len(tierA)), "", "```", "rm -- \\"]
blk += ["  '%s' \\" % p for p in tierA]
blk[-1] = blk[-1][:-2]
blk += ["```", "", "Tier B, %s, %d files:" % (h(SB), len(tierB)), "", "```", "rm -- \\"] + ["  '%s' \\" % p for p in tierB]
blk[-1] = blk[-1][:-2]
blk += ["```", "", "Outside the tree: the four fresh target dirs, %s apparent (keep until part B is built and written up; rebuildable with `ozarchy-p3je-build.sh`):" % h(STD), "", "```", "rm -r -- \\"]
blk += ["  '%s' \\" % t for t in TD]
blk[-1] = blk[-1][:-2]
blk += ["```"]
open(O + "archive-deletable-p3je.txt", "w").write("\n".join(blk) + "\n")
tot = sum(size(R + d) for d in dirs)
print("TierA", SA, len(tierA), h(SA), "TierB", SB, len(tierB), h(SB), "A+B", SA + SB, h(SA + SB), "dirs", len(dirs), tot, h(tot), "targets", STD, h(STD))
