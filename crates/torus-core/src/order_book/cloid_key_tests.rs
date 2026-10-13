//! perf/cloid-engine-cost fix 4: [`CloidKey`] is a lossless, smaller
//! `(trader, cloid)`, and a map keyed by it answers like one keyed by the
//! tuple even when every key hashes alike.

use super::*;
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};

/// splitmix64.
fn mix(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn key_parts(seed: &mut u64) -> (Address, Cloid) {
    let mut b = [0u8; 20];
    for chunk in b.chunks_mut(8) {
        let w = mix(seed).to_le_bytes();
        chunk.copy_from_slice(&w[..chunk.len()]);
    }
    (
        Address::new(b),
        (mix(seed) as u128) << 64 | mix(seed) as u128,
    )
}

/// Every key one byte away from `(t, c)` (36 positions x 2 flips).
fn neighbours(t: Address, c: Cloid) -> Vec<(Address, Cloid)> {
    let mut out = Vec::new();
    for i in 0..20 {
        for flip in [0x01u8, 0x80] {
            let mut b = t.0 .0;
            b[i] ^= flip;
            out.push((Address::new(b), c));
        }
    }
    for bit in [0u32, 7, 63, 64, 71, 127]
        .into_iter()
        .chain((8..128).step_by(8))
    {
        out.push((t, c ^ (1u128 << bit)));
    }
    out
}

#[test]
fn a_cloid_key_round_trips_and_is_smaller() {
    let mut seed = 7;
    for _ in 0..1_000 {
        let (t, c) = key_parts(&mut seed);
        let k = CloidKey::new(t, c);
        assert_eq!((k.trader(), k.cloid()), (t, c));
    }
    for (t, c) in [
        (Address::ZERO, 0),
        (Address::new([0xff; 20]), u128::MAX),
        (Address::new([1; 20]), 1 << 64),
    ] {
        let k = CloidKey::new(t, c);
        assert_eq!((k.trader(), k.cloid()), (t, c));
    }
    // The key (a log record, a claimed-set entry): 40 bytes, was 48. A
    // book's index entry stays 64 (the u128 id keeps align 16).
    assert_eq!(std::mem::size_of::<CloidKey>(), 40);
    assert_eq!(std::mem::size_of::<(Address, Cloid)>(), 48);
    assert_eq!(std::mem::size_of::<(CloidKey, OrderId)>(), 64);
    assert_eq!(std::mem::size_of::<((Address, Cloid), OrderId)>(), 64);
}

/// Equal keys are equal tuples and hash alike; a key one byte away from
/// another is a different key.
#[test]
fn a_cloid_key_is_equal_exactly_when_trader_and_cloid_are() {
    let s = alloy_primitives::map::DefaultHashBuilder::default();
    let mut seed = 11;
    for _ in 0..200 {
        let (t, c) = key_parts(&mut seed);
        let k = CloidKey::new(t, c);
        assert_eq!(k, CloidKey::new(t, c));
        assert_eq!(s.hash_one(k), s.hash_one(CloidKey::new(t, c)));
        for (t2, c2) in neighbours(t, c) {
            assert_ne!(k, CloidKey::new(t2, c2), "{t2} {c2:#x}");
        }
    }
}

/// A hasher that sends every key to one bucket.
#[derive(Default)]
struct Collide;

impl Hasher for Collide {
    fn finish(&self) -> u64 {
        42
    }

    fn write(&mut self, _: &[u8]) {}
}

/// Collisions never change an answer: with every key hashing alike, a map
/// keyed by `CloidKey` holds and finds exactly what the tuple-keyed map
/// does (inserts, overwrites, removes, misses on one-byte neighbours).
#[test]
fn colliding_cloid_keys_still_answer_like_the_tuple() {
    let mut packed: std::collections::HashMap<CloidKey, u64, BuildHasherDefault<Collide>> =
        Default::default();
    let mut plain: std::collections::HashMap<(Address, Cloid), u64> = Default::default();
    let mut seed = 13;
    let mut keys = Vec::new();
    for _ in 0..40 {
        let (t, c) = key_parts(&mut seed);
        keys.push((t, c));
        keys.extend(neighbours(t, c).into_iter().take(6));
    }
    for (i, &(t, c)) in keys.iter().enumerate() {
        let v = mix(&mut seed);
        match i % 5 {
            0..=2 => assert_eq!(
                packed.insert(CloidKey::new(t, c), v),
                plain.insert((t, c), v)
            ),
            3 => assert_eq!(packed.remove(&CloidKey::new(t, c)), plain.remove(&(t, c))),
            _ => {
                let (t2, c2) = keys[(v % keys.len() as u64) as usize];
                assert_eq!(
                    packed.remove(&CloidKey::new(t2, c2)),
                    plain.remove(&(t2, c2))
                );
            }
        }
    }
    assert_eq!(packed.len(), plain.len());
    for &(t, c) in &keys {
        assert_eq!(packed.get(&CloidKey::new(t, c)), plain.get(&(t, c)));
    }
}
