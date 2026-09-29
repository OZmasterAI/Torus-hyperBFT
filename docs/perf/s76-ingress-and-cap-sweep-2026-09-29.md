# s76 bench ingress probes and block-cap sweep — 2026-09-29

All cells: 3-validator loopback devnet on one 18-vCPU host, 10 markets, 300 s,
rate 76,000 (effectively unbounded), bs400, `TORUS_BODY_FETCH_TRACE=1
TIMEOUT_BASE_MS=1200 TORUS_BODY_SERVE_THREAD=1 TORUS_DEFER_PARENT_FEED=0`,
s75 `bench-throughput` binary. Every cell below was accepted with AGREE and
liveness PASS.

## Outcome (merged to main)

| setting | before | now | commit |
| --- | --- | --- | --- |
| node RPC max connections | 64 | 512 (`TORUS_RPC_MAX_CONNS=64` restores) | `adc583e` |
| bench ingress (harness) | val0 only | all 3 validators (`BENCH_RPCS=0` restores) | `b65fb0e`, `e521ebc` |
| block cap + companion caps | 200 / 100k orders / 32k cache | 400 / 200k orders / 64k cache (`TORUS_NATIVE_TOTAL_BLOCK_CAP=200` restores) | `7bcc179` |
| bench in-flight requests (`CONC`) | 256 | 256 (unchanged) | — |

Cells before s76 are not comparable with cells after these defaults.

## 1. Ingress probes (cap 200, binary `bce54d0` = main code at the time)

One cell each beyond the 64/val0 cells; single cells, so only a large jump
would count.

| setup | submitted | processed | expired / node | matched/s |
| --- | --- | --- | --- | --- |
| 64 conns, val0 (4 cells) | 246-311/s | 73.6-84.0k | 1.4-10.9k | 57.7-68.5k |
| 64 conns, all 3 | 312/s | 85.6k | 9.3k | 69.6k |
| 512 conns, val0 | 324/s | 86.8k | 13.6k | 71.1k |
| 512 conns, all 3 | 345/s | 89.7k | 15.8k | 71.9k |

- The 64-connection cap was not a hard choke (0-5 error lines per cell).
- The bench is closed-loop: 256 requests in flight, each waiting for its
  reply, so offered load follows node RPC latency (a bad-disk cell offered
  only 246/s).
- At cap 200 the chain processed ~85-90k actions per cell whatever was
  offered; the rest expired after the 60 s nonce window.
- 512 conns + all 3 validators was adopted as the default for the highest
  offered load (and the node default was raised so bench and nodes run the
  same config).

## 2. Block-cap sweep (`s76-cap-20260929`, main `8b8432c`, CONC 256)

3 cells per cap, alternating, plus a cap-200 warm-up.

| cap | matched/s (window) | expired / node | actions / block | total matched | best 60 s | ms / block |
| --- | --- | --- | --- | --- | --- | --- |
| 200 | 61.0 / 68.3 / 66.3k | 5.7-9.3k | 179-200 | 22.9-25.8M | 84.7-87.0k | 579-737 |
| 300 | 68.2 / 53.5 / 59.7k | 0-5.0k | 222 (r1) | 27.7M (r1) | 87.1k (r1) | 610-630 |
| 400 | 67.9 / 67.6 / 66.3k | 0-1.7k | 219-273 | 27.1-29.2M | 100.1-110.0k | 747-896 |

The cap-300 mean is pulled down by one cell whose bench offered only 229/s.

## 3. Offered-load re-test (`s76-conc-20260929`, CONC 512)

| cap | matched/s (window) | expired / node | actions / block | total matched | best 60 s | ms / block |
| --- | --- | --- | --- | --- | --- | --- |
| 200 | 63.7 / 66.5 / 65.2k | 2.3-8.0k | 169-191 | 24.2-25.1M | 82.5-93.5k | 616-732 |
| 400 | 65.6 / 64.8 / 70.1k | 0-0.2k | 241-250 | 26.5-29.0M | 92.2-98.1k | 824-871 |

CONC 512 did not raise offered load (submitted 270-314/s vs 268-321/s at 256)
and gave lower cap-400 peaks: the host is CPU-saturated (3 nodes ~4.5 cores
each + bench ~1.2 on 18 vCPUs, load1 ~36-40).

## Pooled verdict (6 cells per cap)

| | cap 200 | cap 400 |
| --- | --- | --- |
| total matched orders | mean 24.6M | mean 28.0M (+14%) |
| matched/s, bench window | 65.2k | 67.1k (+3%, noise) |
| matched/s incl. drain | 62.5k | 66.2k |
| expired actions / node | 2.3-9.3k | 0-1.7k |
| ms / block | 579-737 | 747-896 (~+20%) |

- Every cap-400 cell matched more orders in total than every cap-200 cell.
- At cap 200 blocks are full and 2-9% of submitted actions expire; at cap 400
  blocks are 55-70% full and almost nothing expires.
- The bench-window average barely moves because offered load (~300/s) is the
  ceiling and more of a cap-400 cell's work finishes after the window.
  Score cap and load changes on total matched or incl-drain, not only the
  window average.

## Caveats and next

- Loopback only. A full cap-400 block is ~7.6 MB, just under the 8 MB
  direct-push floor (above it: hash manifest + pull). Check over WAN before
  deploying to the testnet.
- 512 connections need file descriptors alongside libp2p and RocksDB: check
  the testnet hosts' open-file limit.
- The single-box rig is now the ceiling (~65-70k window, 110k best 60 s).
  Measuring the chain's own ceiling needs nodes and bench on separate hosts.
