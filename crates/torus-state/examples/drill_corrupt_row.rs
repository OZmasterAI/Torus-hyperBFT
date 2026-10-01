//! Running-state-hash devnet drill helper (docs/plans/running-state-hash-impl.md,
//! Task 9): read or deliberately corrupt ONE row of a STOPPED node's DB.
//!
//!   drill_corrupt_row <data-dir> get     <cf> <hex-key>
//!   drill_corrupt_row <data-dir> add-i128-be <cf> <hex-key> <offset> <delta>
//!
//! `add-i128-be` adds `delta` to the big-endian i128 at byte `offset` of the
//! value and writes the row back. A `NativeBalance` row is
//! `version u8 ‖ available i128 BE ‖ order_margin i128 BE` (raw 1e-8 units), so
//! offset 1 edits `available`. Nothing else is touched: the applied height, running
//! hash and checkpoints (printed before and after) stay as they were, so the
//! node resumes on silently diverged state. Never run against a live node.

use std::path::Path;

use torus_state::running_hash::{read_applied_height, read_running_hash};
use torus_state::StateDb;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    assert!(
        s.len() % 2 == 0 && s.is_ascii(),
        "key must be even-length hex"
    );
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("key must be hex"))
        .collect()
}

fn meta(db: &StateDb) -> String {
    let hash = read_running_hash(db).map(|(h, x)| format!("{h}:{}", hex(&x)));
    format!(
        "applied_height={:?} running_hash={hash:?}",
        read_applied_height(db)
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "usage: {} <data-dir> get|add-i128-be <cf> <hex-key> [offset delta]",
            args[0]
        );
        std::process::exit(2);
    }
    let (dir, op, cf, key) = (
        &args[1],
        args[2].as_str(),
        args[3].as_str(),
        unhex(&args[4]),
    );
    let db = StateDb::open(Path::new(dir)).expect("open DB (is the node stopped?)");
    println!("before: {}", meta(&db));
    let old = db.get_cf_raw(cf, &key).expect("read row");
    println!("{cf}[{}] = {:?}", hex(&key), old.as_deref().map(hex));
    match op {
        "get" => {}
        "add-i128-be" => {
            let off: usize = args
                .get(5)
                .expect("offset")
                .parse()
                .expect("offset must be usize");
            let delta: i128 = args
                .get(6)
                .expect("delta")
                .parse()
                .expect("delta must be i128");
            let mut value = old.expect("row must exist");
            assert!(
                value.len() >= off + 16,
                "value shorter than offset + 16 bytes"
            );
            let cur = i128::from_be_bytes(value[off..off + 16].try_into().unwrap());
            let new = cur.checked_add(delta).expect("i128 overflow");
            value[off..off + 16].copy_from_slice(&new.to_be_bytes());
            db.put_cf_raw(cf, &key, &value).expect("write row");
            println!("i128 BE @{off}: {cur} -> {new}");
            println!(
                "{cf}[{}] = {}",
                hex(&key),
                hex(&db.get_cf_raw(cf, &key).unwrap().unwrap())
            );
            println!("after:  {}", meta(&db));
        }
        _ => {
            eprintln!("unknown op {op}");
            std::process::exit(2);
        }
    }
}
