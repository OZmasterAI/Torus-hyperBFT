//! R02 branch 6b (audit main-chain-deep-review-2026-10-08, inventory
//! section 1b, the session and oracle rows): a LOCAL storage fault (the
//! read or write failed on this node, or a row it stored does not decode)
//! on an action's own state must fail-stop the block (`take_fatal_error`,
//! the committer's check), never become an ordinary rejected-action result
//! that this node alone records:
//! - `exec_create_session`: the owner's session count, the session read,
//!   the session write;
//! - `exec_revoke_session`: the session read and delete;
//! - `exec_submit_oracle_prices` / `resolve_oracle_reporter`: the reporter's
//!   validator record, the signer index and the indexed validator's record
//!   ("not a validator" stays an ordinary reject), the market listing, the
//!   stored sample time, and the submission write (also mid-loop);
//! - `exec_set_oracle_signer`: the validator records, the signer index, and
//!   the index delete / put and validator writes.
//!
//! The action's result keeps its text and shape (the block never flushes,
//! so writes made before the fault are discarded with the overlay). Errors
//! every validator hits alike (session limit, duplicate key, not a
//! validator, unlisted market, ...) keep today's result and never fail-stop;
//! fault-free runs are unchanged.

use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, U256};

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_state::cf::{
    oracle_signer_key, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_SESSIONS, CF_STAKING_VALIDATORS,
    ORACLE_SUBMISSION_PREFIX,
};
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::eip712::sign_oracle_signer_proof;
use torus_types::{
    FixedPoint, MarketId, NativeAction, OracleSignerProof, OracleSubmission, SessionData,
    SessionScope,
};

