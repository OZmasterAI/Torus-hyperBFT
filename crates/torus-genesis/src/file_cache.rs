//! Restart fast path for the genesis file digest (s140 follow-up of bench 9n).
//!
//! [`GenesisDigest::of_json`] canonicalizes the whole file: on a 295 MB bench
//! genesis it costs several seconds (the bulk of a restart), and a restart
//! digests the same file again. A node that computed the digest stores a
//! [`DigestCacheEntry`]: the file's length and SHA-256 over its raw bytes and
//! the digest computed from those bytes. A later boot hashes the raw bytes
//! (fast) and takes the stored digest only when length and SHA-256 match;
//! any other file (edited, reformatted, truncated) is digested in full.
//!
//! **Why it is safe.** The digest is a function of the file bytes (and of
//! the canonical form, named by [`DIGEST_ALGORITHM`]). The bytes hashed are
//! the very buffer the typed genesis is parsed from (one read), so the
//! digest the node binds to and advertises to peers is the digest of the
//! genesis it runs. An entry is used only together with the data dir's
//! stored [`GenesisDigest`] it names, so the P0.0 binding check and the
//! peer handshake see exactly the value a full digest would produce.
//!
//! Any change to the canonical form ([`crate::digest`]) MUST change
//! [`DIGEST_ALGORITHM`], or a cached digest of the old form would survive it.

use std::path::Path;

use alloy_primitives::B256;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::digest::GenesisDigest;
use crate::{Genesis, GenesisError};

/// Names the canonical form a cached digest was computed with (the digest's
/// domain plus a revision of the canonicalization code).
pub const DIGEST_ALGORITHM: &str = "torus-genesis-file-v1/canon-r2";

/// Version of [`DigestCacheEntry::encode`]'s form.
const ENTRY_VERSION: u64 = 1;

/// The exact bytes of a genesis file: length and SHA-256.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFingerprint {
    pub len: u64,
    pub sha256: B256,
}

impl FileFingerprint {
    pub fn of(bytes: &[u8]) -> Self {
        FileFingerprint {
            len: bytes.len() as u64,
            sha256: B256::from_slice(&Sha256::digest(bytes)),
        }
    }
}

/// A stored result: the file with `fingerprint` digests to `digest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestCacheEntry {
    pub fingerprint: FileFingerprint,
    pub digest: B256,
}

impl DigestCacheEntry {
    /// The stored form: JSON
    /// `{"algorithm":..,"digest":"0x..","len":N,"sha256":"0x..","version":1}`.
    pub fn encode(&self) -> Vec<u8> {
        let v = serde_json::json!({
            "version": ENTRY_VERSION,
            "algorithm": DIGEST_ALGORITHM,
            "len": self.fingerprint.len,
            "sha256": format!("{:#x}", self.fingerprint.sha256),
            "digest": format!("{:#x}", self.digest),
        });
        crate::digest::canonical_json(&v)
    }

    /// Read [`Self::encode`]'s form back. `Ok(None)`: an entry of another
    /// version or canonical form (not usable by this build). `Err`: not an
    /// entry at all.
    pub fn decode(bytes: &[u8]) -> Result<Option<Self>, String> {
        let v: Value = serde_json::from_slice(bytes)
            .map_err(|e| format!("stored genesis digest cache: {e}"))?;
        if v["version"].as_u64() != Some(ENTRY_VERSION)
            || v["algorithm"].as_str() != Some(DIGEST_ALGORITHM)
        {
            return Ok(None);
        }
        let hash = |key: &str| -> Result<B256, String> {
            v[key]
                .as_str()
                .and_then(|s| s.parse::<B256>().ok())
                .ok_or_else(|| format!("stored genesis digest cache: bad {key} {}", v[key]))
        };
        let len = v["len"]
            .as_u64()
            .ok_or_else(|| format!("stored genesis digest cache: bad len {}", v["len"]))?;
        Ok(Some(DigestCacheEntry {
            fingerprint: FileFingerprint { len, sha256: hash("sha256")? },
            digest: hash("digest")?,
        }))
    }
}

/// How [`load_genesis_file`] got the digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestSource {
    /// Computed from the file ([`GenesisDigest::of_json`]).
    Computed,
    /// The stored digest: the file's bytes are the ones it was computed from.
    Cached,
}

/// A genesis file as boot reads it.
#[derive(Debug, Clone)]
pub struct LoadedGenesis {
    pub genesis: Genesis,
    pub digest: GenesisDigest,
    pub fingerprint: FileFingerprint,
    pub source: DigestSource,
}

impl LoadedGenesis {
    /// The cache entry to store once the digest is bound: `Some` when it was
    /// computed (a cached one is already stored).
    pub fn cache_entry_to_store(&self) -> Option<DigestCacheEntry> {
        (self.source == DigestSource::Computed).then(|| DigestCacheEntry {
            fingerprint: self.fingerprint,
            digest: self.digest.digest,
        })
    }
}

