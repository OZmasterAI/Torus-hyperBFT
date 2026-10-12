//! s114 (CoreWriter follow-up 1): the EVM tx that queued a CoreWriter /
//! lockbox queue row, `CF_CORE_WRITER_TX` ([`crate::cf::CF_CORE_WRITER_TX`]).
//!
//! A writer precompile (0x0810, 0x0811, 0x0820) only queues an action; the
//! queue row (key `target block ‖ seq`, in the hashed `cf_core_writer_queue`)
//! does not carry the tx that queued it, and must not (that would be a
//! consensus format change for an RPC convenience). This node-local index
//! links them instead: one row per queue row a SUCCESSFUL tx queued, written
//! in the queuing block's flush batch (with its EVM bundle and queue rows, so
//! a crash leaves both or neither), keyed by the same `target ‖ seq` as the
//! queue row. A block that queues nothing writes nothing here.
//!
//! Not hashed (not in `HASHED_CFS`), not a native-root CF: a pure function of
//! the committed block and its pre-state. Pruned with the TARGET block's body
//! (the block whose `coreWriterResults` it annotates).
//!
//! Value v1: `0x01 ‖ tx_hash 32 B ‖ call_index u32 BE` — `call_index` is the
//! row's position among the queue rows that tx queued (0 for its first
//! successful precompile call, in call order).

use alloy_primitives::B256;

/// First byte of a v1 value.
const CORE_WRITER_TX_V1: u8 = 0x01;
/// Length of a v1 value.
const V1_LEN: usize = 1 + 32 + 4;

/// The EVM tx that queued one CoreWriter queue row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreWriterTxLink {
    /// Hash of the EVM tx (in block `target - 1`) that queued the row.
    pub tx_hash: B256,
    /// The row's position among the rows this tx queued (call order).
    pub call_index: u32,
}

/// The index key of queue row `(target_block, seq)` — the queue row's own key.
pub fn key(target_block: u64, seq: u64) -> [u8; 16] {
    let mut k = [0u8; 16];
    k[..8].copy_from_slice(&target_block.to_be_bytes());
    k[8..].copy_from_slice(&seq.to_be_bytes());
    k
}

impl CoreWriterTxLink {
    pub fn encode(&self) -> [u8; V1_LEN] {
        let mut out = [0u8; V1_LEN];
        out[0] = CORE_WRITER_TX_V1;
        out[1..33].copy_from_slice(self.tx_hash.as_slice());
        out[33..].copy_from_slice(&self.call_index.to_be_bytes());
        out
    }

    /// `None` for an unknown version or a wrong length.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != V1_LEN || bytes[0] != CORE_WRITER_TX_V1 {
            return None;
        }
        Some(Self {
            tx_hash: B256::from_slice(&bytes[1..33]),
            call_index: u32::from_be_bytes(bytes[33..].try_into().ok()?),
        })
    }
}

/// Append one link row per `(target block, seq, link)` to `batch` (the
/// queuing block's flush batch). Nothing for an empty list.
pub fn append_links(
    db: &crate::db::StateDb,
    batch: &mut rocksdb::WriteBatch,
    links: &[(u64, u64, CoreWriterTxLink)],
) -> Result<(), crate::error::StateError> {
    if links.is_empty() {
        return Ok(());
    }
    let cf = db
        .inner()
        .cf_handle(crate::cf::CF_CORE_WRITER_TX)
        .ok_or_else(|| {
            crate::error::StateError::MissingColumnFamily(crate::cf::CF_CORE_WRITER_TX.to_string())
        })?;
    for (target, seq, link) in links {
        batch.put_cf(cf, key(*target, *seq), link.encode());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_round_trips_and_the_format_is_stable() {
        let link = CoreWriterTxLink {
            tx_hash: B256::repeat_byte(0xAB),
            call_index: 3,
        };
        let bytes = link.encode();
        let mut want = vec![0x01];
        want.extend_from_slice(&[0xAB; 32]);
        want.extend_from_slice(&[0, 0, 0, 3]);
        assert_eq!(bytes.to_vec(), want);
        assert_eq!(CoreWriterTxLink::decode(&bytes), Some(link));
        assert_eq!(CoreWriterTxLink::decode(&bytes[..36]), None);
        let mut long = bytes.to_vec();
        long.push(0);
        assert_eq!(CoreWriterTxLink::decode(&long), None);
        let mut v2 = bytes;
        v2[0] = 0x02;
        assert_eq!(CoreWriterTxLink::decode(&v2), None);
    }

    #[test]
    fn key_is_the_queue_row_key() {
        let k = key(7, 2);
        assert_eq!(&k[..8], &7u64.to_be_bytes());
        assert_eq!(&k[8..], &2u64.to_be_bytes());
    }
}