/// Armed `(cf, key)` rows.
type Armed = Arc<Mutex<Vec<(&'static str, Vec<u8>)>>>;

/// A backend whose point reads (`reads`), writes (`writes`), deletes
/// (`deletes`) of the armed `(cf, key)` rows, or scans of the armed column
/// families (`scans`) fail; `hits` counts the injected failures. Everything
/// else delegates.
#[derive(Clone)]
struct Failing {
    inner: StateDb,
    reads: Armed,
    writes: Armed,
    deletes: Armed,
    scans: Arc<Mutex<Vec<&'static str>>>,
    hits: Arc<Mutex<usize>>,
}

const INJECTED_READ: &str = "injected action read failure";
const INJECTED_WRITE: &str = "injected action write failure";
const INJECTED_DELETE: &str = "injected action delete failure";
const INJECTED_SCAN: &str = "injected action scan failure";

impl Failing {
    fn arm_read(&self, cf: &'static str, key: &[u8]) {
        self.reads.lock().unwrap().push((cf, key.to_vec()));
    }
    fn arm_write(&self, cf: &'static str, key: &[u8]) {
        self.writes.lock().unwrap().push((cf, key.to_vec()));
    }
    fn arm_delete(&self, cf: &'static str, key: &[u8]) {
        self.deletes.lock().unwrap().push((cf, key.to_vec()));
    }
    fn arm_scan(&self, cf: &'static str) {
        self.scans.lock().unwrap().push(cf);
    }
    fn hits(&self) -> usize {
        *self.hits.lock().unwrap()
    }
    fn armed(list: &Mutex<Vec<(&'static str, Vec<u8>)>>, cf: &str, key: &[u8]) -> bool {
        list.lock()
            .unwrap()
            .iter()
            .any(|(c, k)| *c == cf && k.as_slice() == key)
    }
    fn fail(&self, what: &str) -> StateError {
        *self.hits.lock().unwrap() += 1;
        StateError::Io(std::io::Error::other(what.to_string()))
    }
}

impl StateBackend for Failing {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if Self::armed(&self.reads, cf, key) {
            return Err(self.fail(INJECTED_READ));
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        if Self::armed(&self.writes, cf, key) {
            return Err(self.fail(INJECTED_WRITE));
        }
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        if Self::armed(&self.deletes, cf, key) {
            return Err(self.fail(INJECTED_DELETE));
        }
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        if self.scans.lock().unwrap().contains(&cf) {
            return Err(self.fail(INJECTED_SCAN));
        }
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        for op in ops {
            if let AtomicWriteOp::Put { cf, key, .. } = op {
                if Self::armed(&self.writes, cf, key) {
                    return Err(self.fail(INJECTED_WRITE));
                }
            }
        }
        self.inner.atomic_write(ops)
    }
}

/// Block time (s).
const NOW: u64 = 1_001;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

type Ctx = NativeExecContext<Failing>;

fn setup() -> (tempfile::TempDir, StateDb, Failing, Ctx) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let backend = Failing {
        inner: db.clone(),
        reads: Arc::default(),
        writes: Arc::default(),
        deletes: Arc::default(),
        scans: Arc::default(),
        hits: Arc::default(),
    };
    let ctx = NativeExecContext::new(
        backend.clone(),
        2,
        NOW,
        0,
        1_000,
        10,
        addr(99),
        addr(100),
        addr(101),
    );
    (dir, db, backend, ctx)
}

fn exec(ctx: &mut Ctx, sender: Address, action: &NativeAction) -> NativeActionResult {
    NativeExecutor::execute(ctx, &sender, action)
}

/// The block fail-stops on the injected fault, recorded by `step` (the site).
fn assert_injected(ctx: &mut Ctx, step: &str, injected: &str) {
    let reason = ctx
        .take_fatal_error()
        .unwrap_or_else(|| panic!("{step}: must fail-stop"));
    assert!(
        reason.starts_with(&format!("{step}: ")),
        "recorded by {step}: {reason}"
    );
    assert!(reason.contains(injected), "{step}: {reason}");
    assert_eq!(ctx.take_fatal_error(), None, "{step}: taken once");
}

/// The action is rejected with an error containing `needle` (text kept).
fn assert_rejected(r: &NativeActionResult, needle: &str) {
    assert!(!r.success, "expected a rejection containing {needle:?}: {r:?}");
    let e = r.error.as_deref().unwrap_or("");
    assert!(e.contains(needle), "error {e:?} lacks {needle:?}");
}

/// A deterministic reject: the text kept, no fail-stop.
fn assert_plain_reject(ctx: &mut Ctx, r: &NativeActionResult, needle: &str) {
    assert_rejected(r, needle);
    assert_eq!(ctx.take_fatal_error(), None, "{needle}: no fail-stop");
}

// ---------------------------------------------------------------------------
// 1. Sessions.
// ---------------------------------------------------------------------------

const OWNER: u8 = 1;
const KEY: [u8; 32] = [42; 32];

fn create(key: [u8; 32]) -> NativeAction {
    NativeAction::CreateSession {
        session_pubkey: key,
        expiry: NOW * 1_000 + 60_000,
        scope: SessionScope::Trading,
    }
}

fn revoke(key: [u8; 32]) -> NativeAction {
    NativeAction::RevokeSession {
        session_pubkey: key,
    }
}

fn put_session(db: &StateDb, key: [u8; 32], owner: Address) {
    db.put_session(
        &key,
        &SessionData {
            owner,
            expiry: NOW * 1_000 + 60_000,
            scope: SessionScope::Trading,
            created_at: NOW * 1_000,
        },
    )
    .unwrap();
}

/// The session-count scan fails. RED before R02: the create was rejected
/// "state error: ..." and the block went on.
#[test]
fn r02_create_session_count_scan_fault_fail_stops() {
    let (_d, db, backend, mut ctx) = setup();
    backend.arm_scan(CF_SESSIONS);
    let r = exec(&mut ctx, addr(OWNER), &create(KEY));
    assert!(backend.hits() > 0, "fault injected");
    assert_rejected(&r, &format!("state error: io: {INJECTED_SCAN}"));
    assert_injected(&mut ctx, "create session count read", INJECTED_SCAN);
    assert_eq!(db.get_session(&KEY).unwrap(), None, "nothing written");
}

#[test]
fn r02_create_session_read_fault_fail_stops() {
    let (_d, _db, backend, mut ctx) = setup();
    backend.arm_read(CF_SESSIONS, &KEY);
    let r = exec(&mut ctx, addr(OWNER), &create(KEY));
    assert_rejected(&r, &format!("state error: io: {INJECTED_READ}"));
    assert_injected(&mut ctx, "create session read", INJECTED_READ);
}

/// The existing row under the key does not decode (a local fault: session
/// rows are only written by `put_session`). The count skips it (its
/// decode-as-absent policy, unchanged); the point read fails.
#[test]
fn r02_create_session_undecodable_row_fail_stops() {
    let (_d, db, _backend, mut ctx) = setup();
    db.put_cf_raw(CF_SESSIONS, &KEY, b"\xff").unwrap();
    let r = exec(&mut ctx, addr(OWNER), &create(KEY));
    assert_rejected(&r, "state error: invalid data");
    let reason = ctx.take_fatal_error().expect("must fail-stop");
    assert!(
        reason.starts_with("create session read: state error: invalid data"),
        "{reason}"
    );
}

#[test]
fn r02_create_session_write_fault_fail_stops() {
    let (_d, _db, backend, mut ctx) = setup();
    backend.arm_write(CF_SESSIONS, &KEY);
    let r = exec(&mut ctx, addr(OWNER), &create(KEY));
    assert_rejected(&r, &format!("store failed: io: {INJECTED_WRITE}"));
    assert_injected(&mut ctx, "create session write", INJECTED_WRITE);
}

/// Controls: the session limit, a duplicate key and a bad expiry are
/// ordinary rejects (text kept, no fail-stop); a fresh session is created.
#[test]
fn r02_create_session_deterministic_rejects_unchanged() {
    let (_d, db, backend, mut ctx) = setup();
    let r = exec(&mut ctx, addr(OWNER), &create(KEY));
    assert!(r.success, "{r:?}");
    assert_eq!(ctx.take_fatal_error(), None);
    assert_eq!(db.get_session(&KEY).unwrap().unwrap().owner, addr(OWNER));
    let r = exec(&mut ctx, addr(2), &create(KEY));
    assert_plain_reject(&mut ctx, &r, "session key already exists");
    for k in 1..=4u8 {
        put_session(&db, [k; 32], addr(OWNER));
    }
    let r = exec(&mut ctx, addr(OWNER), &create([9; 32]));
    assert_plain_reject(&mut ctx, &r, "max 5 active sessions exceeded");
    let expired = NativeAction::CreateSession {
        session_pubkey: [8; 32],
        expiry: NOW * 1_000,
        scope: SessionScope::Trading,
    };
    let r = exec(&mut ctx, addr(2), &expired);
    assert_plain_reject(&mut ctx, &r, "session already expired");
    assert_eq!(backend.hits(), 0);
}

#[test]
fn r02_revoke_session_read_fault_fail_stops() {
    let (_d, db, backend, mut ctx) = setup();
    put_session(&db, KEY, addr(OWNER));
    backend.arm_read(CF_SESSIONS, &KEY);
    let r = exec(&mut ctx, addr(OWNER), &revoke(KEY));
    assert_rejected(&r, &format!("state error: io: {INJECTED_READ}"));
    assert_injected(&mut ctx, "revoke session read", INJECTED_READ);
}

#[test]
fn r02_revoke_session_delete_fault_fail_stops() {
    let (_d, db, backend, mut ctx) = setup();
    put_session(&db, KEY, addr(OWNER));
    backend.arm_delete(CF_SESSIONS, &KEY);
    let r = exec(&mut ctx, addr(OWNER), &revoke(KEY));
    assert_rejected(&r, &format!("delete failed: io: {INJECTED_DELETE}"));
    assert_injected(&mut ctx, "revoke session delete", INJECTED_DELETE);
}

/// Controls: not found and not the owner are ordinary rejects; the owner's
/// revoke succeeds.
#[test]
fn r02_revoke_session_deterministic_rejects_unchanged() {
    let (_d, db, backend, mut ctx) = setup();
    let r = exec(&mut ctx, addr(OWNER), &revoke(KEY));
    assert_plain_reject(&mut ctx, &r, "session not found");
    put_session(&db, KEY, addr(OWNER));
    let r = exec(&mut ctx, addr(2), &revoke(KEY));
    assert_plain_reject(&mut ctx, &r, "not session owner");
    let r = exec(&mut ctx, addr(OWNER), &revoke(KEY));
    assert!(r.success, "{r:?}");
    assert_eq!(ctx.take_fatal_error(), None);
    assert_eq!(db.get_session(&KEY).unwrap(), None);
    assert_eq!(backend.hits(), 0);
}

// ---------------------------------------------------------------------------
// 2. Oracle submissions and the hot signer.
// ---------------------------------------------------------------------------

const V1: u8 = 1;
const V2: u8 = 2;
/// Hot signers: the addresses of keys `[50; 32]` / `[51; 32]`
/// (oracle_signer_tests.rs).
const S: Address = alloy_primitives::address!("94622cc2a5b64a58c25a129d48a2beec4b65b779");
const S2: Address = alloy_primitives::address!("5cbdd86a2fa8dc4bddd8a8f69dba48572eec07fb");

fn skey(n: u8) -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_slice(&[n; 32]).unwrap()
}

