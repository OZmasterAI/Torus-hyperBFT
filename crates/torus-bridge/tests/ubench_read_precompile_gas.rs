//! Read precompile gas µbench: ns per call of every reader (0x0800-0x0803)
//! over a sweep of response sizes, the gas the call was charged
//! (`reader_gas(ReadMeter::used)`), a linear fit ns = fixed + slope x N per
//! case, and 30M-gas blocks of one reader end to end.
//!
//! Written against the reader gas API only (`reader_gas`, `reader_budget`,
//! `ReadMeter`), so the same source measures any pricing. s99 owner decisions
//! (final): base 16,400; 500 gas per scanned row + 20 per returned word;
//! getOpenOrders removed. Item 7 step 1 (owner s122) removed getOrderBook
//! too, and with it the book scenarios and the deletion-marker parts that
//! measured it. `CONFIG` prints `reader_gas(0)`.
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
//! * `G-N` (one DB per N): the whole-CF scans getMarkets / getAllPrices /
//!   getValidators over N rows (one target, so `cold` is one sample).
//!
//! Part 2 (end to end, `UB_RG_E2E=1`): a contract that STATICCALLs one
//! reader in a loop with a fixed stipend until its gas is spent (or a call
//! count is reached), run as one 30M-gas tx through `EvmExecutor::execute_tx`
//! (the real `TorusPrecompiles` provider), plus SLOAD loops (cold: a new slot
//! each time, 2,100 gas; warm: one slot, 100 gas). `vary=stride` adds a
//! constant to the first argument word each call: a NEW existing target per
//! call (positions / balances / stakers / prices spread over the key space);
//! `cold` lines are the first tx after a flush + compact + reopen (block
//! cache cold), `warm` the median of the next ones. `ms_at_30M` scales
//! ns/gas to 30M gas when the call count, not the gas, ended the loop.
//!
//! Output lines (whitespace key=value): `ROW`, `FIT`, `E2E`.
//!
//!   cargo test -p torus-bridge --release --test ubench_read_precompile_gas -- --ignored --nocapture
//!
//! Knobs: `UB_RG_SIZES` (default 0,1,4,16,64,256,1024), `UB_RG_TARGETS`
//! (K distinct targets per size, default 16), `UB_RG_WORK` (warm calls per
//! size ~ WORK / (N + 8), default 200000), `UB_RG_FILLER` (default 300000),
//! `UB_RG_E2E` (default 1), `UB_RG_E2E_REPS` (default 3), `UB_RG_POINTS`
//! (cold e2e targets per point reader, default 2000).

use std::path::Path;
use std::time::Instant;

use alloy_primitives::{keccak256, Address, Bytes, U256};
use revm::context::TxEnv;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use torus_core::position::{position_key, MarginType, NativeBalance, Position};
use torus_core::precompiles::{
    execute_precompile_metered, precompile_address, reader_budget, reader_gas, ReadMeter,
    ADDR_BALANCE_READER, ADDR_ORACLE_READER, ADDR_ORDER_BOOK_READER, ADDR_STAKING_READER,
};
use torus_economics::types::{ValidatorState, ValidatorStatus};
use torus_evm::{BlockEnvCfg, EvmExecutor, TORUS_CHAIN_ID};
use torus_state::cf::{
    ALL_CF_NAMES, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_POSITIONS,
    CF_STAKING_DELEGATIONS, CF_STAKING_PERMANENT, CF_STAKING_REWARDS, CF_STAKING_VALIDATORS,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::FixedPoint;

/// Block timestamp (s): oracle rows are written at it, so never stale.
const NOW: u64 = 1_000_000;
/// The gas a part-1 call may use (its meter budget).
const CALL_GAS: u64 = 30_000_000;
const POSITION_MARKET: u64 = 7;

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
        funding: None,
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
        features: 0,
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
    println!(
        "CONFIG sizes={sizes:?} K={k} work={work} filler={filler} e2e={e2e_on} e2e_reps={reps} points={points} \
         base_gas={}",
        reader_gas(0)
    );
    let t = Instant::now();
    scenario_p(&sizes, k, filler, work, e2e_on, reps, points);
    // Whole-CF scans: one DB per N; FIT over the G rows of all N.
    for &n in &sizes {
        scenario_global(n, work);
    }
    println!("DONE total_s={:.1}", t.elapsed().as_secs_f64());
}
