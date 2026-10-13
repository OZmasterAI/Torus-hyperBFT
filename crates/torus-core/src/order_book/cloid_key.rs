//! perf/cloid-engine-cost fix 4: the packed `(trader, cloid)` key of the
//! cloid maps (a book's index and log, the executor's cross-market index,
//! the batch pre-pass's claimed set).

use std::hash::{Hash, Hasher};
use torus_types::{Address, Cloid};

/// `(trader, cloid)` packed into 36 bytes at align 8: 40 with padding,
/// against 48 for the tuple `(Address, u128)` (u128 is align 16).
///
/// Lossless: it holds every byte of both, and equality compares all of
/// them, so two keys are equal exactly when trader and cloid both are
/// ([`Self::trader`] and [`Self::cloid`] give them back). The hash feeds
/// the map's seeded hasher three fixed-width words (two `u128` and one
/// `u32`, every byte once) instead of a length prefix, a 20-byte slice and
/// a `u128`: the slice took foldhash's out-of-line long byte path. A hash
/// only picks a bucket; colliding keys are still told apart by the full
/// compare, so a collision can cost time but never change a lookup, and
/// none of these maps is iterated in an order that reaches a result (the
/// hasher is randomly seeded per process already).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CloidKey {
    /// The cloid, low and high 64 bits.
    cloid: [u64; 2],
    /// Trader bytes 0..16 as two native-endian words.
    head: [u64; 2],
    /// Trader bytes 16..20.
    tail: u32,
}

impl CloidKey {
    #[inline]
    pub fn new(trader: Address, cloid: Cloid) -> Self {
        let b: [u8; 20] = trader.0 .0;
        let word = |i: usize| u64::from_ne_bytes(b[i..i + 8].try_into().unwrap());
        Self {
            cloid: [cloid as u64, (cloid >> 64) as u64],
            head: [word(0), word(8)],
            tail: u32::from_ne_bytes(b[16..20].try_into().unwrap()),
        }
    }

    #[inline]
    pub fn trader(&self) -> Address {
        let mut b = [0u8; 20];
        b[0..8].copy_from_slice(&self.head[0].to_ne_bytes());
        b[8..16].copy_from_slice(&self.head[1].to_ne_bytes());
        b[16..20].copy_from_slice(&self.tail.to_ne_bytes());
        Address::new(b)
    }

    #[inline]
    pub fn cloid(&self) -> Cloid {
        (self.cloid[1] as u128) << 64 | self.cloid[0] as u128
    }
}

impl Hash for CloidKey {
    #[inline]
    fn hash<H: Hasher>(&self, h: &mut H) {
        h.write_u128((self.head[1] as u128) << 64 | self.head[0] as u128);
        h.write_u128((self.cloid[1] as u128) << 64 | self.cloid[0] as u128);
        h.write_u32(self.tail);
    }
}
