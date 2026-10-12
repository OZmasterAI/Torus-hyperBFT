"""Bench-archive rows for ozarchy-p3rss (INDEX rows, cells.md rows, DELETABLE block) -> analyst/archive-*.txt (read-only on results).
Derived from ozarchy-p3s2g-run/analyst/a-archive.py."""
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = R + "ozarchy-p3rss-run/analyst/"
SEC = "`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section '48. Phase 3 step 2 RSS split: commit-mode exec backlog, step 1 layer, step 2 view (campaign ozarchy-p3rss, 2026-10-12)'"
ARM = {
    "cth": ("arm cth (control 2725e321, in-flight tail head)", "checkpoint interval unset; in-flight tail head"),
    "ctl": ("arm ctl (control 2725e321, tail commit)", "checkpoint interval unset; in-flight tail commit"),
    "s1": ("arm s1 (step 1 a44bdde6, `TORUS_CHECKPOINT_INTERVAL_MS=15000`, tail commit)", "checkpoint interval 15 s; in-flight tail commit"),
    "s2": ("arm s2 (step 2 2296be67, `TORUS_CHECKPOINT_INTERVAL_MS=15000`, tail commit)", "checkpoint interval 15 s; in-flight tail commit"),
    "mn": ("arm mn (main 8efcb07a, tail commit; cross-check)", "checkpoint interval unset; in-flight tail commit"),
}
for a in ("cth", "ctl", "s1", "s2"):
    ARM["h" + a] = (ARM[a][0].replace("arm " + a, "arm h" + a) + " + jemalloc heap profile", ARM[a][1] + "; jemalloc LD_PRELOAD heap profile (heap/ dumps)")
CELLS = [("w0", "ctl", "warm-up 60 s, excluded")]
for r, order in enumerate((("cth", "ctl", "s1", "s2"), ("ctl", "s1", "s2", "cth"), ("s1", "s2", "cth", "ctl"), ("s2", "cth", "ctl", "s1")), 1):
    CELLS += [(f"r{r}-{a}", a, "throughput + RSS 120 s") for a in order]
CELLS += [("m1", "mn", "main cross-check 120 s"), ("m2", "mn", "main cross-check 120 s")]
CELLS += [(f"h{i}-{a}", "h" + a, "heap-profiled 120 s") for i, a in ((1, "ctl"), (1, "s1"), (1, "s2"), (2, "s2"), (2, "s1"), (2, "ctl"), (1, "cth"))]
TIERB = ("val0.log.gz", "val1.log.gz", "val2.log.gz", "buckets.csv", "tasks.txt", "sampler.csv")
EXTRA = [
    ("ozarchy-p3rss-build", "build logs and md5s.txt: bench 8efcb07a (05d44313), main 8efcb07a (09827336), P2-3 pool efcd1548 (dacdeec8); reuse records for 2725e321 / a44bdde6 / 2296be67 / c97f56f9"),
    ("ozarchy-p3rss-run", "p3rss campaign, smoke and chain logs, done markers, analyst/ (a-cells.py, a-arms.py, a-smaps.py, a-heap.py, a-heapcat.py, a-archive.py, outputs)"),
    ("ozarchy-p3rss-stage", "staged binaries: b/ (bench 8efcb07a), m/ p/ (built nodes), <key>-run/ (node + bench per key c s1 s2 m k p; reflinks), c- s1- s2-heap/ (jemalloc wrappers); also used by ozarchy-p3g2 and ozarchy-p23b; rebuildable"),
    ("ozarchy-p3rss-tools", "campaign driver (CAMP=A|B), build and stage scripts"),
    ("ozarchy-p3rss-smoke-300m-s0-hs2", "prep smoke cell (hs2, 60 s, heap wrapper check; not analysed)"),
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
tierA = sorted(R + "ozarchy-p3rss-stage/" + p for p in (
    "b/release/bench-throughput", "m/release/torus-node", "p/release/torus-node")
    + tuple(f"{k}-run/release/{f}" for k in ("c", "s1", "s2", "m", "k", "p") for f in ("torus-node", "bench-throughput"))
    + tuple(f"{k}-heap/release/bench-throughput" for k in ("c", "s1", "s2")))
for t, a, what in CELLS:
    d = "ozarchy-p3rss-300m-" + t
    dirs.append(d)
    s = json.load(open(R + d + "/summary.json"))
    md5 = s["binaries"]["torus_node_md5"][:8]
    env_md5 = open(R + d + "/node-environ-trie.txt").readline().split("exe_md5=")[1][:8]
    commit = "%s @ %s (node %s%s)" % (os.path.basename(s["worktree"]), s["commit"][:8], env_md5, "; wrapper md5 " + md5 if md5 != env_md5 else "")
    for f in sorted(os.listdir(R + d)):
        if f in TIERB:
            tierB.append(R + d + "/" + f)
    sb = sum(os.path.getsize(p) for p in tierB if p.startswith(R + d + "/"))
    idx.append("| `%s` | %s | 2026-10-12 | %s | Phase 3 RSS split (backlog / layer / view), tw, Classic, standard shape at 300 markets, shared NVMe, 5 s memory samples (mem-samples.txt, smaps/); %s; %s | %s | R | - | %s |"
               % (d, h(size(R + d)), commit, ARM[a][0], what, SEC, h(sb)))
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
    idx.append("| `%s` | %s | 2026-10-12 | 2725e321 (18fb490d), a44bdde6 (7d1a0874), 2296be67 (1c02893b), 8efcb07a (09827336), bench 05d44313 | %s | %s | R | %s | - |"
               % (d, h(size(R + d)), what, SEC, h(sa) if sa else "-"))
SA = sum(os.path.getsize(p) for p in tierA)
SB = sum(os.path.getsize(p) for p in tierB)
open(O + "archive-index-p3rss.txt", "w").write("\n".join(sorted(idx)) + "\n")
open(O + "archive-cells-p3rss.txt", "w").write("\n".join(sorted(cel)) + "\n")
blk = ["Tier A, %s, %d files (keep until the ozarchy-p3g2 and ozarchy-p23b write-ups are done: they run these binaries):" % (h(SA), len(tierA)), "", "```", "rm -- \\"]
blk += ["  '%s' \\" % p for p in tierA]
blk[-1] = blk[-1][:-2]
blk += ["```", "", "Tier B, %s, %d files:" % (h(SB), len(tierB)), "", "```", "rm -- \\"] + ["  '%s' \\" % p for p in tierB]
blk[-1] = blk[-1][:-2]
blk += ["```"]
open(O + "archive-deletable-p3rss.txt", "w").write("\n".join(blk) + "\n")
tot = sum(size(R + d) for d in dirs)
print("TierA", SA, len(tierA), h(SA), "TierB", SB, len(tierB), h(SB), "A+B", SA + SB, h(SA + SB), "dirs", len(dirs), tot, h(tot))
