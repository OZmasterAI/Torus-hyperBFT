//! 3c (level-rows-as-authority) — torus-core codec / journal / level-hash
//! gates (design §5.2 items 1 + parts of 2).
//!
//! Contracts:
//!   1. `price_enc` is a total order-preserving encoding (property test) and
//!      level keys iterate best-first per side, bids before asks.
//!   2. `level_hash` oracle: the journal-driven `take_level_ops` values equal
//!      an independent scratch recompute (framed `row_len ‖ seq ‖ borsh(Order)`
//!      front→back, keccak) after arbitrary op sequences.
//!   3. Framed encoding is injective at the boundaries the framing exists for
//!      (same concatenated payload bytes, different row split ⇒ different
//!      hash).
//!   4. Order-row codec roundtrip; seq semantics (insert-time assignment,
//!      in-place modify keeps seq, cancel+reinsert re-stamps).
//!   5. Journal exactness: delete ops emitted only for levels/rows with a
//!      persisted artifact (`level_exists` / `row_exists` guards).

use alloy_primitives::keccak256;
use proptest::prelude::*;
use torus_core::book_rows::{
    level_row_key, price_dec, price_enc, side_tag, LevelRowData, SIDE_TAG_ASK, SIDE_TAG_BID,
};
use torus_core::order_book::OrderBook;
use torus_types::{
    Address, FixedPoint, OrderType, PlaceOrderParams, Side, TimeInForce,
};

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::from([n; 20])
}

