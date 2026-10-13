#!/usr/bin/env python3
"""fg.py <folded.txt> <out.svg> <title>: minimal flame graph SVG (FlameGraph-style, root at the bottom) from folded stacks
("a;b;c weight"). No external tools (inferno / FlameGraph are not installed on ozarchy). Frames narrower than 0.05% are dropped;
hover (<title>) shows the frame, its weight (us CPU per 1k fills) and share."""

import html
import sys
import zlib

src, out, title = sys.argv[1], sys.argv[2], sys.argv[3]
root = {"n": 0.0, "c": {}}
for line in open(src):
    line = line.rstrip("\n")
    if not line:
        continue
    stack, _, w = line.rpartition(" ")
    try:
        w = float(w)
    except ValueError:
        continue
    node = root
    node["n"] += w
    for f in stack.split(";"):
        node = node["c"].setdefault(f, {"n": 0.0, "c": {}})
        node["n"] += w

W, FH, PAD = 1800, 16, 10
total = root["n"] or 1.0
minw = total * 0.0005
rects = []
depth_max = 0


def walk(node, x, d):
    global depth_max
    depth_max = max(depth_max, d)
    for name, ch in sorted(node["c"].items()):
        if ch["n"] >= minw:
            rects.append((name, x, d, ch["n"]))
            walk(ch, x, d + 1)
        x += ch["n"]


walk(root, 0.0, 0)
H = (depth_max + 2) * FH + 4 * PAD + 20
o = [
    f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" font-family="monospace" font-size="11">',
    f'<rect width="100%" height="100%" fill="#f8f8f8"/><text x="{PAD}" y="{PAD + 12}" font-size="14">{html.escape(title)}'
    f" (total {total / 1000:.2f} ms CPU per 1k fills)</text>",
]
sc = (W - 2 * PAD) / total
for name, x, d, n in rects:
    px, wd = PAD + x * sc, n * sc
    y = H - PAD - (d + 1) * FH
    h = zlib.crc32(name.encode()) & 0xFFFFFF
    col = f"rgb({200 + (h & 0x37)},{90 + ((h >> 8) & 0x7F)},{40 + ((h >> 16) & 0x3F)})"
    if "cloid" in name.lower():
        col = "rgb(60,140,230)"
    lab = html.escape(name)
    o.append(
        f"<g><title>{lab} ({n / 1000:.3f} ms/1k fills, {n / total * 100:.2f}%)</title>"
        f'<rect x="{px:.1f}" y="{y}" width="{max(wd - 0.3, 0.1):.1f}" height="{FH - 1}" fill="{col}"/>'
    )
    chars = int(wd / 6.6)
    if chars >= 4:
        t = name if len(name) <= chars else name[: chars - 2] + ".."
        o.append(f'<text x="{px + 2:.1f}" y="{y + FH - 4}">{html.escape(t)}</text>')
    o.append("</g>")
o.append("</svg>")
open(out, "w").write("\n".join(o))
print(f"{out}: {len(rects)} frames, depth {depth_max}", file=sys.stderr)