fn proof_for(validator: Address, signer: Address) -> OracleSignerProof {
    let key = match signer {
        S => skey(50),
        S2 => skey(51),
        _ => skey(99),
    };
    sign_oracle_signer_proof(&validator, 7, &key)
}

fn put_validator(db: &StateDb, n: u8, status: ValidatorStatus, signer: Option<Address>) {
    StakingManager::new(db.clone())
        .put_validator(
            &addr(n),
            &ValidatorState {
                address: addr(n),
                pubkey: [n; 32],
                commission_bps: 0,
                self_stake: MIN_SELF_DELEGATION,
                total_delegated: U256::ZERO,
                status,
                jailed_until: None,
                last_commission_change_block: None,
                oracle_signer: signer,
            },
        )
        .unwrap();
}

/// V1 and V2 active validators; markets 1 and 2 listed.
fn oracle_setup() -> (tempfile::TempDir, StateDb, Failing, Ctx) {
    let (d, db, backend, ctx) = setup();
    put_validator(&db, V1, ValidatorStatus::Active, None);
    put_validator(&db, V2, ValidatorStatus::Active, None);
    for m in [1u64, 2] {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed")
            .unwrap();
    }
    (d, db, backend, ctx)
}

/// `V1` with hot signer `S` (record and index).
fn register_signer(db: &StateDb) {
    put_validator(db, V1, ValidatorStatus::Active, Some(S));
    db.put_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&S), addr(V1).as_slice())
        .unwrap();
}

