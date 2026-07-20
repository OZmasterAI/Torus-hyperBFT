//! Row-key / row-value codecs for order-book persistence in
//! `cf_native_order_books` (+ the node-local order-row store).
//!
//! 3c (level-rows-as-authority): single source of truth for the CONSENSUS
//! row layouts. All integers big-endian; key layouts are length-disjoint
//! (8 legacy blob / 9 meta / 25 order+stop / 26 level) and tag-disjoint, so
//! misreads are impossible.
//!
//! ```text
//! meta  key market_id(8) ‖ 0x00                       (9 B)
//!       val next_seq(8) ‖ tick_raw(16) ‖ lot_raw(16) ‖ next_id(16) ‖ ltp_tag(1) [‖ ltp_raw(16)]
//! order key market_id(8) ‖ 0x01 ‖ order_id(16)        (25 B)   [mode 1: root CF;
//!       val seq(8 BE) ‖ borsh(Order)                            mode 2: cf_book_order_rows]
//! stop  key market_id(8) ‖ 0x02 ‖ stop_id(16)         (25 B)
//!       val StopOrder borsh
//! level key market_id(8) ‖ 0x03 ‖ side(1) ‖ price_enc(16)  (26 B)  [mode 2 only]
//!       val total_qty_raw(i128 BE, 16) ‖ order_count(u32 BE, 4) ‖ level_hash(32)  (52 B)
//! ```
//!
//! **STATE-ROOT PREIMAGE**: `cf_native_order_books` is one of the 6
//! `NATIVE_ROOT_CFS`; these layouts are FROZEN once deployed. Tag byte `0x03`
//! for level rows is deliberate — `0x02` is frozen as the stop tag here; the
//! parallel branch's `ROW_TAG_LEVEL = 0x02` is NOT adopted. Tags `0x04+` are
//! reserved.
//!
//! `price_enc` (mined from `feat/precompile-0800-topn-gas` @ 39c74a1,
//! re-tagged): sign-flipped BE i128, bitwise-NOT for bids — forward
//! lexicographic iteration is best-first on BOTH sides (bids high→low, asks
//! low→high), and all bid keys sort before all ask keys per market.

use torus_types::{FixedPoint, MarketId, OrderId, Side};

/// Key-tag byte for the per-market meta row.
pub const ROW_TAG_META: u8 = 0x00;
/// Key-tag byte for per-order rows.
pub const ROW_TAG_ORDER: u8 = 0x01;
/// Key-tag byte for pending-stop rows.
pub const ROW_TAG_STOP: u8 = 0x02;
/// Key-tag byte for price-level authority rows (3c; 0x02 is frozen = stops).
pub const ROW_TAG_LEVEL: u8 = 0x03;

/// Side byte inside a level-row key. Bids sort before asks.
pub const SIDE_TAG_BID: u8 = 0x00;
pub const SIDE_TAG_ASK: u8 = 0x01;

/// Byte length of a level-row value: qty(16) ‖ count(4) ‖ hash(32).
pub const LEVEL_ROW_VALUE_LEN: usize = 52;

/// Canonical side byte for level-row keys.
pub const fn side_tag(side: Side) -> u8 {
    match side {
        Side::Buy => SIDE_TAG_BID,
        Side::Sell => SIDE_TAG_ASK,
    }
}

/// Inverse of [`side_tag`]. `None` for an unknown byte.
pub const fn side_from_tag(tag: u8) -> Option<Side> {
    match tag {
        SIDE_TAG_BID => Some(Side::Buy),
        SIDE_TAG_ASK => Some(Side::Sell),
        _ => None,
    }
}

/// Order-preserving price encoding for level-row keys: sign-flipped BE i128
/// (total order for signed prices), bitwise-NOT for bids — so forward
/// lexicographic iteration walks BOTH sides best-first.
pub fn price_enc(tag: u8, raw_price: i128) -> [u8; 16] {
    let mut b = ((raw_price as u128) ^ (1u128 << 127)).to_be_bytes();
    if tag == SIDE_TAG_BID {
        for byte in &mut b {
            *byte = !*byte;
        }
    }
    b
}

/// Inverse of [`price_enc`].
pub fn price_dec(tag: u8, enc: &[u8; 16]) -> i128 {
    let mut b = *enc;
    if tag == SIDE_TAG_BID {
        for byte in &mut b {
            *byte = !*byte;
        }
    }
    (u128::from_be_bytes(b) ^ (1u128 << 127)) as i128
}

/// Meta row key: `market_id(8 BE) ‖ 0x00` (9 bytes).
pub fn book_meta_key(market_id: MarketId) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[..8].copy_from_slice(&market_id.to_be_bytes());
    k[8] = ROW_TAG_META;
    k
}

/// Order row key: `market_id(8 BE) ‖ 0x01 ‖ order_id(16 BE)` (25 bytes).
pub fn book_order_key(market_id: MarketId, order_id: OrderId) -> [u8; 25] {
    let mut k = [0u8; 25];
    k[..8].copy_from_slice(&market_id.to_be_bytes());
    k[8] = ROW_TAG_ORDER;
    k[9..].copy_from_slice(&order_id.to_be_bytes());
    k
}

