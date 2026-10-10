#!/usr/bin/env python3
# ozarchy-acc2-lsm.py LOG  (run as: python3 -I ozarchy-acc2-lsm.py LOG). Read-only facts from one validator's RocksDB LOG:
#   cf=<name> files=<n> bytes=<sum of file_size> rows=<n>  table_file_creation events of the append-only CFs and the trade CFs;
#       rows = sum of (num_entries - num_range_deletions): data rows written. A file holding only the startup range tombstone
#       (ensure_trade_history_format delete_range, every start) counts as 1 file and 0 rows.
#   max_total_wal_size=<bytes>   first OPTIONS dump line of the open (536870912 = 512 MiB default, 2147483648 = 2048 MiB)
#   wal_compression=<value>      first OPTIONS dump line (0 = off, 7 = ZSTD)
#   recovering_logs=<n,n,...>    "Recovering log #N" lines of this open = WAL files replayed at this start (none = fresh DB)
import json
import re
import sys

CFS = ("cf_native_trades", "cf_native_user_trades", "cf_native_pending")
files = {c: [0, 0, 0] for c in CFS}  # table files, bytes, data rows
mwal = None
wal = None
rec = []
for line in open(sys.argv[1], errors="replace"):
    if mwal is None and "Options.max_total_wal_size:" in line:
        mwal = line.split("Options.max_total_wal_size:")[1].strip()
    if wal is None and "Options.wal_compression:" in line:
        wal = line.split("Options.wal_compression:")[1].strip()
    m = re.search(r"Recovering log #(\d+)", line)
    if m and m.group(1) not in rec:
        rec.append(m.group(1))
    i = line.find("EVENT_LOG_v1 ")
    if i < 0 or "table_file_creation" not in line:
        continue
    try:
        d = json.loads(line[i + len("EVENT_LOG_v1 ") :])
    except ValueError:
        continue
    if d.get("cf_name") in files:
        tp = d.get("table_properties") or {}
        files[d["cf_name"]][0] += 1
        files[d["cf_name"]][1] += int(d.get("file_size", 0))
        files[d["cf_name"]][2] += int(tp.get("num_entries", 0)) - int(
            tp.get("num_range_deletions", 0)
        )
for c in CFS:
    print(
        "cf=%s files=%d bytes=%d rows=%d" % (c, files[c][0], files[c][1], files[c][2])
    )
print("max_total_wal_size=%s" % (mwal or "none"))
print("wal_compression=%s" % (wal or "none"))
print("recovering_logs=%s" % (",".join(rec) or "none"))
