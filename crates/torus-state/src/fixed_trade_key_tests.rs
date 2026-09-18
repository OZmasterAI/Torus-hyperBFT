use super::*;
use crate::cf::{CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES};
use crate::{DbTuning, StateBackend};

fn rows(fills: u32) -> Vec<DeferredTradeRow> {
    let mut out = Vec::new();
    for index in 0..fills {
        let mut primary = [0; 20];
        primary[16..].copy_from_slice(&index.to_be_bytes());
        let mut maker = [0; 32];
        maker[0] = 1;
        maker[28..].copy_from_slice(&index.to_be_bytes());
        let mut taker = maker;
        taker[0] = 2;
        out.push(DeferredTradeRow::Primary {
            key: primary,
            value: vec![3, index as u8],
        });
        out.push(DeferredTradeRow::User {
            key: maker,
            value: vec![4, index as u8],
        });
        out.push(DeferredTradeRow::User {
            key: taker,
            value: vec![5, index as u8],
        });
    }
    out
}

fn snapshot(db: &StateDb) -> Vec<(&'static str, Vec<(Vec<u8>, Vec<u8>)>)> {
    [CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES]
        .into_iter()
        .map(|cf| (cf, db.iterate_cf(cf, None).unwrap()))
        .collect()
}

#[test]
fn fixed_trade_keys_raw_typed_chunks_preserve_rows_groups_and_duplicate_order() {
    for chunk in [1, 2, 7, 2048, 0] {
        let raw_dir = tempfile::tempdir().unwrap();
        let fixed_dir = tempfile::tempdir().unwrap();
        let tuning = DbTuning {
            stats_level: 1,
            ..Default::default()
        };
        let raw_db = StateDb::open_with_tuning(raw_dir.path(), &tuning).unwrap();
        let fixed_db = StateDb::open_with_tuning(fixed_dir.path(), &tuning).unwrap();
        let mut fixed = rows(if chunk == 2048 { 685 } else { 4 });
        let (key, _) = match &fixed[0] {
            DeferredTradeRow::Primary { key, value } => (*key, value),
            _ => unreachable!(),
        };
        fixed.push(DeferredTradeRow::Primary {
            key,
            value: b"last duplicate wins".to_vec(),
        });
        let raw: Vec<_> = fixed
            .clone()
            .into_iter()
            .map(DeferredTradeRow::into_raw)
            .collect();
        let policy = BgWriterPolicy {
            chunk_kvs: chunk,
            low_pri: chunk != 0,
        };
        let raw_before = raw_db.runtime_stats().tickers.unwrap().write_self;
        let fixed_before = fixed_db.runtime_stats().tickers.unwrap().write_self;
        write_kvs_chunked(&raw_db, &raw, policy).unwrap();
        write_trade_rows_chunked(&fixed_db, &fixed, policy).unwrap();
        assert_eq!(snapshot(&fixed_db), snapshot(&raw_db));
        let groups = if chunk == 0 {
            1
        } else {
            (fixed.len() + chunk - 1) / chunk
        } as u64;
        assert_eq!(
            raw_db.runtime_stats().tickers.unwrap().write_self - raw_before,
            groups
        );
        assert_eq!(
            fixed_db.runtime_stats().tickers.unwrap().write_self - fixed_before,
            groups
        );
        assert_eq!(
            fixed_db.get_cf_raw(CF_NATIVE_TRADES, &key).unwrap(),
            Some(b"last duplicate wins".to_vec())
        );
    }
}

