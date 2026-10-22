#!/usr/bin/env python3
"""Collect dissemination counts in one pass, without whole-log shell variables.

Besides the per-node line counts (printed as the --dissem string summarize.py
takes, and written to --out), every dissemination-failure line is recorded
with its per-peer evidence, for the KILL_NO_RESTART verdict
(health.kill_dissemination):
  ts        the line's UTC timestamp (epoch seconds; None if unparsable)
  kinds     the failure counters the line counts in (same semantics as counts)
  peer_id   OUTBOUND FAILURE lines: the peer the send failed toward
  block     body / justify fetch fallbacks: the block hash's 7-char base64
            prefix (the form the node logs block hashes in)
  proposers those lines: the nodes whose log re-sent that block's header
            ("header re-send: view=V block=<prefix> ... (still in own view)",
            logged only by the block's proposer); [] = proposer unknown
and each node's own libp2p id (`local_peer_id=`), which maps peer_id to a node.
Logs may be gzipped (val<i>.log.gz, the copies run-cell.sh keeps).
"""
import argparse
import base64
import datetime
import gzip
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
# the counters summarize.py's dissemination verdict fails on
FAIL_KEYS = ('exhausted', 'sync_fallback', 'da_outbound_fail', 'starvation')
MAX_FAILURE_RECORDS = 5000  # per node; more lines are counted, not recorded (the verdict is then strict)
LOCAL_PEER = re.compile(rb'local_peer_id=(\w+)')
PEER = re.compile(rb'\bpeer=(\w+)')
HASH = re.compile(rb'for \[([0-9]+(?:, [0-9]+){31})\]')
RESEND = re.compile(rb'header re-send: view=\d+ block=([A-Za-z0-9+/]{7})')
TS = re.compile(rb'^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(\.\d+)?Z')


def line_ts(line):
    m = TS.match(line)
    if not m:
        return None
    t = datetime.datetime.strptime(m.group(1).decode(), '%Y-%m-%dT%H:%M:%S')
    return t.replace(tzinfo=datetime.timezone.utc).timestamp() + float(m.group(2) or 0)


def block_prefix(numbers):
    return base64.b64encode(bytes(int(x) for x in numbers.split(b','))).decode()[:7]


def failure_record(line, kinds):
    peer = PEER.search(line) if 'da_outbound_fail' in kinds else None
    block = HASH.search(line)
    return {'ts': line_ts(line), 'kinds': kinds,
            'peer_id': peer.group(1).decode() if peer else None,
            'block': block_prefix(block.group(1)) if block else None}


def open_log(path):
    return gzip.open(path, 'rb') if str(path).endswith('.gz') else open(path, 'rb')


def collect(path):
    counts = dict.fromkeys(FIELDS, 0)
    total_bytes = queue_bytes = queue_lines = 0
    local_peer, failures, resent = None, [], set()
    with open_log(path) as stream:
        for raw in stream:
            total_bytes += len(raw)
            line = ANSI.sub(b'', raw)
            kinds = [key for key, pattern in PATTERNS.items() if pattern.search(line)]
            for key in kinds:
                counts[key] += 1  # Same line-count semantics as grep -c.
            if b'FULL-BODY push' in line:
                for value in re.findall(rb'bytes=([0-9]+)', line):
                    counts['body_push_max_bytes'] = max(counts['body_push_max_bytes'], int(value))
            if b'Send Queue full' in line:
                queue_lines += 1
                queue_bytes += len(raw)
            if local_peer is None and b'local_peer_id=' in line:
                local_peer = LOCAL_PEER.search(line).group(1).decode()
            m = RESEND.search(line) if b'header re-send' in line else None
            if m:
                resent.add(m.group(1).decode())
            fail = [k for k in kinds if k in FAIL_KEYS]
            if fail and len(failures) < MAX_FAILURE_RECORDS:
                failures.append(failure_record(line, fail))
    return {'counts': counts, 'log_bytes': total_bytes,
            'send_queue_full_lines': queue_lines, 'send_queue_full_bytes': queue_bytes,
            'local_peer_id': local_peer, 'failures': failures, 'resent': resent}


def collect_all(paths):
    """val0..val2 logs -> {node: collect()}, each block fallback's proposers
    resolved across the three logs (the re-send sets are not kept)."""
    result = {f'val{i}': collect(path) for i, path in enumerate(paths)}
    for entry in result.values():
        for f in entry['failures']:
            if f['block'] is not None:
                f['proposers'] = sorted(n for n, e in result.items() if f['block'] in e['resent'])
    for entry in result.values():
        entry.pop('resent')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--logs', type=Path, nargs=3, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    result = collect_all(args.logs)
    args.out.write_text(json.dumps(result, indent=2)+'\n')
    print(' '.join(node+':'+','.join(f'{key}={entry["counts"][key]}' for key in FIELDS)
                   for node, entry in result.items()))


if __name__ == '__main__':
    main()
