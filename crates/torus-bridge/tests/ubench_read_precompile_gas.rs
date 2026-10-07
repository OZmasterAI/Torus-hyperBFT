//! Read precompile gas µbench (sizing `GAS_PRECOMPILE_READ_PER_UNIT`, the
//! 50-gas placeholder of fix/read-precompile-gas): ns per call of every
//! reader (0x0800-0x0803) over a sweep of response sizes, the work units the
//! call was charged (`ReadMeter::used`), and a linear fit ns = fixed + slope
//! x N per case, so ns per unit = slope / (units per N).
//!
//! Units (crates/torus-core/src/precompiles.rs): a reader pays
//! `GAS_PRECOMPILE_READ` (2,600) + `GAS_PRECOMPILE_READ_PER_UNIT` (50) per
//! unit; one unit = one hashed row a prefix scan returned
//! (`scan_prefix_metered`), 32 bytes of a classic whole-book blob (sized
//! before it is read), or one 32-byte word of the answer. Point reads
//! (`get_cf_raw`) are not charged beyond the base.
//!
//! Part 1 (units): `execute_precompile_metered` over a `NativeStateOverlay`
//! of a RocksDB `StateDb` (the backend and meter the EVM provider passes;
//! budget `reader_budget(30M)`). Three cache states per scenario:
//! `mem` (rows still in the memtable), then every CF flushed + compacted and
//! the DB reopened: `cold` (first call of each target after the reopen:
//! block cache cold, OS page cache warm) and `warm` (repeated calls).
//! Scenarios:
//! * `P` (one DB): getOpenOrders (N order rows of one trader + market),
//!   getStakingInfo (N delegation rows, a fixed 4-word answer), and the point
//!   readers getPosition / getBalances / getPrice; `UB_RG_FILLER` random rows
//!   in each of orders / delegations / positions / balances around them.
//! * `B-classic`, `B-rows1`, `B-rows2` (one DB per `BookMode`): getOrderBook
//!   of a market with N resting bids (classic: N prices, the whole-book blob,
//!   which reverts after it is charged; rows1: N orders at ONE price, N rows
//!   and a 1-level answer; rows2: N price levels, one level row each).
//! * `G-N` (one DB per N): the whole-CF scans getMarkets / getAllPrices /
//!   getValidators over N rows (one target, so `cold` is one sample).
//! * `getOpenOrders-tombstones` (in `P`): a trader whose N orders were
//!   written and deleted: an empty answer over N RocksDB deletion markers,
//!   which the scan steps over uncharged (they survive the flush +
//!   `compact_range` before `cold` / `warm` here, likely a trivial move).
//!
//! Part 2 (end to end, `UB_RG_E2E=1`): a contract that STATICCALLs one
//! reader in a loop with a fixed stipend until its gas is spent, run as one
//! 30M-gas tx through `EvmExecutor::execute_tx` (the real
//! `TorusPrecompiles` provider), plus SLOAD loops (cold: a new slot each
//! time, 2,100 gas; warm: one slot, 100 gas) as the EVM's own price point.
//! `vary` adds 1 to the first argument word each call (a new trader / market
//! / staker: mostly empty prefixes, a cold seek per call).
//!
//! Output lines (whitespace key=value, one per measurement): `ROW` (case,
//! state, N, units, gas, median ns, ns/gas), `FIT` (per case + state) and
//! `E2E`.
//!
//!   cargo test -p torus-bridge --release --test ubench_read_precompile_gas -- --ignored --nocapture
//!
//! Knobs: `UB_RG_SIZES` (default 0,1,4,16,64,256,1024), `UB_RG_TARGETS`
//! (K distinct targets per size, default 16), `UB_RG_WORK` (warm calls per
//! size ~ WORK / (N + 8), default 200000), `UB_RG_FILLER` (default 300000),
//! `UB_RG_E2E` (default 1), `UB_RG_E2E_REPS` (default 3).

use std::path::Path;
use std::time::Instant;

