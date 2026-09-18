//! Explicit, default-off C1 crash qualification. Never releases the parked write.
//! Evidence proves a real nonce-guard parent read, not economic-state dependency.
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};
use torus_state::{
    cf::{CF_BLOCK_BODIES, CF_BLOCK_HEADERS, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT},
    StateDb,
};

pub(crate) const ENV: &str = "TORUS_C1_QUALIFICATION";

#[derive(Clone)]
pub(crate) struct Config {
    pub run_id: String,
    pub validator: String,
    pub parent: u64,
    pub nonce_key: Vec<u8>,
    pub evidence: PathBuf,
    pub timeout: Duration,
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// Linux boot clock is shared across processes and includes suspend. Its
/// millisecond floor is conservative when acceptance requires strictly < expiry.
pub(crate) fn boot_millis() -> Result<u128, String> {
    let uptime = std::fs::read_to_string("/proc/uptime").map_err(|e| e.to_string())?;
    let token = uptime
        .split_whitespace()
        .next()
        .ok_or("missing boot clock")?;
    let (seconds, fraction) = token.split_once('.').ok_or("malformed boot clock")?;
    if seconds.is_empty()
        || fraction.is_empty()
        || !seconds
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err("malformed boot clock".into());
    }
    let seconds: u128 = seconds.parse::<u128>().map_err(|e| e.to_string())?;
    let mut millis = fraction
        .bytes()
        .take(3)
        .fold(0u128, |n, b| n * 10 + (b - b'0') as u128);
    for _ in fraction.len()..3 {
        millis *= 10;
    }
    seconds
        .checked_mul(1000)
        .and_then(|n| n.checked_add(millis))
        .ok_or_else(|| "boot clock overflow".into())
}

fn unhex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() != 56 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("nonce_key must be 28 bytes of hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}
impl Config {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.len() > 4096 {
            return Err("C1 config too large".into());
        }
        let v: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        let o = v.as_object().ok_or("C1 config must be an object")?;
        let fields = [
            "run_id",
            "validator",
            "parent",
            "nonce_key",
            "evidence",
            "timeout_s",
        ];
        if o.len() != fields.len() || o.keys().any(|k| !fields.contains(&k.as_str())) {
            return Err("unknown/missing C1 config field".into());
        }
        let string = |k: &str| o[k].as_str().ok_or_else(|| format!("{k} must be a string"));
        let run_id = string("run_id")?.to_owned();
        if run_id.is_empty()
            || run_id.len() > 64
            || !run_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err("invalid run_id".into());
        }
        let validator = string("validator")?.to_ascii_lowercase();
        if validator.len() != 64 || !validator.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("validator must be Ed25519 public key hex".into());
        }
        let parent = o["parent"]
            .as_u64()
            .filter(|n| *n >= 2 && *n < u64::MAX)
            .ok_or("parent must be in 2..u64::MAX")?;
        let seconds = o["timeout_s"]
            .as_u64()
            .filter(|n| (1..=300).contains(n))
            .ok_or("timeout_s must be 1..300")?;
        Ok(Self {
            run_id,
            validator,
            parent,
            nonce_key: unhex(string("nonce_key")?)?,
            evidence: PathBuf::from(string("evidence")?),
            timeout: Duration::from_secs(seconds),
        })
    }
}

struct State {
    file: File,
    invalid: bool,
    parked: bool,
    ready: bool,
    parent_hash: Option<String>,
    child_hash: Option<String>,
    witness: Option<Value>,
    sequence: u64,
}

pub(crate) struct Qualification {
    config: Config,
    identity: Value,
    deadline: Instant,
    state: Mutex<State>,
    changed: Condvar,
    failed: Arc<AtomicBool>,
}

pub(crate) fn marker(db: &StateDb) -> Result<u64, String> {
    let bytes = db
        .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
        .map_err(|e| e.to_string())?
        .ok_or("missing durable marker")?;
    Ok(u64::from_be_bytes(
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| "malformed durable marker")?,
    ))
}

