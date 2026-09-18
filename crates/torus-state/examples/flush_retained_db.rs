//! Offline maintenance for retained benchmark databases. Keeps every logical row;
//! flushes all column families so RocksDB can retire obsolete recovery logs.
//! RocksDB may delete obsolete WAL internally. Changes physical crash-recovery
//! evidence: requires a retention decision; never use on an unresolved failure.
//! Usage: flush_retained_db EXISTING_DB NEW_PROOF_JSONL
use alloy_primitives::Keccak256;
use rocksdb::{FlushOptions, IteratorMode, Options, DB};
use serde::Serialize;
use std::{
    error::Error,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};
use torus_state::StateDb;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug, PartialEq, Eq, Serialize)]
struct DbIdentity {
    rocksdb_id: String,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn identity(path: &Path) -> Result<DbIdentity> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    #[cfg(unix)]
    let metadata = fs::metadata(path)?;
    Ok(DbIdentity {
        rocksdb_id: fs::read_to_string(path.join("IDENTITY"))?,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

fn strict_options() -> Options {
    let mut options = Options::default();
    // PointInTimeRecovery (RocksDB's default) can truncate a corrupt WAL before
    // every census, making matching digests a false preservation claim.
    options.set_wal_recovery_mode(rocksdb::DBRecoveryMode::AbsoluteConsistency);
    options
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct CfDigest {
    name: String,
    rows: u64,
    bytes: u64,
    keccak: String,
}

fn inventory(db: &DB, names: &[String]) -> Result<Vec<CfDigest>> {
    names
        .iter()
        .map(|name| {
            let cf = db.cf_handle(name).ok_or("missing column family")?;
            let mut digest = Keccak256::new();
            digest.update(b"retained-benchmark-db-v1");
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name.as_bytes());
            let (mut rows, mut bytes) = (0u64, 0u64);
            for row in db.iterator_cf(cf, IteratorMode::Start) {
                let (key, value) = row?;
                digest.update((key.len() as u64).to_le_bytes());
                digest.update(&key);
                digest.update((value.len() as u64).to_le_bytes());
                digest.update(&value);
                rows += 1;
                bytes += (key.len() + value.len()) as u64;
            }
            Ok(CfDigest {
                name: name.clone(),
                rows,
                bytes,
                keccak: digest.finalize().to_string(),
            })
        })
        .collect()
}

fn physical_bytes(path: &Path) -> Result<serde_json::Value> {
    let (mut total, mut wal, mut sst) = (0u64, 0u64, 0u64);
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        total += meta.len();
        match entry.path().extension().and_then(|s| s.to_str()) {
            Some("log") => wal += meta.len(),
            Some("sst") => sst += meta.len(),
            _ => (),
        }
    }
    Ok(serde_json::json!({"total":total,"wal":wal,"sst":sst}))
}

fn flush_retained(path: &Path, proof_path: &Path) -> Result<()> {
    if !path.join("CURRENT").is_file() {
        return Err("not an existing RocksDB database".into());
    }
    let path = path.canonicalize()?;
    let proof_parent = proof_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    if proof_parent.starts_with(&path) {
        return Err("proof must be stored outside the database directory".into());
    }
    let original_identity = identity(&path)?;
    let mut names = DB::list_cf(&Options::default(), &path)?;
    names.sort();
    let mut expected: Vec<_> = torus_state::cf::ALL_CF_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    expected.push("default".into());
    expected.sort();
    expected.dedup();
    if names != expected {
        return Err("column-family schema differs; refusing maintenance".into());
    }
    let mut proof = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(proof_path)?;
    let initial_physical = physical_bytes(&path)?;

    // Read-only census before opening a writer. No key/value data enters logs.
    let readonly = DB::open_cf_for_read_only(&strict_options(), &path, &names, false)?;
    let before = inventory(&readonly, &names)?;
    let sequence = readonly.latest_sequence_number();
    drop(readonly);
    if identity(&path)? != original_identity {
        return Err("database identity changed during census".into());
    }
    writeln!(
        proof,
        "{}",
        serde_json::json!({"phase":"before","path":path,"identity":original_identity,"sequence":sequence,"physical":initial_physical,"column_families":before})
    )?;
    proof.sync_all()?;

    // RocksDB's exclusive writer lock rejects an active node. The state wrapper
    // bounds replay memory and uses the project's normal CF options. Opening it
    // can itself recover/flush logs, but does not issue application writes.
    let db = StateDb::open_existing(&path)?;
    if identity(&path)? != original_identity
        || db.inner().latest_sequence_number() != sequence
        || inventory(db.inner(), &names)? != before
    {
        return Err("database changed between census and writer lock; refusing flush".into());
    }
    let mut options = FlushOptions::default();
    options.set_wait(true);
    let families: Vec<_> = names
        .iter()
        .map(|name| db.inner().cf_handle(name).unwrap())
        .collect();
    db.inner().flush_cfs_opt(&families, &options)?;
    db.sync_wal()?;
    let flushed = inventory(db.inner(), &names)?;
    let flushed_equal = flushed == before
        && db.inner().latest_sequence_number() == sequence
        && identity(&path)? == original_identity;
    writeln!(
        proof,
        "{}",
        serde_json::json!({"phase":"flushed","sequence":db.inner().latest_sequence_number(),"column_families":flushed,"equal":flushed_equal})
    )?;
    proof.sync_all()?;
    if !flushed_equal {
        return Err("logical state differs after flush; preserve database and proof".into());
    }
    drop(db);

    let readonly = DB::open_cf_for_read_only(&strict_options(), &path, &names, false)?;
    let after = inventory(&readonly, &names)?;
    let after_sequence = readonly.latest_sequence_number();
    let equal =
        after == before && after_sequence == sequence && identity(&path)? == original_identity;
    drop(readonly);
    writeln!(
        proof,
        "{}",
        serde_json::json!({"phase":"after","sequence":after_sequence,"physical":physical_bytes(&path)?,"column_families":after,"equal":equal})
    )?;
    proof.sync_all()?;
    if !equal {
        return Err("logical state differs after maintenance; preserve database and proof".into());
    }
    println!(
        "All column-family records and sequence preserved; proof {}",
        proof_path.display()
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: flush_retained_db EXISTING_DB NEW_PROOF_JSONL".into());
    }
    flush_retained(Path::new(&args[0]), Path::new(&args[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_wal_checksum_error_refuses_maintenance_instead_of_proving_a_prefix() {
        // All corruption is confined to this newly created fixture. WAL-backed
        // writes need no memtable flush at clean close. Locate the payload bytes
        // rather than relying on a particular WAL header/record offset.
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let db = StateDb::open(&path).unwrap();
        let values = [
            b"retention-fixture-first-valid-record-1111".as_slice(),
            b"retention-fixture-middle-corrupt-record-2222".as_slice(),
            b"retention-fixture-last-valid-record-3333".as_slice(),
        ];
        for (index, value) in values.iter().enumerate() {
            db.inner().put([index as u8], value).unwrap();
        }
        db.sync_wal().unwrap();
        let full_sequence = db.inner().latest_sequence_number();
        drop(db);

        let locate =
            |bytes: &[u8], value: &[u8]| bytes.windows(value.len()).position(|w| w == value);
        let (wal_path, mut bytes) = fs::read_dir(&path)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                if path.extension().and_then(|s| s.to_str()) != Some("log") {
                    return None;
                }
                let bytes = fs::read(&path).unwrap();
                values
                    .iter()
                    .all(|value| locate(&bytes, value).is_some())
                    .then_some((path, bytes))
            })
            .next()
            .expect("three unflushed fixture records must remain in one WAL");
        let positions: Vec<_> = values
            .iter()
            .map(|value| locate(&bytes, value).unwrap())
            .collect();
        assert!(positions[0] < positions[1] && positions[1] < positions[2]);
        // Flip an interior payload byte without changing its CRC or truncating
        // the file. A complete later record remains, so this is not a torn tail.
        bytes[positions[1] + 8] ^= 1;
        fs::write(&wal_path, &bytes).unwrap();

        let names = DB::list_cf(&Options::default(), &path).unwrap();
        let permissive =
            DB::open_cf_for_read_only(&Options::default(), &path, &names, false).unwrap();
        assert_eq!(permissive.get([0]).unwrap().as_deref(), Some(values[0]));
        assert!(
            permissive.get([2]).unwrap().is_none(),
            "default recovery silently loses later valid record"
        );
        assert!(permissive.latest_sequence_number() < full_sequence);
        drop(permissive);

        let proof = root.path().join("proof.jsonl");
        assert!(flush_retained(&path, &proof).is_err());
        assert!(
            fs::read(&proof).unwrap().is_empty(),
            "must not certify a truncated census"
        );
        assert_eq!(
            fs::read(&wal_path).unwrap(),
            bytes,
            "refusal must not rewrite fixture WAL"
        );
        assert!(
            StateDb::open_existing(&path).is_err(),
            "writer must also require complete recovery"
        );
    }

    #[test]
    fn flush_preserves_every_cf_and_reopened_sequence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let db = StateDb::open(&path).unwrap();
        for name in torus_state::cf::ALL_CF_NAMES {
            let cf = db.cf_handle(name).unwrap();
            db.inner().put_cf(cf, b"binary\0key", [0xff, 0, 4]).unwrap();
        }
        db.inner().put(b"default", b"retained too").unwrap();
        drop(db);
        let proof = root.path().join("proof.jsonl");
        flush_retained(&path, &proof).unwrap();
        let lines: Vec<serde_json::Value> = fs::read_to_string(&proof)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        for phase in &lines[1..] {
            assert_eq!(phase["equal"], true);
            assert_eq!(lines[0]["column_families"], phase["column_families"]);
        }
        assert!(lines[2]["column_families"]
            .as_array()
            .unwrap()
            .iter()
            .all(|cf| cf["rows"] == 1));
        assert!(
            flush_retained(&path, &proof).is_err(),
            "must not overwrite proof"
        );
        assert!(flush_retained(&path, &path.join("proof.jsonl")).is_err());
        assert!(
            !path.join("proof.jsonl").exists(),
            "proof must never be created in DB"
        );
        let writer = StateDb::open_existing(&path).unwrap();
        assert!(
            flush_retained(&path, &root.path().join("locked-proof.jsonl")).is_err(),
            "must not flush while another writer owns the DB"
        );
        drop(writer);
        assert!(flush_retained(&root.path().join("missing"), &root.path().join("other")).is_err());
        assert!(StateDb::open_existing(&root.path().join("missing")).is_err());
        assert!(!root.path().join("missing/CURRENT").exists());
    }
}