use alloy_primitives::{keccak256, Address, Bytes, U256};
use revm::context::TxEnv;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::position::{position_key, MarginType, NativeBalance, Position};
use torus_core::precompiles::{
    execute_precompile_metered, precompile_address, reader_budget, reader_gas, write_stored_order, ReadMeter,
    StoredOrder, ADDR_BALANCE_READER, ADDR_ORACLE_READER, ADDR_ORDER_BOOK_READER, ADDR_STAKING_READER,
    GAS_PRECOMPILE_READ_PER_UNIT,
};
use torus_economics::types::{ValidatorState, ValidatorStatus};
use torus_state::cf::{
    ALL_CF_NAMES, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_ORDERS, CF_NATIVE_POSITIONS,
    CF_STAKING_DELEGATIONS, CF_STAKING_PERMANENT, CF_STAKING_REWARDS, CF_STAKING_VALIDATORS,
};
use torus_state::{NativeStateOverlay, StateDb};
use torus_evm::{BlockEnvCfg, EvmExecutor, TORUS_CHAIN_ID};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// Block timestamp (s): oracle rows are written at it, so never stale.
const NOW: u64 = 1_000_000;
/// The gas a part-1 call may use (its meter budget).
const CALL_GAS: u64 = 30_000_000;
const BOOK_MARKET_BASE: u64 = 1;
const OPEN_ORDERS_MARKET: u64 = 7;

fn env(k: &str, d: u64) -> u64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn sizes() -> Vec<u64> {
    std::env::var("UB_RG_SIZES")
        .unwrap_or_else(|_| "0,1,4,16,64,256,1024".into())
        .split(',')
        .map(|s| s.trim().parse().expect("UB_RG_SIZES: comma-separated integers"))
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
    db.put_cf_raw(CF_STAKING_DELEGATIONS, &key, &delegation_row(staker, validator, 1_000)).unwrap();
}

fn put_position(db: &StateDb, trader: &Address, m: u64) {
    let pos = Position {
        trader: *trader,
        market_id: m,
        is_long: true,
        size: fp(3),
        entry_price: fp(100),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    };
    db.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(trader, m), &borsh::to_vec(&pos).unwrap()).unwrap();
}