fn durable_hash(db: &StateDb, height: u64) -> Result<String, String> {
    let row = db
        .get_cf_raw(CF_BLOCK_HEADERS, &height.to_be_bytes())
        .map_err(|e| e.to_string())?
        .ok_or("missing durable header")?;
    if row.len() <= 32 {
        return Err("short durable header".into());
    }
    let header: torus_types::TorusBlockHeader =
        serde_json::from_slice(&row[32..]).map_err(|e| e.to_string())?;
    let hash = alloy_primitives::keccak256(header.canonical_header_bytes());
    if header.height != height || hash.as_slice() != &row[..32] {
        return Err("durable header identity mismatch".into());
    }
    let body = db
        .get_cf_raw(CF_BLOCK_BODIES, &height.to_be_bytes())
        .map_err(|e| e.to_string())?
        .ok_or("missing durable body")?;
    let body = torus_state::block_body::decode_body_record(&body).map_err(|e| e.to_string())?;
    if body.native_actions.is_empty()
        || body.native_actions.len() != header.native_action_count as usize
        || !body.evm_transactions.is_empty()
    {
        return Err("C1 requires native-only durable bodies".into());
    }
    Ok(hex(hash.as_slice()))
}

impl Qualification {
    pub fn from_env(
        db: &StateDb,
        failed: Arc<AtomicBool>,
        validator: Option<[u8; 32]>,
    ) -> Result<Option<Arc<Self>>, String> {
        let Some(raw) = std::env::var_os(ENV) else {
            return Ok(None);
        };
        let config = Config::parse(raw.to_str().ok_or("non-UTF8 C1 config")?)?;
        let actual = validator.ok_or("C1 needs validator signing identity")?;
        Self::arm(config, db, failed, &hex(&actual)).map(Some)
    }
    pub fn arm(
        config: Config,
        db: &StateDb,
        failed: Arc<AtomicBool>,
        validator: &str,
    ) -> Result<Arc<Self>, String> {
        if config.validator != validator {
            return Err("C1 validator identity mismatch".into());
        }
        if let Some(bytes) = db
            .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
            .map_err(|e| e.to_string())?
        {
            let applied = u64::from_be_bytes(
                bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| "malformed durable marker at arm")?,
            );
            if applied >= config.parent {
                return Err(
                    "C1 cannot arm after target parent/replay; disable hook on restart".into(),
                );
            }
        }
        let evidence_parent = config
            .evidence
            .parent()
            .ok_or("evidence needs parent directory")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let db_path = db
            .inner()
            .path()
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if evidence_parent.starts_with(db_path) {
            return Err("C1 evidence must be outside RocksDB".into());
        }
        let path = evidence_parent.join(
            config
                .evidence
                .file_name()
                .ok_or("missing evidence filename")?,
        );
        let stat = std::fs::read_to_string("/proc/self/stat").map_err(|e| e.to_string())?;
        let start_ticks = stat
            .rsplit_once(") ")
            .and_then(|(_, s)| s.split_whitespace().nth(19))
            .ok_or("cannot identify process start")?
            .to_owned();
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|e| e.to_string())?;
        let armed_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis();
        let armed_boot_ms = boot_millis()?;
        let identity = json!({"run_id":config.run_id,"validator":config.validator,"pid":std::process::id(),"process_start_ticks":start_ticks,"kernel_boot_id":boot.trim(),"parent":config.parent,"child":config.parent+1,
            "armed_unix_ms": armed_unix_ms.to_string(), "deadline_unix_ms": (armed_unix_ms + config.timeout.as_millis()).to_string(),
            "armed_boot_ms": armed_boot_ms.to_string(), "deadline_boot_ms": (armed_boot_ms + config.timeout.as_millis()).to_string()});
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        let this = Arc::new(Self {
            deadline: Instant::now() + config.timeout,
            config,
            identity,
            state: Mutex::new(State {
                file,
                invalid: false,
                parked: false,
                ready: false,
                parent_hash: None,
                child_hash: None,
                witness: None,
                sequence: 0,
            }),
            changed: Condvar::new(),
            failed,
        });
        {
            let mut s = this.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(error) = this.publish(&mut s, "ARMED", json!({})) {
                this.invalidate_locked(&mut s, &error);
                return Err(error);
            }
        }
        // Bounded arming deadline, including when no job ever reaches W.
        let watch = this.clone();
        std::thread::Builder::new()
            .name("torus-c1-deadline".into())
            .spawn(move || {
                let mut s = watch.state.lock().unwrap_or_else(|e| e.into_inner());
                while !s.invalid {
                    let left = watch.deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        watch.invalidate_locked(&mut s, "deadline expired");
                        break;
                    }
                    s = watch
                        .changed
                        .wait_timeout(s, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            })
            .map_err(|e| {
                this.invalidate("cannot start C1 deadline worker");
                e.to_string()
            })?;
        Ok(this)
    }
    fn publish(&self, s: &mut State, event: &str, detail: Value) -> Result<(), String> {
        s.sequence += 1;
        let row = json!({"schema":1,"identity":self.identity,"sequence":s.sequence,"event":event,"parent_hash":s.parent_hash,"child_hash":s.child_hash,"detail":detail});
        let mut bytes = serde_json::to_vec(&row).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        s.file
            .write_all(&bytes)
            .and_then(|_| s.file.sync_all())
            .map_err(|e| e.to_string())
    }
    fn invalidate_locked(&self, s: &mut State, reason: &str) {
        if !s.invalid {
            s.invalid = true;
            self.failed.store(true, Ordering::SeqCst);
            let _ = self.publish(s, "INVALID", json!({"reason":reason}));
            self.changed.notify_all();
        }
    }
    pub fn invalidate(&self, reason: &str) {
        self.invalidate_locked(
            &mut self.state.lock().unwrap_or_else(|e| e.into_inner()),
            reason,
        );
    }
    pub fn check_eligibility(&self, height: u64, eligible: bool) -> bool {
        if (height == self.config.parent || height == self.config.parent + 1) && !eligible {
            self.invalidate("C1 target hit serial/non-native barrier");
            return false;
        }
        !self.failed.load(Ordering::SeqCst)
    }
    pub fn selected_nonce(&self, height: u64, key: &[u8]) -> bool {
        height == self.config.parent + 1 && key == self.config.nonce_key
    }
    /// Called inside W's panic boundary, before all trie/resident locks/writes.
    pub fn before_write(&self, db: &StateDb, height: u64, is_flush: bool) -> Result<(), String> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.invalid || Instant::now() >= self.deadline {
            self.invalidate_locked(&mut s, "C1 invalidated/expired");
            return Err("C1 invalidated".into());
        }
        if height < self.config.parent {
            return Ok(());
        }
        let check: Result<(), String> = (|| {
            if height != self.config.parent || !is_flush || s.parked {
                return Err("wrong C1 job/duplicate park".into());
            }
            if marker(db)? != height - 1 {
                return Err("C1 durable marker is not N-1".into());
            }
            s.parent_hash = Some(durable_hash(db, height)?);
            Ok(())
        })();
        if let Err(e) = check {
            self.invalidate_locked(&mut s, &e);
            return Err(e);
        }
        s.parked = true;
        if let Err(e) = self.publish(
            &mut s,
            "W_PARKED",
            json!({"durable_marker":height-1,"job":"Flush","before_write":true}),
        ) {
            self.invalidate_locked(&mut s, &e);
            return Err(e);
        }
        self.changed.notify_all();
        while !s.invalid {
            s = self.changed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        Err("C1 parked write cancelled without being written".into())
    }
    pub fn begin_child(
        &self,
        db: &StateDb,
        height: u64,
        parent: Option<u64>,
        hash: &[u8],
        overlay_marker: Option<Vec<u8>>,
        eligible: bool,
    ) -> bool {
        if height != self.config.parent + 1 {
            return true;
        }
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while !s.parked && !s.invalid {
            s = self.changed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        let result: Result<(), String> = (|| {
            if Instant::now() >= self.deadline
                || s.invalid
                || !eligible
                || parent != Some(self.config.parent)
                || overlay_marker != Some(self.config.parent.to_be_bytes().to_vec())
            {
                return Err("C1 child does not use required pending parent".into());
            }
            if marker(db)? != self.config.parent - 1 {
                return Err("C1 parent already durable".into());
            }
            let persisted = durable_hash(db, height)?;
            if persisted != hex(hash) {
                return Err("C1 child canonical hash mismatch".into());
            }
            s.child_hash = Some(persisted);
            Ok(())
        })();
        if let Err(e) = result {
            self.invalidate_locked(&mut s, &e);
            return false;
        }
        true
    }
    /// Only the real replay-guard call invokes this, using its returned value.
    pub fn nonce_read(
        &self,
        db: &StateDb,
        height: u64,
        key: &[u8],
        source: Option<u64>,
        value: &Option<Vec<u8>>,
    ) -> bool {
        if !self.selected_nonce(height, key) {
            return true;
        }
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let result: Result<(), String> = (|| {
            if Instant::now() >= self.deadline
                || s.invalid
                || !s.parked
                || s.child_hash.is_none()
                || source != Some(self.config.parent)
                || value.as_deref() != Some(self.config.parent.to_be_bytes().as_slice())
            {
                return Err("C1 nonce guard did not read the selected parent value".into());
            }
            let durable = db
                .get_cf_raw(torus_state::cf::CF_NATIVE_NONCES, key)
                .map_err(|e| e.to_string())?;
            if durable.is_some() || marker(db)? != self.config.parent - 1 {
                return Err("C1 selected nonce is not parent-only".into());
            }
            if s.witness.is_none() {
                let witness = json!({"source":"nonce_replay_guard","cf":torus_state::cf::CF_NATIVE_NONCES,"key_hash":hex(alloy_primitives::keccak256(key).as_slice()),"value_hash":hex(alloy_primitives::keccak256(value.as_ref().unwrap()).as_slice()),"parent_height":source,"durable_value_absent":true,"decision":"skip_already_consumed"});
                self.publish(&mut s, "PARENT_READ", witness.clone())?;
                s.witness = Some(witness);
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.invalidate_locked(&mut s, &e);
            return false;
        }
        true
    }
    /// Never returns success for the selected child: await SIGKILL or fail-stop.
    pub fn computed_child(&self, db: &StateDb, height: u64) -> bool {
        if height != self.config.parent + 1 {
            return true;
        }
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if Instant::now() >= self.deadline
            || s.invalid
            || !s.parked
            || s.ready
            || s.witness.is_none()
            || s.child_hash.is_none()
            || self.failed.load(Ordering::SeqCst)
            || marker(db) != Ok(self.config.parent - 1)
        {
            self.invalidate_locked(&mut s, "C1 compute missing witness/park/marker");
            return false;
        }
        s.ready = true;
        let detail = json!({"durable_marker":self.config.parent-1,"overlay_parent":self.config.parent,"nonce_witness":s.witness,"child_compute_complete":true,"before_child_handoff":true});
        if let Err(e) = self.publish(&mut s, "READY", detail) {
            self.invalidate_locked(&mut s, &e);
            return false;
        }
        self.changed.notify_all();
        while !s.invalid {
            s = self.changed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_requires_explicit_bounded_complete_arm() {
        let valid = json!({"run_id":"test-1","validator":"00".repeat(32),"parent":5,"nonce_key":"11".repeat(28),"evidence":"/tmp/evidence.jsonl","timeout_s":30});
        assert!(Config::parse(&valid.to_string()).is_ok());
        for (field, value) in [
            ("parent", json!(1)),
            ("parent", json!(u64::MAX)),
            ("timeout_s", json!(0)),
            ("timeout_s", json!(301)),
            ("run_id", json!("")),
            ("nonce_key", json!("00")),
            ("validator", json!("fixture")),
        ] {
            let mut bad = valid.clone();
            bad[field] = value;
            assert!(Config::parse(&bad.to_string()).is_err(), "{field}");
        }
        let mut extra = valid.clone();
        extra["release"] = json!(true);
        assert!(Config::parse(&extra.to_string()).is_err());
        assert!(Config::parse("").is_err());
    }
}
