# Configuration Reference

## CLI Flags

### Node Operation

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--genesis <path>` | PathBuf | none | Path to genesis.json. Required on first run to initialize the database. Ignored if the database already contains accounts, except for the chain parameters it carries (e.g. `consensus.book_mode`, checked against the data dir's marker). Without it, a restart takes the book mode from the data dir's `__book_mode__` marker. |
| `--data-dir <path>` | PathBuf | `./data` | RocksDB data directory. Created if it doesn't exist. |
| `--log-level <level>` | String | `info` | Log verbosity. One of: `trace`, `debug`, `info`, `warn`, `error`. Sets the `RUST_LOG` environment variable. |

### Identity

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--keystore <path>` | PathBuf | none | Path to encrypted keystore file containing the Ed25519 validator key. |
| `--passphrase-file <path>` | PathBuf | none | Path to a file containing the keystore passphrase (for unattended startup). File should have restrictive permissions. |
| `--validator-key <hex>` | String | none | **DEPRECATED**. Raw Ed25519 signing key as 64-character hex. Visible in process listings. Use `--keystore` instead. |
| `--state-hash-attest-key <path>` | PathBuf | none | Running state hash (genesis `consensus.state_hash_activation_height` set): file holding the validator's **account** key, the secp256k1 key of the address the validator is registered under (64 hex chars, optional `0x`; mode 0600 or stricter, else the node refuses to start). Not the Ed25519 consensus key. Set: the node signs and submits one `AttestStateHash` per durable checkpoint (every 100 blocks), nonce = now in ms, and re-signs a vote that is not on chain after 65 s (60 s nonce window + 5 s skew). It submits only while that address is an Active validator, silently otherwise; the startup line `running state hash: automatic AttestStateHash enabled addr=...` shows the address. Unset: never attests (still hashes, checks votes and the quorum hash, and can fail-stop with `TORUS_STATE_HASH_FAILSTOP=1`), and its stake counts toward no quorum. A hash-unverified node (`torus_state_hash_unverified` = 1: no valid running-hash chain in its DB) never attests, even with the flag. Must not be the price feeder's key: the feeder uses a separate oracle signer (`SetOracleSigner`). Native replay protection is per (sender, nonce), so a same-millisecond action from the same account drops the attestation until the 65 s resubmit. Details: [validator guide](validator-guide.md#state-hash-attestations---state-hash-attest-key). |

### Network

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--p2p-listen <multiaddr>` | String | `/ip4/0.0.0.0/udp/30333/quic-v1` | libp2p listen multiaddress. Uses QUIC transport over UDP. |
| `--p2p-peers <addrs>` | String | none | Comma-separated bootstrap peer multiaddresses. Each must include a `/p2p/<peer_id>` suffix. |
| `--rpc-addr <addr>` | String | `0.0.0.0:8545` | JSON-RPC HTTP + WebSocket listen address. |

### State Management

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--restore-from-snapshot <path>` | PathBuf | none | Restore database from a snapshot directory before starting. The snapshot must have been created by the node's snapshot system. |
| `--archive` | bool | false | Archive node: retains all historical data (no state pruner, no block-tree pruner, no DA body GC). Config file key `archive`. Cannot be combined with `--retention-blocks`. Run at least one archive **validator** per network: block sync is served by validators only (an `--rpc-only` archive node serves no block sync). |
| `--retention-blocks <N>` | u64 | 100000 | History retention, **on by default since s131** (prune by default, owner s131): keep the last N blocks of historical block bodies, receipts and DA bodies; older data is removed to save disk space. The state pruner keeps N blocks, the block-tree pruner max(N, 1000) heights, the RH4 DA body GC max(N, 1000) + 128 dispatched blocks. `0`: pruning off (the same as `--archive`). Config file key `retention-blocks`. Cannot be combined with `--archive`. Node-local. See `pruning-guide.md`. |
| `--trade-retention-blocks <N>` | u64 | none | Trade-history retention (D13): keep only the trade rows (`cf_native_trades`, `cf_native_user_trades`) of the last N blocks. Unset = keep all trade history. Independent of `--retention-blocks`; never prunes at or above the durable applied height. Config file key `trade-retention-blocks`. Cannot be combined with `--archive`. Node-local (no consensus or state-hash effect). See `pruning-guide.md`. |

**Default behavior** (since s131): if neither `--archive` nor `--retention-blocks` is specified, the node keeps the last 100,000 blocks (validators and `--rpc-only` nodes alike). Archive nodes need `--archive` (or `--retention-blocks 0`); run at least one archive **validator** per network: it is the only block-sync source for a new node or for a validator offline longer than the window (sync servers are validators only, there is no state sync; a node below every peer's retained range logs `requested height H is below every peer's retained range — needs an archive validator peer`).

**Precedence** (s131): the command line wins over the config file for history retention as a whole: `--archive` or `--retention-blocks` on the command line decides it and both file keys (`archive`, `retention-blocks`) are ignored. Without either flag the file decides; `archive = true` with `retention-blocks = N` in the file is a startup error naming both keys. The config file rejects unknown keys (a misspelled key is a startup error, not a silent default).

**Upgrading an archive data dir** (s131): a start with the default retention on a data dir that was never pruned, never ran with retention on, and whose tip is past 100,000 refuses to start (`refusing to start: data dir ... holds unpruned history ...`). Restart with `--archive` to keep every block, or `--retention-blocks N` to prune to N (old DA bodies of such a dir are never reclaimed). See `pruning-guide.md`, "Upgrading an archive node".

### Subcommands

| Command | Description |
|---------|-------------|
| `keygen --output <path>` | Generate a new Ed25519 keypair and write an encrypted keystore file. Default output: `./validator.keystore`. |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Overridden by `--log-level`. Controls tracing filter. Examples: `info`, `torus_consensus=debug,info`, `trace`. |
| `TORUS_RPC_MAX_SCAN_ROWS` | The maximum number of rows one public read RPC may read before it stops with `-32005` (R24). Default `250000`. An unset, empty, unparsable or `0` value uses the default. Read once at startup. Applies to `torus_getOpenInterest`, `torus_getUserLimits`, `torus_getOpenOrders`, `torus_getOrderByCloid`, `torus_getOrderBook`, `torus_getMarkPrice`, `torus_getBalances`, `torus_getLiquidatorVault`, `torus_getPosition` (cross), `torus_getLeader`, `torus_getProposal`, `torus_getProposals`, `torus_getUserTrades`, `torus_getTradeHistory`, `torus_getTradeHistoryRange`, `torus_getMarkets` and `torus_getBlockTrades`. Classic book blobs, packed user-trade rows, market trade rows and proposal rows count one row per started 128 bytes. Under the limit every method answers as before. Lower it on public RPC nodes. See `docs/api/read-limits.md` (every read method and what bounds it). |
| `TORUS_ALL_MARKET_TRADES` | Allows all-markets `newTrades` WebSocket subscriptions (no `marketId`). Unset: allowed only on `--rpc-only` nodes, rejected with `-32602` on validators. `1`: allowed. `0`: rejected. Any other value logs a warning and uses the default. Read once at startup. Per-market `newTrades` and `userFills` are always allowed. See `docs/api/streams.md`. |
| `TORUS_BOOK_ROWS` | **Deprecated (s131): the order-book storage mode is the genesis field `consensus.book_mode`** (see `book_mode` under ChainConfig Fields). Unset or empty: the genesis mode. Set to the genesis mode's number (`0` classic, `1` order rows, `2` level authority, `3` level authority chunked): accepted with a deprecation warning. Any other value, including a mode the genesis does not name, refuses to start, naming both settings. Before s131 this variable selected the mode on each node (unset = Classic), which could not stop a mixed fleet on a fresh DB. |
| `TORUS_CHECKPOINT_INTERVAL_MS` | Item 6 Phase 3 coalesced state checkpoints: execution time (milliseconds) between checkpoints; executed state reaches disk once per interval and at every 100th block, barrier and shutdown, not every block. Unset or empty: the role default, 15000 (15 s) on validators, 0 on `--rpc-only` nodes (step 6). `0`: per-block flush. Not a whole number: a warning and the per-block flush. With a deferred book mode (genesis `consensus.book_mode` = `level_authority` or `level_authority_chunked`, modes 2 / 3) an explicit value above 0 refuses to start, and the unset default is off (per-block flush, a warning). Needs the exec pipeline (`TORUS_EXEC_PIPELINE` not `0`). Node-local. Validator RPC reads other than the submit checks lag by up to one interval (`docs/validator-home-network.md`). Read once at startup. |
| `TORUS_CHECKPOINT_MAX_BYTES` | Item 6 Phase 3: also checkpoint when the since-checkpoint layer reaches this many bytes. Unset, `0` or not a number (warned): no byte cap. Only with checkpoints on. |
| `TORUS_BOOK_COALESCE` | 8k L3.1 checkpoint-coalesced Classic book save, node-local, **on by default since s131 where its preconditions hold** (perf doc section 110, bench 8y, deep 1M-resting book, n = 2: matched/s 1.160 [1.077, 1.249], exec-thread CPU per 1k fills 0.802 [0.760, 0.847], CPU-s per 1M fills 0.947, blocks/s 3.42 -> 15.76, p50 193 -> 13 ms, p99 1,461 -> 672 ms, timeouts 2.91 -> 0.42 per 1k, `save_order_books` 52.2 -> 0.64 ms per block; standard exec-thread CPU 0.958 [0.936, 0.980]; window close at 1M resting: mean 61 ms, p99 ~126 ms). Unset, empty, `1` or `true`: requested. `0` or `false`: off (the kill switch; one info line at start, `L3.1 coalesced Classic book save OFF: TORUS_BOOK_COALESCE is 0 / false ...`). Any other value: off, with a warning. When on, pipelined blocks skip the whole-book encode of each dirty book, and every checkpoint (interval, attest height, epoch boundary, barrier, shutdown) encodes each book its window dirtied once, from the resident books. Same DB at every checkpoint height as the per-block save. Runs only with a checkpoint interval and no `TORUS_CHECKPOINT_MAX_BYTES` (the cap cannot count the coalesced blobs), Classic books with `TORUS_RESIDENT_BOOKS` and `TORUS_RESIDENT_ADVANCE_UNTOUCHED` not `0`, and the running state hash neither scheduled in genesis (`consensus.activations.running_state_hash` or its alias `consensus.state_hash_activation_height`) nor already turned on by the committed headers (P0.0 phase 3: the hash starts at the first header carrying the `RunningStateHash` bit; a header turning it on while L3.1 is on fail-stops), unless the schedule also starts L6.1 no later than the hash (`activations.book_window_commit` or the bool `consensus.book_window_commit`; L6.1 takes book bytes out of every non-attest `h_n`, `docs/plans/l61-window-committed-books.md`; a header carrying the hash bit without the L6.1 bit then fail-stops); when requested and any of these fails it logs ONE warning at start, `L3.1 coalesced Classic book save OFF: <reason>` (the first failing check, in this order: checkpoint interval, byte cap, running hash, book settings), and stays off (so `--rpc-only` nodes, `TORUS_EXEC_PIPELINE=0`, `TORUS_BOOK_ROWS=1`/`2`/`3`, a byte cap or a hash-on genesis run the per-block save). When on, the node logs `L3.1 coalesced Classic book save ON` once at start; every start logs exactly one of the ON / OFF lines (bench harnesses grep `book save ON` / `book save OFF`). A resident-book rebuild while a window owes books fail-stops (restart replays from the checkpoint). Metrics: `torus_exec_book_coalesce_skipped`, `torus_exec_book_coalesce_close_seconds`, `torus_exec_book_coalesce_close_books`. |
| `TORUS_EPOCH_FAST_PATH` | s124 epoch-boundary fast path, **on by default since s128** (bench section 84: block p50 57 -> 15 ms, matched/s within noise). With it off, every epoch-boundary block (height a multiple of the epoch length) first drains the exec pipeline (a forced checkpoint of everything not yet on disk, then a wait for the flush worker) and then runs and writes its state on the execution thread. With it on, a boundary block with no EVM tx and no buffered slash skips that drain and takes the pipelined path like any other block. With checkpoints on (`TORUS_CHECKPOINT_INTERVAL_MS`) such a boundary is also checkpointed at its own height, so the next epoch's validator-set plan it computes reaches disk without waiting for later blocks (consensus reads that plan only from disk). A boundary with an EVM tx or a slash still drains first. State, roots and the running state hash are identical either way; only timing changes. Unset, empty, `1` or `true`: on. `0` or `false`: off (turn it off this way). Any other value: off, with a warning (a typo never silently enables a consensus-timing change). Needs the exec pipeline (`TORUS_EXEC_PIPELINE` not `0`); without it the setting has no effect (logged at startup; a warning when the variable is set explicitly). Node-local. Read once at startup; when on, the node logs `epoch fast path ON` at startup and boundary blocks log `pipelined=true` in `executing finalized block`. |
| `TORUS_EVM_FAST_PATH` | Item 7 option 0, the EVM fast path, **on by default since s131** (bench Q11, perf doc section 107: at EVM load exec-thread CPU per 1k fills 0.981, matched/s 1.079, blocks/s 3.53x, block-time p99 0.496x vs off; no EVM load: within noise). With it off, every block with EVM txs first drains the exec pipeline (a forced checkpoint of everything not yet on disk, then a wait for the flush worker) and then runs on the execution thread, which writes its EVM state, receipts and indices itself (Q5: about 0.5 s of exec idle per EVM block). With it on, such a block takes the pipelined path like a native block: its EVM part reads through the block's layered state, its EVM rows ride the block's pending set, and the flush worker does its EVM trie step and writes its receipts, tx index, block hash index and CoreWriter links in the block's batch (interval 0) or the checkpoint's. A buffered slash, a header or body not yet on disk, or an epoch boundary with `TORUS_EPOCH_FAST_PATH=0` still drain first. State, roots, receipts and the running state hash are identical either way; only timing changes, so validators may run mixed settings. With checkpoints on, EVM state and receipts reach the RPC with the block's checkpoint (see [EVM fast path](#evm-fast-path-item-7-option-0) for the RPC freshness rule). The mempool's commit prune then reads the published ingress view instead of the disk. Unset, empty, `1` or `true`: on. `0` or `false`: off (turn it off this way). Any other value: off, with a warning (a typo never silently enables a consensus-timing change). Needs the exec pipeline (`TORUS_EXEC_PIPELINE` not `0`); without it the setting has no effect (logged at startup; a warning when the variable is set explicitly). Node-local; turning it off needs no data change. Read once at startup; when on, the node logs `EVM fast path ON` at startup and EVM blocks log `pipelined=true` in `executing finalized block`. |
| `TORUS_PARALLEL_CANCEL_ALL` | s123 parallel cancel-all book pass, **on by default since s128 (8 workers)**. A batched cancel-all run's per-market book work (stops, `cancel_all_many`, the cancelled orders' margin) runs as up to N jobs on the exec pool, one job group per market, once the run has at least `TORUS_PARALLEL_CANCEL_ALL_MIN_VISITS` (default 64) (market, action) visits. Unset, empty or whitespace: 8 workers. `N` >= 2: N workers (capped at 64). `0` or `1`: the serial loop (turn it off this way). Any other value (not a whole number): the serial loop, so a typo never turns the pool on. 8 is the benched value (sections 82-84, 300 markets); tune it per validator core count. Node-local: results, state and roots are byte-identical for any value. Read once at startup. |
| `TORUS_PARALLEL_CANCEL_ALL_MIN_VISITS` | Work gate of `TORUS_PARALLEL_CANCEL_ALL`: a cancel-all run with fewer (market, action) visits keeps the serial loop. Default 64; unset or not a number: 64; `0`: parallel whenever the run touches 2 or more markets. Node-local, byte-identical either way. Read once at startup. |
| `TORUS_HEADER_RESEND_MS` | s75 fix D, stuck-leader header re-send: a leader still in its own view one interval after proposing re-sends its proposal header directly to every other validator, then every interval. Default **250 ms since s128** (was 3000 ms; bench section 77: 33-47 lost-proposal timeouts per 300 s cell at 3 s vs 3 at 250 ms). `0`: off (turn it off this way). Any other whole number: that many milliseconds. Unset, empty or not a number: the default. `devnet/wsl/env.sh` and `devnet/docker-compose.yml` also export 250. Read once at startup. |
| `TORUS_PRUNE_MODE` | s132, how the state pruner (`--retention-blocks`) removes old block bodies, receipts, action status records and CoreWriter tx links. `filter` (default; unset, empty or any value other than `range`, unknown values warned): no deletes; the pruner persists the cutoff, reads hide rows below it, whole SST files below it are dropped (`DeleteFilesInRange`) and a compaction filter drops the rest when RocksDB compacts anyway. `range`: the eager `delete_range` per batch (before s132; perf doc 103.6: its range tombstones double `cf_block_bodies` compaction). Reads answer the same in both modes; a data dir switches freely across restarts. No effect on an archive node (`--archive` or `--retention-blocks 0`). Node-local. Read once at startup. Metrics: `torus_pruner_files_dropped`, `torus_pruner_filter_rows_dropped`, `torus_pruner_filter_cutoff`. See `docs/pruning-guide.md`. |
| `TORUS_DA_BODY_RETENTION` | RH4 DA body GC window in dispatched blocks: committed native-action bodies (and their shards) are deleted this many blocks after commit, and only once the block tree pruned their block 128 heights ago. Unset, empty or not a number: max(N, 1000) + 128 from `--retention-blocks` N (100,128 with the default N = 100,000). `0`: GC off (every body kept). Other whole numbers: that window (floor 128); below the block-tree window a peer lagging further cannot get bodies from this node. Ignored on an archive node (`--archive` or `--retention-blocks 0`: no GC). Node-local. Read once at startup. |
| `TORUS_DA_GC_INLINE` | `1`: the RH4 DA body GC pass runs on the consensus thread inside the commit dispatch (before the s131 GC worker; perf doc section 103 inline arm). Anything else, unset included: the `torus-da-gc` worker thread (default). Node-local. Read once at startup. |
| `TORUS_DA_GC_SLOW_MS` | A DA body GC pass at least this long logs one warn line with its phases and counts `torus_da_gc_slow_passes`. Default 50; not a number: the default. Read once at startup. |
| `TORUS_ADMISSION_EXEC_AWARE` | Exec-aware native admission, opt-in. RPC ingress sheds non-cancel native actions with a retryable "busy" before signature checks once the pooled normal actions reach the admission limit (recent 10 s commit rate x `TORUS_ADMISSION_HORIZON_MS`, default 20000, never below 4 full blocks). With this on, the normal actions of committed blocks that were handed to execution but have not executed yet (they left the pool at dispatch; up to 64 queued blocks plus parked ones) count toward that limit too, so ingress also sheds while execution lags. Cancels and oracle submissions are never counted and never shed by this check. `1` or `true`: on. Unset, empty, `0` or `false`: off (admission exactly as before). Any other value: off, with a warning. No effect when `TORUS_ADMISSION_HORIZON_MS=0` (admission limit off). Node-local. Read once at startup. Gauge: `torus_mempool_native_committed_unexecuted` (always 0 when off). |
| `TORUS_BOOK_FAST_ENCODE` | 8k L3.3 exact-size Classic book encoder, **on by default since s131** (merged off s131, default on after bench 8u). On: each dirty book's whole-book blob is written by `OrderBook::encode_classic` (one allocation sized from the book's counts) instead of `borsh::to_vec`; the bytes are identical (proptest + state-equality test), so it is node-local and can differ per validator. `0` or `false` (any case): off (turn it off this way). Unset, empty, `1` or `true`: on; anything else: a warning and on. Read once at startup. Perf doc section 97.8: `save_books` -15% (standard load and 1M-resting deep book), exec on-CPU -3.2% on the deep book. Section 105 (bench 8u, one binary, 1M-resting deep book, n = 1, B/A): native blocks/s 1.086, block p50 0.884, p99 0.919, CPU-s per 1M fills 0.989, matched/s 1.020, exec on-CPU per 1k fills 0.973, `save_books` per 1k fills 0.869; the earlier exec-engine timer rise is not a scheduling artefact (+2.0% node-consistent, mostly the liquidation step, not resolved, likely code layout). **Only affects the Classic book mode (genesis `consensus.book_mode` absent or `classic`).** Modes 1, 2 and 3 never call the Classic encoder, so the flag does nothing there. If the testnet genesis picks mode 3 (pending bench 8p and the owner's book-mode decision; design note `docs/plans/deep-book-save-options.md` on branch `docs/deep-book-save-options`), this code is dead and should be removed in a cleanup. |
| `TORUS_SETTLE_NO_MERGE` | 8k L2.2 no-merge position flush, **on by default since s131** (merged off s131, default on after bench 8t). On: the parallel settle's pass B keeps each market's position cache as a separate run instead of `merge_disjoint`-ing it into the batch cache, and the end-of-call flush writes the batch cache and the runs together in the same sorted key order (`PositionCache::flush_with_runs`): the same writes in the same order, without moving every entry into one map. Byte-identical (state-equality test over the P2 blocks: write sets, running hash, every CF, native root), so it is node-local and can differ per validator. If the runs are not provably disjoint (aliased rows across overlapping market caches), that flush falls back to the exact flag-off path (merge in order + `flush_all`) and counts `torus_exec_settle_no_merge_fallbacks_total`; it should stay 0. `0` or `false` (any case): off (turn it off this way). Unset, empty, `1` or `true`: on; anything else: a warning and on. Read once at startup. Perf doc section 101 (bench 8q, 1M-resting deep book): exec on-CPU per 1k fills 0.972 [0.963, 0.982], matched/s 1.022 [1.010, 1.033], 0 fallbacks. Section 108 (bench 8t, one binary, B/A): standard shape, n = 3: exec-thread on-CPU per 1k fills 0.957 [0.936, 0.978], native blocks/s 1.095 [1.080, 1.111] (14.23 -> 15.58), block p99 0.877 [0.832, 0.924], matched/s 0.998 (load-bound), timeouts 0 vs 0; deep book, 900 s cells, n = 2: timeouts 27 vs 28 (pooled with 8n and 8q 46 vs 48, so the 8q rise was noise), exec on-CPU 0.977, blocks/s 1.018; fallbacks 0 on every node of every cell. |
| `TORUS_SETTLE_STREAMED` | 8k L2.1 streamed in-order settle, **off by default** (branch `perf/l21-streamed-settle`, merged in batch 10 by owner exception (perf doc sections 123/124: the 9j winner CL21 ran it on); section 114, bench 9b: blocks/s deep 1.206, std 1.103, but exec-thread on-CPU per 1k fills deep 1.044, which fails the exec-path merge rule). `1` or `true` (any case): when the per-market match setup runs in the pool jobs (`TORUS_PARALLEL_MATCH_SETUP`, on by default, >= 2 markets) and the parallel settle can engage, each market's pool job matches the market and then computes its settle plan (pass A), and the exec thread runs settle pass B market by market in market-id order as each job finishes, while the pool still works on later markets (instead of waiting for all matches, then all plans, then running pass B). Jobs take markets in id order from a shared cursor (worker cap: `TORUS_MATCH_WORKERS`). Byte-identical with `TORUS_SETTLE_NO_MERGE` off and on (tests: `settle_streamed_tests.rs`, the app-level P2 state-equality test: write sets, running hash, every CF, native root), so it is node-local and can differ per validator. In Auto mode pass B starts once the fills seen so far pass the parallel-settle gate; plans of calls that end below the gate are discarded (pool CPU only). If a plan fails (panic or fill error) or holds another market's key, pass B stops before that market and, on a failure, the sequential loop resumes there (`torus_exec_settle_streamed_resumed_fallbacks_total`, should stay 0). Overlap counters: `torus_exec_settle_streamed_markets_total`, `torus_exec_settle_streamed_early_markets_total`. Wake-ups: an exec thread waiting for a market's job parks until that market AND the market `batch - 1` later are both ready, `batch = min(8, max(1, markets / 8))`, so it wakes about once per run of `batch` ready markets instead of once per market (scheduling only; `torus_exec_settle_streamed_parks_total` counts the parks). Unset, empty, `0` or `false`: off; anything else: a warning and off. Read once at startup. |

## Port Assignments

| Port | Protocol | Service | Configurable Via |
|------|----------|---------|-----------------|
| 8545 | TCP | JSON-RPC | `--rpc-addr` |
| 30333 | UDP | P2P (QUIC) | `--p2p-listen` |
| 9090 | TCP | Telemetry (Prometheus) | Hardcoded |

## ChainConfig Fields

Set via genesis.json. Cannot be changed after genesis initialization, except the four EVM block
values (`evm_gas_limit`, `evm_block_interval`, `evm_big_block_interval`, `evm_big_gas_limit`),
which governance can change (see the two "EVM blocks" sections below).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `chain_id` | u64 | 7777 | EVM chain ID |
| `chain_name` | string | `"torus"` | Human-readable chain name |
| `evm_gas_limit` | u64 | 3,000,000 | EVM gas budget per EVM block: the genesis header's `evm_gas_limit`. Genesis key `evm.gas_limit` (required). |
| `evm_block_interval` | u64 | 11 | N: EVM transactions only in blocks with `height % N == 0`. The genesis header's `evm_block_interval`. Genesis key `evm.block_interval` (absent = 11; 0 is refused; 1 = EVM in every block). |
| `evm_big_block_interval` | u64 | 0 | Item 7 step 2, M: with M > 0, a block with `height % M == 0` is a big EVM block (0 = big blocks off). The genesis header's `evm_big_block_interval`. Genesis key `evm.big_block_interval` (absent = 0). Not range-checked at genesis; governance takes only 0 or 30..=10,000. |
| `evm_big_gas_limit` | u64 | 10,000,000 | Item 7 step 2: the big lane's gas limit, added to `evm_gas_limit` at a big height. The genesis header's `evm_big_gas_limit`. Genesis key `evm.big_gas_limit` (absent = 10,000,000). Not range-checked at genesis; governance takes 1,000,000..=30,000,000. |
| `base_fee_per_gas` | u64 | 1,000,000,000 | Initial base fee (1 gwei). Item 7 step 2: the genesis parent header carries 1 gwei for both lanes (`base_fee_per_gas`, `big_base_fee_per_gas`); from there each lane's fee follows F3 (see "EVM blocks (item 7 step 2)"). |
| `epoch_length` | u64 | 100,000 | Blocks per validator epoch |
| `max_validators` | u32 | 21 | Maximum active validator set size |
| `min_stake` | U256 | 10,000 TRS | Minimum self-delegation to register |
| `fee_burn_bps` | u16 | 1000 | Fee burn percentage (basis points) |
| `fee_validator_bps` | u16 | 0 | Fee to block proposer (basis points) |
| `fee_treasury_bps` | u16 | 4500 | Fee to treasury (basis points) |
| `fee_dev_pool_bps` | u16 | 4500 | Fee to dev pool (basis points) |
| `state_hash_activation_height` | u64 or none | 1 (owner s136) | First height of the running state hash: the P0.0 feature `running_state_hash` (`ChainConfig::state_hash_activation_height()`). Genesis `consensus.activations.running_state_hash`, or the alias `consensus.state_hash_activation_height` (0 is refused there). **On at genesis by default (owner s136):** a genesis naming no activation (no `consensus.activations`, no alias) runs it from block 1, with L6.1 `book_window_commit` when Classic (`torus_types::DEFAULT_GENESIS_ACTIVATIONS`; a node started without `--genesis` too); `"activations": {}` is the explicit opt-out (`gen-3val-genesis.sh`: `STATE_HASH_ACTIVATION=off`; unset = the default). |
| `book_mode` | string | `classic` | Order-book storage mode (owner s131), chain-wide and fixed for the chain's life. Genesis key `consensus.book_mode`: `"classic"`, `"order_rows"`, `"level_authority"`, `"level_authority_chunked"`, or the numbers 0-3; absent = `classic`; anything else refuses the genesis. Every node runs this mode: `TORUS_BOOK_ROWS` may only repeat it (deprecated), and a data dir whose `__book_mode__` marker names another mode refuses to start. Mode 3 = `"level_authority_chunked"`. `"classic"` and an absent field are the same chain (generators leave it out). Upgrading a pre-s131 data dir: a node that ran with `TORUS_BOOK_ROWS=1`-`3` refuses to start until the genesis file names that mode (see the upgrade table in `docs/node-operator-guide.md`, Order-book storage mode). |
| `book_window_commit` | P0.0 feature (id 1) | 1 with the default hash (Classic genesis naming no activation, batch 10) | L6.1 window-committed Classic order books, **consensus-changing** (state transition and running state hash), chain-wide. The P0.0 feature `book_window_commit` (`ChainConfig::book_window_activation()`): genesis `consensus.activations.book_window_commit` (its own height), or the bool `consensus.book_window_commit: true` (its alias: from the running hash's height; what the bench harness's `BOOK_WINDOW_COMMIT=1` writes; an explicit other height next to it refuses to start). Needs `running_state_hash` in the schedule (refused otherwise) and the Classic genesis book mode (`consensus.book_mode`; a row mode refuses the genesis). Every block header carries bit 1 from its height; execution follows the header's bit, never the node's schedule. From that height the running hash skips `cf_native_order_books` except at every 100th height, where the block always runs the native phase (even when empty: governance finalizes / executes due proposals and the liquidation pass runs there), writes the blob of every book and hashes them. Allows `TORUS_BOOK_COALESCE` with the hash on when L6.1 starts no later than the hash. **Default:** a Classic genesis naming no activation schedules it from block 1 with the running hash (`DEFAULT_GENESIS_ACTIVATIONS`, what the 9j head-to-head measured as CL21); a row book mode gets the hash only; an explicit `activations` map or the hash alias alone does not include it. Design: `docs/plans/l61-window-committed-books.md`. |

## EVM blocks (item 7 step 1)

The block header carries `evm_gas_limit` (the EVM gas budget of an EVM block) and
`evm_block_interval` (N). Both start at the genesis values above. Every shipped genesis file uses
3M / 11; the CI Uniswap genesis (`devnet/uniswap/make-ci-genesis.py`) uses a 10M budget, because
the Router deploy needs about 4.2M gas, and keeps N = 11. The bench genesis generators
(`testnet/gen-weighted-genesis.sh`, `devnet/wsl/gen-3val-genesis.sh`) copy the EVM section of
`testnet/genesis-weighted-base.json`, so bench cells run at 3M / 11 too (an untracked
`testnet/genesis-weighted-full.json` generated before the change keeps N = 5:
`gen-3val-genesis.sh` reuses it until it is rebuilt with `FORCE=1`).

N = 11 is the s123 owner value for benches and testing (it was 5). It is prime, so it shares no
factor with any validator count below 11: with round-robin leaders (one view per height), every
validator leads some EVM heights. At 3.2 blocks/s an EVM block comes every ~3.4 s, which at 3M
gas is ~0.9M EVM gas/s. Retune before testnet.

A header must carry its parent's values, except at an epoch boundary, where it carries the epoch
plan's values (unset in the plan = the parent's). Validators check this before voting and at
block insertion. Execution of boundary H copies the governance values into the plan for H + L
(L = `epoch_length`), so a change reaches the header between L and 2L blocks after it executes.

| Governance key (`ParameterChange`) | Range | Header field |
|---|---|---|
| `evm_block_gas_limit` | 1,000,000..=10,000,000 | `evm_gas_limit` |
| `evm_block_interval` | 2..=100 | `evm_block_interval` |

Values outside the range are refused at submission and at execution. A stored value that is
absent or invalid leaves the chain's value unchanged.

`TORUS_EVM_BLOCK_GAS_BUDGET` (item 7 step 2, owner decision 6) is node-local and has **no
default**: unset, empty or not a whole number means no cap, and the header limits alone size
both lanes. Set, it caps only the small lane: that node fills the small lane of a block, and
admits a small-lane transaction, only up to `min(evm_gas_limit, TORUS_EVM_BLOCK_GAS_BUDGET)`. It
never caps the big lane (drain or admission). It is an A/B knob; leave it unset on validators.
With it set below `evm_gas_limit`, `eth_estimateGas` still searches up to the header limit and
can quote gas that the same node's admission refuses. (Until step 2 it defaulted to 5,000,000
and capped every EVM block.)

Per block, checked before voting (a block that breaks one gets no vote; at a big height the
caps are larger, see "EVM blocks (item 7 step 2)"):

* EVM transactions only when `height % N == 0`; an honest proposer leaves them pooled in the other
  blocks.
* At most `evm_gas_limit / 21,000` EVM transactions.
* At most 256 KiB (262,144 bytes) of raw EVM transaction bytes; an honest proposer leaves the
  transactions that would go over pooled.
* Declared gas of the EVM transactions at most `evm_gas_limit` (step 0).

Admission to the EVM pool refuses a transaction above 256 KiB (`TxTooLarge`) or with a gas
limit above the `evm_gas_limit` of the last committed header handed to execution (or
`TORUS_EVM_BLOCK_GAS_BUDGET`, if lower). When a committed header lowers that limit, pooled
transactions above it are evicted (they could never be selected). `eth_call` and
`eth_estimateGas` use the head header's `evm_gas_limit` as their default gas and clamp a larger
`gas` to it. Step 2 makes all of this per lane: a big-lane sender gets the big lane's limit, byte
cap and fee (next section).

Constants: `DEFAULT_EVM_GAS_LIMIT`, `DEFAULT_EVM_BLOCK_INTERVAL`, `MAX_EVM_BLOCK_TX_BYTES`,
`EVM_MIN_TX_GAS` and the two key names and ranges, in `crates/torus-types/src/lib.rs`.

## EVM blocks (item 7 step 2)

Step 2 adds HL-style big EVM blocks: a second, slower EVM lane with its own gas limit and base
fee, which an address opts into. Design `docs/plans/item7-step2-design.md` (owner decisions
s123), plan `docs/plans/item7-step2-impl.md` (with the owner's build decisions, s126).

### Big heights and per-height caps

The header carries three more values, all in the canonical header bytes:
`evm_big_block_interval` (M), `evm_big_gas_limit` and `big_base_fee_per_gas`. A height is
**big** when M > 0 and `height % M == 0`. EVM transactions may ride a height when
`height % N == 0` or when it is big; a big height carries both lanes. Genesis
`evm.big_block_interval` defaults to 0 (big blocks off) and `evm.big_gas_limit` to 10,000,000;
every shipped genesis file spells out 0 and 10M. Big blocks start only when governance sets M
(owner decision 1).

Per height, checked before voting (off a big height, or with M = 0, these are step 1's caps):

| Cap | Not big (`height % N == 0`) | Big height |
|---|---|---|
| Gas: declared gas of the EVM transactions, and the execution gas limit | `evm_gas_limit` | `evm_gas_limit + evm_big_gas_limit` |
| EVM transaction count | gas cap / 21,000 | gas cap / 21,000 |
| Raw EVM transaction bytes | 256 KiB | 768 KiB (786,432 bytes, owner decision 8) |

`eth_getBlockByNumber` / `eth_getBlockByHash` report the height's gas cap as `gasLimit`.

Step 1's boundary rule covers all six EVM header values (`evm_gas_limit`, N, M,
`evm_big_gas_limit`, `base_fee_per_gas`, `big_base_fee_per_gas`): a header carries its parent's
values, at an epoch boundary the epoch plan's (unset = the parent's). Validators check this
before voting and at block insertion.

### Governance keys

| Governance key (`ParameterChange`) | Range | Effect |
|---|---|---|
| `evm_big_block_interval` | 0 (off) or 30..=10,000 | Header `evm_big_block_interval` (M), through the epoch plan like N |
| `evm_big_block_gas_limit` | 1,000,000..=30,000,000 | Header `evm_big_gas_limit`, through the epoch plan |
| `core_writer_order_delay` | 1..=100 | K, the CoreWriter order delay (below); read when a row is queued, no header field |

Values outside the range are refused at submission and at execution. A stored value that is
absent or invalid leaves the chain's value unchanged (for K: the default, 10). M and the big
limit reach the header between L and 2L blocks after the vote executes, like N. At 3.2 blocks/s,
M = 30 is a big block every ~9 s; M = 190 about once a minute (HL's cadence). Setting M back to 0
switches big blocks off: the pools move the big lane's transactions to the small lane.

### Lanes and `EvmUserModify`

`NativeAction::EvmUserModify { using_big_blocks }` (HL `evmUserModify`; canonical tag 30,
EIP-712 `EvmUserModify(bool usingBigBlocks,uint64 nonce)`) sets (`true`) or clears (`false`)
the sender's lane row, `"lan" ‖ address` in `CF_NATIVE_ORACLE` (hashed state). It is not a
trading action, so a session key cannot sign it. Wallet: `torus-wallet evm-user-modify
--using-big-blocks true|false`. An address is in the **big lane** while its row exists and the
header's M > 0; otherwise in the small lane. Lanes are proposer routing and fees, never a vote
rule.

* **Admission** reads the sender's lane from the ingress view (the latest executed state) and
  the last committed header, and applies that lane's caps: small = `evm_gas_limit` (and
  `TORUS_EVM_BLOCK_GAS_BUDGET`, if set), 256 KiB, the small base fee; big =
  `evm_big_gas_limit`, 768 KiB, the big base fee. Gas above the lane's limit is
  `GasLimitExceeded`, bytes above its cap `TxTooLarge`, a max fee below its base fee
  `FeeTooLow`. A small-lane address cannot send a transaction above the small limit; it opts
  into the big lane first.
* **One lane per sender.** A new transaction of a sender that still has transactions pooled in
  its other lane is refused (`EvmLaneSwitchPending`) until they leave the pool. A transaction
  returning to the pool (this node's lost proposal, a re-proposal, the move when M goes to 0)
  joins the lane that already holds its sender's transactions, so it never leaves a nonce gap
  (owner s126).
* A pooled transaction that no lane can take after a limit drop or after M goes to 0 is dropped
  with a warning. The sender's later nonces stay pooled and wait until the missing nonce is sent
  again (owner s126, as step 1's gas-limit eviction).
* One `max_pool_size` covers both lanes: at the cap, a new transaction must outbid the lowest
  effective tip of either lane (each ranked at its own base fee).
* **Proposer:** at a big height an honest proposer drains the small lane first (small limit,
  256 KiB), then the big lane (big limit, the bytes left of 768 KiB), each at the base fee of the
  header it proposes.
* **Execution** charges each transaction the base fee of its sender's lane when the block
  executes (the row at that time, not the transaction's place in the block). A sender that
  switches lanes while a transaction waits under the old lane may pay the new lane's fee, or see
  it skipped if its max fee is below that fee.

### Fees per lane (F3)

Each lane has its own EIP-1559 base fee (`base_fee_per_gas`, `big_base_fee_per_gas`), changed
once per epoch from executed gas: headers are built before execution, so no per-block gas signal
exists at vote time. The native phase adds each EVM block's gas used, split by lane, to the
hashed `CF_CONSENSUS_META` row `evm_epoch_gas`. Execution of boundary H computes each lane's next
fee with `calc_next_block_base_fee`, counting the epoch `[H - L, H)` as one block whose gas limit
is the lane's limit times its EVM heights in that window (small lane: every EVM height, big ones
included; big lane: the big heights), and writes both fees into the plan for H + L. So a fee
moves at most +/-12.5% per epoch, reaches the headers at boundary H + L, and never drops below
`EVM_MIN_BASE_FEE` = 1 gwei (an idle chain stays at 1 gwei, today's fixed fee).

### CoreWriter delay and cap (owner decision 4)

CoreWriter order actions (0x0810 `placeOrder`, `cancelOrder`, `cancelAll`, `cancelByCloid`)
queued by block B's EVM drain in block B + K (K = `core_writer_order_delay`, default 10, about
3 s at 3.2 blocks/s), so a contract cannot jump the native mempool. An order action never
targets a block before the previous order action's target, so lowering K cannot let a cancel
drain before an earlier order. Every other kind (staking 0x0811, lockbox 0x0820) drains in
B + 1, as before. At most `CORE_WRITER_DRAIN_CAP` = 200 rows drain per block: the cap is applied
when a row is queued, and a target block that already holds 200 rows sends the row on to the
next block with room, in queue order. Rows are still written only by EVM blocks. Results:
`docs/api/corewriter-results.md`.

### Best bid / ask precompile (owner decision 3)

0x080e (HL's bbo address), `bbo(bytes32 marketId) -> (uint128 bid, uint128 ask)`: the market's
best bid and ask (FixedPoint raw; 0 for an empty side or an unknown market) as of the last block
that changed its book. **16,440 gas**: the reader base 16,400 (the point read included, as for
`getPrice`) + 2 answer words x 20 (owner s125). It reads `cf_native_bbo`, one row per market with
a non-empty book, written at block end for every market whose book changed, in every book mode
and both save paths, with M = 0 too; an emptied book deletes its row. The column family is
node-written and not hashed (derived from the hashed book rows). The call shape is ours, not
HL's (HL takes `abi.encode(uint64 asset)` and returns two `uint64`), so HL contracts need
porting. Pricing: `docs/perf/read-precompile-gas.md`.

### Routing (owner decision 7)

EVM transactions are not gossiped: the RPC node that receives one forwards it to the leaders
expected to carry it.

* **Targets:** a small-lane transaction goes to the leader of the next EVM height (a big height
  counts); a big-lane transaction to the leaders of the next two big heights.
* **Upcoming N and M:** heights from the next epoch boundary on are predicted with that
  boundary's N and M from its epoch plan, once this node can read the plan (its execution has
  applied the previous boundary); until then with the committed values.
* **In-flight filter:** on every commit, the committed block's EVM transactions leave every
  pool at once (they execute up to ~14 s later under load). Their copies are refused as
  `DuplicateTx` for 256 blocks (`EVM_INFLIGHT_KEEP_BLOCKS`), and the sender's next pool nonce
  counts them (a lower nonce is `NonceTooLow`) until its state nonce catches up or the height
  executes.
* **Re-send:** the node tracks each forwarded transaction with its target height. When the
  target commits without it and it is still pooled, it is re-sent to the next predicted
  leader(s), at most `EVM_RESEND_MAX` = 8 times (log line `EVM tx re-sent: its target EVM
  height committed without it`); after that it waits for this node's own EVM turn. The
  transactions of this node's lost proposal go back to the pool and are tracked at their lane's
  next EVM height.

### RPC

* `eth_gasPrice`: the small lane's base fee. `torus_bigBlockGasPrice`: the big lane's (HL
  `bigBlockGasPrice`), from the last committed header.
* `torus_usingBigBlocks(address)`: `true` when the address's lane row exists (its choice; the
  big lane applies only while M > 0). Read through the ingress view, so a choice shows as soon
  as it executes.
* `eth_feeHistory(blockCount, newestBlock, rewardPercentiles, blockType?)`: `blockType`
  `"small"` (default) or `"big"` selects the lane's base fees; any other value is `-32602`.
  `gasUsedRatio` stays the header's (always 0: headers are built before execution).
* `eth_call` / `eth_estimateGas` run in the caller's lane (`from`, else the zero address): its
  gas limit (the default gas, the clamp, the estimate's search bound) and its base fee. The lane
  row comes from the ingress view, as for admission, routing and `torus_usingBigBlocks` (owner
  s126, option B); the call itself runs on the DB state at the eth head.
* `eth_sendRawTransaction` refuses a transaction with bytes after it (`mempool error: tx decode
  failed: trailing bytes after the tx (N bytes)`, code -32603), so every admitted transaction
  has one byte form (owner s126). Both accepted encodings, the bare EIP-2718 bytes and alloy's
  network form, still pass. Any decode failure now reads `mempool error: tx decode failed: ...`
  (was `internal error: tx rlp decode: ...`). Blocks and execution still take trailing bytes;
  it is not a vote rule.

### Limit: `TORUS_NATIVE_TOTAL_BLOCK_CAP` <= 7,203 while big blocks are on

The worst compact proposal at a big height is 768 KiB of EVM bytes + 8 bytes of length per
transaction (1,904 transactions at the 10M + 30M governance ceilings) + 32 bytes per native
action hash + 16 KiB of header, QC and framing. At the compiled native cap (400) that is
830,864 bytes, under the 1 MiB consensus accept gate (`o5_feed_gates.rs`). A
`TORUS_NATIVE_TOTAL_BLOCK_CAP` above 7,203 would let a worst big height exceed 1 MiB: every
receiver drops the proposal and penalizes its proposer. Keep it at or below 7,203 while M > 0.

Constants: `MAX_EVM_BIG_BLOCK_TX_BYTES`, `DEFAULT_EVM_BIG_BLOCK_INTERVAL`,
`DEFAULT_EVM_BIG_GAS_LIMIT`, `EVM_MIN_BASE_FEE`, `DEFAULT_CORE_WRITER_ORDER_DELAY`,
`CORE_WRITER_DRAIN_CAP` and the three key names and ranges in `crates/torus-types/src/lib.rs`;
`EVM_INFLIGHT_KEEP_BLOCKS` and `EVM_RESEND_MAX` in `crates/torus-mempool/src/lib.rs`.

## EVM fast path (item 7 option 0)

`TORUS_EVM_FAST_PATH` (environment table above; design `docs/plans/item7-step3-design.md`, plan
`docs/plans/item7-option0-impl.md`). On by default since s131 (bench Q11 passed, perf doc section 107);
`TORUS_EVM_FAST_PATH=0` turns it off per node.

### RPC freshness (design question 4 a)

No checkpoint is forced at an EVM height. With the flag on and checkpoints on
(`TORUS_CHECKPOINT_INTERVAL_MS`, 15 s on validators), a block's EVM state, receipts, tx index and
block hash index reach the disk in the next regular checkpoint (the interval, every 100th block, the
byte cap, a barrier, shutdown), in the same atomic batch as the applied-height marker. The eth head
(`eth_blockNumber`, the `latest` tag) is that marker, so:

* the eth head, the receipts and the EVM state move together, at the checkpoint; a receipt is never
  visible before its state, and a state change never before its block;
* they lag execution by about one checkpoint interval while blocks keep executing, plus the flush
  worker's write time, as native state has since item 6 Phase 3. There is no wall-clock maximum: the
  triggers are checked only when a block executes (no idle checkpoint timer), so if execution pauses
  a receipt can stay `null` until the next trigger. A wallet polling `eth_getTransactionReceipt`
  typically sees `null` for ~15 s after its tx executes;
* `eth_getTransactionCount(address, "pending")` and mempool admission read the ingress view (the
  executed state), so senders get fresh nonces; `"latest"` follows the eth head.

With the flag off, every EVM height drains to disk before it runs, so the eth head reaches each EVM
block right after it executes (the native blocks between them still lag by up to one interval).
`--rpc-only` nodes (interval 0) write every block. Fresher reads through the view are design question
4 b, not built. Details: `docs/api/eth-head.md`.

### Metrics

`torus_exec_evm_blocks_total` (every executed block with EVM txs) and
`torus_exec_evm_fast_path_blocks_total` (those that ran pipelined): their ratio is the pipelined share at
EVM heights. `torus_exec_evm_idle_before_seconds` (per EVM block, the exec thread's time from the
previous block's end to this block's start, its drain included), `torus_exec_evm_block_seconds` (the
block wall of EVM blocks), `torus_exec_evm_seconds` (the EVM section on the exec thread, every block,
unchanged), `torus_flush_worker_evm_trie_seconds` (the flush worker's EVM trie step; never observed with
the flag off). See `docs/monitoring-setup.md`.

## Market delisting (governance)

A governance `DelistMarket` proposal makes a market reduce-only, cancels its resting orders and
stops (refunded), and starts a grace period. When the grace period ends, the settlement step closes
every remaining position at one settlement price against the liquidator vault, with no fee. Owner
decisions and the options that were not chosen: `docs/Torus-hyperBFT-roadmap.md`, P1, "Decisions
(owner s134)".

| Governance key (`ParameterChange`) | Range | Default | Effect |
|---|---|---|---|
| `delist_grace_blocks` | 0..=302,400 | 43,200 (24 h at 2 s) | N: the settlement starts at `close_at` = delist height + N. Read once, when the delist executes; a later change does not move a running grace period. 0 = close in the delist's own block. An absent or invalid stored value reads as the default. |

**Settlement price.** It is fixed once, at the first settlement block (height >= `close_at`), and
never changes after that. The first available source wins:

1. the governance close price (below), if one was set;
2. the usable oracle mark;
3. the last aggregated oracle price, of any age (no cap);
4. none: the settlement waits, and the step reports an error result every block until a price
   exists.

**Governance close price: `SetDelistClosePrice { market_id, price }`.** This is a `SubmitProposal`
action. It needs the normal vote and timelock.

- Submission: `price` > 0, and `market_id` has a market row. The market need not be delisted yet,
  so the proposal can be voted alongside the delist.
- Execution at height E: the market is delisted and its settlement price is not fixed yet. That
  covers the grace period, `close_at` itself (governance runs before the settlement step), a
  settlement still waiting for a price after `close_at` (no oracle price at all), and the delist's
  own block (the close-price proposal needs a higher proposal id than the delist). Otherwise the
  proposal ends `Failed` with nothing written: the price is already fixed (first settlement block),
  the market is already settled, or the market is not delisted. A later close-price proposal that
  executes in time replaces an earlier one.
- A settlement waiting after `close_at` takes an executed close price in the same block: the walk
  starts at it, ahead of a mark that appears in that block.
- Plan the timing. To set the price that the settlement uses when an oracle price exists, the vote
  plus the timelock (24 h by default) must end at or before `close_at`. With the default grace
  period (also 24 h), submit the close-price proposal before or together with the delist, or raise
  `delist_grace_blocks` first. Once the price is fixed, governance cannot change it.
- Not chosen (owner s134): the earlier, stricter rule "E <= `close_at` only". It left a market
  with no oracle price at all waiting forever after `close_at`, with no governance way out.

**Oracle submissions for a settled market.** A market's settlement is complete when its settlement
row is gone and the liquidator vault and both ADL escrows hold no position in it. From then on
validators ignore that market's entries in `SubmitOraclePrices`: they are not stored, the rest of
the submission is, and the action still succeeds. The block-start aggregation also skips the
market. Until then (grace period, settlement walk, ADL drain) prices are accepted and aggregated as
usual. The price feeder stops publishing a market once `torus_getMarkets` reports its settlement as
`settled` (it re-reads the list every cycle, logs a startup warning and reports the market as
omitted with reason `delist settled`); you can then remove it from the feeder's `[[markets]]`
config.

**What the node shows (node-local, no consensus effect).** RPC intake refuses an order that is not
reduce-only on a delisted market (`PlaceOrder`, or any order of a `PlaceOrderBatch`) with the
executor's text `market N is delisted: only reduce-only orders`, before signature checks; the
executor still decides in consensus. `torus_getMarkets` adds per market: `status` (`active` or
`delisted`), `delisted`, `delistHeight`, `closeAt` (the grace end; kept after the settlement),
`settlement` (`none`, `grace` before `closeAt`, `settling` from `closeAt` until complete, including
waiting for a price and the ADL drain, `settled`), `closePrice` (the fixed settlement price, from
the first settlement block on; kept after the settlement: when the walk ends, every node writes a
permanent record of the market's close price, close height and walk-end height in the same block),
`governanceClosePrice` (a `SetDelistClosePrice` price not yet used) and `settledHeight` (the height
the settlement completed, the oracle stopped; after the ADL drain if one was pending).

**Relisting.** A delisted id is never listed again. List the asset again under a new id with a new
`ListMarket` proposal.

## Economic Constants

Defined in `torus-economics/src/types.rs`:

| Constant | Value | Description |
|----------|-------|-------------|
| `MIN_SELF_DELEGATION` | 10,000 TRS (10^22 wei) | Minimum self-stake for validators |
| `MAX_COMMISSION_BPS` | 5000 (50%) | Maximum commission rate |
| `MAX_COMMISSION_CHANGE_BPS` | 100 (1%) | Maximum commission change per update |
| `COMMISSION_COOLDOWN_BLOCKS` | 28,800 | Blocks between commission changes |
| `DOUBLE_SIGN_SLASH_BPS` | 500 (5%) | Slash fraction for equivocation |
| `DOWNTIME_SLASH_BPS` | 10 (0.1%) | Slash fraction for downtime jailing |
| `JAIL_DURATION_BLOCKS` | 28,800 | Jail cooldown before unjail |
| `KEY_ROTATION_COOLDOWN_EPOCHS` | 1 | Epochs between key rotations |

## Consensus Parameters

Set via `hotstuff_rs::Configuration` in main.rs:

| Parameter | Value | Description |
|-----------|-------|-------------|
| `max_view_time` | 2000 ms | Maximum time per consensus view |
| `progress_msg_buffer_capacity` | 1024 | Message buffer size |
| `block_sync_request_limit` | 10 | Max concurrent sync requests |
| `block_sync_server_advertise_time` | 10 s | Sync server advertisement interval |
| `block_sync_response_timeout` | 3 s | Sync response timeout |
| `block_sync_blacklist_expiry_time` | 10 s | Peer blacklist duration |
| `block_sync_trigger_min_view_difference` | 2 | Views behind before triggering sync |
| `block_sync_trigger_timeout` | 60 s | Sync trigger timeout |