fn put_balance(db: &StateDb, trader: &Address) {
    let b = NativeBalance { available: fp(1_000), order_margin: fp(5) };
    db.put_cf_raw(CF_NATIVE_BALANCES, trader.as_slice(), &borsh::to_vec(&b).unwrap()).unwrap();
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
        &AccountInfo { balance: U256::from(1_000_000u64), nonce: 1, code_hash, code: None, account_id: None },
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
    units: u64,
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

/// One call through the EVM provider's entry (metered, journaled overlay).
fn call_once(ov: &NativeStateOverlay, id: u16, inp: &[u8]) -> (u64, u64, bool) {
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
    (ns, meter.used(), ok)
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
        let mut units = None;
        let mut ok_all = true;
        let mut record = |(t, u, ok): (u64, u64, bool), ns: &mut Vec<u64>| {
            assert!(units.is_none() || units == Some(u), "{}: targets of N={} differ in units", g.case, g.n);
            units = Some(u);
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
        let units = units.unwrap_or(0);
        let med = median(&mut ns);
        let gas = reader_gas(units);
        println!(
            "ROW scen={scen} case={} state={state} N={} units={units} gas={gas} ok={ok_all} samples={} ns={med:.0} ns_per_gas={:.3}",
            g.case,
            g.n,
            ns.len(),
            med / gas as f64
        );
        rows.push(Row { case: g.case, state, n: g.n, units, ns: med });
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
        let rs: Vec<&Row> = rows.iter().filter(|r| r.case == case && r.state == state).collect();
        let worst = rs
            .iter()
            .map(|r| (r.ns / reader_gas(r.units) as f64, r.n))
            .fold((0.0, 0), |a, b| if b.0 > a.0 { b } else { a });
        if rs.len() < 2 {
            println!(
                "FIT case={case} state={state} fixed_ns={:.0} units={} worst_ns_per_gas={:.3}@N={}",
                rs[0].ns, rs[0].units, worst.0, worst.1
            );
            continue;
        }
        let xs: Vec<f64> = rs.iter().map(|r| r.n as f64).collect();
        let (a, b) = fit(&xs, &rs.iter().map(|r| r.ns).collect::<Vec<_>>());
        let (ua, ub) = fit(&xs, &rs.iter().map(|r| r.units as f64).collect::<Vec<_>>());
        let per_unit = if ub > 0.0 { b / ub } else { 0.0 };
        println!(
            "FIT case={case} state={state} fixed_ns={a:.0} ns_per_N={b:.1} units_fixed={ua:.1} units_per_N={ub:.2} \
             ns_per_unit={per_unit:.1} asym_ns_per_gas={:.3} worst_ns_per_gas={:.3}@N={}",
            per_unit / GAS_PRECOMPILE_READ_PER_UNIT as f64,
            worst.0,
            worst.1
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
/// STATICCALL `target` with `stipend` gas until less than ~stipend is left;
/// a failed inner call reverts the tx. `vary`: +1 on argument word 0 per call.
fn loop_code(target: u16, stipend: u32, vary: bool) -> Vec<u8> {
    let thr = stipend as u64 * 65 / 63 + 10_000;
    let mut c = vec![0x36, 0x60, 0x00, 0x60, 0x00, 0x37, 0x5b]; // CALLDATACOPY(0,0,size); JUMPDEST @6
    c.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x36, 0x60, 0x00]); // retSize retOff argsSize argsOff
    c.push(0x61);
    c.extend_from_slice(&target.to_be_bytes());
    c.push(0x63);
    c.extend_from_slice(&stipend.to_be_bytes());
    c.extend_from_slice(&[0xfa, 0x15, 0x60]); // STATICCALL ISZERO PUSH1 <fail>
    let fail_at = c.len();
    c.extend_from_slice(&[0x00, 0x57]); // JUMPI
    if vary {
        c.extend_from_slice(&[0x60, 0x04, 0x51, 0x60, 0x01, 0x01, 0x60, 0x04, 0x52]);
    }
    c.extend_from_slice(&[0x5a, 0x63]);
    c.extend_from_slice(&(thr as u32).to_be_bytes());
    c.extend_from_slice(&[0x10, 0x60, 0x06, 0x57, 0x00]); // LT PUSH1 6 JUMPI STOP
    c[fail_at] = c.len() as u8;
    c.extend_from_slice(&[0x5b, 0x60, 0x00, 0x60, 0x00, 0xfd]); // fail: REVERT(0,0)
    assert!(c.len() < 256);
    c
}

/// SLOAD loop: `cold` loads slot i (i = 0, 1, ...), else slot 0 every time.
fn sload_code(cold: bool) -> Vec<u8> {
    let mut c = vec![0x5b];
    if cold {
        c.extend_from_slice(&[0x60, 0x00, 0x51, 0x80, 0x54, 0x50, 0x60, 0x01, 0x01, 0x60, 0x00, 0x52]);
    } else {
        c.extend_from_slice(&[0x60, 0x00, 0x54, 0x50]);
    }
    c.extend_from_slice(&[0x5a, 0x63, 0x00, 0x00, 0x27, 0x10, 0x10, 0x60, 0x00, 0x57, 0x00]);
    c
}

fn run_tx(db: &StateDb, code: &[u8], data: &[u8], reps: u64) -> (u64, f64, bool) {
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
        ms.push(t.elapsed().as_micros() as u64);
        gas = r.gas_used;
        ok &= r.success;
    }
    (gas, median(&mut ms) / 1_000.0, ok)
}

#[allow(clippy::too_many_arguments)]
fn e2e(db: &StateDb, scen: &str, case: &str, n: u64, id: u16, data: &[u8], units: u64, vary: bool, reps: u64) {
    let stipend = (reader_gas(units) + 5_000) as u32;
    let (gas, ms, ok) = run_tx(db, &loop_code(id, stipend, vary), data, reps);
    let ns_gas = ms * 1e6 / gas as f64;
    println!(
        "E2E scen={scen} case={case} N={n} vary={vary} units={units} stipend={stipend} ok={ok} gas_used={gas} \
         ms={ms:.2} ns_per_gas={ns_gas:.3} ms_at_30M={:.1}",
        ns_gas * 30.0
    );
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

fn open(dir: &tempfile::TempDir) -> StateDb {
    StateDb::open(dir.path()).expect("open db")
}

fn scenario_p(sizes: &[u64], k: u64, filler: u64, work: u64, e2e_on: bool, reps: u64) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let t = Instant::now();
    for i in 0..filler {
        let tr = addr(9, i);
        let o = StoredOrder { order_id: i as u128, price: fp(100), remaining_qty: fp(1), side: 0 };
        write_stored_order(&db, &tr, 1 + i % 50, &o).unwrap();
        put_delegation(&db, &addr(10, i), &addr(11, i % 64));
        put_position(&db, &addr(12, i), 1 + i % 50);
        put_balance(&db, &addr(13, i));
    }
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(OPEN_ORDERS_MARKET), &agg_row(105)).unwrap();
    let mut groups = Vec::new();
    for (si, &n) in sizes.iter().enumerate() {
        let (mut oo, mut st, mut tomb) = (Vec::new(), Vec::new(), Vec::new());
        for kk in 0..k {
            let idx = si as u64 * k + kk;
            let tr = addr(1, idx);
            for j in 0..n {
                let o = StoredOrder { order_id: j as u128 + 1, price: fp(100 + j as i64), remaining_qty: fp(1), side: 0 };
                write_stored_order(&db, &tr, OPEN_ORDERS_MARKET, &o).unwrap();
            }
            oo.push(input("getOpenOrders(address,bytes32)", &[addr_word(&tr), u64_word(OPEN_ORDERS_MARKET)]));
            // N orders placed and cancelled: N deletion markers under the
            // prefix, stepped over uncharged until a compaction drops them.
            let tt = addr(14, idx);
            for j in 0..n {
                let mut key = [0u8; 44];
                key[..20].copy_from_slice(tt.as_slice());
                key[20..28].copy_from_slice(&OPEN_ORDERS_MARKET.to_be_bytes());
                key[28..44].copy_from_slice(&(j as u128 + 1).to_be_bytes());
                db.put_cf_raw(CF_NATIVE_ORDERS, &key, &[0u8; 49]).unwrap();
                db.delete_cf_raw(CF_NATIVE_ORDERS, &key).unwrap();
            }
            tomb.push(input("getOpenOrders(address,bytes32)", &[addr_word(&tt), u64_word(OPEN_ORDERS_MARKET)]));
            let sk = addr(2, idx);
            for j in 0..n {
                put_delegation(&db, &sk, &addr(3, j));
            }
            let mut perm = sk.as_slice().to_vec();
            perm.extend_from_slice(&U256::from(7u64).to_be_bytes::<32>());
            perm.extend_from_slice(&5u64.to_le_bytes());
            db.put_cf_raw(CF_STAKING_PERMANENT, sk.as_slice(), &perm).unwrap();
            db.put_cf_raw(CF_STAKING_REWARDS, sk.as_slice(), &perm[..52]).unwrap();
            st.push(input("getStakingInfo(address)", &[addr_word(&sk)]));
        }
        groups.push(Group { case: "getOpenOrders", id: ADDR_ORDER_BOOK_READER, n, inputs: oo });
        groups.push(Group { case: "getStakingInfo", id: ADDR_STAKING_READER, n, inputs: st });
        groups.push(Group { case: "getOpenOrders-tombstones", id: ADDR_ORDER_BOOK_READER, n, inputs: tomb });
    }
    let points = k * 4;
    let (mut gp, mut gb, mut gpr) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..points {
        let tr = addr(4, i);
        put_position(&db, &tr, OPEN_ORDERS_MARKET);
        gp.push(input("getPosition(address,bytes32)", &[addr_word(&tr), u64_word(OPEN_ORDERS_MARKET)]));
        let tb = addr(5, i);
        put_balance(&db, &tb);
        put_evm_account(&db, &tb, None);
        gb.push(input("getBalances(address)", &[addr_word(&tb)]));
        db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(1_000 + i), &agg_row(100 + i as i64)).unwrap();
        gpr.push(input("getPrice(bytes32)", &[u64_word(1_000 + i)]));
    }
    groups.push(Group { case: "getPosition", id: ADDR_ORDER_BOOK_READER, n: 1, inputs: gp });
    groups.push(Group { case: "getBalances", id: ADDR_BALANCE_READER, n: 1, inputs: gb });
    groups.push(Group { case: "getPrice", id: ADDR_ORACLE_READER, n: 1, inputs: gpr });
    println!("SETUP scen=P filler={filler} K={k} ms={}", t.elapsed().as_millis());

    let db = run_states(dir.path(), db, &groups, "P", work);
    if e2e_on {
        let big = *sizes.iter().max().unwrap();
        let ov = NativeStateOverlay::new(db.clone());
        for g in &groups {
            if g.n == big || g.n == 1 || g.n == 0 {
                let (_, units, _) = call_once(&ov, g.id, &g.inputs[0]);
                e2e(&db, "P", g.case, g.n, g.id, &g.inputs[0], units, false, reps);
            }
        }
        // vary: a new (mostly empty) trader / staker each call.
        for (case, id, inp) in [
            ("getOpenOrders", ADDR_ORDER_BOOK_READER, &groups[0].inputs[0]),
            ("getStakingInfo", ADDR_STAKING_READER, &groups[1].inputs[0]),
        ] {
            e2e(&db, "P", case, 0, id, inp, 100, true, reps);
        }
        let gp = groups.iter().find(|g| g.case == "getPosition").unwrap();
        e2e(&db, "P", "getPosition", 0, gp.id, &gp.inputs[0], 100, true, reps);

        // SLOAD price point: slots 0..40k exist (cold loop reads ~14k of them).
        for s in 0..40_000u64 {
            db.put_storage(&LOOP_CONTRACT, &U256::from(s), &U256::from(s + 1)).unwrap();
        }
        for cold in [true, false] {
            let (gas, ms, ok) = run_tx(&db, &sload_code(cold), &[], reps);
            let ns_gas = ms * 1e6 / gas as f64;
            println!(
                "E2E scen=P case=SLOAD-{} N=0 vary={cold} units=0 stipend=0 ok={ok} gas_used={gas} ms={ms:.2} \
                 ns_per_gas={ns_gas:.3} ms_at_30M={:.1}",
                if cold { "cold" } else { "warm" },
                ns_gas * 30.0
            );
        }
    }
}