/// Read and parse the genesis file at `path` and digest it. `cache` is the
/// data dir's stored entry and the stored digest; the stored digest is taken
/// only when the entry names it and the file's bytes match the entry.
pub fn load_genesis_file(
    path: &Path,
    cache: Option<(&DigestCacheEntry, &GenesisDigest)>,
) -> Result<LoadedGenesis, GenesisError> {
    let _ = cache;
    let text = std::fs::read_to_string(path)?;
    let genesis = Genesis::from_json(&text)?;
    let fingerprint = FileFingerprint::of(text.as_bytes());
    let digest = GenesisDigest::of_json(&text)?;
    Ok(LoadedGenesis { genesis, digest, fingerprint, source: DigestSource::Computed })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVNET: &str = include_str!("../../../devnet/genesis.json");

    fn edit(f: impl FnOnce(&mut Value)) -> String {
        let mut v: Value = serde_json::from_str(DEVNET).unwrap();
        f(&mut v);
        serde_json::to_string_pretty(&v).unwrap()
    }

    /// A file written to a temp dir, loaded with `cache`; also returns how
    /// many full digests the load computed.
    fn load(
        text: &str,
        cache: Option<(&DigestCacheEntry, &GenesisDigest)>,
    ) -> (LoadedGenesis, usize) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("genesis.json");
        std::fs::write(&path, text).unwrap();
        let before = crate::digest::full_digest_count();
        let loaded = load_genesis_file(&path, cache).unwrap();
        (loaded, crate::digest::full_digest_count() - before)
    }

    fn first_boot(text: &str) -> (DigestCacheEntry, GenesisDigest) {
        let (loaded, n) = load(text, None);
        assert_eq!((loaded.source, n), (DigestSource::Computed, 1));
        (loaded.cache_entry_to_store().expect("a computed digest is stored"), loaded.digest)
    }

    #[test]
    fn the_same_file_takes_the_stored_digest_without_digesting() {
        let (entry, stored) = first_boot(DEVNET);
        assert_eq!(entry.fingerprint, FileFingerprint::of(DEVNET.as_bytes()));
        assert_eq!(entry.digest, stored.digest);
        let (again, n) = load(DEVNET, Some((&entry, &stored)));
        assert_eq!(again.source, DigestSource::Cached);
        assert_eq!(n, 0, "a restart on the same file re-digested it");
        // Exactly what a full digest gives: the binding check and the peer
        // handshake see the same value.
        assert_eq!(again.digest, GenesisDigest::of_json(DEVNET).unwrap());
        assert_eq!(again.cache_entry_to_store(), None, "nothing new to store");
        assert_eq!(again.genesis.chain_name, "torus-devnet", "the typed genesis is still parsed");
    }

    #[test]
    fn a_changed_file_is_digested_in_full() {
        let (entry, stored) = first_boot(DEVNET);
        for text in [
            edit(|v| v["chain_name"] = "edited".into()),
            // Same length: one digit changed.
            DEVNET.replacen("\"timeout_base_ms\": 1200", "\"timeout_base_ms\": 1300", 1),
        ] {
            assert_ne!(text, DEVNET);
            let (loaded, n) = load(&text, Some((&entry, &stored)));
            assert_eq!((loaded.source, n), (DigestSource::Computed, 1), "{text:.120}");
            assert_eq!(loaded.digest, GenesisDigest::of_json(&text).unwrap());
            assert_ne!(loaded.digest.digest, stored.digest, "an edit changes the digest");
        }
    }

    #[test]
    fn a_reformatted_file_is_digested_again_and_keeps_its_digest() {
        let (entry, stored) = first_boot(DEVNET);
        let compact = serde_json::to_string(&serde_json::from_str::<Value>(DEVNET).unwrap()).unwrap();
        let (loaded, n) = load(&compact, Some((&entry, &stored)));
        assert_eq!((loaded.source, n), (DigestSource::Computed, 1));
        assert_eq!(loaded.digest, stored);
        let new = loaded.cache_entry_to_store().expect("the new bytes get an entry");
        assert_eq!(new.fingerprint, FileFingerprint::of(compact.as_bytes()));
        let (again, n) = load(&compact, Some((&new, &stored)));
        assert_eq!((again.source, n), (DigestSource::Cached, 0));
    }

    #[test]
    fn an_entry_naming_another_digest_is_not_used() {
        let (entry, stored) = first_boot(DEVNET);
        let other = GenesisDigest::of_json(&edit(|v| v["chain_name"] = "x".into())).unwrap();
        // The stored digest is not the one the entry was computed for.
        let (loaded, n) = load(DEVNET, Some((&entry, &other)));
        assert_eq!((loaded.source, n), (DigestSource::Computed, 1));
        assert_eq!(loaded.digest, stored);
        assert_ne!(loaded.digest.digest, other.digest);
    }

    #[test]
    fn a_file_the_typed_parse_refuses_is_refused_with_a_matching_entry() {
        let bad = edit(|v| v["evm"]["block_interval"] = 0.into());
        let fp = FileFingerprint::of(bad.as_bytes());
        let digest = GenesisDigest::of_json(&bad).unwrap();
        let entry = DigestCacheEntry { fingerprint: fp, digest: digest.digest };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("genesis.json");
        std::fs::write(&path, &bad).unwrap();
        let err = load_genesis_file(&path, Some((&entry, &digest))).unwrap_err();
        assert!(err.to_string().contains("block_interval"), "{err}");
    }

    #[test]
    fn entry_round_trips_and_another_algorithm_is_ignored() {
        let (entry, _) = first_boot(DEVNET);
        let bytes = entry.encode();
        assert_eq!(DigestCacheEntry::decode(&bytes), Ok(Some(entry.clone())));
        let mut v: Value = serde_json::from_slice(&bytes).unwrap();
        v["algorithm"] = "torus-genesis-file-v0".into();
        assert_eq!(DigestCacheEntry::decode(v.to_string().as_bytes()), Ok(None));
        let mut v: Value = serde_json::from_slice(&bytes).unwrap();
        v["version"] = 2.into();
        assert_eq!(DigestCacheEntry::decode(v.to_string().as_bytes()), Ok(None));
        assert!(DigestCacheEntry::decode(b"not json").is_err());
        let mut v: Value = serde_json::from_slice(&bytes).unwrap();
        v["sha256"] = "0x12".into();
        assert!(DigestCacheEntry::decode(v.to_string().as_bytes()).is_err());
    }
}
