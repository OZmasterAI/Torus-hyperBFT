//! Read precompile gas µbench: ns per call of every reader (0x0800-0x0803)
//! over a sweep of response sizes, the gas the call was charged
//! (`reader_gas(ReadMeter::used)`), a linear fit ns = fixed + slope x N per
//! case, and 30M-gas blocks of one reader end to end.
//!
//! Written against the reader gas API only (`reader_gas`, `reader_budget`,
//! `ReadMeter`), so the same source measures any pricing. s99 owner decisions
//! (final): base 16,400; 500 gas per scanned row + 20 per returned word / 32 B
//! blob chunk; getOpenOrders removed; getOrderBook answers the 64 best levels
//! per side in modes 2/3, reverts on a mode-1 market, classic unchanged.
//! `CONFIG` prints `reader_gas(0)`.
//!
//! Part 1: `execute_precompile_metered` over a `NativeStateOverlay` of a
//! RocksDB `StateDb` (the backend and meter the EVM provider passes; budget
//! `reader_budget(30M)`). Three cache states per scenario: `mem` (rows still
//! in the memtable), then every CF flushed + compacted and the DB reopened:
//! `cold` (first call of each target after the reopen: block cache cold, OS
//! page cache warm) and `warm` (repeated calls). Scenarios:
//! * `P` (one DB): getStakingInfo (N delegation rows, a fixed 4-word answer)
//!   and the point readers getPosition / getBalances / getPrice;
//!   `UB_RG_FILLER` random rows in each of delegations / positions / balances.
//! * `B-classic`, `B-rows1`, `B-rows2` (one DB per `BookMode`): getOrderBook
//!   of a market with N resting orders (classic: N bid prices, the whole-book
//!   blob, which reverts after it is charged; rows1: N bids at one price, which
//!   reverts as unsupported; rows2: N bid and N ask levels).
//! * `G-N` (one DB per N): the whole-CF scans getMarkets / getAllPrices /
//!   getValidators over N rows (one target, so `cold` is one sample).
//!
//! Part 2 (end to end, `UB_RG_E2E=1`): a contract that STATICCALLs one
//! reader in a loop with a fixed stipend until its gas is spent (or a call
//! count is reached), run as one 30M-gas tx through `EvmExecutor::execute_tx`
//! (the real `TorusPrecompiles` provider), plus SLOAD loops (cold: a new slot
//! each time, 2,100 gas; warm: one slot, 100 gas). `vary=stride` adds a
//! constant to the first argument word each call: a NEW existing target per
//! call (positions / balances / stakers / prices spread over the key space;
//! consecutive rows2 markets of 64 bid + 64 ask levels); `cold` lines are the
//! first tx after a flush + compact + reopen (block cache cold), `warm` the
//! median of the next ones. `ms_at_30M` scales ns/gas to 30M gas when the
//! call count, not the gas, ended the loop.
//!
//! Part 3 (`T`, deletion markers): getOrderBook over a mode-2 market with one
//! live bid level whose N better levels were written and deleted, in four
//! states: `overlay` (deletes pending in the reader's overlay over live DB
//! rows: the current / parent block), `flushed` (the deletes written by a
//! block flush, `NativeStateOverlay::flush_with_native_trie_stats`, then
//! `wait_background_compaction`: the node-local compaction, if the build
//! schedules one), `memtable` (RocksDB tombstones written directly, not
//! flushed) and `sst` (that memtable flushed to an L0 file, no compaction).
//! `skipped` = RocksDB `internal_delete_skipped_count` of one call.
//!
//! Part 4 (`CHURN`): one trader keeps `UB_RG_CHURN_ORDERS` (default 1,000 =
//! `OPEN_ORDER_BASE_LIMIT`) bids open in a mode-2 market and every block
//! cancels them all (CancelAllOrders) and places as many new ones at worse
//! prices, through the real executor; blocks are flushed like the node does
//! (`flush_with_native_trie_stats`) every `UB_RG_CHURN_BLOCK_MS` (default
//! 100); after each flush one getOrderBook on that market is measured.
//!
//! Part 5 (`MCHURN`): `UB_RG_MCHURN_MARKETS` (default 50) markets each delete
//! and re-write `UB_RG_MCHURN_ORDERS` (default 100, then 10) bid level rows per
//! block (through the block overlay and its flush), in a book CF with
//! `UB_RG_BOOK_MB` (default 400) MB of incompressible filler rows of 4,000
//! other markets: the compaction work per block (RocksDB tickers) and the
//! tombstones one getOrderBook walks.
//!
//! Output lines (whitespace key=value): `ROW`, `FIT`, `E2E`, `TOMB`, `COMPACT`,
//! `CHURN`, `CHURNSUM`, `MCHURN`, `MCHURNSUM`.
//!
//!   cargo test -p torus-bridge --release --test ubench_read_precompile_gas -- --ignored --nocapture
//!
//! Knobs: `UB_RG_SIZES` (default 0,1,4,16,64,256,1024), `UB_RG_TARGETS`
//! (K distinct targets per size, default 16), `UB_RG_WORK` (warm calls per
//! size ~ WORK / (N + 8), default 200000), `UB_RG_FILLER` (default 300000),
//! `UB_RG_E2E` (default 1), `UB_RG_E2E_REPS` (default 3), `UB_RG_POINTS`
//! (cold e2e targets per point reader, default 2000), `UB_RG_SCANS` (cold e2e
//! rows2 getOrderBook markets, default 700), `UB_RG_TOMBS` (default 1024,16384),
//! `UB_RG_CHURN_BLOCKS` (default 150), `UB_RG_CHURN_ORDERS`,
//! `UB_RG_CHURN_BLOCK_MS`, `UB_RG_MCHURN_BLOCKS` (default 100), `UB_RG_ONLY`
//! (`churn`: parts 4-5 only; `tomb`: parts 3-5).

use std::path::Path;
use std::time::{Duration, Instant};