/// Stop row key: `market_id(8 BE) ‖ 0x02 ‖ stop_id(16 BE)` (25 bytes).
pub fn book_stop_key(market_id: MarketId, stop_id: OrderId) -> [u8; 25] {
    let mut k = [0u8; 25];
    k[..8].copy_from_slice(&market_id.to_be_bytes());
    k[8] = ROW_TAG_STOP;
    k[9..].copy_from_slice(&stop_id.to_be_bytes());
    k
}

/// Level row key from raw journal parts:
/// `market_id(8 BE) ‖ 0x03 ‖ side(1) ‖ price_enc(16)` (26 bytes).
pub fn level_row_key_tagged(market_id: MarketId, tag: u8, raw_price: i128) -> [u8; 26] {
    let mut k = [0u8; 26];
    k[..8].copy_from_slice(&market_id.to_be_bytes());
    k[8] = ROW_TAG_LEVEL;
    k[9] = tag;
    k[10..].copy_from_slice(&price_enc(tag, raw_price));
    k
}

/// Level row key: `market_id(8 BE) ‖ 0x03 ‖ side(1) ‖ price_enc(16)`.
pub fn level_row_key(market_id: MarketId, side: Side, price: FixedPoint) -> [u8; 26] {
    level_row_key_tagged(market_id, side_tag(side), price.raw())
}

/// One price level's consensus aggregate — the level-row VALUE parts.
///
/// `level_hash` is the order-identity commitment:
/// `keccak256(for each order in queue front→back: row_len(u32 LE) ‖ seq(8 BE) ‖ borsh(Order))`
/// where `seq(8 BE) ‖ borsh(Order)` is byte-identical to the order-row VALUE
/// and `row_len` is that payload's length (framing makes concatenation
/// injective).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelRowData {
    /// Sum of member `remaining_qty` raws (> 0 by invariant).
    pub total_qty_raw: i128,
    /// Number of resting orders in the queue (>= 1 by invariant).
    pub order_count: u32,
    /// Per-level keccak order-identity commitment.
    pub level_hash: [u8; 32],
}

impl LevelRowData {
    /// Encode the 52-byte level-row value.
    pub fn encode(&self) -> [u8; LEVEL_ROW_VALUE_LEN] {
        let mut v = [0u8; LEVEL_ROW_VALUE_LEN];
        v[..16].copy_from_slice(&self.total_qty_raw.to_be_bytes());
        v[16..20].copy_from_slice(&self.order_count.to_be_bytes());
        v[20..].copy_from_slice(&self.level_hash);
        v
    }

    /// Strict decode of a 52-byte level-row value.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != LEVEL_ROW_VALUE_LEN {
            return Err(format!(
                "level row value must be {LEVEL_ROW_VALUE_LEN} bytes, got {}",
                bytes.len()
            ));
        }
        Ok(Self {
            total_qty_raw: i128::from_be_bytes(bytes[..16].try_into().unwrap()),
            order_count: u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            level_hash: bytes[20..].try_into().unwrap(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_enc_roundtrips_and_preserves_order() {
        let samples: Vec<i128> = vec![
            i128::MIN,
            -1_000_000,
            -1,
            0,
            1,
            42,
            1_000_000_000_000,
            i128::MAX,
        ];
        for tag in [SIDE_TAG_BID, SIDE_TAG_ASK] {
            for &p in &samples {
                assert_eq!(price_dec(tag, &price_enc(tag, p)), p, "roundtrip {p}");
            }
        }
        // Asks: encoding order == numeric order (best = lowest first).
        // Bids: encoding order == REVERSE numeric order (best = highest first).
        for w in samples.windows(2) {
            let (a, b) = (w[0], w[1]);
            assert!(price_enc(SIDE_TAG_ASK, a) < price_enc(SIDE_TAG_ASK, b));
            assert!(price_enc(SIDE_TAG_BID, a) > price_enc(SIDE_TAG_BID, b));
        }
    }

    #[test]
    fn level_row_value_roundtrip_and_strict_length() {
        let d = LevelRowData {
            total_qty_raw: 123_456_789_i128,
            order_count: 7,
            level_hash: [0xAB; 32],
        };
        let enc = d.encode();
        assert_eq!(enc.len(), LEVEL_ROW_VALUE_LEN);
        assert_eq!(LevelRowData::decode(&enc).unwrap(), d);
        assert!(LevelRowData::decode(&enc[..51]).is_err());
        let mut long = enc.to_vec();
        long.push(0);
        assert!(LevelRowData::decode(&long).is_err());
    }

    #[test]
    fn key_layouts_are_length_and_tag_disjoint() {
        let meta = book_meta_key(1);
        let ord = book_order_key(1, 9);
        let stop = book_stop_key(1, 9);
        let lvl = level_row_key(1, Side::Buy, FixedPoint::from_raw(100));
        assert_eq!(meta.len(), 9);
        assert_eq!(ord.len(), 25);
        assert_eq!(stop.len(), 25);
        assert_eq!(lvl.len(), 26);
        assert_eq!(meta[8], ROW_TAG_META);
        assert_eq!(ord[8], ROW_TAG_ORDER);
        assert_eq!(stop[8], ROW_TAG_STOP);
        assert_eq!(lvl[8], ROW_TAG_LEVEL);
        assert_ne!(ROW_TAG_STOP, ROW_TAG_LEVEL, "0x02 is frozen = stops");
    }
}