fn scenario_book(mode: BookMode, scen: &str, sizes: &[u64], k: u64, work: u64, e2e_on: bool, reps: u64) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let t = Instant::now();
    let mut ctx = NativeExecContext::new_with_mode(
        db.clone(),
        1,
        1_001,
        0,
        100,
        10,
        Address::new([99; 20]),
        Address::new([100; 20]),
        Address::new([101; 20]),
        mode,
        None,
    );
    let mut block = Vec::new();
    let mut groups = Vec::new();
    for (si, &n) in sizes.iter().enumerate() {
        let mut inputs = Vec::new();
        for kk in 0..k {
            let m = BOOK_MARKET_BASE + si as u64 * k + kk;
            for j in 0..n {
                // One maker per 512 orders (an account may hold 1,000 open orders).
                let maker = addr(6, m * 1_000 + j / 512);
                if j % 512 == 0 {
                    ctx.positions
                        .put_native_balance(&maker, &NativeBalance { available: fp(1_000_000_000), order_margin: FixedPoint::ZERO })
                        .unwrap();
                }
                let price = if mode == BookMode::OrderRows { 1_000 } else { 1_000 + j as i64 };
                block.push((
                    maker,
                    NativeAction::PlaceOrder(PlaceOrderParams {
                        market_id: m,
                        is_buy: true,
                        price: fp(price),
                        quantity: fp(1),
                        order_type: OrderType::Limit,
                        time_in_force: TimeInForce::GTC,
                        reduce_only: false,
                        client_order_id: None,
                    }),
                ));
            }
            inputs.push(input("getOrderBook(bytes32)", &[u64_word(m)]));
        }
        groups.push(Group { case: "getOrderBook", id: ADDR_ORDER_BOOK_READER, n, inputs });
    }
    let r = NativeExecutor::execute_batch(&mut ctx, &block);
    let failed: Vec<_> = r.results.iter().enumerate().filter(|(_, x)| !x.success).take(3).collect();
    assert!(failed.is_empty(), "{scen}: seed block failed ({} of {}): {:?}", failed.len(), block.len(),
        failed.iter().map(|(i, x)| (i, x.error.clone())).collect::<Vec<_>>());
    ctx.save_order_books();
    drop(ctx);
    println!("SETUP scen={scen} K={k} orders={} ms={}", block.len(), t.elapsed().as_millis());

    let db = run_states(dir.path(), db, &groups, scen, work);
    if e2e_on {
        let big = *sizes.iter().max().unwrap();
        let ov = NativeStateOverlay::new(db.clone());
        for g in &groups {
            if g.n == big || g.n == 1 {
                let (_, units, _) = call_once(&ov, g.id, &g.inputs[0]);
                // Classic reverts (the blob is not an OrderBookSnapshot): the
                // loop needs successful calls, so classic runs part 1 only.
                if mode != BookMode::Classic {
                    e2e(&db, scen, g.case, g.n, g.id, &g.inputs[0], units, false, reps);
                }
            }
        }
        if mode != BookMode::Classic {
            // vary walks markets 1, 2, ... (the seeded ones first): a stipend
            // for the deepest book keeps every call in budget.
            let g = &groups[0];
            let deepest = groups.last().unwrap();
            let (_, max_units, _) = call_once(&ov, deepest.id, &deepest.inputs[0]);
            e2e(&db, scen, "getOrderBook", 0, g.id, &g.inputs[0], max_units, true, reps);
        }
    }
}