use alloy_primitives::{keccak256, Address, Bytes, U256};
use revm::context::TxEnv;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use rocksdb::perf::{set_perf_stats, PerfContext, PerfMetric, PerfStatsLevel};
use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::book_rows::{book_order_key, level_row_key_tagged, SIDE_TAG_BID};
use torus_core::position::{position_key, MarginType, NativeBalance, Position};
use torus_core::precompiles::{
    execute_precompile_metered, precompile_address, reader_budget, reader_gas, ReadMeter,
    ADDR_BALANCE_READER, ADDR_ORACLE_READER, ADDR_ORDER_BOOK_READER, ADDR_STAKING_READER,
};
use torus_economics::types::{ValidatorState, ValidatorStatus};
use torus_evm::{BlockEnvCfg, EvmExecutor, TORUS_CHAIN_ID};
use torus_state::cf::{
    ALL_CF_NAMES, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_STAKING_DELEGATIONS, CF_STAKING_PERMANENT, CF_STAKING_REWARDS,
    CF_STAKING_VALIDATORS,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// Block timestamp (s): oracle rows are written at it, so never stale.
const NOW: u64 = 1_000_000;
/// The gas a part-1 call may use (its meter budget).
const CALL_GAS: u64 = 30_000_000;
const BOOK_MARKET_BASE: u64 = 1;
const POSITION_MARKET: u64 = 7;
/// First market of the cold e2e book targets (one market per call).
const COLD_BOOK_MARKET: u64 = 100_000;
/// Levels per side of each cold e2e getOrderBook target: the s99 cap.
const COLD_LEVELS_PER_SIDE: i64 = 64;

fn env(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

fn list(k: &str, d: &str) -> Vec<u64> {
    std::env::var(k)
        .unwrap_or_else(|_| d.into())
        .split(',')
        .map(|s| s.trim().parse().expect("comma-separated integers"))
        .collect()
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A pseudo-random address (spread over the key space, unique per tag + i).
fn addr(tag: u8, i: u64) -> Address {
    let h = splitmix(i ^ ((tag as u64) << 56));
    let mut b = [0u8; 20];
    b[..8].copy_from_slice(&h.to_be_bytes());
    b[8..16].copy_from_slice(&splitmix(h).to_be_bytes());
    b[16] = tag;
    b[17..20].copy_from_slice(&(i as u32).to_be_bytes()[1..]);
    Address::new(b)
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn addr_word(a: &Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a.as_slice());
    w
}

fn u64_word(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// Added to argument word 0 per e2e call (`vary=stride`): the low 20 bytes
/// (an address) jump across the key space; the low 8 bytes (a market id) too.
fn addr_stride() -> U256 {
    U256::from_be_slice(&[
        0x9E, 0x37, 0x79, 0xB9, 0x7F, 0x4A, 0x7C, 0x15, 0xF3, 0x9C, 0xC0, 0x60, 0x5C, 0xED, 0xC8,
        0x34, 0x10, 0x82, 0x27, 0x6B,
    ])
}

/// Word 0 of call `i` of a strided loop.
fn strided(w0: [u8; 32], stride: U256, i: u64) -> [u8; 32] {
    (U256::from_be_bytes(w0).wrapping_add(stride.wrapping_mul(U256::from(i)))).to_be_bytes()
}

fn word_addr(w: &[u8; 32]) -> Address {
    Address::from_slice(&w[12..])
}

fn input(sig: &str, words: &[[u8; 32]]) -> Vec<u8> {
    let mut v = keccak256(sig.as_bytes())[..4].to_vec();
    for w in words {
        v.extend_from_slice(w);
    }
    v
}

fn agg_row(price: i64) -> Vec<u8> {
    let mut v = Vec::with_capacity(36);
    v.extend_from_slice(&fp(price).raw().to_be_bytes());
    v.extend_from_slice(&100u64.to_be_bytes());
    v.extend_from_slice(&3u32.to_be_bytes());
    v.extend_from_slice(&NOW.to_be_bytes());
    v
}

fn agg_key(m: u64) -> Vec<u8> {
    let mut k = b"agg".to_vec();
    k.extend_from_slice(&m.to_be_bytes());
    k
}

fn delegation_row(staker: &Address, validator: &Address, amount: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(80);
    v.extend_from_slice(staker.as_slice());
    v.extend_from_slice(validator.as_slice());
    v.extend_from_slice(&U256::from(amount).to_be_bytes::<32>());
    v.extend_from_slice(&1u64.to_le_bytes());
    v
}

fn put_delegation(db: &StateDb, staker: &Address, validator: &Address) {
    let mut key = staker.as_slice().to_vec();
    key.extend_from_slice(validator.as_slice());
    db.put_cf_raw(
        CF_STAKING_DELEGATIONS,
        &key,
        &delegation_row(staker, validator, 1_000),
    )
    .unwrap();
}

fn put_staker_rows(db: &StateDb, sk: &Address) {
    let mut perm = sk.as_slice().to_vec();
    perm.extend_from_slice(&U256::from(7u64).to_be_bytes::<32>());
    perm.extend_from_slice(&5u64.to_le_bytes());
    db.put_cf_raw(CF_STAKING_PERMANENT, sk.as_slice(), &perm)
        .unwrap();
    db.put_cf_raw(CF_STAKING_REWARDS, sk.as_slice(), &perm[..52])
        .unwrap();
}

fn put_position(db: &StateDb, trader: &Address, m: u64) {
    let pos = Position {
        trader: *trader,
        market_id: m,
        is_long: true,
        size: fp(3),
        entry_price: fp(100),
        cost_basis: fp(300),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    };
    db.put_cf_raw(
        CF_NATIVE_POSITIONS,
        &position_key(trader, m),
        &borsh::to_vec(&pos).unwrap(),
    )
    .unwrap();
}

fn put_balance(db: &StateDb, trader: &Address) {
    let b = NativeBalance {
        available: fp(1_000),
        order_margin: fp(5),
    };
    db.put_cf_raw(
        CF_NATIVE_BALANCES,
        trader.as_slice(),
        &borsh::to_vec(&b).unwrap(),
    )
    .unwrap();
}

fn put_evm_account(db: &StateDb, a: &Address, code: Option<&[u8]>) {
    let code_hash = match code {
        Some(c) => {
            let h = keccak256(c);
            db.put_code(&h, c).unwrap();
            h
        }
        None => keccak256(b""),
    };
    db.put_account(
        a,
        &AccountInfo {
            balance: U256::from(1_000_000u64),
            nonce: 1,
            code_hash,
            code: None,
            account_id: None,
        },
    )
    .unwrap();
}

/// One case at one size: K calldata targets (distinct keys).
struct Group {
    case: &'static str,
    id: u16,
    n: u64,
    inputs: Vec<Vec<u8>>,
}

struct Row {
    case: &'static str,
    state: &'static str,
    n: u64,
    gas: u64,
    ns: f64,
}

fn median(v: &mut [u64]) -> f64 {
    v.sort_unstable();
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        v[n / 2] as f64
    } else {
        (v[n / 2 - 1] + v[n / 2]) as f64 / 2.0
    }
}

/// One call through the EVM provider's entry (metered, journaled overlay):
/// (ns, gas charged, ok).
fn call_once(ov: &impl StateBackend, id: u16, inp: &[u8]) -> (u64, u64, bool) {
    let mut meter = ReadMeter::with_max(reader_budget(CALL_GAS));
    let t = Instant::now();
    let r = execute_precompile_metered(
        &precompile_address(id),
        inp,
        &Address::ZERO,
        U256::ZERO,
        ov,
        100,
        NOW,
        false,
        &mut meter,
    );
    let ns = t.elapsed().as_nanos() as u64;
    let ok = r.is_ok();
    let _ = std::hint::black_box(r);
    (ns, reader_gas(meter.used()), ok)
}

fn measure(
    db: &StateDb,
    groups: &[Group],
    state: &'static str,
    scen: &str,
    work: u64,
    rows: &mut Vec<Row>,
) {
    let ov = NativeStateOverlay::new(db.clone());
    for g in groups {
        let mut ns = Vec::new();
        let mut gas = None;
        let mut ok_all = true;
        let mut record = |(t, u, ok): (u64, u64, bool), ns: &mut Vec<u64>| {
            assert!(
                gas.is_none() || gas == Some(u),
                "{}: targets of N={} differ in gas",
                g.case,
                g.n
            );
            gas = Some(u);
            ok_all &= ok;
            ns.push(t);
        };
        if state == "cold" {
            for inp in &g.inputs {
                record(call_once(&ov, g.id, inp), &mut ns);
            }
        } else {
            for inp in &g.inputs {
                std::hint::black_box(call_once(&ov, g.id, inp));
            }
            let rounds = (work / (g.inputs.len() as u64 * (g.n + 8))).clamp(3, 2_000);
            for _ in 0..rounds {
                for inp in &g.inputs {
                    record(call_once(&ov, g.id, inp), &mut ns);
                }
            }
        }
        let gas = gas.unwrap_or(0);
        let med = median(&mut ns);
        println!(
            "ROW scen={scen} case={} state={state} N={} gas={gas} ok={ok_all} samples={} ns={med:.0} ns_per_gas={:.3} \
             ms_at_30M={:.1}",
            g.case,
            g.n,
            ns.len(),
            med / gas as f64,
            med / gas as f64 * 30.0
        );
        rows.push(Row {
            case: g.case,
            state,
            n: g.n,
            gas,
            ns: med,
        });
    }
}

/// Least squares y = a + b x.
fn fit(xs: &[f64], ys: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mx = xs.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let sxx: f64 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    (my - b * mx, b)
}

fn print_fits(rows: &[Row]) {
    let mut keys: Vec<(&str, &str)> = Vec::new();
    for r in rows {
        if !keys.contains(&(r.case, r.state)) {
            keys.push((r.case, r.state));
        }
    }
    for (case, state) in keys {
        let rs: Vec<&Row> = rows
            .iter()
            .filter(|r| r.case == case && r.state == state)
            .collect();
        let worst = rs
            .iter()
            .map(|r| (r.ns / r.gas as f64, r.n))
            .fold((0.0, 0), |a, b| if b.0 > a.0 { b } else { a });
        if rs.len() < 2 {
            println!(
                "FIT case={case} state={state} fixed_ns={:.0} gas={} worst_ns_per_gas={:.3}@N={}",
                rs[0].ns, rs[0].gas, worst.0, worst.1
            );
            continue;
        }
        let xs: Vec<f64> = rs.iter().map(|r| r.n as f64).collect();
        let (a, b) = fit(&xs, &rs.iter().map(|r| r.ns).collect::<Vec<_>>());
        let (ga, gb) = fit(&xs, &rs.iter().map(|r| r.gas as f64).collect::<Vec<_>>());
        let asym = if gb > 0.0 { b / gb } else { 0.0 };
        println!(
            "FIT case={case} state={state} fixed_ns={a:.0} ns_per_N={b:.1} gas_fixed={ga:.0} gas_per_N={gb:.1} \
             asym_ns_per_gas={asym:.3} worst_ns_per_gas={:.3}@N={}",
            worst.0, worst.1
        );
    }
}

/// Flush + compact every CF, drop the handle, reopen: rows in SST files and
/// a cold block cache.
fn reopen(path: &Path, db: StateDb) -> StateDb {
    let raw = db.inner();
    for name in ALL_CF_NAMES {
        if let Ok(cf) = db.cf_handle(name) {
            raw.flush_cf(cf).unwrap();
            raw.compact_range_cf(cf, None::<&[u8]>, None::<&[u8]>);
        }
    }
    db.wait_background_compaction();
    drop(db);
    StateDb::open(path).expect("reopen db")
}

/// mem -> reopen -> cold -> warm, printing ROW/FIT; returns the reopened DB.
fn run_states(path: &Path, db: StateDb, groups: &[Group], scen: &str, work: u64) -> StateDb {
    let mut rows = Vec::new();
    measure(&db, groups, "mem", scen, work, &mut rows);
    let db = reopen(path, db);
    measure(&db, groups, "cold", scen, work, &mut rows);
    measure(&db, groups, "warm", scen, work, &mut rows);
    print_fits(&rows);
    db
}

// ---------------------------------------------------------------------------
// End to end: a STATICCALL loop through the real provider.
// ---------------------------------------------------------------------------

const LOOP_CONTRACT: Address = Address::new([0xC0; 20]);
const LOOP_CALLER: Address = Address::new([0xCA; 20]);
const E2E_GAS: u64 = 30_000_000;

/// CALLDATACOPY the tx data (the reader's calldata) to memory 0, then
/// STATICCALL `target` with `stipend` gas, at most `max_calls` times and
/// while more than ~stipend is left; a failed inner call reverts the tx.
/// `stride`: added to argument word 0 after each call.
fn loop_code(target: u16, stipend: u32, stride: Option<U256>, max_calls: u32) -> Vec<u8> {
    let thr = stipend as u64 * 65 / 63 + 10_000;
    let mut c = vec![0x36, 0x60, 0x00, 0x60, 0x00, 0x37]; // CALLDATACOPY(0,0,size)
    c.push(0x63); // counter at mem[0x400]
    c.extend_from_slice(&max_calls.to_be_bytes());
    c.extend_from_slice(&[0x61, 0x04, 0x00, 0x52]);
    let top = c.len() as u8;
    c.push(0x5b); // JUMPDEST
    c.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x36, 0x60, 0x00]); // retSize retOff argsSize argsOff
    c.push(0x61);
    c.extend_from_slice(&target.to_be_bytes());
    c.push(0x63);
    c.extend_from_slice(&stipend.to_be_bytes());
    c.extend_from_slice(&[0xfa, 0x15, 0x60]); // STATICCALL ISZERO PUSH1 <fail>
    let fail_at = c.len();
    c.extend_from_slice(&[0x00, 0x57]); // JUMPI
    if let Some(s) = stride {
        c.extend_from_slice(&[0x60, 0x04, 0x51, 0x7f]); // PUSH1 4 MLOAD PUSH32 stride
        c.extend_from_slice(&s.to_be_bytes::<32>());
        c.extend_from_slice(&[0x01, 0x60, 0x04, 0x52]); // ADD PUSH1 4 MSTORE
    }
    // counter = mem[0x400] - 1 (stored back); jump to end at 0.
    c.extend_from_slice(&[
        0x61, 0x04, 0x00, 0x51, 0x60, 0x01, 0x90, 0x03, 0x80, 0x61, 0x04, 0x00, 0x52, 0x15, 0x60,
    ]);
    let end_at = c.len();
    c.extend_from_slice(&[0x00, 0x57]); // JUMPI end
    c.extend_from_slice(&[0x5a, 0x63]);
    c.extend_from_slice(&(thr as u32).to_be_bytes());
    c.extend_from_slice(&[0x10, 0x60, top, 0x57]); // GAS > thr: loop
    c.push(0x00); // STOP
    c[end_at] = c.len() as u8;
    c.extend_from_slice(&[0x5b, 0x00]); // end: STOP
    c[fail_at] = c.len() as u8;
    c.extend_from_slice(&[0x5b, 0x60, 0x00, 0x60, 0x00, 0xfd]); // fail: REVERT(0,0)
    assert!(c.len() < 256);
    c
}

/// SLOAD loop: `cold` loads slot i (i = 0, 1, ...), else slot 0 every time.
fn sload_code(cold: bool) -> Vec<u8> {
    let mut c = vec![0x5b];
    if cold {
        c.extend_from_slice(&[
            0x60, 0x00, 0x51, 0x80, 0x54, 0x50, 0x60, 0x01, 0x01, 0x60, 0x00, 0x52,
        ]);
    } else {
        c.extend_from_slice(&[0x60, 0x00, 0x54, 0x50]);
    }
    c.extend_from_slice(&[
        0x5a, 0x63, 0x00, 0x00, 0x27, 0x10, 0x10, 0x60, 0x00, 0x57, 0x00,
    ]);
    c
}

/// Runs the tx `reps` times: (gas used, ms of each rep, all succeeded).
fn run_tx(db: &StateDb, code: &[u8], data: &[u8], reps: u64) -> (u64, Vec<f64>, bool) {
    put_evm_account(db, &LOOP_CONTRACT, Some(code));
    let ex = EvmExecutor::new(TORUS_CHAIN_ID);
    let cfg = BlockEnvCfg {
        number: 100,
        timestamp: NOW,
        beneficiary: Address::with_last_byte(0xFE),
        gas_limit: E2E_GAS,
        base_fee: 0,
    };
    let mut ms = Vec::new();
    let mut gas = 0;
    let mut ok = true;
    for _ in 0..reps {
        let tx = TxEnv {
            caller: LOOP_CALLER,
            gas_limit: E2E_GAS,
            gas_price: 0,
            kind: TxKind::Call(LOOP_CONTRACT),
            value: U256::ZERO,
            data: Bytes::copy_from_slice(data),
            nonce: 0,
            chain_id: Some(TORUS_CHAIN_ID),
            ..Default::default()
        };
        let t = Instant::now();
        let (r, _) = ex.execute_tx(db, &cfg, tx).expect("e2e tx");
        ms.push(t.elapsed().as_micros() as f64 / 1_000.0);
        gas = r.gas_used;
        ok &= r.success;
    }
    (gas, ms, ok)
}

fn median_f(v: &[f64]) -> f64 {
    let mut us: Vec<u64> = v.iter().map(|m| (m * 1_000.0) as u64).collect();
    median(&mut us) / 1_000.0
}

#[allow(clippy::too_many_arguments)]
fn print_e2e(
    scen: &str,
    case: &str,
    n: u64,
    state: &str,
    vary: &str,
    call_gas: u64,
    ok: bool,
    gas: u64,
    ms: f64,
) {
    let ns_gas = ms * 1e6 / gas as f64;
    println!(
        "E2E scen={scen} case={case} N={n} state={state} vary={vary} call_gas={call_gas} ok={ok} gas_used={gas} \
         ms={ms:.2} ns_per_gas={ns_gas:.3} ms_at_30M={:.1}",
        ns_gas * 30.0
    );
}

/// A warm e2e loop over one target (`stride` None) or many.
#[allow(clippy::too_many_arguments)]
fn e2e(
    db: &StateDb,
    scen: &str,
    case: &str,
    n: u64,
    id: u16,
    data: &[u8],
    call_gas: u64,
    stride: Option<U256>,
    reps: u64,
) {
    let stipend = (call_gas + 5_000) as u32;
    let (gas, ms, ok) = run_tx(db, &loop_code(id, stipend, stride, u32::MAX), data, reps);
    let vary = if stride.is_some() { "stride" } else { "none" };
    print_e2e(
        scen,
        case,
        n,
        "warm",
        vary,
        call_gas,
        ok,
        gas,
        median_f(&ms),
    );
}

/// The cold e2e block: flush + compact + reopen, then `reps` txs of at most
/// `calls` calls over strided targets; the first tx is `cold`, the median of
/// the rest `warm`. Returns the reopened DB.
#[allow(clippy::too_many_arguments)]
fn e2e_cold(
    path: &Path,
    db: StateDb,
    scen: &str,
    case: &str,
    n: u64,
    id: u16,
    data: &[u8],
    stride: U256,
    calls: u64,
    reps: u64,
) -> StateDb {
    let (_, call_gas, ok) = call_once(&NativeStateOverlay::new(db.clone()), id, data);
    assert!(ok, "{case}: first target fails");
    let db = reopen(path, db);
    let stipend = (call_gas + 5_000) as u32;
    let (gas, ms, ok) = run_tx(
        &db,
        &loop_code(id, stipend, Some(stride), calls as u32),
        data,
        reps.max(2),
    );
    print_e2e(scen, case, n, "cold", "stride", call_gas, ok, gas, ms[0]);
    print_e2e(
        scen,
        case,
        n,
        "warm",
        "stride",
        call_gas,
        ok,
        gas,
        median_f(&ms[1..]),
    );
    db
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

fn open(dir: &tempfile::TempDir) -> StateDb {
    StateDb::open(dir.path()).expect("open db")
}

#[allow(clippy::too_many_arguments)]
fn scenario_p(sizes: &[u64], k: u64, filler: u64, work: u64, e2e_on: bool, reps: u64, points: u64) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let t = Instant::now();
    for i in 0..filler {
        put_delegation(&db, &addr(10, i), &addr(11, i % 64));
        put_position(&db, &addr(12, i), 1 + i % 50);
        put_balance(&db, &addr(13, i));
    }
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(POSITION_MARKET), &agg_row(105))
        .unwrap();
    let mut groups = Vec::new();
    for (si, &n) in sizes.iter().enumerate() {
        let mut st = Vec::new();
        for kk in 0..k {
            let idx = si as u64 * k + kk;
            let sk = addr(2, idx);
            for j in 0..n {
                put_delegation(&db, &sk, &addr(3, j));
            }
            put_staker_rows(&db, &sk);
            st.push(input("getStakingInfo(address)", &[addr_word(&sk)]));
        }
        groups.push(Group {
            case: "getStakingInfo",
            id: ADDR_STAKING_READER,
            n,
            inputs: st,
        });
    }
    let pk = k * 4;
    let (mut gp, mut gb, mut gpr) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..pk {
        let tr = addr(4, i);
        put_position(&db, &tr, POSITION_MARKET);
        gp.push(input(
            "getPosition(address,bytes32)",
            &[addr_word(&tr), u64_word(POSITION_MARKET)],
        ));
        let tb = addr(5, i);
        put_balance(&db, &tb);
        put_evm_account(&db, &tb, None);
        gb.push(input("getBalances(address)", &[addr_word(&tb)]));
        db.put_cf_raw(
            CF_NATIVE_ORACLE,
            &agg_key(1_000 + i),
            &agg_row(100 + i as i64),
        )
        .unwrap();
        gpr.push(input("getPrice(bytes32)", &[u64_word(1_000 + i)]));
    }
    groups.push(Group {
        case: "getPosition",
        id: ADDR_ORDER_BOOK_READER,
        n: 1,
        inputs: gp,
    });
    groups.push(Group {
        case: "getBalances",
        id: ADDR_BALANCE_READER,
        n: 1,
        inputs: gb,
    });
    groups.push(Group {
        case: "getPrice",
        id: ADDR_ORACLE_READER,
        n: 1,
        inputs: gpr,
    });

    // Cold e2e targets: one existing target per call, strided from word 0.
    let s = addr_stride();
    let w_pos = addr_word(&addr(20, 0));
    let w_bal = addr_word(&addr(21, 0));
    let w_stk = addr_word(&addr(22, 0));
    let w_px = u64_word(5_000_000);
    for i in 0..points {
        put_position(&db, &word_addr(&strided(w_pos, s, i)), POSITION_MARKET);
        let b = word_addr(&strided(w_bal, s, i));
        put_balance(&db, &b);
        put_evm_account(&db, &b, None);
        let sk = word_addr(&strided(w_stk, s, i));
        put_delegation(&db, &sk, &addr(3, i % 64));
        put_staker_rows(&db, &sk);
        let m = strided(w_px, s, i);
        db.put_cf_raw(
            CF_NATIVE_ORACLE,
            &agg_key(u64::from_be_bytes(m[24..].try_into().unwrap())),
            &agg_row(100),
        )
        .unwrap();
    }
    println!(
        "SETUP scen=P filler={filler} K={k} points={points} ms={}",
        t.elapsed().as_millis()
    );

    let mut db = run_states(dir.path(), db, &groups, "P", work);
    if !e2e_on {
        return;
    }
    let big = *sizes.iter().max().unwrap();
    let ov = NativeStateOverlay::new(db.clone());
    for g in &groups {
        if g.n == big || g.n == 1 || g.n == 0 {
            let (_, gas, _) = call_once(&ov, g.id, &g.inputs[0]);
            e2e(&db, "P", g.case, g.n, g.id, &g.inputs[0], gas, None, reps);
        }
    }
    drop(ov);
    let mkt = u64_word(POSITION_MARKET);
    for (case, id, data, n, calls) in [
        (
            "getPosition",
            ADDR_ORDER_BOOK_READER,
            input("getPosition(address,bytes32)", &[w_pos, mkt]),
            1,
            points,
        ),
        (
            "getBalances",
            ADDR_BALANCE_READER,
            input("getBalances(address)", &[w_bal]),
            1,
            points,
        ),
        (
            "getPrice",
            ADDR_ORACLE_READER,
            input("getPrice(bytes32)", &[w_px]),
            1,
            points,
        ),
        (
            "getStakingInfo",
            ADDR_STAKING_READER,
            input("getStakingInfo(address)", &[w_stk]),
            1,
            points,
        ),
    ] {
        db = e2e_cold(dir.path(), db, "P", case, n, id, &data, s, calls, reps);
    }

    // SLOAD price point: slots 0..40k exist (cold loop reads ~14k of them).
    for s in 0..40_000u64 {
        db.put_storage(&LOOP_CONTRACT, &U256::from(s), &U256::from(s + 1))
            .unwrap();
    }
    for cold in [true, false] {
        let (gas, ms, ok) = run_tx(&db, &sload_code(cold), &[], reps);
        print_e2e(
            "P",
            if cold { "SLOAD-cold" } else { "SLOAD-warm" },
            0,
            "warm",
            "none",
            0,
            ok,
            gas,
            median_f(&ms),
        );
    }
}

