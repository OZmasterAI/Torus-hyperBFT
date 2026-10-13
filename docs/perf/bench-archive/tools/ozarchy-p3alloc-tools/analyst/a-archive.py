"""Bench-archive rows for ozarchy-p3alloc (INDEX rows, cells.md rows, DELETABLE block) -> analyst/archive-*.txt (read-only on results).
Derived from ozarchy-p23b-run/analyst/a-archive.py."""
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = R + "ozarchy-p3alloc-run/analyst/"
SEC = "`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section '51. Phase 3 allocator A/B: glibc vs jemalloc on step 2 `7b006739` and main `219f0c20`, commit-tail mode (campaign ozarchy-p3alloc, 2026-10-12)'"
ARM = {
    "gm": ("arm gm (control main 219f0c20, glibc)", "checkpoint interval unset; glibc; in-flight tail commit"),
    "g2": ("arm g2 (step 2 7b006739 at 15 s, glibc)", "checkpoint interval 15000 ms; glibc; in-flight tail commit"),
    "jgm": ("arm jgm (control main 219f0c20, jemalloc 5.3.1 LD_PRELOAD, defaults)", "checkpoint interval unset; LD_PRELOAD jemalloc (no MALLOC_CONF); in-flight tail commit"),
    "jg2": ("arm jg2 (step 2 7b006739 at 15 s, jemalloc 5.3.1 LD_PRELOAD, defaults)", "checkpoint interval 15000 ms; LD_PRELOAD jemalloc (no MALLOC_CONF); in-flight tail commit"),
}
CELLS = [("w0", "g2", "warm-up 60 s, excluded")]
for r, order in enumerate((("gm", "g2", "jgm", "jg2"), ("g2", "jgm", "jg2", "gm"), ("jgm", "jg2", "gm", "g2"), ("jg2", "gm", "g2", "jgm")), 1):
    CELLS += [(f"r{r}-{a}", a, "throughput 120 s") for a in order]
TIERB = ("val0.log.gz", "val1.log.gz", "val2.log.gz", "buckets.csv", "tasks.txt", "sampler.csv")
EXTRA = [
    ("ozarchy-p3alloc-run", "p3alloc campaign log, done marker, analyst/ (a-cells.py, a-arms.py, a-archive.py, cells.json, arms.txt, digest.txt)"),
    ("ozarchy-p3alloc-tools", "campaign driver (CAMP=J allocator A/B, CAMP=S soak), jemalloc-defaults-stats.txt"),
    ("ozarchy-p3next-build", "build logs and md5s.txt for m 219f0c20 (987f4eb6) and g2 7b006739 (ca388315)"),
    ("ozarchy-p3next-tools", "build-stage.sh, ozarchy-p3next-build.sh"),
    ("ozarchy-p3next-stage", "staged nodes m / g2 (+ -run copies, -je LD_PRELOAD wrappers) and bench 05d44313 (rebuildable); also used by ozarchy-p3term and the soak"),
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
tierA = sorted(os.path.join(dp, f) for dp, _, fs in os.walk(R + "ozarchy-p3next-stage") for f in fs if os.path.getsize(os.path.join(dp, f)) > 1 << 20)
for t, a, what in CELLS:
    d = "ozarchy-p3alloc-300m-" + t
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
    idx.append("| `%s` | %s | 2026-10-12 | %s | Phase 3 allocator A/B (glibc vs jemalloc), tw, Classic, standard shape at 300 markets, shared NVMe, commit tail; %s; %s | %s | R | %s | %s |"
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
    idx.append("| `%s` | %s | 2026-10-12 | 219f0c20 (987f4eb6), 7b006739 (ca388315), bench 05d44313 | %s | %s | R | %s | - |"
               % (d, h(size(R + d)), what, SEC, h(sa) if sa else "-"))
SA = sum(os.path.getsize(p) for p in tierA)
SB = sum(os.path.getsize(p) for p in tierB)
open(O + "archive-index-p3alloc.txt", "w").write("\n".join(sorted(idx)) + "\n")
open(O + "archive-cells-p3alloc.txt", "w").write("\n".join(sorted(cel)) + "\n")
blk = ["Tier A, %s, %d files (staged binaries in ozarchy-p3next-stage, rebuildable from 219f0c20 / 7b006739 / 8efcb07a; keep until the p3term and soak write-ups are done):" % (h(SA), len(tierA)), "", "```", "rm -- \\"]
blk += ["  '%s' \\" % p for p in tierA]
blk[-1] = blk[-1][:-2]
blk += ["```", "", "Tier B, %s, %d files:" % (h(SB), len(tierB)), "", "```", "rm -- \\"] + ["  '%s' \\" % p for p in tierB]
blk[-1] = blk[-1][:-2]
blk += ["```"]
open(O + "archive-deletable-p3alloc.txt", "w").write("\n".join(blk) + "\n")
tot = sum(size(R + d) for d in dirs)
print("TierA", SA, len(tierA), h(SA), "TierB", SB, len(tierB), h(SB), "A+B", SA + SB, h(SA + SB), "dirs", len(dirs), tot, h(tot))
