//! On-disk record codec for `CF_BLOCK_BODIES` (one record per committed height).
//!
//! Two record formats coexist on disk and BOTH decode:
//!
//! * **Legacy JSON** — `serde_json::to_vec(&TorusBlockBody)`. Every JSON object
//!   record starts with `b'{'` (0x7B), which is how it is recognised.
//! * **Tagged bin v1** — [`BODY_RECORD_TAG_BIN_V1`] (0x01) followed by the
//!   `bincode` (v1, default options — the DA/gossip/compact-block wire encoding)
//!   of the body. 0x01 can never begin a JSON document, so the dispatch on the
//!   first byte is unambiguous.
//!
//! WHY (perf, consensus-thread commit critical path): the commit-time durable
//! body write (`persist_committed_block_durably`, FIX 1a) used to `clone()` every
//! native action into a `TorusBlockBody` and `serde_json::to_vec` it — for a
//! 200-action block of 400-order batches that is a multi-MB JSON text built on
//! the single HotStuff thread, then repeated once more on the exec thread. The
//! bin record is written straight from the borrowed `TorusBlock` (no clone) and
//! is several times smaller and faster to produce.
//!
//! The record format is a LOCAL storage detail: it is never hashed, gossiped, or
//! part of any consensus-visible value, so a node may switch formats (or run
//! mixed heights) without coordination. Every reader (`load_replay_body`, the RPC
//! body endpoints) goes through [`decode_body_record`].

use serde::Serialize;
use torus_types::{CoreWriterAction, SignedNativeAction, TorusBlock, TorusBlockBody};

/// First byte of a tagged bin (v1) body record.
pub const BODY_RECORD_TAG_BIN_V1: u8 = 0x01;

/// First byte of every legacy JSON body record (`serde_json` object).
const JSON_OBJECT_OPEN: u8 = b'{';

/// Borrowed mirror of [`TorusBlockBody`]: same field ORDER and TYPES-on-the-wire
/// (a `&[T]` bincode-encodes exactly like a `Vec<T>` — u64 length + elements, and
/// serde_json-encodes as the same array), so bytes produced from this decode as a
/// `TorusBlockBody`. Lets the writer skip the deep clone `TorusBlock::body()`
/// performs.
#[derive(Serialize)]
struct BodyRef<'a> {
    native_actions: &'a [SignedNativeAction],
    evm_transactions: &'a [Vec<u8>],
    core_writer_actions: &'a [CoreWriterAction],
}

/// Body record write format. Selected once per process by
/// [`body_record_format`] (default bin v1; `TORUS_BODY_RECORD_LEGACY_JSON=1`
/// forces the legacy JSON writer, e.g. ahead of a planned binary downgrade —
/// every reader on this branch decodes both).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyRecordFormat {
    LegacyJson,
    BinV1,
}

/// Pure parse of `TORUS_BODY_RECORD_LEGACY_JSON` (default: bin v1). Truthy
/// spellings `1`/`true`/`yes`/`on` select the legacy JSON writer.
pub fn parse_body_record_format(raw: Option<String>) -> BodyRecordFormat {
    match raw.as_deref().map(str::trim) {
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("on") => {
            BodyRecordFormat::LegacyJson
        }
        _ => BodyRecordFormat::BinV1,
    }
}

/// Process-wide write format (read once at first use, like the other
/// `TORUS_*` runtime knobs in this crate).
pub fn body_record_format() -> BodyRecordFormat {
    static FORMAT: std::sync::OnceLock<BodyRecordFormat> = std::sync::OnceLock::new();
    *FORMAT.get_or_init(|| {
        parse_body_record_format(std::env::var("TORUS_BODY_RECORD_LEGACY_JSON").ok())
    })
}

fn encode_ref(body_ref: &BodyRef<'_>, format: BodyRecordFormat) -> Result<Vec<u8>, String> {
    match format {
        BodyRecordFormat::LegacyJson => serde_json::to_vec(body_ref).map_err(|e| e.to_string()),
        BodyRecordFormat::BinV1 => {
            let payload_len = bincode::serialized_size(body_ref).map_err(|e| e.to_string())?;
            let mut out = Vec::with_capacity(1 + payload_len as usize);
            out.push(BODY_RECORD_TAG_BIN_V1);
            bincode::serialize_into(&mut out, body_ref).map_err(|e| e.to_string())?;
            Ok(out)
        }
    }
}

/// Encode a block's body as a `CF_BLOCK_BODIES` record in the process-wide
/// write format, borrowing the block's payload (no clone of the actions).
pub fn encode_body_record(block: &TorusBlock) -> Result<Vec<u8>, String> {
    encode_body_record_as(block, body_record_format())
}