fn place(maker: Address, m: u64, is_buy: bool, price: i64) -> (Address, NativeAction) {
    (
        maker,
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: m,
            is_buy,
            price: fp(price),
            quantity: fp(1),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    )
}

fn exec_ctx<T: StateBackend + Clone>(
    state: T,
    height: u64,
    mode: BookMode,
) -> NativeExecContext<T> {
    NativeExecContext::new_with_mode(
        state,
        height,
        1_000 + height,
        0,
        100,
        10,
        Address::new([99; 20]),
        Address::new([100; 20]),
        Address::new([101; 20]),
        mode,
        None,
    )
}

fn fund<T: StateBackend + Clone>(ctx: &NativeExecContext<T>, maker: &Address) {
    ctx.positions
        .put_native_balance(
            maker,
            &NativeBalance {
                available: fp(1_000_000_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
}

fn execute_all<T: StateBackend + Clone>(
    ctx: &mut NativeExecContext<T>,
    block: &[(Address, NativeAction)],
    what: &str,
) {
    for chunk in block.chunks(50_000) {
        let r = NativeExecutor::execute_batch(ctx, chunk);
        let failed: Vec<_> = r
            .results
            .iter()
            .enumerate()
            .filter(|(_, x)| !x.success)
            .take(3)
            .collect();
        assert!(
            failed.is_empty(),
            "{what}: block failed ({} of {}): {:?}",
            failed.len(),
            chunk.len(),
            failed
                .iter()
                .map(|(i, x)| (i, x.error.clone()))
                .collect::<Vec<_>>()
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn scenario_book(
    mode: BookMode,
    scen: &str,
    sizes: &[u64],
    k: u64,
    work: u64,
    e2e_on: bool,
    reps: u64,
    scans: u64,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let t = Instant::now();
    let mut ctx = exec_ctx(db.clone(), 1, mode);
    let mut block = Vec::new();
    let mut groups = Vec::new();
    for (si, &n) in sizes.iter().enumerate() {
        let mut inputs = Vec::new();
        for kk in 0..k {
            let m = BOOK_MARKET_BASE + si as u64 * k + kk;
            for j in 0..n {
                // One maker per 256 levels (an account may hold 1,000 open orders).
                let maker = addr(6, m * 1_000 + j / 256);
                if j % 256 == 0 {
                    fund(&ctx, &maker);
                }
                let price = if mode == BookMode::OrderRows {
                    1_000
                } else {
                    1_000 + j as i64
                };
                block.push(place(maker, m, true, price));
                // rows2: N levels per side (asks above every bid).
                if mode == BookMode::LevelAuthority {
                    block.push(place(maker, m, false, 1_000_000 + j as i64));
                }
            }
            inputs.push(input("getOrderBook(bytes32)", &[u64_word(m)]));
        }
        groups.push(Group {
            case: "getOrderBook",
            id: ADDR_ORDER_BOOK_READER,
            n,
            inputs,
        });
    }
    // Cold e2e targets (rows2 only; rows1 reverts, classic reverts):
    // `scans` consecutive markets of 64 bid + 64 ask levels each, the capped
    // worst row-heavy answer (128 rows, 264 words).
    if mode == BookMode::LevelAuthority {
        for i in 0..scans {
            let m = COLD_BOOK_MARKET + i;
            let maker = addr(15, m);
            fund(&ctx, &maker);
            for j in 0..COLD_LEVELS_PER_SIDE {
                block.push(place(maker, m, true, 1_000 + j));
                block.push(place(maker, m, false, 2_000 + j));
            }
        }
    }
    execute_all(&mut ctx, &block, scen);
    ctx.save_order_books();
    drop(ctx);
    println!(
        "SETUP scen={scen} K={k} orders={} ms={}",
        block.len(),
        t.elapsed().as_millis()
    );

    let mut db = run_states(dir.path(), db, &groups, scen, work);
    if !e2e_on {
        return;
    }
    let big = *sizes.iter().max().unwrap();
    let ov = NativeStateOverlay::new(db.clone());
    for g in &groups {
        if g.n == big || g.n == 1 {
            let (_, gas, _) = call_once(&ov, g.id, &g.inputs[0]);
            // Classic (the blob is not an OrderBookSnapshot) and rows1
            // (unsupported) revert: the loop needs successful calls, so they
            // run part 1 only.
            if mode == BookMode::LevelAuthority {
                e2e(&db, scen, g.case, g.n, g.id, &g.inputs[0], gas, None, reps);
            }
        }
    }
    drop(ov);
    if mode == BookMode::LevelAuthority {
        let data = input("getOrderBook(bytes32)", &[u64_word(COLD_BOOK_MARKET)]);
        let n = 2 * COLD_LEVELS_PER_SIDE as u64;
        db = e2e_cold(
            dir.path(),
            db,
            scen,
            "getOrderBook",
            n,
            ADDR_ORDER_BOOK_READER,
            &data,
            U256::from(1),
            scans,
            reps,
        );
    }
    drop(db);
}

fn scenario_global(n: u64, work: u64) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    for m in 1..=n {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &[1u8; 64])
            .unwrap();
        db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &agg_row(100 + m as i64))
            .unwrap();
        let v = addr(8, m);
        let vs = ValidatorState {
            address: v,
            pubkey: [7; 32],
            commission_bps: 500,
            self_stake: U256::from(1_000_000u64),
            total_delegated: U256::from(5_000u64),
            status: ValidatorStatus::Active,
            jailed_until: None,
            last_commission_change_block: None,
            oracle_signer: None,
        };
        db.put_cf_raw(
            CF_STAKING_VALIDATORS,
            v.as_slice(),
            &borsh::to_vec(&vs).unwrap(),
        )
        .unwrap();
    }
    let groups = vec![
        Group {
            case: "getMarkets",
            id: ADDR_BALANCE_READER,
            n,
            inputs: vec![input("getMarkets()", &[])],
        },
        Group {
            case: "getAllPrices",
            id: ADDR_ORACLE_READER,
            n,
            inputs: vec![input("getAllPrices()", &[])],
        },
        Group {
            case: "getValidators",
            id: ADDR_STAKING_READER,
            n,
            inputs: vec![input("getValidators()", &[])],
        },
    ];
    let scen = format!("G-{n}");
    let mut rows = Vec::new();
    measure(&db, &groups, "mem", &scen, work, &mut rows);
    let db = reopen(dir.path(), db);
    measure(&db, &groups, "cold", &scen, work, &mut rows);
    measure(&db, &groups, "warm", &scen, work, &mut rows);
    drop(db);
}

// ---------------------------------------------------------------------------
// Parts 3 and 4: deletion markers.
// ---------------------------------------------------------------------------

/// RocksDB tombstones the current thread's iterators skipped during `f`.
fn deletes_skipped(f: impl FnOnce()) -> u64 {
    set_perf_stats(PerfStatsLevel::EnableCount);
    let mut ctx = PerfContext::default();
    ctx.reset();
    f();
    let n = ctx.metric(PerfMetric::InternalDeleteSkippedCount);
    set_perf_stats(PerfStatsLevel::Disable);
    n
}

/// The level rows a target's N markers are made from: bids of the mode-2
/// market better than its one live bid (1,000), so getOrderBook's best-first
/// scan walks all of them before the live level.
fn marker_keys(n: u64) -> Vec<(&'static str, Vec<u8>)> {
    (0..n)
        .map(|j| {
            let key =
                level_row_key_tagged(COLD_BOOK_MARKET, SIDE_TAG_BID, fp(2_000 + j as i64).raw());
            (CF_NATIVE_ORDER_BOOKS, key.to_vec())
        })
        .collect()
}

/// A mode-2 DB with the target market's one live bid level, a few levels in
/// 50 other markets and `filler` positions.
fn tomb_db(dir: &tempfile::TempDir, filler: u64) -> StateDb {
    let db = open(dir);
    for i in 0..filler {
        put_position(&db, &addr(12, i), 1 + i % 50);
    }
    let mut ctx = exec_ctx(db.clone(), 1, BookMode::LevelAuthority);
    let maker = addr(15, 0);
    fund(&ctx, &maker);
    let mut block = vec![place(maker, COLD_BOOK_MARKET, true, 1_000)];
    for i in 0..filler / 64 {
        block.push(place(
            maker,
            COLD_BOOK_MARKET + 1 + i % 50,
            true,
            1_000 + (i / 50) as i64,
        ));
    }
    execute_all(&mut ctx, &block, "tomb seed");
    ctx.save_order_books();
    drop(ctx);
    db
}

fn flush_cfs(db: &StateDb) {
    db.inner()
        .flush_cf(db.cf_handle(CF_NATIVE_ORDER_BOOKS).unwrap())
        .unwrap();
}

/// ns (median of `work` calls), gas, answer bytes and skipped tombstones of
/// one target.
fn tomb_call(state: &impl StateBackend, inp: &[u8], work: u64) -> (f64, u64, usize, u64) {
    let out = execute_precompile_metered(
        &precompile_address(ADDR_ORDER_BOOK_READER),
        inp,
        &Address::ZERO,
        U256::ZERO,
        state,
        100,
        NOW,
        false,
        &mut ReadMeter::with_max(reader_budget(CALL_GAS)),
    )
    .expect("tombstone target answers");
    let skipped = deletes_skipped(|| {
        std::hint::black_box(call_once(state, ADDR_ORDER_BOOK_READER, inp));
    });
    let mut ns = Vec::new();
    let mut gas = 0;
    for _ in 0..work.max(1) {
        let (t, g, ok) = call_once(state, ADDR_ORDER_BOOK_READER, inp);
        assert!(ok);
        gas = g;
        ns.push(t);
    }
    (median(&mut ns), gas, out.len(), skipped)
}

fn print_tomb(case: &str, n: u64, state: &str, (ns, gas, bytes, skipped): (f64, u64, usize, u64)) {
    println!(
        "TOMB case={case} N={n} state={state} gas={gas} answer_bytes={bytes} skipped={skipped} ns={ns:.0} \
         ns_per_marker={:.1} ns_per_gas={:.3} ms_at_30M={:.1}",
        ns / n.max(1) as f64,
        ns / gas as f64,
        ns / gas as f64 * 30.0
    );
}

fn tomb_inputs() -> [(&'static str, Vec<u8>); 1] {
    [(
        "getOrderBook",
        input("getOrderBook(bytes32)", &[u64_word(COLD_BOOK_MARKET)]),
    )]
}

fn scenario_tombstones(ns_list: &[u64], filler: u64, reps: u64) {
    for &n in ns_list {
        for (case, inp) in &tomb_inputs() {
            let work = (2_000_000 / (n + 64)).clamp(5, 2_000);
            let keys = marker_keys(n);

            // overlay: rows in an SST, deleted in the reader's overlay.
            let dir = tempfile::tempdir().unwrap();
            let db = tomb_db(&dir, filler);
            for (cf, k) in &keys {
                db.put_cf_raw(cf, k, &[0u8; 64]).unwrap();
            }
            flush_cfs(&db);
            let ov = NativeStateOverlay::new(db.clone());
            for (cf, k) in &keys {
                ov.delete_cf_raw(cf, k).unwrap();
            }
            print_tomb(case, n, "overlay", tomb_call(&ov, inp, work));

            // flushed: the deletes written by a block flush (+ the background
            // compaction the build schedules for them, if any).
            let t = Instant::now();
            ov.flush_with_native_trie_stats(&db, None, None, None)
                .unwrap();
            let flush_ms = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let (done, failed) = db.wait_background_compaction();
            println!(
                "COMPACT case={case} N={n} flush_ms={flush_ms:.1} wait_ms={:.1} runs_done={done} runs_failed={failed}",
                t.elapsed().as_secs_f64() * 1e3
            );
            print_tomb(
                case,
                n,
                "flushed",
                tomb_call(&NativeStateOverlay::new(db.clone()), inp, work),
            );
            drop(ov);
            drop(db);

            // memtable / sst: tombstones written straight to RocksDB.
            let dir = tempfile::tempdir().unwrap();
            let db = tomb_db(&dir, filler);
            for (cf, k) in &keys {
                db.put_cf_raw(cf, k, &[0u8; 64]).unwrap();
            }
            flush_cfs(&db);
            for (cf, k) in &keys {
                db.delete_cf_raw(cf, k).unwrap();
            }
            print_tomb(
                case,
                n,
                "memtable",
                tomb_call(&NativeStateOverlay::new(db.clone()), inp, work),
            );
            // sst: the memtable flushed to an L0 file above the rows' file; no
            // compaction (a full manual compaction would merge them away).
            flush_cfs(&db);
            print_tomb(
                case,
                n,
                "sst",
                tomb_call(&NativeStateOverlay::new(db.clone()), inp, work),
            );
            if n == ns_list[0] {
                let (_, gas, _) = call_once(
                    &NativeStateOverlay::new(db.clone()),
                    ADDR_ORDER_BOOK_READER,
                    inp,
                );
                let stipend = (gas + 5_000) as u32;
                let (g, ms, ok) = run_tx(
                    &db,
                    &loop_code(ADDR_ORDER_BOOK_READER, stipend, None, u32::MAX),
                    inp,
                    reps,
                );
                print_e2e(
                    "T",
                    &format!("{case}-tombstones-sst"),
                    n,
                    "warm",
                    "none",
                    gas,
                    ok,
                    g,
                    median_f(&ms),
                );
            }
        }
    }
}

/// Part 4: per block, cancel the trader's `orders` open orders and place as
/// many new ones (ids keep rising, so every older deleted id sits before the
/// live rows in key order), flush the block like the node, then measure one
/// reader call on the trader + market.
fn scenario_churn(blocks: u64, orders: u64, block_ms: u64, filler: u64) {
    let pace = Duration::from_millis(block_ms);
    for (case, inp) in tomb_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let db = tomb_db(&dir, filler);
        let maker = addr(16, 0);
        let (mut peak, mut peak_block, mut skipped_sum, mut last) = (0u64, 0u64, 0u64, Vec::new());
        let t0 = Instant::now();
        for b in 0..blocks {
            let started = Instant::now();
            let ov = NativeStateOverlay::new(db.clone());
            // The real executor in mode 2: cancel the trader's levels, place
            // `orders` new bids, each block worse than the last, so the
            // deleted (better) levels sit before the live ones in the
            // best-first scan.
            let mut ctx = exec_ctx(ov.clone(), 2 + b, BookMode::LevelAuthority);
            if b == 0 {
                fund(&ctx, &maker);
            }
            let mut block = vec![(
                maker,
                NativeAction::CancelAllOrders {
                    market_id: Some(COLD_BOOK_MARKET),
                },
            )];
            let top = 1_000_000 - (b * orders) as i64;
            block.extend((0..orders as i64).map(|i| place(maker, COLD_BOOK_MARKET, true, top - i)));
            execute_all(&mut ctx, &block, "churn block");
            ctx.save_order_books();
            drop(ctx);
            let tf = Instant::now();
            ov.flush_with_native_trie_stats(&db, None, None, None)
                .unwrap();
            let flush_ms = tf.elapsed().as_secs_f64() * 1e3;
            drop(ov);
            let (ns, gas, bytes, skipped) =
                tomb_call(&NativeStateOverlay::new(db.clone()), &inp, 5);
            println!(
                "CHURN case={case} block={b} orders={orders} flush_ms={flush_ms:.1} gas={gas} answer_bytes={bytes} \
                 skipped={skipped} ns={ns:.0} ms_at_30M={:.1} t_ms={}",
                ns / gas as f64 * 30.0,
                t0.elapsed().as_millis()
            );
            if skipped > peak {
                (peak, peak_block) = (skipped, b);
            }
            skipped_sum += skipped;
            if b + 20 >= blocks {
                last.push(ns / gas as f64 * 30.0);
            }
            if let Some(rest) = pace.checked_sub(started.elapsed()) {
                std::thread::sleep(rest);
            }
        }
        println!(
            "CHURNSUM case={case} blocks={blocks} orders={orders} block_ms={block_ms} peak_skipped={peak}@block={peak_block} \
             mean_skipped={:.0} last20_ms_at_30M_median={:.1} runs={:?}",
            skipped_sum as f64 / blocks as f64,
            median_f(&last),
            db.wait_background_compaction()
        );
    }
}

/// A real mode-2 level row value (one resting bid), reused for every churn
/// level of part 5.
fn level_row_template() -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let mut ctx = exec_ctx(db.clone(), 1, BookMode::LevelAuthority);
    let maker = addr(17, 0);
    fund(&ctx, &maker);
    execute_all(&mut ctx, &[place(maker, 1, true, 1_000)], "template");
    ctx.save_order_books();
    drop(ctx);
    db.iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&1u64.to_be_bytes()))
        .unwrap()
        .into_iter()
        .find(|(k, _)| k.len() == 26)
        .expect("one level row")
        .1
}

