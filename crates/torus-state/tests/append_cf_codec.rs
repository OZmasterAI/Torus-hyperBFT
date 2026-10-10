//! Trades + DA compaction tuning (docs/plans/append-cf-compaction.md):
//! node-local codec for the append-only CFs and WAL compression. Both default
//! to exact-today, are persisted in RocksDB's OPTIONS file, and must open an
//! existing data dir written with the other setting (in-place upgrade and
//! downgrade, mixed fleet).

use rocksdb::WriteBatch;
use std::{fs, path::Path};
use torus_state::cf::{
    CF_NATIVE_PENDING, CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::db::{parse_append_cf_codec, parse_wal_compression, AppendCfCodec};
use torus_state::{DbTuning, StateDb};

const APPEND_CFS: [&str; 3] = [CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES, CF_NATIVE_PENDING];

#[test]
fn append_cf_codec_parser_defaults_to_today() {
    assert_eq!(DbTuning::default().append_cf_codec, AppendCfCodec::Today);
    assert_eq!(parse_append_cf_codec(None), AppendCfCodec::Today);
    for value in ["", "zstd", "ZSTD1", "lz", "lz4 x", "none"] {
        assert_eq!(
            parse_append_cf_codec(Some(value)),
            AppendCfCodec::Today,
            "{value}"
        );
    }
    assert_eq!(parse_append_cf_codec(Some(" zstd1 ")), AppendCfCodec::Zstd1);
    assert_eq!(parse_append_cf_codec(Some("lz4")), AppendCfCodec::Lz4);
}

#[test]
fn wal_compression_parser_defaults_to_off() {
    assert!(!DbTuning::default().wal_compression);
    assert!(!parse_wal_compression(None));
    for value in ["", "0", "off", "lz4", "yes please"] {
        assert!(!parse_wal_compression(Some(value)), "{value}");
    }
    for value in ["zstd", " zstd ", "1", "on"] {
        assert!(parse_wal_compression(Some(value)), "{value}");
    }
}

/// `name = value` pairs of one section of the newest OPTIONS file.
fn persisted(path: &Path, section: &str) -> Vec<(String, String)> {
    let mut files: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("OPTIONS-")
        })
        .collect();
    files.sort();
    let text = fs::read_to_string(files.last().expect("persisted OPTIONS file")).unwrap();
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == section;
            continue;
        }
        if let (true, Some((k, v))) = (inside, line.split_once('=')) {
            out.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    assert!(!out.is_empty(), "section {section} in OPTIONS");
    out
}

fn opt<'a>(pairs: &'a [(String, String)], name: &str) -> &'a str {
    pairs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("{name} in OPTIONS"))
}

/// (bottommost codec, bottommost options string) of one CF.
fn cf_bottom(path: &Path, cf: &str) -> (String, String) {
    let p = persisted(path, &format!("[CFOptions \"{cf}\"]"));
    (
        opt(&p, "bottommost_compression").to_owned(),
        opt(&p, "bottommost_compression_opts").to_owned(),
    )
}

#[test]
fn codec_and_wal_options_are_persisted_on_the_append_cfs_only() {
    let today = {
        let dir = tempfile::tempdir().unwrap();
        let _db = StateDb::open_with_tuning(dir.path(), &DbTuning::default()).unwrap();
        assert_eq!(
            opt(&persisted(dir.path(), "[DBOptions]"), "wal_compression"),
            "kNoCompression"
        );
        for cf in APPEND_CFS {
            assert_eq!(cf_bottom(dir.path(), cf).0, "kZSTD", "{cf}");
        }
        cf_bottom(dir.path(), CF_NATIVE_POSITIONS)
    };
    assert_eq!(today.0, "kZSTD");

    for (codec, bottom) in [
        (AppendCfCodec::Zstd1, "kZSTD"),
        (AppendCfCodec::Lz4, "kLZ4Compression"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let tuning = DbTuning {
            append_cf_codec: codec,
            wal_compression: true,
            ..Default::default()
        };
        let _db = StateDb::open_with_tuning(dir.path(), &tuning).unwrap();
        assert_eq!(
            opt(&persisted(dir.path(), "[DBOptions]"), "wal_compression"),
            "kZSTD"
        );
        for cf in APPEND_CFS {
            let (b, opts) = cf_bottom(dir.path(), cf);
            assert_eq!(b, bottom, "{codec:?} {cf}");
            if codec == AppendCfCodec::Zstd1 {
                assert!(opts.contains("level=1;"), "{cf}: {opts}");
                assert!(opts.contains("enabled=true"), "{cf}: {opts}");
            }
        }
        // Every other CF keeps today's options.
        assert_eq!(
            cf_bottom(dir.path(), CF_NATIVE_POSITIONS),
            today,
            "{codec:?}"
        );
    }
}

fn write_rows(db: &StateDb, tag: u8, n: u32) {
    for cf in APPEND_CFS.iter().chain([&CF_NATIVE_POSITIONS]) {
        let mut batch = WriteBatch::default();
        for i in 0..n {
            batch.put_cf(
                db.cf_handle(cf).unwrap(),
                [&[tag][..], &i.to_be_bytes()].concat(),
                [tag; 512],
            );
        }
        db.write(batch).unwrap();
    }
}

fn check_rows(db: &StateDb, tag: u8, n: u32) {
    for cf in APPEND_CFS.iter().chain([&CF_NATIVE_POSITIONS]) {
        for i in 0..n {
            let key = [&[tag][..], &i.to_be_bytes()].concat();
            assert_eq!(
                db.get_cf_raw(cf, &key).unwrap(),
                Some(vec![tag; 512]),
                "{cf} tag {tag} row {i}"
            );
        }
    }
}

fn compact_all(db: &StateDb) {
    for cf in APPEND_CFS.iter().chain([&CF_NATIVE_POSITIONS]) {
        let h = db.cf_handle(cf).unwrap();
        db.inner().flush_cf(h).unwrap();
        db.inner().compact_range_cf(h, None::<&[u8]>, None::<&[u8]>);
    }
}

/// In-place upgrade and downgrade: a dir written with today's options opens
/// with each new setting and reads every row back (old SSTs and WAL records
/// in their old codec), and the reverse. Rows written last are left in the
/// WAL only (no flush), so the next open replays a WAL written under the
/// other setting.
#[test]
fn existing_dirs_open_across_settings_in_both_directions() {
    const N: u32 = 200;
    for codec in [AppendCfCodec::Zstd1, AppendCfCodec::Lz4] {
        let new = DbTuning {
            append_cf_codec: codec,
            wal_compression: true,
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let steps = [DbTuning::default(), new.clone(), DbTuning::default(), new];
        for (step, tuning) in steps.iter().enumerate() {
            let db = StateDb::open_with_tuning(dir.path(), tuning).unwrap();
            for tag in 0..step as u8 {
                check_rows(&db, tag, N);
            }
            if step > 0 {
                compact_all(&db);
            }
            write_rows(&db, step as u8, N);
            drop(db);
        }
        let db = StateDb::open_with_tuning(dir.path(), &DbTuning::default()).unwrap();
        for tag in 0..steps.len() as u8 {
            check_rows(&db, tag, N);
        }
    }
}