fn submit(prices: &[(MarketId, i64)]) -> NativeAction {
    NativeAction::SubmitOraclePrices(OracleSubmission {
        prices: prices.iter().map(|&(m, p)| (m, fp(p))).collect(),
        timestamp: NOW * 1_000,
    })
}

fn sub_key(market: MarketId, v: Address) -> Vec<u8> {
    let mut k = ORACLE_SUBMISSION_PREFIX.to_vec();
    k.extend_from_slice(&market.to_be_bytes());
    k.extend_from_slice(v.as_slice());
    k
}

fn set(sender: u8, signer: Address) -> NativeAction {
    let proof = (signer != Address::ZERO).then(|| proof_for(addr(sender), signer));
    NativeAction::SetOracleSigner { signer, proof }
}

/// The sender's own validator record does not read. RED before R02: the
/// `Err(String)` of `resolve_oracle_reporter` became a plain reject.
#[test]
fn r02_oracle_reporter_validator_read_fault_fail_stops() {
    let (_d, _db, backend, mut ctx) = oracle_setup();
    backend.arm_read(CF_STAKING_VALIDATORS, addr(V1).as_slice());
    let r = exec(&mut ctx, addr(V1), &submit(&[(1, 100)]));
    assert_rejected(&r, &format!("state error: io: {INJECTED_READ}"));
    assert_injected(&mut ctx, "oracle reporter validator read", INJECTED_READ);
}