/// RocksDB compaction tickers (read bytes, write bytes, CPU us) so far.
fn compaction_tickers(db: &StateDb) -> (u64, u64, u64) {
    let t = db
        .runtime_stats()
        .tickers
        .expect("RocksDB tickers (stats level >= 1)");
    (
        t.compact_read_bytes,
        t.compact_write_bytes,
        t.compaction_cpu_micros,
    )
}

/// Part 5 (`MCHURN`): `markets` markets each cancel and re-place `orders`
/// mode-1 order rows every block (rows written / deleted through the block
/// overlay and its flush, like the executor's save), in a book CF holding
/// `book_mb` MB of incompressible filler rows of 4,000 other markets, pushed
/// to the bottommost level. Per block: the flush, the compaction work since
/// the previous block (RocksDB tickers: automatic + background range
/// compactions), and one getOrderBook on the first churn market (the
/// tombstones it walks, its 30M-gas block time).
fn scenario_multi_churn(blocks: u64, markets: u64, per_block: &[u64], book_mb: u64, block_ms: u64) {
    let template = level_row_template();
    let pace = Duration::from_millis(block_ms);
    const FILLER_MARKETS: u64 = 4_000;
    const ROW: usize = 160;
    for &orders in per_block {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir);
        let t = Instant::now();
        let cf = db.cf_handle(CF_NATIVE_ORDER_BOOKS).unwrap();
        let filler = book_mb * 1_000_000 / (ROW as u64 + 25);
        let mut batch = rocksdb::WriteBatch::default();
        for i in 0..filler {
            let mut v = vec![0u8; ROW];
            for (j, c) in v.chunks_mut(8).enumerate() {
                c.copy_from_slice(
                    &splitmix(i.wrapping_mul(31).wrapping_add(j as u64)).to_be_bytes(),
                );
            }
            batch.put_cf(
                cf,
                book_order_key(2 * (1 + i % FILLER_MARKETS), i as u128),
                &v,
            );
            if batch.len() >= 100_000 {
                db.inner().write(std::mem::take(&mut batch)).unwrap();
            }
        }
        // Churn markets: odd ids spread among the filler markets.
        let churn: Vec<u64> = (0..markets)
            .map(|j| 2 * (j * FILLER_MARKETS / markets) + 1)
            .collect();
        // Block b's levels: bids below block b - 1's, so the deleted (better)
        // levels sit before the live ones in the best-first scan.
        let level = |m: u64, b: u64, i: u64| {
            level_row_key_tagged(
                m,
                SIDE_TAG_BID,
                fp(1_000_000 - (b * orders + i) as i64).raw(),
            )
        };
        for &m in &churn {
            for i in 0..orders {
                batch.put_cf(cf, level(m, 0, i), &template);
            }
        }
        db.inner().write(batch).unwrap();
        db.inner().flush_cf(cf).unwrap();
        db.inner()
            .compact_range_cf(cf, None::<&[u8]>, None::<&[u8]>);
        db.wait_background_compaction();
        let sst = db
            .inner()
            .property_int_value_cf(cf, "rocksdb.total-sst-files-size")
            .unwrap()
            .unwrap_or(0);
        println!(
            "MCHURN setup markets={markets} orders={orders} filler_rows={filler} book_cf_mb={:.0} setup_s={:.1}",
            sst as f64 / 1e6,
            t.elapsed().as_secs_f64()
        );

        let inp = input("getOrderBook(bytes32)", &[u64_word(churn[0])]);
        let (mut sum_r, mut sum_w, mut sum_cpu, mut peak, mut times) =
            (0u64, 0u64, 0u64, 0u64, Vec::new());
        let mut prev = compaction_tickers(&db);
        let t0 = Instant::now();
        for b in 1..=blocks {
            let started = Instant::now();
            let ov = NativeStateOverlay::new(db.clone());
            for &m in &churn {
                for i in 0..orders {
                    ov.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &level(m, b - 1, i))
                        .unwrap();
                    ov.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &level(m, b, i), &template)
                        .unwrap();
                }
            }
            let tf = Instant::now();
            ov.flush_with_native_trie_stats(&db, None, None, None)
                .unwrap();
            let flush_ms = tf.elapsed().as_secs_f64() * 1e3;
            drop(ov);
            let (ns, gas, _, skipped) = tomb_call(&NativeStateOverlay::new(db.clone()), &inp, 5);
            if let Some(rest) = pace.checked_sub(started.elapsed()) {
                std::thread::sleep(rest);
            }
            // Compaction work during this block interval.
            let now = compaction_tickers(&db);
            let (r, w, cpu) = (now.0 - prev.0, now.1 - prev.1, now.2 - prev.2);
            prev = now;
            (sum_r, sum_w, sum_cpu) = (sum_r + r, sum_w + w, sum_cpu + cpu);
            peak = peak.max(skipped);
            let ms30 = ns / gas as f64 * 30.0;
            times.push(ms30);
            println!(
                "MCHURN markets={markets} orders={orders} block={b} flush_ms={flush_ms:.1} compact_read_mb={:.1} \
                 compact_write_mb={:.1} compact_cpu_ms={:.1} gas={gas} skipped={skipped} ns={ns:.0} ms_at_30M={ms30:.1} t_ms={}",
                r as f64 / 1e6,
                w as f64 / 1e6,
                cpu as f64 / 1e3,
                t0.elapsed().as_millis()
            );
        }
        let tw = Instant::now();
        let runs = db.wait_background_compaction();
        let tail = compaction_tickers(&db);
        println!(
            "MCHURNSUM markets={markets} orders={orders} blocks={blocks} book_cf_mb={:.0} per_block_compact_read_mb={:.2} \
             per_block_compact_write_mb={:.2} per_block_compact_cpu_ms={:.1} tail_wait_ms={:.0} tail_write_mb={:.1} \
             peak_skipped={peak} median_ms_at_30M={:.1} max_ms_at_30M={:.1} runs={runs:?}",
            sst as f64 / 1e6,
            sum_r as f64 / 1e6 / blocks as f64,
            sum_w as f64 / 1e6 / blocks as f64,
            sum_cpu as f64 / 1e3 / blocks as f64,
            tw.elapsed().as_secs_f64() * 1e3,
            (tail.1 - prev.1) as f64 / 1e6,
            median_f(&times),
            times.iter().cloned().fold(0.0, f64::max)
        );
    }
}

