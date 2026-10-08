import json, sys
for c in sys.argv[1:]:
    s = json.load(open(c + "/summary.json")); out = []
    for v in ("val0", "val1", "val2"):
        p = s["phase_by_node"][v]; e = p["phases"]["engine"]; b = p.get("by_id") or {}
        out.append(f"{v} ph1 {e.get('phase1_actions_ms')} eng {e['ms']} acts/blk {p.get('actions_per_exec_block')} byid/blk {b.get('per_native_block')} probed {b.get('books_probed_per_action')}")
    print(c.split("300m-")[1], " | ".join(out))
