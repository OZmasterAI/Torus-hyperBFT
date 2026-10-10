# ozarchy-acc: per-CF codec of the SST files a validator wrote (from its RocksDB LOG table_file_creation events).
# usage: ozarchy-acc-codec-check.py LOG   -> prints "cf codec level counts" for the 3 append-only CFs and WAL option line
import json, sys, collections, re
APPEND = ("cf_native_trades", "cf_native_user_trades", "cf_native_pending")
c = collections.Counter()
wal = []
for line in open(sys.argv[1], errors="replace"):
    if "Options.wal_compression" in line:
        wal.append(line.split(":")[-1].strip())
    i = line.find("EVENT_LOG_v1 ")
    if i < 0 or "table_file_creation" not in line:
        continue
    try:
        d = json.loads(line[i + len("EVENT_LOG_v1 "):])
    except ValueError:
        continue
    if d.get("cf_name") not in APPEND:
        continue
    tp = d.get("table_properties", {})
    m = re.search(r"level=(\d+)", tp.get("compression_options", ""))
    c[(d["cf_name"], tp.get("compression", "?"), m.group(1) if m else "?")] += 1
for k, v in sorted(c.items()):
    print("%s %s level=%s files=%d" % (k[0], k[1], k[2], v))
print("wal_compression_opt=" + (wal[0] if wal else "none"))