#[test]
#[ignore = "µbench (sizing the read precompile gas) — run with --ignored --nocapture on a quiet box"]
fn ubench_read_precompile_gas() {
    let sizes = list("UB_RG_SIZES", "0,1,4,16,64,256,1024");
    let k = env("UB_RG_TARGETS", 16);
    let work = env("UB_RG_WORK", 200_000);
    let filler = env("UB_RG_FILLER", 300_000);
    let e2e_on = env("UB_RG_E2E", 1) == 1;
    let reps = env("UB_RG_E2E_REPS", 3);
    let points = env("UB_RG_POINTS", 2_000);
    let scans = env("UB_RG_SCANS", 700);
    let tombs = list("UB_RG_TOMBS", "1024,16384");
    let churn = (
        env("UB_RG_CHURN_BLOCKS", 150),
        env("UB_RG_CHURN_ORDERS", 1_000),
        env("UB_RG_CHURN_BLOCK_MS", 100),
    );
    let only = std::env::var("UB_RG_ONLY").unwrap_or_default();
    let mchurn = (
        env("UB_RG_MCHURN_BLOCKS", 100),
        env("UB_RG_MCHURN_MARKETS", 50),
        list("UB_RG_MCHURN_ORDERS", "100,10"),
        env("UB_RG_BOOK_MB", 400),
    );
    println!(
        "CONFIG sizes={sizes:?} K={k} work={work} filler={filler} e2e={e2e_on} e2e_reps={reps} points={points} \
         scans={scans} tombs={tombs:?} churn={churn:?} mchurn={mchurn:?} only={only:?} base_gas={}",
        reader_gas(0)
    );
    let t = Instant::now();
    if only.is_empty() {
        scenario_p(&sizes, k, filler, work, e2e_on, reps, points);
        scenario_book(
            BookMode::Classic,
            "B-classic",
            &sizes,
            k,
            work,
            e2e_on,
            reps,
            scans,
        );
        scenario_book(
            BookMode::OrderRows,
            "B-rows1",
            &sizes,
            k,
            work,
            e2e_on,
            reps,
            scans,
        );
        scenario_book(
            BookMode::LevelAuthority,
            "B-rows2",
            &sizes,
            k,
            work,
            e2e_on,
            reps,
            scans,
        );
        // Whole-CF scans: one DB per N; FIT over the G rows of all N.
        for &n in &sizes {
            scenario_global(n, work);
        }
    }
    if only.is_empty() || only == "tomb" {
        scenario_tombstones(&tombs, filler / 10, reps);
    }
    scenario_churn(churn.0, churn.1, churn.2, filler / 10);
    scenario_multi_churn(mchurn.0, mchurn.1, &mchurn.2, mchurn.3, churn.2);
    println!("DONE total_s={:.1}", t.elapsed().as_secs_f64());
}