/// The hot-signer index read fails (sender is not a validator itself).
#[test]
fn r02_oracle_signer_index_read_fault_fail_stops() {
    let (_d, db, backend, mut ctx) = oracle_setup();
    register_signer(&db);
    backend.arm_read(CF_NATIVE_ORACLE, &oracle_signer_key(&S));
    let r = exec(&mut ctx, S, &submit(&[(1, 100)]));
    assert_rejected(&r, &format!("io: {INJECTED_READ}"));
    assert_injected(&mut ctx, "oracle signer index read", INJECTED_READ);
}

/// The indexed validator's record does not read.
#[test]
fn r02_oracle_signer_validator_read_fault_fail_stops() {
    let (_d, db, backend, mut ctx) = oracle_setup();
    register_signer(&db);
    backend.arm_read(CF_STAKING_VALIDATORS, addr(V1).as_slice());
    let r = exec(&mut ctx, S, &submit(&[(1, 100)]));
    assert_rejected(&r, &format!("state error: io: {INJECTED_READ}"));
    assert_injected(&mut ctx, "oracle signer validator read", INJECTED_READ);
}

/// An undecodable validator record (a local fault) fail-stops.
#[test]
fn r02_oracle_reporter_undecodable_validator_fail_stops() {
    let (_d, db, _backend, mut ctx) = oracle_setup();
    db.put_cf_raw(CF_STAKING_VALIDATORS, addr(V1).as_slice(), b"\xff")
        .unwrap();
    let r = exec(&mut ctx, addr(V1), &submit(&[(1, 100)]));
    assert!(!r.success, "{r:?}");
    let reason = ctx.take_fatal_error().expect("must fail-stop");
    assert!(
        reason.starts_with("oracle reporter validator read: "),
        "{reason}"
    );
}

#[test]
fn r02_oracle_market_read_fault_fail_stops() {
    let (_d, _db, backend, mut ctx) = oracle_setup();
    backend.arm_read(CF_NATIVE_MARKETS, &2u64.to_be_bytes());
    let r = exec(&mut ctx, addr(V1), &submit(&[(1, 100), (2, 50)]));
    assert_rejected(&r, &format!("state error: io: {INJECTED_READ}"));
    assert_injected(&mut ctx, "oracle market read", INJECTED_READ);
}

#[test]
fn r02_oracle_sample_read_fault_fail_stops() {
    let (_d, _db, backend, mut ctx) = oracle_setup();
    backend.arm_read(CF_NATIVE_ORACLE, &sub_key(1, addr(V1)));
    let r = exec(&mut ctx, addr(V1), &submit(&[(1, 100)]));
    assert_rejected(&r, &format!("state error: io: {INJECTED_READ}"));
    assert_injected(&mut ctx, "oracle sample read", INJECTED_READ);
}

/// The stored submission row does not decode (a local fault).
#[test]
fn r02_oracle_undecodable_sample_fail_stops() {
    let (_d, db, _backend, mut ctx) = oracle_setup();
    db.put_cf_raw(CF_NATIVE_ORACLE, &sub_key(1, addr(V1)), b"\xff")
        .unwrap();
    let r = exec(&mut ctx, addr(V1), &submit(&[(1, 100)]));
    assert_rejected(&r, "borsh");
    let reason = ctx.take_fatal_error().expect("must fail-stop");
    assert!(reason.starts_with("oracle sample read: borsh"), "{reason}");
}

