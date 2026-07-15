import sys, re
txt = open(sys.argv[1]).read()
def find(pat):
    m = re.search(pat, txt)
    return m.group(1) if m else "NA"
included_s = find(r'Included:.*?\(([\d.]+)/s')
orders_s   = find(r'Orders:.*?\(([\d.]+)/s')
block_ms   = find(r'Block time:\s*([\d.]+)ms')
drop       = find(r'Drop rate:\s*([\d.]+)%')
print(f"included_s={included_s} orders_s={orders_s} block_ms={block_ms} drop_pct={drop}")
