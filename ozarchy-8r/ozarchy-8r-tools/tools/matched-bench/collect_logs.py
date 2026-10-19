#!/usr/bin/env python3
"""Collect dissemination counts in one pass, without whole-log shell variables."""
import argparse
import json
from pathlib import Path
import re

ANSI = re.compile(rb'\x1b\[[0-9;]*m')
PATTERNS = {
    'manifest': re.compile(rb'HASH-ONLY manifest push'),
    'body_push': re.compile(rb'FULL-BODY push'),
    'exhausted': re.compile(rb'body fetch (exhausted|has no remaining targets)'),
    'sync_fallback': re.compile(rb'falling back to sync'),
    'da_outbound_fail': re.compile(rb'(native-da|block-data) OUTBOUND FAILURE'),
    'starvation': re.compile(rb'header-first body starvation'),
    'pacing': re.compile(rb'exec-backlog pacing'),
}
FIELDS = ('manifest', 'body_push', 'body_push_max_bytes', 'exhausted',
          'sync_fallback', 'da_outbound_fail', 'starvation', 'pacing')


def collect(path):
    counts = dict.fromkeys(FIELDS, 0)
    total_bytes = queue_bytes = queue_lines = 0
    with path.open('rb') as stream:
        for raw in stream:
            total_bytes += len(raw)
            line = ANSI.sub(b'', raw)
            for key, pattern in PATTERNS.items():
                if pattern.search(line):
                    counts[key] += 1  # Same line-count semantics as grep -c.
            if b'FULL-BODY push' in line:
                for value in re.findall(rb'bytes=([0-9]+)', line):
                    counts['body_push_max_bytes'] = max(counts['body_push_max_bytes'], int(value))
            if b'Send Queue full' in line:
                queue_lines += 1
                queue_bytes += len(raw)
    return {'counts': counts, 'log_bytes': total_bytes,
            'send_queue_full_lines': queue_lines, 'send_queue_full_bytes': queue_bytes}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--logs', type=Path, nargs=3, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    result = {f'val{i}': collect(path) for i, path in enumerate(args.logs)}
    args.out.write_text(json.dumps(result, indent=2)+'\n')
    print(' '.join(node+':'+','.join(f'{key}={entry["counts"][key]}' for key in FIELDS)
                   for node, entry in result.items()))


if __name__ == '__main__':
    main()