/// [`encode_body_record`] with an explicit format (tests / tooling).
pub fn encode_body_record_as(
    block: &TorusBlock,
    format: BodyRecordFormat,
) -> Result<Vec<u8>, String> {
    encode_ref(
        &BodyRef {
            native_actions: &block.native_actions,
            evm_transactions: &block.evm_transactions,
            core_writer_actions: &block.core_writer_actions,
        },
        format,
    )
}

/// Encode an already-materialized [`TorusBlockBody`] (RPC/test fixtures) in the
/// process-wide write format. Byte-identical to [`encode_body_record`] over the
/// block it was taken from.
pub fn encode_body_record_owned(body: &TorusBlockBody) -> Result<Vec<u8>, String> {
    encode_body_record_owned_as(body, body_record_format())
}

/// [`encode_body_record_owned`] with an explicit format.
pub fn encode_body_record_owned_as(
    body: &TorusBlockBody,
    format: BodyRecordFormat,
) -> Result<Vec<u8>, String> {
    encode_ref(
        &BodyRef {
            native_actions: &body.native_actions,
            evm_transactions: &body.evm_transactions,
            core_writer_actions: &body.core_writer_actions,
        },
        format,
    )
}

/// Which format a stored record is in, by its first byte. `None` for an empty
/// record or an unknown tag (treated as corrupt by [`decode_body_record`]).
pub fn body_record_format_of(bytes: &[u8]) -> Option<BodyRecordFormat> {
    match bytes.first() {
        Some(&BODY_RECORD_TAG_BIN_V1) => Some(BodyRecordFormat::BinV1),
        Some(&JSON_OBJECT_OPEN) => Some(BodyRecordFormat::LegacyJson),
        _ => None,
    }
}