/// The second market's submission write fails after the first was written
/// (partial rows): the block fail-stops, so the committer discards the
/// overlay with both.
#[test]
fn r02_oracle_submission_write_fault_mid_loop_fail_stops() {
    let (_d, db, backend, mut ctx) = oracle_setup();
    backend.arm_write(CF_NATIVE_ORACLE, &sub_key(2, addr(V1)));
    let r = exec(&mut ctx, addr(V1), &submit(&[(1, 100), (2, 50)]));
    assert_rejected(&r, &format!("state error: io: {INJECTED_WRITE}"));
    assert!(
        db.get_cf_raw(CF_NATIVE_ORACLE, &sub_key(1, addr(V1)))
            .unwrap()
            .is_some(),
        "the first row was written before the fault"
    );
    assert_injected(&mut ctx, "oracle submission write", INJECTED_WRITE);
}

/// Controls: not a validator / signer, an inactive validator, an unlisted
/// market, a stale index entry and a bad sample time are ordinary rejects;
/// a validator's and a signer's submissions succeed.
#[test]
fn r02_oracle_submit_deterministic_rejects_unchanged() {
    let (_d, db, backend, mut ctx) = oracle_setup();
    let r = exec(&mut ctx, addr(77), &submit(&[(1, 100)]));
    assert_plain_reject(
        &mut ctx,
        &r,
        &format!("{} is not a registered validator or oracle signer", addr(77)),
    );
    put_validator(&db, 3, ValidatorStatus::Jailed, None);
    let r = exec(&mut ctx, addr(3), &submit(&[(1, 100)]));
    assert_plain_reject(&mut ctx, &r, "is not active");
    let r = exec(&mut ctx, addr(V1), &submit(&[(5, 100)]));
    assert_plain_reject(&mut ctx, &r, "market 5 is not listed");
    // A stale index entry (V2 never named S): not a reporter.
    db.put_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&S), addr(V2).as_slice())
        .unwrap();
    let r = exec(&mut ctx, S, &submit(&[(1, 100)]));
    assert_plain_reject(&mut ctx, &r, "is not a registered validator or oracle signer");
    let late = NativeAction::SubmitOraclePrices(OracleSubmission {
        prices: vec![(1, fp(100))],
        timestamp: 1,
    });
    let r = exec(&mut ctx, addr(V1), &late);
    assert_plain_reject(&mut ctx, &r, "more than");
    let r = exec(&mut ctx, addr(V1), &submit(&[(1, 100), (2, 50)]));
    assert!(r.success, "{r:?}");
    register_signer(&db);
    let r = exec(&mut ctx, S, &submit(&[(1, 101)]));
    assert!(r.success, "{r:?}");
    assert_eq!(ctx.take_fatal_error(), None);
    assert_eq!(backend.hits(), 0);
}

#[test]
fn r02_set_oracle_signer_validator_read_fault_fail_stops() {
    let (_d, _db, backend, mut ctx) = oracle_setup();
    backend.arm_read(CF_STAKING_VALIDATORS, addr(V1).as_slice());
    let r = exec(&mut ctx, addr(V1), &set(V1, S));
    assert_rejected(&r, &format!("state error: io: {INJECTED_READ}"));
    assert_injected(&mut ctx, "set oracle signer validator read", INJECTED_READ);
}

/// The "is the signer a validator" read fails.
#[test]
fn r02_set_oracle_signer_signer_read_fault_fail_stops() {
    let (_d, _db, backend, mut ctx) = oracle_setup();
    backend.arm_read(CF_STAKING_VALIDATORS, S.as_slice());
    let r = exec(&mut ctx, addr(V1), &set(V1, S));
    assert_rejected(&r, &format!("state error: io: {INJECTED_READ}"));
    assert_injected(&mut ctx, "set oracle signer signer read", INJECTED_READ);
}

