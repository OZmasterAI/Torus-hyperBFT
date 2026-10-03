//! Per-block executed/skipped record, `CF_BLOCK_ACTION_STATUS` (s84 decision 1).
//!
//! A certified block is never rejected for something inside it: execution
//! runs every action and SKIPS (no state change) any that fails its validity
//! check (unresolvable signature or session, replayed or duplicate nonce,
//! undecodable EVM tx, a tx revm refuses). This record says which, so the RPC
//! and the explorer show every action as executed or skipped.
//!
//! One row per executed block that carries at least one native action or EVM
//! tx, keyed by 8-byte BE height, written in the block's flush batch (same
//! atomic write as its state and applied-height marker).
//!
//! Node-local derived data, NOT hashed by the running state hash (like
//! receipts and block bodies): it is a pure function of the committed block and
//! its pre-state, and every skip decision already shows in hashed state (an
//! executed native action consumes its `CF_NATIVE_NONCES` row, an executed EVM
//! tx moves its sender's account nonce).
//!
//! Encoding (v1): `0x01 ‖ evm_count u32 BE ‖ native_count u32 BE ‖ evm bitmap ‖
//! native bitmap`, each bitmap `ceil(count / 8)` bytes, bit `i % 8` (LSB first)
//! of byte `i / 8` set = action `i` skipped.

/// First byte of a v1 record.
const ACTION_STATUS_V1: u8 = 0x01;

/// Which actions of one block execution skipped, by position in the block body
/// (`evm_transactions`, `native_actions`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockActionStatus {
    pub evm_skipped: Vec<bool>,
    pub native_skipped: Vec<bool>,
}

fn push_bitmap(out: &mut Vec<u8>, bits: &[bool]) {
    let start = out.len();
    out.resize(start + bits.len().div_ceil(8), 0);
    for (i, _) in bits.iter().enumerate().filter(|(_, skipped)| **skipped) {
        out[start + i / 8] |= 1 << (i % 8);
    }
}

fn read_bitmap(bytes: &[u8], count: usize) -> Vec<bool> {
    (0..count)
        .map(|i| bytes[i / 8] & (1 << (i % 8)) != 0)
        .collect()
}

impl BlockActionStatus {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            9 + self.evm_skipped.len().div_ceil(8) + self.native_skipped.len().div_ceil(8),
        );
        out.push(ACTION_STATUS_V1);
        out.extend_from_slice(&(self.evm_skipped.len() as u32).to_be_bytes());
        out.extend_from_slice(&(self.native_skipped.len() as u32).to_be_bytes());
        push_bitmap(&mut out, &self.evm_skipped);
        push_bitmap(&mut out, &self.native_skipped);
        out
    }

    /// `None` for an unknown version or a truncated / overlong record.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&version, rest) = bytes.split_first()?;
        if version != ACTION_STATUS_V1 || rest.len() < 8 {
            return None;
        }
        let evm_count = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
        let native_count = u32::from_be_bytes(rest[4..8].try_into().ok()?) as usize;
        let (evm_bits, native_bits) = rest[8..].split_at_checked(evm_count.div_ceil(8))?;
        if native_bits.len() != native_count.div_ceil(8) {
            return None;
        }
        Some(Self {
            evm_skipped: read_bitmap(evm_bits, evm_count),
            native_skipped: read_bitmap(native_bits, native_count),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_packs_one_bit_per_action() {
        let status = BlockActionStatus {
            evm_skipped: vec![false, true, false],
            native_skipped: (0..10).map(|i| i == 0 || i == 9).collect(),
        };
        let bytes = status.encode();
        // version + 2 counts + 1 EVM byte + 2 native bytes.
        assert_eq!(bytes.len(), 1 + 8 + 1 + 2);
        assert_eq!(bytes[9], 0b010);
        assert_eq!(&bytes[10..], &[0b1, 0b10]);
        assert_eq!(BlockActionStatus::decode(&bytes), Some(status));
    }

    #[test]
    fn empty_record_round_trips() {
        let status = BlockActionStatus::default();
        assert_eq!(BlockActionStatus::decode(&status.encode()), Some(status));
    }

    #[test]
    fn rejects_unknown_version_and_bad_lengths() {
        let bytes = BlockActionStatus {
            evm_skipped: vec![true],
            native_skipped: vec![true; 9],
        }
        .encode();
        let mut wrong_version = bytes.clone();
        wrong_version[0] = 0x02;
        assert_eq!(BlockActionStatus::decode(&wrong_version), None);
        assert_eq!(BlockActionStatus::decode(&bytes[..bytes.len() - 1]), None);
        let mut overlong = bytes.clone();
        overlong.push(0);
        assert_eq!(BlockActionStatus::decode(&overlong), None);
        assert_eq!(BlockActionStatus::decode(&[]), None);
    }
}