#[test]
fn fixed_trade_keys_disconnected_return_preserves_batch_and_sync_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let (tx, rx) = sync_channel(1);
    drop(rx);
    let writer = BackgroundCfWriter {
        tx: Some(tx),
        handle: None,
        queued: Arc::new(AtomicUsize::new(0)),
    };
    let expected = rows(3);
    let fixed = expected.clone();
    let original_allocation = fixed.as_ptr();
    let returned = writer
        .send_batch(DeferredTradeBatch::Fixed(fixed))
        .unwrap_err();
    let DeferredTradeBatch::Fixed(ref actual) = returned else {
        panic!("representation changed")
    };
    assert_eq!(actual.as_ptr(), original_allocation);
    assert_eq!(actual, &expected);
    assert_eq!(writer.queued_batches(), 0);
    // Exact production fallback contract: borrowed rows in order, ignoring each
    // individual put result (here all writes succeed in a fresh fixture DB).
    returned.for_each_row(|cf, key, value| {
        let _ = db.put_cf_raw(cf, key, value);
    });
    for row in expected {
        let (cf, key, value) = row.as_parts();
        assert_eq!(db.get_cf_raw(cf, key).unwrap().as_deref(), Some(value));
    }
    let raw: Vec<_> = rows(1)
        .into_iter()
        .map(DeferredTradeRow::into_raw)
        .collect();
    let pointer = raw.as_ptr();
    let returned_raw = writer.send(raw).unwrap_err();
    assert_eq!(
        returned_raw.as_ptr(),
        pointer,
        "legacy raw API returns original allocation"
    );
    assert_eq!(writer.queued_batches(), 0);
}

#[test]
fn fixed_trade_keys_mixed_batches_drain_and_reopen_in_fifo_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let writer = BackgroundCfWriter::spawn_with_policy(
        db.clone(),
        "fixed-trade-fixture",
        1,
        BgWriterPolicy {
            chunk_kvs: 2,
            low_pri: true,
        },
    );
    writer
        .send(
            rows(2)
                .into_iter()
                .map(DeferredTradeRow::into_raw)
                .collect(),
        )
        .unwrap();
    let mut typed = rows(3);
    typed.push(DeferredTradeRow::Primary {
        key: [0; 20],
        value: b"typed middle".to_vec(),
    });
    writer.send_batch(DeferredTradeBatch::Fixed(typed)).unwrap();
    writer
        .send(vec![(CF_NATIVE_TRADES, vec![0; 20], b"raw final".to_vec())])
        .unwrap();
    drop(writer); // queue cap remains a batch cap; close/drain/join is the barrier.
    let expected = snapshot(&db);
    assert_eq!(
        db.get_cf_raw(CF_NATIVE_TRADES, &[0; 20]).unwrap(),
        Some(b"raw final".to_vec())
    );
    assert_eq!(expected[0].1.len(), 3);
    assert_eq!(expected[1].1.len(), 6);
    drop(db);
    let reopened = StateDb::open(dir.path()).unwrap();
    assert_eq!(snapshot(&reopened), expected);
}

#[test]
fn fixed_trade_keys_chunk_failure_stops_without_later_rows() {
    let all = rows(3);
    let mut calls = 0;
    let mut committed = Vec::new();
    let error = for_trade_chunks(&all, 2, |part| {
        calls += 1;
        if calls == 2 {
            return Err("injected second group failure");
        }
        committed.extend_from_slice(part);
        Ok(())
    })
    .unwrap_err();
    assert_eq!(error, "injected second group failure");
    assert_eq!(calls, 2);
    assert_eq!(committed, all[..2]);
    let empty: Result<(), ()> = for_trade_chunks(&[], 0, |_| panic!("empty must not write"));
    assert!(empty.is_ok());
}

#[test]
fn fixed_trade_keys_read_only_write_failure_does_not_materialize_rows() {
    let dir = tempfile::tempdir().unwrap();
    drop(StateDb::open(dir.path()).unwrap());
    let db = StateDb::open_read_only(dir.path()).unwrap();
    assert!(write_trade_rows_chunked(
        &db,
        &rows(2),
        BgWriterPolicy {
            chunk_kvs: 2,
            low_pri: true
        }
    )
    .is_err());
    assert!(snapshot(&db).iter().all(|(_, rows)| rows.is_empty()));
}