#[test]
fn r02_set_oracle_signer_index_read_fault_fail_stops() {
    let (_d, _db, backend, mut ctx) = oracle_setup();
    backend.arm_read(CF_NATIVE_ORACLE, &oracle_signer_key(&S));
    let r = exec(&mut ctx, addr(V1), &set(V1, S));
    assert_rejected(&r, &format!("io: {INJECTED_READ}"));
    assert_injected(&mut ctx, "set oracle signer index read", INJECTED_READ);
}

/// Rotation S -> S2: the old index entry's delete fails.
#[test]
fn r02_set_oracle_signer_index_delete_fault_fail_stops() {
    let (_d, db, backend, mut ctx) = oracle_setup();
    register_signer(&db);
    backend.arm_delete(CF_NATIVE_ORACLE, &oracle_signer_key(&S));
    let r = exec(&mut ctx, addr(V1), &set(V1, S2));
    assert_rejected(&r, &format!("io: {INJECTED_DELETE}"));
    assert_injected(&mut ctx, "set oracle signer index delete", INJECTED_DELETE);
}

/// Rotation S -> S2: the new index entry's write fails after the old one
/// was deleted (a partial write, discarded with the overlay).
#[test]
fn r02_set_oracle_signer_index_write_fault_fail_stops() {
    let (_d, db, backend, mut ctx) = oracle_setup();
    register_signer(&db);
    backend.arm_write(CF_NATIVE_ORACLE, &oracle_signer_key(&S2));
    let r = exec(&mut ctx, addr(V1), &set(V1, S2));
    assert_rejected(&r, &format!("io: {INJECTED_WRITE}"));
    assert_eq!(
        db.get_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&S)).unwrap(),
        None,
        "the old entry was deleted before the fault"
    );
    assert_injected(&mut ctx, "set oracle signer index write", INJECTED_WRITE);
}

/// The validator record write fails after both index writes.
#[test]
fn r02_set_oracle_signer_validator_write_fault_fail_stops() {
    let (_d, _db, backend, mut ctx) = oracle_setup();
    backend.arm_write(CF_STAKING_VALIDATORS, addr(V1).as_slice());
    let r = exec(&mut ctx, addr(V1), &set(V1, S));
    assert_rejected(&r, &format!("state error: io: {INJECTED_WRITE}"));
    assert_injected(&mut ctx, "set oracle signer validator write", INJECTED_WRITE);
}

/// Controls: not a validator, tombstoned, the signer is a validator, the
/// signer already serves another validator, a bad proof are ordinary
/// rejects; set, rotate and clear succeed.
#[test]
fn r02_set_oracle_signer_deterministic_rejects_unchanged() {
    let (_d, db, backend, mut ctx) = oracle_setup();
    let r = exec(&mut ctx, addr(77), &set(77, S));
    assert_plain_reject(&mut ctx, &r, "is not a registered validator");
    put_validator(&db, 3, ValidatorStatus::Tombstoned, None);
    let r = exec(&mut ctx, addr(3), &set(3, S));
    assert_plain_reject(&mut ctx, &r, "is tombstoned");
    let r = exec(&mut ctx, addr(V1), &set(V1, addr(V2)));
    assert_plain_reject(&mut ctx, &r, "is a validator");
    let bad = NativeAction::SetOracleSigner {
        signer: S,
        proof: Some(proof_for(addr(V2), S)),
    };
    let r = exec(&mut ctx, addr(V1), &bad);
    assert_plain_reject(&mut ctx, &r, "missing or invalid proof of possession");
    let r = exec(&mut ctx, addr(V1), &set(V1, S));
    assert!(r.success, "{r:?}");
    let r = exec(&mut ctx, addr(V2), &set(V2, S));
    assert_plain_reject(&mut ctx, &r, "already serves");
    let r = exec(&mut ctx, addr(V1), &set(V1, S2));
    assert!(r.success, "{r:?}");
    let r = exec(&mut ctx, addr(V1), &set(V1, Address::ZERO));
    assert!(r.success, "{r:?}");
    assert_eq!(ctx.take_fatal_error(), None);
    assert_eq!(backend.hits(), 0);
}