/// Decode a `CF_BLOCK_BODIES` record of EITHER format.
pub fn decode_body_record(bytes: &[u8]) -> Result<TorusBlockBody, String> {
    match body_record_format_of(bytes) {
        Some(BodyRecordFormat::BinV1) => bincode::deserialize::<TorusBlockBody>(&bytes[1..])
            .map_err(|e| format!("bin body record: {e}")),
        Some(BodyRecordFormat::LegacyJson) => serde_json::from_slice::<TorusBlockBody>(bytes)
            .map_err(|e| format!("json body record: {e}")),
        None => Err(match bytes.first() {
            None => "empty body record".to_string(),
            Some(b) => format!("unknown body record tag 0x{b:02x}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use torus_types::{
        ActionSignature, FixedPoint, NativeAction, OrderType, PlaceOrderParams, Signature,
        TimeInForce, TorusBlockHeader,
    };

    fn order(i: u64) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: i % 10,
            is_buy: i % 2 == 0,
            price: FixedPoint::from_raw(1_000_000 + i as i128),
            quantity: FixedPoint::from_raw(5_000 + i as i128),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: Some(i),
        }
    }

    fn signed_batch(seed: u64, n: usize) -> SignedNativeAction {
        SignedNativeAction {
            action: NativeAction::PlaceOrderBatch(
                (0..n as u64).map(|i| order(seed * 1000 + i)).collect(),
            ),
            nonce: 1_700_000_000_000 + seed,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [seed as u8; 32],
                s: [!(seed as u8); 32],
            }),
        }
    }

    fn block(height: u64, actions: usize, batch: usize) -> TorusBlock {
        TorusBlock {
            header: TorusBlockHeader {
                height,
                parent_hash: B256::ZERO,
                timestamp: 1,
                proposer: Address::ZERO,
                state_root: B256::ZERO,
                receipts_root: B256::ZERO,
                logs_bloom: Default::default(),
                evm_gas_used: 0,
                evm_fee_revenue: 0,
                evm_gas_limit: 0,
                native_action_count: actions as u32,
                evm_tx_count: 0,
                base_fee_per_gas: 0,
                epoch: 0,
                validator_set_hash: B256::ZERO,
                sig_attestation: [0u8; 64],
            },
            native_actions: (0..actions as u64)
                .map(|s| signed_batch(s, batch))
                .collect(),
            evm_transactions: vec![vec![1, 2, 3], vec![]],
            core_writer_actions: vec![CoreWriterAction::CancelOrder { order_id: 7 }],
        }
    }

    fn body_eq(a: &TorusBlockBody, b: &TorusBlockBody) -> bool {
        // TorusBlockBody has no PartialEq; compare via the canonical JSON form.
        serde_json::to_vec(a).unwrap() == serde_json::to_vec(b).unwrap()
    }

    #[test]
    fn bin_record_round_trips_and_is_tagged() {
        let blk = block(5, 3, 4);
        let rec = encode_body_record_as(&blk, BodyRecordFormat::BinV1).unwrap();
        assert_eq!(rec[0], BODY_RECORD_TAG_BIN_V1);
        assert_eq!(body_record_format_of(&rec), Some(BodyRecordFormat::BinV1));
        let back = decode_body_record(&rec).unwrap();
        assert!(body_eq(&back, &blk.body()));
        // The tagged payload IS the wire (bincode) encoding of the owned body.
        assert_eq!(&rec[1..], &bincode::serialize(&blk.body()).unwrap()[..]);
    }

    #[test]
    fn legacy_json_record_still_decodes() {
        let blk = block(6, 2, 3);
        let legacy = serde_json::to_vec(&blk.body()).unwrap();
        assert_eq!(
            body_record_format_of(&legacy),
            Some(BodyRecordFormat::LegacyJson)
        );
        let back = decode_body_record(&legacy).unwrap();
        assert!(body_eq(&back, &blk.body()));
        // The explicit legacy writer produces the same bytes serde_json did.
        assert_eq!(
            encode_body_record_as(&blk, BodyRecordFormat::LegacyJson).unwrap(),
            legacy
        );
    }

    #[test]
    fn borrowed_and_owned_encoders_are_byte_identical() {
        let blk = block(7, 4, 5);
        for f in [BodyRecordFormat::BinV1, BodyRecordFormat::LegacyJson] {
            assert_eq!(
                encode_body_record_as(&blk, f).unwrap(),
                encode_body_record_owned_as(&blk.body(), f).unwrap()
            );
        }
    }

    #[test]
    fn empty_body_round_trips_in_both_formats() {
        let blk = TorusBlock {
            evm_transactions: vec![],
            core_writer_actions: vec![],
            ..block(1, 0, 0)
        };
        for f in [BodyRecordFormat::BinV1, BodyRecordFormat::LegacyJson] {
            let rec = encode_body_record_as(&blk, f).unwrap();
            let back = decode_body_record(&rec).unwrap();
            assert!(back.native_actions.is_empty() && back.evm_transactions.is_empty());
        }
    }

    #[test]
    fn corrupt_records_are_rejected_not_panicked() {
        assert!(decode_body_record(&[]).is_err());
        assert!(decode_body_record(&[0x02, 1, 2, 3]).is_err());
        assert!(decode_body_record(b"body-3").is_err());
        assert!(decode_body_record(&[BODY_RECORD_TAG_BIN_V1, 0xff, 0xff]).is_err());
        assert!(decode_body_record(b"{not json").is_err());
    }

    #[test]
    fn format_knob_defaults_to_bin_and_accepts_legacy_spellings() {
        assert_eq!(parse_body_record_format(None), BodyRecordFormat::BinV1);
        assert_eq!(
            parse_body_record_format(Some("0".into())),
            BodyRecordFormat::BinV1
        );
        assert_eq!(
            parse_body_record_format(Some("".into())),
            BodyRecordFormat::BinV1
        );
        for s in ["1", "true", "yes", "on", " 1 "] {
            assert_eq!(
                parse_body_record_format(Some(s.into())),
                BodyRecordFormat::LegacyJson,
                "{s}"
            );
        }
    }

    /// Size sanity on a bench-shaped body (100 actions x 400-order batches): the
    /// bin record must be materially smaller than the JSON one. Encode times are
    /// printed for the run log (not asserted — timing is not a unit-test contract).
    #[test]
    fn bin_record_is_smaller_than_json_on_bench_shaped_body() {
        let blk = block(9, 100, 400);
        let t0 = std::time::Instant::now();
        let json = encode_body_record_as(&blk, BodyRecordFormat::LegacyJson).unwrap();
        let t_json = t0.elapsed();
        let t1 = std::time::Instant::now();
        let bin = encode_body_record_as(&blk, BodyRecordFormat::BinV1).unwrap();
        let t_bin = t1.elapsed();
        let t2 = std::time::Instant::now();
        let legacy_clone_json = serde_json::to_vec(&blk.body()).unwrap();
        let t_clone_json = t2.elapsed();
        eprintln!(
            "body(100x400): json={}B in {:?} (clone+json {:?}) | bin={}B in {:?}",
            json.len(),
            t_json,
            t_clone_json,
            bin.len(),
            t_bin
        );
        assert_eq!(json, legacy_clone_json);
        assert!(
            bin.len() * 2 < json.len(),
            "bin {} vs json {}",
            bin.len(),
            json.len()
        );
    }
}
