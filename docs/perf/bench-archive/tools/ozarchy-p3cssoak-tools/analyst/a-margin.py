"""ozarchy-p3cssoak: margin user at the end of each soak cell, from the harness state digest (50 sampled senders, val0).
python3 -I a-margin.py CELL... > margin.txt. Per account (torus-rpc account.rs: native_balance = available + order_margin,
cash_balance = available, total_margin_used = order_margin + position IM [+ isolated]): order margin = nativeBalance - cashBalance,
position IM = totalMarginUsed - order margin, both as % of accountValue; free = freeMargin. Accounts sorted by address
(address / 2^160 in 0-1; the old cancel share walked senders in address order).
Limits: end of run only (after the drain), 50 of 5000 senders; no per-minute or per-reject data."""
import json
import sys

R = "/home/oz/bench-results-matched/"
def split(cell):
    rows = []
    for line in open(R + cell + "/state-digest-val0.txt"):
        if '"cashBalance"' not in line:
            continue
        a = json.loads(line)
        f = {k: float(a[k]) for k in ("accountValue", "cashBalance", "nativeBalance", "totalMarginUsed", "freeMargin", "maintenanceMarginUsed")}
        om = f["nativeBalance"] - f["cashBalance"]
        pim = f["totalMarginUsed"] - om
        rows.append((f["freeMargin"] / f["accountValue"], om / f["accountValue"], pim / f["accountValue"], (f["accountValue"] - f["cashBalance"]) / f["accountValue"]))
    rows.sort()
    n = len(rows)
    low = [r for r in rows if r[0] < 0.05]
    lowr = [r for r in low if r[0] + r[1] >= 0.05]  # would be >= 5% free without reservations
    q = lambda xs, p: sorted(xs)[min(len(xs) - 1, int(p * len(xs)))]
    print(f"{cell}: n={n} free% p10/median {100*q([r[0] for r in rows],.1):.1f}/{100*q([r[0] for r in rows],.5):.1f} | order-margin% median/max {100*q([r[1] for r in rows],.5):.2f}/{100*max(r[1] for r in rows):.2f} | position IM% median/max {100*q([r[2] for r in rows],.5):.1f}/{100*max(r[2] for r in rows):.1f} | "
          f"accounts free<5%: {len(low)}, of which order margin lifts them >=5%: {len(lowr)}; their posIM% {[round(100*r[2]) for r in low]} om% {[round(100*r[1],2) for r in low]} upnl% {[round(100*r[3],1) for r in low]}")

def addr(cell):
    addrs = [l.strip() for l in open(R + cell + "/digest-accounts.txt") if l.strip()]
    acc = [json.loads(l) for l in open(R + cell + "/state-digest-val0.txt") if '"cashBalance"' in l]
    assert len(addrs) == len(acc), (len(addrs), len(acc))
    rows = []
    for a, d in zip(addrs, acc):
        av = float(d["accountValue"]); om = float(d["nativeBalance"]) - float(d["cashBalance"])
        pim = float(d["totalMarginUsed"]) - om
        rows.append((int(a, 16) / 2**160, float(d["freeMargin"]) / av, pim / av, om / av))
    rows.sort()
    lo = [r for r in rows if r[0] < 0.5]; hi = [r for r in rows if r[0] >= 0.5]
    m = lambda xs: sum(xs) / len(xs) if xs else float("nan")
    q = [rows[i * len(rows) // 4:(i + 1) * len(rows) // 4] for i in range(4)]
    print(cell, "| posIM% mean by address quartile:", " ".join(f"{100*m([r[2] for r in g]):.0f}" for g in q),
          "| free<5% accounts' address pos (0-1):", [round(r[0], 2) for r in rows if r[1] < 0.05],
          "| order margin% max", round(100 * max(r[3] for r in rows), 2), "| posIM% median", round(100 * sorted(r[2] for r in rows)[25]))

for c in sys.argv[1:]:
    split(c)
    addr(c)
