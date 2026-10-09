#!/usr/bin/env python3
"""ozarchy-bkm: best-effort read of the node-local __book_mode__ marker byte from a RocksDB data dir's WAL files.
WriteBatch put record: ... key(varint len) b'__book_mode__' value(varint len 0x01) <byte>. Prints 'byte=<v> hits=<n> ...' or 'not_found (<n> wal files)'."""
import sys, glob, collections
K = b'__book_mode__'
hits = collections.Counter()
files = sorted(glob.glob(sys.argv[1] + '/*.log'))
for f in files:
    try:
        b = open(f, 'rb').read()
    except OSError:
        continue
    i = b.find(K)
    while i >= 0:
        j = i + len(K)
        if b[i - 1:i] == bytes([len(K)]) and b[j:j + 1] == b'\x01' and j + 1 < len(b):
            hits[b[j + 1]] += 1
        i = b.find(K, j)
print(' '.join(f'byte={v} hits={n}' for v, n in sorted(hits.items())) or f'not_found ({len(files)} wal files)')
