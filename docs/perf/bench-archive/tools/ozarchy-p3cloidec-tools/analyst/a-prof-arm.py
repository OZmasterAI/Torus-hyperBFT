"""a-prof-arm.py (ozarchy-p3cloidec analyst): runs section 60's a-prof.py UNCHANGED ON DISK for one arm's binary.
In-memory parameterisation only (a-paired.py execs a-arms.py the same way):
  ELF -> the arm's staged torus-node (the two arms are different binaries; a2l-cache.json is per OUT dir);
  the two line-range sites -> the arm's own lines of the same code (pre-pass serial block, book place contains_key);
  extra SITES for functions that only exist at bc44a845 (resolve_cloid_log, drain_cloid_keys, order_id_by_cloid_key).
Usage: a-prof-arm.py ELF NE_LO NE_HI OB_LO OB_HI OUT CELL..."""
import sys
SRC = "/home/oz/bench-results-matched/ozarchy-p3cloidprof-run/analyst/a-prof.py"
elf, nlo, nhi, olo, ohi, out = sys.argv[1:7]
cells = sys.argv[7:]
s = open(SRC).read()
subs = [
    ('ELF = "/home/oz/bench-results-matched/ozarchy-p3cloid-stage/cl-run/release/torus-node"', f'ELF = "{elf}"'),
    ('_ln(l, "native_executor.rs", 6879, 6940)', f'_ln(l, "native_executor.rs", {int(nlo)}, {int(nhi)})'),
    ('_ln(l, "order_book.rs", 1397, 1402)', f'_ln(l, "order_book.rs", {int(olo)}, {int(ohi)})'),
    ('    ("3 cleanup", "OrderBook::forget_cloids", lambda f, l: f.endswith("forget_cloids")),\n]',
     '    ("3 cleanup", "OrderBook::forget_cloids", lambda f, l: f.endswith("forget_cloids")),\n'
     '    ("1 index", "OrderBook::resolve_cloid_log (F worker)", lambda f, l: f.endswith("resolve_cloid_log")),\n'
     '    ("1 index", "OrderBook::drain_cloid_keys (F)", lambda f, l: f.endswith("drain_cloid_keys")),\n'
     '    ("1 index", "OrderBook::order_id_by_cloid_key (F)", lambda f, l: f.endswith("order_id_by_cloid_key")),\n]'),
]
for a, b in subs:
    assert s.count(a) == 1, a
    s = s.replace(a, b)
sys.argv = [SRC, out] + cells
exec(compile(s, SRC, "exec"), {"__name__": "__main__", "__file__": SRC})
