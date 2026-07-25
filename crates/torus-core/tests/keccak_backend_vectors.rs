//! Keccak-256 backend byte-identity pins (`alloy-primitives/asm-keccak`).
//!
//! # Why this file exists
//!
//! The workspace enables `alloy-primitives`' `asm-keccak` feature (root
//! `Cargo.toml`), which swaps `alloy_primitives::keccak256` from the pure-Rust
//! RustCrypto `sha3` core to the cryptogams assembly backend
//! (`keccak-asm` → `sha3-asm`). `keccak256` is the preimage function under
//! EVERY consensus commitment in this chain — state root, mode-2 `level_hash`,
//! tx/block hashes, EVM `SHA3`. A backend that disagreed on ONE byte for ONE
//! input would fork the network.
//!
//! # The practical difficulty, stated honestly
//!
//! Only ONE backend is compiled into `alloy_primitives::keccak256` per build
//! (the choice is a `cfg_if` on the feature, `alloy-primitives/src/utils/mod.rs`),
//! so a single test binary cannot call "alloy-with-asm" and "alloy-without-asm"
//! side by side. This file therefore pins against fixed expected digests plus a
//! second, genuinely independent in-build implementation:
//!
//! 1. **`asm_backend_matches_pinned_vectors`** — asserts
//!    `alloy_primitives::keccak256` against HARD-CODED constants. Four of them
//!    are externally published values that anyone can verify without this repo
//!    (see `PUBLISHED` below); the rest were generated from the pure-Rust
//!    `sha3` crate (the exact implementation the non-`asm-keccak` build of
//!    alloy-primitives uses internally) and hard-coded.
//! 2. **`asm_backend_matches_pure_rust_sha3`** — recomputes every vector with
//!    `sha3::Keccak256` (RustCrypto, pure Rust, a direct `torus-core`
//!    dependency and NOT affected by alloy's feature flags) inside this same
//!    binary and compares. This is a live cross-implementation differential of
//!    the asm backend against a pure-Rust one on the machine actually building
//!    the node.
//!
//! Inputs deliberately straddle the Keccak-256 rate (136 bytes = 1088 bits):
//! empty, 1 byte, 135/136/137, 271/272/273 (second block boundary), all-zero
//! and all-`0xff` full blocks, a 4 KB ramp, and pseudorandom 1 KB / 64 KB
//! buffers, so single-block, exact-fit, padding-only-block and multi-block
//! absorb paths are all exercised.
//!
//! # What this DOES prove
//!
//! - The `keccak256` compiled into this build returns the same 32 bytes as the
//!   pure-Rust reference for every input above, including all rate-boundary
//!   cases, and the same bytes as externally published Keccak-256 vectors.
//! - Therefore enabling `asm-keccak` did not change any digest this workspace
//!   commits to, for these inputs.
//!
//! # What this does NOT prove
//!
//! - It is not exhaustive over all inputs. It is vector coverage of the
//!   documented edge cases, not a proof of equivalence over the whole domain.
//! - It says nothing about OTHER target architectures: the cryptogams script
//!   selected by `sha3-asm/build.rs` is arch- and target-feature-dependent
//!   (x86_64 baseline vs avx512vl vs zen5 vs aarch64...). A different build
//!   machine exercises different assembly, so this test must pass ON the
//!   machine/target that produces the release binary, not just once in CI.
//! - It does not cover the streaming `alloy_primitives::Keccak256` state type
//!   (whose concrete type also changes with the feature). That surface is
//!   covered by `keccak_stream_clone_equivalence` in
//!   `crates/torus-core/src/order_book.rs`, which compares streaming
//!   `sha3::Keccak256` against one-shot `alloy_primitives::keccak256` at random
//!   split points.

use sha3::Digest;

/// Digests taken from published sources, independent of this repository and of
/// any Rust Keccak crate. If the backend ever disagrees with one of these, the
/// backend is wrong.
///
/// - `keccak256("")` — the canonical empty-input Keccak-256 digest (the EVM's
///   `EMPTY_CODE_HASH` / `EMPTY_STRING_HASH`).
/// - `keccak256("abc")` — the classic Keccak-256 test vector.
/// - `keccak256("hello")` — widely published.
/// - `keccak256("Transfer(address,address,uint256)")` — ERC-20 `Transfer`
///   event `topic0`, verifiable against any Ethereum block explorer.
const PUBLISHED: &[&str] = &["empty", "abc", "hello", "erc20_transfer_sig"];

