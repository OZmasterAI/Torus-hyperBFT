# Read precompile gas microbench (ozarchy, 2026-10-07)

Dir: `read-precompile-gas`. Recorded in the docs: yes. The write-up is `docs/perf/read-precompile-gas.md` from `bench/read-precompile-gas` (`0e9d918b`, "recommend 125 gas per unit (was 50)"),
merged to main as `00bf7346` (s100); main's `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` (Open list) and `docs/plans/item6-phase1-impl.md` refer to it. That doc names `rep{1,2,3}.log` and `summary.txt` in this dir.
The dir `read-precompile-gas-ac` was in use when this archive was made and is not covered.

## Purpose and setup

Size the 50-gas `GAS_PRECOMPILE_READ_PER_UNIT` placeholder for the reader precompiles 0x0800-0x0803 (branch doc, title: "Read precompile gas per unit (sizing the 50-gas placeholder)").
Bench `crates/torus-bridge/tests/ubench_read_precompile_gas.rs` on `bench/read-precompile-gas` (base `98f035ee`). Binary `ubench_read_precompile_gas.bin`, sha256 `c3b0e2d3bb4023ae208781918dc53b9fde8c18c13b87a117032eba900b256cec` (`bin.sha256`).
`campaign.sh`: three repetitions, each started when the 1-min load is below 1.5, `"$R/ubench_read_precompile_gas.bin" --ignored --nocapture --test-threads=1 > rep$rep.log`. `campaign.log`: rep1 19:36:53 load 1.00, rep2 19:37:18, rep3 19:37:43, all `rc=0`.
`try1-campaign.log` / `try1-rep1.log`: an earlier launch, `rep=1 rc=101` at 19:36:08 (failed after 18 s). `build.done` is `rc=0`, `campaign.done` `exit=0`, `tests.done` `nextest rc=0` / `doc rc=0` (logs `nextest.log`, `doc.log`).

## Key results

The branch doc's recommendation (quoted): "**Recommendation: `GAS_PRECOMPILE_READ_PER_UNIT` = 125.**" The fit table below is the head of `summary.txt` (median of 3 per point; ns = fixed + slope*N), copied unchanged:

```
== FIT (median of 3 per point; ns = fixed + slope*N; ns/unit = slope / units-per-N)
scen       case                       state  fixed_ns     ns/N   u/N  ns/unit ns/gas@50 asym  worst ns/gas    @N spread%
P          getOpenOrders              mem        1917    349.5  5.00     69.9          1.398         1.391  1024     5.7
P          getStakingInfo             mem        2236    323.8  1.00    323.8          6.476         6.185  1024     4.0
P          getOpenOrders-tombstones   mem        2370     86.1  0.00      nan            nan        30.157  1024     7.5
P          getPosition                mem        2000        -     -        -              -         0.702     1     4.0
P          getBalances                mem        1680        -     -        -              -         0.600     1     3.6
P          getPrice                   mem         860        -     -        -              -         0.313     1     2.3
P          getOpenOrders              cold      17379    490.7  5.00     98.1          1.963         4.344    16     3.5
P          getStakingInfo             cold      19388    513.0  1.00    513.0         10.261        10.079  1024     4.7
P          getOpenOrders-tombstones   cold      17830    100.6  0.00      nan            nan        40.114  1024    11.0
P          getPosition                cold      20506        -     -        -              -         7.195     1     1.1
P          getBalances                cold      18445        -     -        -              -         6.588     1     0.8
P          getPrice                   cold       2030        -     -        -              -         0.738     1     2.0
P          getOpenOrders              warm       3649    413.0  5.00     82.6          1.652         1.647  1024     3.3
P          getStakingInfo             warm       6526    382.5  1.00    382.5          7.650         7.374  1024     2.5
P          getOpenOrders-tombstones   warm       4017     78.7  0.00      nan            nan        28.124  1024     4.5
P          getPosition                warm       4150        -     -        -              -         1.456     1     3.6
P          getBalances                warm       3900        -     -        -              -         1.393     1     1.8
P          getPrice                   warm       2010        -     -        -              -         0.731     1     1.5
B-classic  getOrderBook               mem        1609      6.6  3.03      2.2          0.043         0.620     0     8.3
B-classic  getOrderBook               cold       3962     24.8  3.03      8.2          0.164         1.362     1     5.8
B-classic  getOrderBook               warm       3724      2.4  3.03      0.8          0.016         1.297     1     8.4
B-rows1    getOrderBook               mem        2038    360.4  1.00    360.2          7.203         6.829  1024     4.2
B-rows1    getOrderBook               cold       4799    469.4  1.00    469.1          9.381         8.917  1024     9.1
B-rows1    getOrderBook               warm       3845    417.5  1.00    417.2          8.345         7.940  1024     3.3
B-rows2    getOrderBook               mem        2222    350.6  3.00    116.8          2.337         2.304  1024     2.6
B-rows2    getOrderBook               cold       4565    438.9  3.00    146.3          2.925         2.897   256     4.5
B-rows2    getOrderBook               warm       4316    407.3  3.00    135.7          2.715         2.687  1024     3.6
G          getMarkets                 mem         957    313.0  3.00    104.3          2.087         2.057  1024     3.7
G          getAllPrices               mem        1102    323.9  4.00     81.0          1.619         1.603  1024     3.6
G          getValidators              mem        1050    366.0  4.00     91.5          1.830         1.811  1024     3.1
G          getMarkets                 cold      39171    421.3  3.00    140.4          2.809        15.221     1    31.2
G          getAllPrices               cold      12927    410.5  4.00    102.6          2.053         4.465     1    15.3
G          getValidators              cold      10630    495.7  4.00    123.9          2.479         3.945     1    10.4
G          getMarkets                 warm       1777    373.7  3.00    124.6          2.491         2.460  1024     4.8
G          getAllPrices               warm       2103    379.6  4.00     94.9          1.898         1.881  1024     3.9
G          getValidators              warm       1503    439.6  4.00    109.9          2.198         2.177  1024     4.2
```

Remaining sections of `summary.txt`: "ROWS at max N and N=0/1" (per case and state) and "E2E (30M-gas tx, median of 3 reps x 3 inner reps)".

## Files

* `bin.sha256` (148 B)
* `build.done` (5 B)
* `build.log` (12 KiB)
* `campaign.done` (7 B)
* `campaign.log` (357 B)
* `campaign.sh` (548 B)
* `doc.log` (2 KiB)
* `nextest.log` (1 KiB)
* `rep1.log` (29 KiB)
* `rep2.log` (29 KiB)
* `rep3.log` (29 KiB)
* `smoke.log` (16 KiB)
* `summary.txt` (18 KiB)
* `tests.done` (22 B)
* `try1-campaign.done` (9 B)
* `try1-campaign.log` (121 B)
* `try1-rep1.log` (13 KiB)
* `ubench_read_precompile_gas.bin` (426.8 MiB)

Only `ubench_read_precompile_gas.bin` is heavy (see `DELETABLE.md`); `tmp/` is an empty dir.
