"""ozarchy-p3mw: checkpoint merge-wait counters per cell from metrics-before/after (python3 -I a-mw.py PREFIX TAG:ARM ...).
Per node delta over the metrics window (bench start -> bench end): native blocks (torus_exec_native_blocks_total),
settle_wait / upkeep / deferred_join seconds (sum, count), deferred_read_waits (counter), checkpoints, drop_inline.
Prints ms per native block (sum / native blocks) and ms per observation, mean of 3 nodes, and per-arm means."""
import re
import statistics as st
import sys

V = ("val0", "val1", "val2")
K = ["torus_exec_native_blocks_total", "torus_exec_checkpoints_total", "torus_exec_checkpoint_drop_inline_total",
     "torus_exec_checkpoint_deferred_read_waits", "torus_exec_checkpoint_deferred_read_waits_total",
     "torus_exec_checkpoint_settle_wait_seconds_sum", "torus_exec_checkpoint_settle_wait_seconds_count",
     "torus_exec_checkpoint_upkeep_seconds_sum", "torus_exec_checkpoint_upkeep_seconds_count",
     "torus_exec_checkpoint_deferred_join_seconds_sum", "torus_exec_checkpoint_deferred_join_seconds_count"]


def met(p):
    d = {}
    for line in open(p):
        m = re.match(r"^([a-z_]+)(\{[^}]*\})? ([0-9.eE+-]+)$", line.strip())
        if m and m.group(1) in K:
            d[m.group(1)] = d.get(m.group(1), 0.0) + float(m.group(3))
    return d


def main():
    P = sys.argv[1]
    arms = {}
    for ta in sys.argv[2:]:
        tag, arm = ta.split(":")
        rows = []
        for v in V:
            b, a = met(f"{P}{tag}/metrics-before-{v}.txt"), met(f"{P}{tag}/metrics-after-{v}.txt")
            d = {k: a.get(k, float("nan")) - b.get(k, 0.0) if k in a else float("nan") for k in K}
            nb = d["torus_exec_native_blocks_total"]
            rw = d["torus_exec_checkpoint_deferred_read_waits"]
            if rw != rw:
                rw = d["torus_exec_checkpoint_deferred_read_waits_total"]
            rows.append(dict(
                nb=nb, ckpts=d["torus_exec_checkpoints_total"], inline=d["torus_exec_checkpoint_drop_inline_total"], read_waits=rw,
                settle_pb=1000 * d["torus_exec_checkpoint_settle_wait_seconds_sum"] / nb,
                settle_obs=1000 * d["torus_exec_checkpoint_settle_wait_seconds_sum"] / d["torus_exec_checkpoint_settle_wait_seconds_count"],
                upkeep_pb=1000 * d["torus_exec_checkpoint_upkeep_seconds_sum"] / nb,
                join_pb=1000 * d["torus_exec_checkpoint_deferred_join_seconds_sum"] / nb,
                join_n=d["torus_exec_checkpoint_deferred_join_seconds_count"],
                join_obs=(1000 * d["torus_exec_checkpoint_deferred_join_seconds_sum"] / d["torus_exec_checkpoint_deferred_join_seconds_count"]
                          if d["torus_exec_checkpoint_deferred_join_seconds_count"] else float("nan"))))
        r = {k: st.mean(x[k] for x in rows) for k in rows[0]}
        r["read_waits_nodes"] = [x["read_waits"] for x in rows]
        arms.setdefault(arm, []).append(r)
        print(f"{tag} {arm} native_blocks={r['nb']:.0f} ckpts={r['ckpts']:.1f} settle_wait ms/native blk={r['settle_pb']:.2f} (per obs {r['settle_obs']:.2f}) "
              f"upkeep ms/blk={r['upkeep_pb']:.2f} deferred_join ms/blk={r['join_pb']:.3f} (n={r['join_n']:.0f}, per obs {r['join_obs']:.3f}) "
              f"deferred_read_waits per node={r['read_waits_nodes']} drop_inline={r['inline']:.0f}")
    print()
    for arm, rs in arms.items():
        print(arm, {k: round(st.mean(x[k] for x in rs), 3) for k in rs[0] if k != "read_waits_nodes"},
              "read_waits total", sum(sum(0 if w != w else w for w in x["read_waits_nodes"]) for x in rs))


main()