fn scenario_global(n: u64, work: u64) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    for m in 1..=n {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &[1u8; 64]).unwrap();
        db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(m), &agg_row(100 + m as i64)).unwrap();
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
        db.put_cf_raw(CF_STAKING_VALIDATORS, v.as_slice(), &borsh::to_vec(&vs).unwrap()).unwrap();
    }
    let groups = vec![
        Group { case: "getMarkets", id: ADDR_BALANCE_READER, n, inputs: vec![input("getMarkets()", &[])] },
        Group { case: "getAllPrices", id: ADDR_ORACLE_READER, n, inputs: vec![input("getAllPrices()", &[])] },
        Group { case: "getValidators", id: ADDR_STAKING_READER, n, inputs: vec![input("getValidators()", &[])] },
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
    let sizes = sizes();
    let k = env("UB_RG_TARGETS", 16);
    let work = env("UB_RG_WORK", 200_000);
    let filler = env("UB_RG_FILLER", 300_000);
    let e2e_on = env("UB_RG_E2E", 1) == 1;
    let reps = env("UB_RG_E2E_REPS", 3);
    println!(
        "CONFIG sizes={sizes:?} K={k} work={work} filler={filler} e2e={e2e_on} e2e_reps={reps} \
         base_gas={} gas_per_unit={GAS_PRECOMPILE_READ_PER_UNIT}",
        reader_gas(0)
    );
    let t = Instant::now();
    scenario_p(&sizes, k, filler, work, e2e_on, reps);
    scenario_book(BookMode::Classic, "B-classic", &sizes, k, work, e2e_on, reps);
    scenario_book(BookMode::OrderRows, "B-rows1", &sizes, k, work, e2e_on, reps);
    scenario_book(BookMode::LevelAuthority, "B-rows2", &sizes, k, work, e2e_on, reps);
    // Whole-CF scans: one DB per N; FIT over the G rows of all N.
    for &n in &sizes {
        scenario_global(n, work);
    }
    println!("DONE total_s={:.1}", t.elapsed().as_secs_f64());
}
