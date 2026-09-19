// Frozen 90e04d2 allocating trade encoder, independent of production offsets.
use super::*;

struct LegacyTradeKvs {
    trade_key: [u8; 20],
    trade_data: Vec<u8>,
    maker_key: [u8; 32],
    maker_data: Vec<u8>,
    taker_key: [u8; 32],
    taker_data: Vec<u8>,
}

impl LegacyTradeKvs {
    /// Byte-identical to the classic inline `persist_trade` construction.
    fn build(
        market_id: MarketId,
        block_height: u64,
        timestamp: u64,
        trade_index: u32,
        fill: &torus_core::order_book::Fill,
    ) -> Self {
        let taker_side: u8 = if fill.maker_side == Side::Buy { 1 } else { 0 };

        // Primary key: market_id(8) + block_number(8) + trade_index(4)
        let mut trade_key = [0u8; 20];
        trade_key[..8].copy_from_slice(&market_id.to_be_bytes());
        trade_key[8..16].copy_from_slice(&block_height.to_be_bytes());
        trade_key[16..20].copy_from_slice(&trade_index.to_be_bytes());

        let trade_id = trade_index as u128;
        let price_raw = fill.price.raw();
        let quantity_raw = fill.quantity.raw();

        // Borsh-serialize trade data (matches StoredTrade layout).
        // 16+16+16+1+8+8 = 65 bytes exactly (the classic with_capacity(64)
        // paid one realloc per fill).
        let mut trade_data = Vec::with_capacity(65);
        trade_data.extend_from_slice(&trade_id.to_le_bytes());
        trade_data.extend_from_slice(&price_raw.to_le_bytes());
        trade_data.extend_from_slice(&quantity_raw.to_le_bytes());
        trade_data.push(taker_side);
        trade_data.extend_from_slice(&block_height.to_le_bytes());
        trade_data.extend_from_slice(&timestamp.to_le_bytes());

        // Secondary index: per-user trades (descending block order).
        // 16+8+16+16+1+1+8+8 = 74 bytes exactly.
        let desc_block = u64::MAX - block_height;
        let mut maker_data = Vec::with_capacity(74);
        maker_data.extend_from_slice(&trade_id.to_le_bytes());
        maker_data.extend_from_slice(&market_id.to_le_bytes());
        maker_data.extend_from_slice(&price_raw.to_le_bytes());
        maker_data.extend_from_slice(&quantity_raw.to_le_bytes());
        maker_data.push(taker_side);
        maker_data.push(0u8); // role: maker
        maker_data.extend_from_slice(&block_height.to_le_bytes());
        maker_data.extend_from_slice(&timestamp.to_le_bytes());

        let mut maker_key = [0u8; 32];
        maker_key[..20].copy_from_slice(fill.maker.as_slice());
        maker_key[20..28].copy_from_slice(&desc_block.to_be_bytes());
        maker_key[28..32].copy_from_slice(&trade_index.to_be_bytes());

        // Taker entry (flip role byte at offset 57: 16+8+16+16+1)
        let mut taker_data = maker_data.clone();
        taker_data[57] = 1u8; // role: taker
        let mut taker_key = [0u8; 32];
        taker_key[..20].copy_from_slice(fill.taker.as_slice());
        taker_key[20..28].copy_from_slice(&desc_block.to_be_bytes());
        taker_key[28..32].copy_from_slice(&trade_index.to_be_bytes());

        Self {
            trade_key,
            trade_data,
            maker_key,
            maker_data,
            taker_key,
            taker_data,
        }
    }

}

#[test]
fn fixed_trade_rows_and_restamping_match_legacy_encoder() {
    use torus_core::order_book::Fill;
    for side in [Side::Buy, Side::Sell] {
        for same_trader in [false, true] {
            for raw in [i128::MIN, -1, 0, 1, i128::MAX] {
                let fill = Fill {
                    maker_order_id: u128::MAX,
                    taker_order_id: 7,
                    price: FixedPoint::from_raw(raw),
                    quantity: FixedPoint::from_raw(raw.wrapping_add(1)),
                    maker: Address::repeat_byte(0xA1),
                    taker: Address::repeat_byte(if same_trader { 0xA1 } else { 0xB2 }),
                    maker_side: side,
                    timestamp: 9, // persisted timestamp comes from the context
                };
                for (market, height, timestamp) in [(0, 0, 0), (u64::MAX, u64::MAX, u64::MAX)] {
                    for initial in [0, 0x8000_0001, u32::MAX] {
                        let mut actual = TradeKvs::build(market, height, timestamp, initial, &fill);
                        for index in [initial, 42, u32::MAX, 0] {
                            actual.stamp_trade_index(index);
                            let expected = LegacyTradeKvs::build(market, height, timestamp, index, &fill);
                            assert_eq!(actual.trade_key, expected.trade_key);
                            assert_eq!(actual.maker_key, expected.maker_key);
                            assert_eq!(actual.taker_key, expected.taker_key);
                            assert_eq!(actual.trade_data.as_slice(), expected.trade_data);
                            assert_eq!(actual.maker_data.as_slice(), expected.maker_data);
                            assert_eq!(actual.taker_data.as_slice(), expected.taker_data);
                            let mut packed = PackedCfBatch::default();
                            packed.push(CF_NATIVE_TRADES, &actual.trade_key, &actual.trade_data);
                            packed.push(CF_NATIVE_USER_TRADES, &actual.maker_key, &actual.maker_data);
                            packed.push(CF_NATIVE_USER_TRADES, &actual.taker_key, &actual.taker_data);
                            assert_eq!(packed.into_raw(), vec![
                                (CF_NATIVE_TRADES, expected.trade_key.to_vec(), expected.trade_data),
                                (CF_NATIVE_USER_TRADES, expected.maker_key.to_vec(), expected.maker_data),
                                (CF_NATIVE_USER_TRADES, expected.taker_key.to_vec(), expected.taker_data),
                            ]);
                        }
                    }
                }
            }
        }
    }
}
