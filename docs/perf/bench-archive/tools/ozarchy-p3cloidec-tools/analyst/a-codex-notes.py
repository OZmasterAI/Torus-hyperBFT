"""a-codex-notes.py (ozarchy-p3cloidec): the analyst's inline query for the Codex notes, saved for the archive with the
query unchanged and only the paths parameterised. Reads prof-F/prof-cells.json and prof-B/prof-cells.json (a-prof-arm.py
output). Prints (stdout, saved as codex-notes.txt): reserve_rehash frames (inclusive), self time at the pre-pass head lines
(F 6924-6933 per-batch cloid count; B 6890-6899 any()), self at F's order_book log push / resolve lines, self at B's
order_book index / forget lines, Vec growth frames. ms CPU per 1k val1 fills, W1, mean(sd) per set.
Usage: python3 -I a-codex-notes.py [ANALYST_DIR]   (default: the run's analyst dir)"""
import json, statistics as st, re, sys
A = (sys.argv[1].rstrip("/") + "/") if len(sys.argv) > 1 else "/home/oz/bench-results-matched/ozarchy-p3cloidec-run/analyst/"
F = json.load(open(A + "prof-F/prof-cells.json")); B = json.load(open(A + "prof-B/prof-cells.json"))
S = {"F100": [r for r in F.values() if r["arm"] == "c100"], "B100": [r for r in B.values() if r["arm"] == "c100"], "B0": [r for r in B.values() if r["arm"] == "c0"]}
def show(key, pat, title):
    print("==", title)
    ks = {k for rs in S.values() for r in rs for k in r[key] if re.search(pat, k)}
    rows = []
    for k in ks:
        v = {s: [r[key].get(k, 0.0) for r in rs] for s, rs in S.items()}
        rows.append((max(st.mean(x) for x in v.values()), k, v))
    for _, k, v in sorted(rows, reverse=True)[:25]:
        print(f"  {k[:150]:150} " + " ".join(f"{s} {st.mean(x):.4f}({(st.stdev(x) if len(x)>1 else 0):.4f})" for s, x in v.items()))
show("incl", r"reserve_rehash", "children: reserve_rehash (any)")
show("self_loc", r"native_executor\.rs:69(2[4-9]|3[0-3])$|native_executor\.rs:6(89[0-9])$", "self @ pre-pass head lines (F 6924-6933 count filter; B 6890-6899 any())")
show("self_loc", r"order_book\.rs:(2453|2450|2451|3046|3064|3045|3062|3066)$", "self @ F order_book log push/resolve lines")
show("self_loc", r"order_book\.rs:(29[3-5][0-9])$", "self @ B order_book index/forget lines")
show("incl", r"RawVec|reserve::do_reserve_and_handle|grow_one|finish_grow", "children: Vec growth")
