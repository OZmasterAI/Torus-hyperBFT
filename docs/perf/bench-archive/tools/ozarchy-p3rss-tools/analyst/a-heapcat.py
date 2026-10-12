"""Heap-profiled cells -> live heap (jemalloc, unbiased) by component, per node at the last dump <= bench end (python3 -I).
Usage: a-heapcat.py CELLS.json PREFIX STAGE [--other N]   (STAGE: dir with <key>-run/release/torus-node; arm -> key below)"""
import collections
import glob
import json
import os
import re
import statistics as st
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import importlib.util  # noqa: E402

spec = importlib.util.spec_from_file_location("aheap", os.path.join(os.path.dirname(os.path.abspath(__file__)), "a-heap.py"))
src = open(spec.origin).read().split("\ndef main")[0]
H = {}
exec(compile(src, spec.origin, "exec"), H)  # noqa: S102

KEY = {"hctl": "c", "hs1": "s1", "hs2": "s2", "hcth": "c", "hg2": "g2"}
# (component, regex over the joined frames), first match wins
RULES = [
    ("view slices (step 2 FrozenPending::slice / ReadView)", r"FrozenPending>::slice|torus_state::read_view|ReadView>::|install_slices"),
    ("layer L (step 1 CheckpointLayer)", r"CheckpointLayer>::"),
    ("frozen pending sets (pipeline hand-off / above S)", r"FrozenPending>::|backend::FrozenPending|checkpoint_upkeep|spawn_layer_merge"),
    ("mempool (actions held for proposal)", r"torus_mempool"),
    ("committed blocks / exec queue (decoded bodies, actions)", r"PlaceOrderParams|NativeAction|NativeBlock|exec_pipeline|BlockBody|decode_block|validate_block|da_reconstruct|CommittedBlockMsg"),
    ("order books (resident + save buffers) (OrderBook, TraderOrders, StopOrder)", r"order_book::|OrderBook|TraderOrders|StopOrder"),
    ("exec working set (MatchResult, engine)", r"market_workers|MatchResult|settle_market"),
    ("resident positions / rows (TraderPositions, ResidentRows)", r"TraderPositions|ResidentRows|resident_rows|EndResidentJob"),
    ("RocksDB memtables", r"rocksdb::(Concurrent)?Arena|MemTable|InlineSkipList"),
    ("RocksDB block cache / reads", r"rocksdb::.*(Block|LRUCache|Cache|Table|Fetch)"),
    ("RocksDB other", r"rocksdb::"),
    ("state overlay / pending maps (NativeStateOverlay)", r"NativeStateOverlay|overlay|Pending"),
    ("network / RPC", r"torus_network|torus_rpc|libp2p|hyper|jsonrpsee|quinn"),
]
RX = [(n, re.compile(r)) for n, r in RULES]


def classify(fr):
    j = " | ".join(fr)
    for n, r in RX:
        if r.search(j):
            return n
    return "other"


def main():
    C = json.load(open(sys.argv[1]))
    P, STG = sys.argv[2], sys.argv[3]
    nother = int(sys.argv[sys.argv.index("--other") + 1]) if "--other" in sys.argv else 0
    res = {}
    others = collections.Counter()
    for tag, x in C.items():
        if x["arm"] not in KEY:
            continue
        binary = f"{STG}/{KEY[x['arm']]}-run/release/torus-node"
        pids = [line.split()[0].split("=")[1] for line in open(f"{P}{tag}/node-environ-trie.txt")]
        dumps = []
        ND = 3
        for p in pids:
            fs = [(os.path.getmtime(f), f) for f in glob.glob(f"{P}{tag}/heap/jeprof.{p}.*.heap")]
            fs = [f for f in fs if f[0] <= x["t1"]]
            dumps += [f for _, f in sorted(fs)[-ND:]]
        cats = collections.defaultdict(list)
        for _, rows in H["frames_for"](binary, dumps):
            agg = collections.Counter()
            for fr, b in rows:
                c = classify(fr)
                agg[c] += b
                if c == "other":
                    others[" < ".join(f for f in fr if "torus" in f or "std::" not in f)[:300]] += b
            for n, _ in RULES + [("other", "")]:
                cats[n].append(agg.get(n, 0) / 1e6)
            cats["TOTAL"].append(sum(agg.values()) / 1e6)
        res[tag] = dict(arm=x["arm"], mb={k: st.mean(v) for k, v in cats.items()}, dumps=[os.path.basename(d) for d in dumps])
        print(tag, x["arm"], {k: round(v) for k, v in res[tag]["mb"].items()}, flush=True)
    arms = collections.defaultdict(list)
    for r in res.values():
        arms[r["arm"]].append(r["mb"])
    names = [n for n, _ in RULES] + ["other", "TOTAL"]
    print("\nlive heap MB per node at the last dump <= bench end, arm mean:")
    print(f"{'component':62s}" + "".join(f"{a:>9s}" for a in arms))
    for n in names:
        print(f"{n:62s}" + "".join(f"{st.mean(m.get(n, 0) for m in ms):9.0f}" for ms in arms.values()))
    json.dump(res, open(os.path.join(os.path.dirname(sys.argv[1]), "heapcat.json"), "w"), indent=0)
    for k, b in others.most_common(nother):
        print(f"  other {b / 1e6:8.1f} MB {k}")


main()
