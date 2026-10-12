"""Bench-archive rows for ozarchy-p3rss (INDEX rows, cells.md rows, DELETABLE block) -> analyst/archive-*.txt (read-only on results).
Derived from ozarchy-p3s2g-run/analyst/a-archive.py."""
import json
import os
import subprocess

R = "/home/oz/bench-results-matched/"
O = R + "ozarchy-p3g2-run/analyst/"
SEC = "`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section '49. Phase 3 gate rerun: step 1 `b1605417` and step 2 `51a67f57` vs main `8efcb07a`, commit-tail mode, with the checkpoint timers (campaign ozarchy-p3g2, 2026-10-12)'"
ARM = {
    "gm": ("arm gm (main 8efcb07a, tail commit)", "checkpoint interval unset; in-flight tail commit"),
    "g1": ("arm g1 (step 1 b1605417, `TORUS_CHECKPOINT_INTERVAL_MS=15000`, tail commit)", "checkpoint interval 15 s; in-flight tail commit"),
    "g2": ("arm g2 (step 2 51a67f57, `TORUS_CHECKPOINT_INTERVAL_MS=15000`, tail commit)", "checkpoint interval 15 s; in-flight tail commit"),
    "hg2": ("arm hg2 (step 2 51a67f57 + jemalloc heap profile)", "checkpoint interval 15 s; in-flight tail commit; jemalloc LD_PRELOAD heap profile (heap/ dumps)"),
}
CELLS = [("w0", "gm", "warm-up 60 s, excluded")]
for r, order in enumerate((("gm", "g1", "g2"), ("g1", "g2", "gm"), ("g2", "gm", "g1"), ("g2", "g1", "gm")), 1):
    CELLS += [(f"r{r}-{a}", a, "throughput 120 s") for a in order]
CELLS += [("h1-g2", "hg2", "heap-profiled 120 s; agreement DISAGREE (mark-only, slow digest on val2; section 49), kept")]
TIERB = ("val0.log.gz", "val1.log.gz", "val2.log.gz", "buckets.csv", "tasks.txt", "sampler.csv")
EXTRA = [
    ("ozarchy-p3g2-build", "build logs and md5s.txt: step 1 b1605417 (c2cfb1c9), step 2 51a67f57 (448bd3ac); reuse records for main 8efcb07a (09827336) and bench 05d44313"),
    ("ozarchy-p3g2-run", "p3g2 campaign log, done marker, analyst/ (section 48 scripts + outputs)"),
    ("ozarchy-p3g2-stage", "staged binaries: b/, g1/, g2/, {m,g1,g2}-run/ (reflinks), g2-heap/ (jemalloc prof wrapper), {m,g2}-je/ (jemalloc no-prof wrappers, staged for an allocator A/B that now runs on the next step 2 head; unused); rebuildable"),
    ("ozarchy-p3g2-tools", "campaign driver (CAMP=G), build script, build-stage script"),
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
tierA = sorted(R + "ozarchy-p3g2-stage/" + p for p in (
    "b/release/bench-throughput", "g1/release/torus-node", "g2/release/torus-node")
    + tuple(f"{k}-run/release/{f}" for k in ("m", "g1", "g2") for f in ("torus-node", "bench-throughput"))
    + ("g2-heap/release/bench-throughput", "m-je/release/bench-throughput", "g2-je/release/bench-throughput"))
for t, a, what in CELLS:
    d = "ozarchy-p3g2-300m-" + t
    dirs.append(d)
    s = json.load(open(R + d + "/summary.json"))
    md5 = s["binaries"]["torus_node_md5"][:8]
    env_md5 = open(R + d + "/node-environ-trie.txt").readline().split("exe_md5=")[1][:8]
    commit = "%s @ %s (node %s%s)" % (os.path.basename(s["worktree"]), s["commit"][:8], env_md5, "; wrapper md5 " + md5 if md5 != env_md5 else "")
    for f in sorted(os.listdir(R + d)):
        if f in TIERB:
            tierB.append(R + d + "/" + f)
    sb = sum(os.path.getsize(p) for p in tierB if p.startswith(R + d + "/"))
    idx.append("| `%s` | %s | 2026-10-12 | %s | Phase 3 gate rerun (step 1 / step 2 vs main, checkpoint timers), tw, Classic, standard shape at 300 markets, shared NVMe, harness bench/summarize-ckpt-timers, 5 s memory samples; %s; %s | %s | R | - | %s |"
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
    idx.append("| `%s` | %s | 2026-10-12 | 8efcb07a (09827336), b1605417 (c2cfb1c9), 51a67f57 (448bd3ac), bench 05d44313 | %s | %s | R | %s | - |"
               % (d, h(size(R + d)), what, SEC, h(sa) if sa else "-"))
SA = sum(os.path.getsize(p) for p in tierA)
SB = sum(os.path.getsize(p) for p in tierB)
open(O + "archive-index-p3g2.txt", "w").write("\n".join(sorted(idx)) + "\n")
open(O + "archive-cells-p3g2.txt", "w").write("\n".join(sorted(cel)) + "\n")
blk = ["Tier A, %s, %d files (rebuildable):" % (h(SA), len(tierA)), "", "```", "rm -- \\"]
blk += ["  '%s' \\" % p for p in tierA]
blk[-1] = blk[-1][:-2]
blk += ["```", "", "Tier B, %s, %d files:" % (h(SB), len(tierB)), "", "```", "rm -- \\"] + ["  '%s' \\" % p for p in tierB]
blk[-1] = blk[-1][:-2]
blk += ["```"]
open(O + "archive-deletable-p3g2.txt", "w").write("\n".join(blk) + "\n")
tot = sum(size(R + d) for d in dirs)
print("TierA", SA, len(tierA), h(SA), "TierB", SB, len(tierB), h(SB), "A+B", SA + SB, h(SA + SB), "dirs", len(dirs), tot, h(tot))