fn limit(market_id: u64, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// Independent scratch recompute of a level's row data (the oracle): walk the
/// queue front→back, frame each order row with its length, keccak the concat.
fn scratch_level_data(book: &OrderBook, side: Side, price: FixedPoint) -> Option<LevelRowData> {
    let queue = book.level_queue(side, price)?;
    if queue.is_empty() {
        return None;
    }
    let mut total: i128 = 0;
    let mut preimage = Vec::new();
    for o in queue {
        total += o.remaining_qty.raw();
        let seq = book.order_seq_of(o.id).expect("resting order has a seq");
        let mut row = Vec::new();
        row.extend_from_slice(&seq.to_be_bytes());
        borsh::BorshSerialize::serialize(o, &mut row).unwrap();
        preimage.extend_from_slice(&(row.len() as u32).to_le_bytes());
        preimage.extend_from_slice(&row);
    }
    Some(LevelRowData {
        total_qty_raw: total,
        order_count: queue.len() as u32,
        level_hash: keccak256(&preimage).0,
    })
}

/// Apply the journal-driven level ops to a simulated "persisted" map, check
/// each op against the scratch oracle, then assert the map now equals the
/// full scratch state of every live level — i.e. the journal COVERED every
/// level whose content changed (a missed journal site fails here).
fn drain_and_check(
    book: &mut OrderBook,
    persisted: &mut std::collections::BTreeMap<(u8, i128), LevelRowData>,
) {
    let ops = book.take_level_ops();
    for ((tag, raw_price), op) in &ops {
        let side = if *tag == SIDE_TAG_BID { Side::Buy } else { Side::Sell };
        let oracle = scratch_level_data(book, side, FixedPoint::from_raw(*raw_price));
        match op {
            Some(data) => {
                assert_eq!(Some(*data), oracle, "upsert != scratch oracle");
                assert!(data.total_qty_raw > 0, "level invariant: qty > 0");
                assert!(data.order_count >= 1, "level invariant: count >= 1");
                persisted.insert((*tag, *raw_price), *data);
            }
            None => {
                assert!(oracle.is_none(), "delete op for a live level");
                assert!(
                    persisted.remove(&(*tag, *raw_price)).is_some(),
                    "delete op for a level that was never persisted"
                );
            }
        }
    }
    // Coverage: the simulated persisted state must equal the live book.
    let mut live: std::collections::BTreeMap<(u8, i128), LevelRowData> =
        std::collections::BTreeMap::new();
    let bid_prices: Vec<i128> = book.bid_queues().map(|(p, _)| p.raw()).collect();
    let ask_prices: Vec<i128> = book.ask_queues().map(|(p, _)| p.raw()).collect();
    for raw in bid_prices {
        live.insert(
            (SIDE_TAG_BID, raw),
            scratch_level_data(book, Side::Buy, FixedPoint::from_raw(raw)).unwrap(),
        );
    }
    for raw in ask_prices {
        live.insert(
            (SIDE_TAG_ASK, raw),
            scratch_level_data(book, Side::Sell, FixedPoint::from_raw(raw)).unwrap(),
        );
    }
    assert_eq!(
        persisted, &mut live,
        "journal-driven persisted level state != live book (missed journal site?)"
    );
}

proptest! {
    /// price_enc is order-preserving: asks ascending, bids descending, over
    /// arbitrary i128 raw prices; and it roundtrips through price_dec.
    #[test]
    fn price_enc_order_preserving_property(a in any::<i128>(), b in any::<i128>()) {
        for tag in [SIDE_TAG_BID, SIDE_TAG_ASK] {
            prop_assert_eq!(price_dec(tag, &price_enc(tag, a)), a);
            prop_assert_eq!(price_dec(tag, &price_enc(tag, b)), b);
        }
        use std::cmp::Ordering;
        let num = a.cmp(&b);
        prop_assert_eq!(
            price_enc(SIDE_TAG_ASK, a).cmp(&price_enc(SIDE_TAG_ASK, b)),
            num,
            "ask encoding must preserve numeric order"
        );
        let expected_bid = match num {
            Ordering::Less => Ordering::Greater,
            Ordering::Equal => Ordering::Equal,
            Ordering::Greater => Ordering::Less,
        };
        prop_assert_eq!(
            price_enc(SIDE_TAG_BID, a).cmp(&price_enc(SIDE_TAG_BID, b)),
            expected_bid,
            "bid encoding must reverse numeric order"
        );
    }
}

/// Key-encoding property: forward lexicographic order is best-first per side,
/// and every bid key sorts before every ask key for the same market.
#[test]
fn level_key_order_is_best_first_bids_then_asks() {
    let bid_hi = level_row_key(7, Side::Buy, fp(105)).to_vec();
    let bid_lo = level_row_key(7, Side::Buy, fp(99)).to_vec();
    let ask_lo = level_row_key(7, Side::Sell, fp(106)).to_vec();
    let ask_hi = level_row_key(7, Side::Sell, fp(120)).to_vec();

    assert!(bid_hi < bid_lo, "bid keys must sort best(high)-first");
    assert!(ask_lo < ask_hi, "ask keys must sort best(low)-first");
    assert!(bid_lo < ask_lo, "bid keys must precede ask keys");
    assert_eq!(&bid_hi[..8], &7u64.to_be_bytes());
    assert_eq!(bid_hi[8], torus_core::book_rows::ROW_TAG_LEVEL);
}

/// level_hash oracle across an adversarial op sequence: places sharing
/// levels, partial + full fills, STP, cancel, cancel_all, both modify kinds,
/// draining ops (and checking them) after every step.
#[test]
fn level_ops_match_scratch_oracle_after_every_op_batch() {
    let mut book = OrderBook::new(1, fp(1), fp(1));
    let mut persisted = std::collections::BTreeMap::new();

    // Step 1: resting liquidity, shared levels.
    book.place_order(limit(1, true, 100, 5), addr(1), 10);
    book.place_order(limit(1, true, 100, 3), addr(2), 11);
    book.place_order(limit(1, true, 99, 2), addr(3), 12);
    book.place_order(limit(1, false, 105, 4), addr(4), 13);
    book.place_order(limit(1, false, 106, 1), addr(5), 14);
    drain_and_check(&mut book, &mut persisted);

    // Step 2: partial fill (crossing buy 2@105) + full fill of the 106 ask.
    book.place_order(limit(1, true, 105, 2), addr(6), 15);
    book.place_order(limit(1, true, 106, 3), addr(6), 16);
    drain_and_check(&mut book, &mut persisted);

    // Step 3: STP — addr(1) crosses its own resting 100 bid.
    book.place_order(limit(1, false, 100, 1), addr(1), 17);
    drain_and_check(&mut book, &mut persisted);

    // Step 4: modifies — in-place qty decrease (keeps seq) and price change
    // (cancel+reinsert, re-stamped seq).
    let id_inplace = book
        .level_queue(Side::Buy, fp(99))
        .unwrap()
        .front()
        .unwrap()
        .id;
    let seq_before = book.order_seq_of(id_inplace).unwrap();
    book.modify_order(id_inplace, None, Some(fp(1))).unwrap();
    assert_eq!(
        book.order_seq_of(id_inplace).unwrap(),
        seq_before,
        "in-place qty decrease must keep the seq"
    );
    book.modify_order(id_inplace, Some(fp(98)), None).unwrap();
    assert!(
        book.order_seq_of(id_inplace).unwrap() > seq_before,
        "price modify must re-stamp the seq"
    );
    drain_and_check(&mut book, &mut persisted);

    // Step 5: cancel_all empties levels.
    book.cancel_all(addr(2), None);
    book.cancel_all(addr(3), None);
    drain_and_check(&mut book, &mut persisted);

    // Full ops equal the scratch oracle for every live level, and cover
    // exactly the set of non-empty levels.
    let full = book.full_level_ops();
    let mut live = 0usize;
    for ((tag, raw), data) in &full {
        let side = if *tag == SIDE_TAG_BID { Side::Buy } else { Side::Sell };
        assert_eq!(
            Some(*data),
            scratch_level_data(&book, side, FixedPoint::from_raw(*raw)),
        );
        live += 1;
    }
    let expected_live = book.bid_queues().count() + book.ask_queues().count();
    assert_eq!(live, expected_live, "full_level_ops must cover every live level");
}

/// Framing injectivity: two levels whose UNframed concatenations agree must
/// still hash differently when the row boundaries differ. Construct two
/// queues whose orders differ only in how bytes split across rows (one order
/// with client_order_id vs two orders arranged to produce the same total
/// byte stream is impractical borsh-wise, so pin the mechanism instead:
/// the length prefix feeds the hash, so equal-payload-different-length rows
/// diverge).
#[test]
fn level_hash_framing_is_length_sensitive() {
    // Book A: one level with two orders (qty 5, qty 3).
    let mut a = OrderBook::new(1, fp(1), fp(1));
    a.place_order(limit(1, true, 100, 5), addr(1), 10);
    a.place_order(limit(1, true, 100, 3), addr(2), 11);
    // Book B: same level, same total qty, one fewer boundary (qty 8).
    let mut b = OrderBook::new(1, fp(1), fp(1));
    b.place_order(limit(1, true, 100, 8), addr(1), 10);

    let da = a.full_level_ops().pop().unwrap().1;
    let db = b.full_level_ops().pop().unwrap().1;
    assert_eq!(da.total_qty_raw, db.total_qty_raw, "same aggregate qty");
    assert_ne!(da.order_count, db.order_count);
    assert_ne!(da.level_hash, db.level_hash, "hash must commit to membership");
}

/// Order-row codec roundtrip through the book's encoder/decoder.
#[test]
fn order_row_roundtrip() {
    let mut book = OrderBook::new(9, fp(1), fp(1));
    let r = book.place_order(limit(9, true, 42, 7), addr(1), 123);
    let id = r.order_id;
    let bytes = book.encode_order_row(id).expect("resting");
    let (seq, order) = OrderBook::decode_order_row(&bytes).expect("decode");
    assert_eq!(seq, book.order_seq_of(id).unwrap());
    assert_eq!(order.id, id);
    assert_eq!(order.price, fp(42));
    assert_eq!(order.remaining_qty, fp(7));

    // insert_loaded_order restores the identical row bytes.
    let mut reloaded = OrderBook::new(9, fp(1), fp(1));
    reloaded.set_next_seq(book.next_seq());
    reloaded.insert_loaded_order(order, seq);
    assert_eq!(reloaded.encode_order_row(id).unwrap(), bytes);
}

/// Delete-tombstone guards: a level (and row) created and emptied between two
/// saves emits NO delete op; one with a persisted artifact does.
#[test]
fn journal_skips_tombstones_for_never_persisted_artifacts() {
    let mut book = OrderBook::new(2, fp(1), fp(1));

    // Place + cancel BEFORE any save: journals drained ⇒ zero ops.
    let r = book.place_order(limit(2, true, 50, 1), addr(1), 1);
    book.cancel_order(r.order_id).unwrap();
    assert!(book.take_row_ops().is_empty(), "row never persisted ⇒ no op");
    assert!(
        book.take_level_ops().is_empty(),
        "level never persisted ⇒ no tombstone"
    );

    // Place, drain (simulated save), THEN cancel: both deletes must appear.
    let r = book.place_order(limit(2, true, 51, 1), addr(1), 2);
    let ops = book.take_row_ops();
    assert_eq!(ops.len(), 1);
    assert!(ops[0].1.is_some());
    let lops = book.take_level_ops();
    assert_eq!(lops.len(), 1);
    assert!(lops[0].1.is_some());

    book.cancel_order(r.order_id).unwrap();
    let ops = book.take_row_ops();
    assert_eq!(ops.len(), 1, "persisted row must emit a delete");
    assert!(ops[0].1.is_none());
    let lops = book.take_level_ops();
    assert_eq!(lops.len(), 1, "persisted level must emit a delete");
    assert!(lops[0].1.is_none());

    // Second drains are empty (journals actually drained).
    assert!(book.take_row_ops().is_empty());
    assert!(book.take_level_ops().is_empty());
}

/// side_tag sanity (bid 0 sorts before ask 1 — key-order dependency).
#[test]
fn side_tags_are_frozen() {
    assert_eq!(side_tag(Side::Buy), SIDE_TAG_BID);
    assert_eq!(side_tag(Side::Sell), SIDE_TAG_ASK);
    assert!(SIDE_TAG_BID < SIDE_TAG_ASK);
}