/// `(name, input, expected keccak256 hex)`.
///
/// Provenance: entries named in [`PUBLISHED`] are externally sourced; the rest
/// were produced by the pure-Rust `sha3` crate (see the module docs) and are
/// pinned here so the assertion does not depend on regenerating them.
fn vectors() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let mut xs = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    vec![
        // --- published, externally verifiable ---
        (
            "empty",
            vec![],
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470",
        ),
        (
            "abc",
            b"abc".to_vec(),
            "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45",
        ),
        (
            "hello",
            b"hello".to_vec(),
            "1c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36deac8",
        ),
        (
            "erc20_transfer_sig",
            b"Transfer(address,address,uint256)".to_vec(),
            "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
        ),
        // --- single bytes ---
        (
            "one_byte_00",
            vec![0x00],
            "bc36789e7a1e281436464229828f817d6612f7b477d66591ff96a9e064bcc98a",
        ),
        (
            "one_byte_ff",
            vec![0xff],
            "8b1a944cf13a9a1c08facb2c9e98623ef3254d2ddb48113885c3e8e97fec8db9",
        ),
        // --- rate boundary: 136 B is the Keccak-256 rate ---
        (
            "ramp_135",
            (0..135u32).map(|i| i as u8).collect(),
            "cbdfd9dee5faad3818d6b06f95a219fd290b0e1706f6a82e5a595b9ce9faca62",
        ),
        (
            "ramp_136",
            (0..136u32).map(|i| i as u8).collect(),
            "7ce759f1ab7f9ce437719970c26b0a66ff11fe3e38e17df89cf5d29c7d7f807e",
        ),
        (
            "ramp_137",
            (0..137u32).map(|i| i as u8).collect(),
            "ac73d4fae68b8453f764007c1a20ce95994187861f0c3227a3a8e99a73a3b1db",
        ),
        // --- second block boundary (2 * 136 = 272) ---
        (
            "ramp_271",
            (0..271u32).map(|i| i as u8).collect(),
            "7c974895b2a88303ff2dc6b58f438ceb0b298cac91099ac0539cc0f477506191",
        ),
        (
            "ramp_272",
            (0..272u32).map(|i| i as u8).collect(),
            "fdf2ec49e749960d3c8521a0219af8d03e30e2b3bf19bd16150ee0eaf133d66e",
        ),
        (
            "ramp_273",
            (0..273u32).map(|i| i as u8).collect(),
            "4f707289a9c3ccd0c4a51f2f17339f5dd171d371c04ff7783b735b5b22682eaf",
        ),
        // --- degenerate full blocks (all-zero / all-ones lane content) ---
        (
            "zeros_136",
            vec![0u8; 136],
            "3a5912a7c5faa06ee4fe906253e339467a9ce87d533c65be3c15cb231cdb25f9",
        ),
        (
            "ff_136",
            vec![0xffu8; 136],
            "2d417340362cd4144efbf52adc1bfb7a4b40254f55f3b0f09efa6a1ef299b51a",
        ),
        // --- multi-kilobyte / pseudorandom ---
        (
            "lcg_4096",
            (0..4096u32)
                .map(|i| (i.wrapping_mul(31).wrapping_add(7)) as u8)
                .collect(),
            "1a8708a1b490543348ef1468dc6dd07aa8f556e311f5ce378e71452b0d180d03",
        ),
        (
            "xorshift_1000",
            (0..1000).map(|_| (xs() & 0xFF) as u8).collect(),
            "1539b98dbdacac14cc519260deebd66b0aeaa242d490551e39d14d7a2a1b2836",
        ),
        (
            "xorshift_65536",
            (0..65536).map(|_| (xs() & 0xFF) as u8).collect(),
            "1597b1d83e38ffe62c8073b0f1dfbd9d3a737cc99618ae8f923aee4885e5f679",
        ),
    ]
}

fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Pin 1: whichever backend is compiled in must reproduce the fixed digests.
#[test]
fn asm_backend_matches_pinned_vectors() {
    let mut published_seen = 0usize;
    for (name, data, expected) in vectors() {
        let got = alloy_primitives::keccak256(&data).0;
        assert_eq!(
            hex_of(&got),
            expected,
            "keccak256 backend disagrees with pinned vector `{name}` (len={})",
            data.len()
        );
        if PUBLISHED.contains(&name) {
            published_seen += 1;
        }
    }
    assert_eq!(
        published_seen,
        PUBLISHED.len(),
        "an externally-published anchor vector went missing from the table"
    );
}

/// Pin 2: live differential against a pure-Rust Keccak-256 in the same binary.
///
/// `sha3` is a direct dependency of this crate and is NOT reconfigured by
/// alloy-primitives' `asm-keccak` feature, so with the feature ON this compares
/// assembly against pure Rust; with it OFF it is a (still useful, but weaker)
/// same-family check.
#[test]
fn asm_backend_matches_pure_rust_sha3() {
    for (name, data, _) in vectors() {
        let alloy = alloy_primitives::keccak256(&data).0;
        let pure: [u8; 32] = sha3::Keccak256::digest(&data).into();
        assert_eq!(
            hex_of(&alloy),
            hex_of(&pure),
            "alloy keccak256 backend != pure-Rust sha3 on `{name}` (len={})",
            data.len()
        );
    }
}

/// Regeneration aid: prints the table in source form from the pure-Rust
/// backend. Ignored by default; run with
/// `cargo test -p torus-core --test keccak_backend_vectors -- --ignored --nocapture regenerate`.
#[test]
#[ignore = "generator, not an assertion"]
fn regenerate_vectors() {
    for (name, data, _) in vectors() {
        let d: [u8; 32] = sha3::Keccak256::digest(&data).into();
        println!("(\"{}\", \"{}\"),", name, hex_of(&d));
    }
}
