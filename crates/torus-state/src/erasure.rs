//! Erasure-coded body dissemination — recovery-path primitive (Sprint 5 T3.1,
//! PHASE 1 / Option A of `docs/plans/sprint5-erasure-coding.md`).
//!
//! A node missing a native-action body today PULLs the WHOLE body by hash from
//! ONE peer (`/torus/native-da/{1.0,2.0}`), making the proposer a per-block
//! hotspot (s338: 155 pull timeouts + 238 substream exhaustions to one peer).
//! This module supplies the primitive that lets a lagging node instead gather
//! `k` small shards from `k` DIFFERENT peers and reconstruct — killing the
//! single-source hotspot. It is the shared codec both Phase A (recovery) and a
//! future Phase B (ingress dispersal) build on.
//!
//! # Integrity — verify-then-reconstruct (NOT reconstruct-then-verify)
//! The proposer builds a Merkle tree over the `n` shards and commits the root
//! (`erasure_root`). Each shard ships with its [`ShardProof`]; a fetching node
//! calls [`verify_shard`] BEFORE feeding it to [`reconstruct`], so a Byzantine
//! peer cannot poison reconstruction (reconstruct-then-verify only detects the
//! poison post-hoc ⇒ combinatorial retry — rejected by the design doc).
//! A **body-hash backstop** always applies: the caller re-hashes the
//! reconstructed body against the proposal's referenced hash regardless of the
//! per-shard proofs — the ultimate integrity check.
//!
//! # Never-wedge
//! [`reconstruct`] returns [`ErasureError::InsufficientShards`] when fewer than
//! `k` shards are present. The caller MUST treat that as a signal to fall back
//! to the existing whole-body pull — erasure is additive and never a new way to
//! wedge (`docs/plans/native-da-hash-only-push.md` guardrail).
//!
//! # Determinism
//! `erasure_root` and the shard layout are pure functions of the body bytes and
//! the RS params `(k, n)`, so every honest node computes byte-identical shards
//! and root — safe on any hash/consensus-adjacent path.
//!
//! # STATUS — UNCOMPILED SCAFFOLD
//! There is no local Rust toolchain in this workspace, so this module has NOT
//! been compiled. `encode` / `reconstruct` / `verify_shard` are filled in
//! against the `reed-solomon-erasure` v6 API and the tests express the intended
//! contract (RED-first: they cannot even build against `HEAD`, which has no
//! erasure module). TODO markers flag the spots that must be confirmed once a
//! toolchain is available. The consensus-format `erasure_root` header field is
//! DEFERRED (needs a genesis relaunch — see the design doc, Q2).

use alloy_primitives::{keccak256, B256};

/// Reed-Solomon parameters: any `k` of `n` shards reconstruct the body.
///
/// Availability wants `k = f+1` and `n = |validators|` (Polkadot-style
/// validator-custody), giving `n-f >= 2f+1` honest shards always present ⇒
/// always reconstructable. `(k, n)` are an epoch-stable function of the
/// validator-set size so every node derives the identical layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErasureParams {
    /// Data shards — the reconstruction threshold (`k`).
    pub k: usize,
    /// Total shards, data + parity (`n`). Parity count is `n - k`.
    pub n: usize,
}

impl ErasureParams {
    /// `k` data shards, `n` total. Validates lazily via [`validate`](Self::validate).
    pub fn new(k: usize, n: usize) -> Self {
        Self { k, n }
    }

    /// Reject degenerate params before touching the RS backend. `galois_8`
    /// bounds `n <= 255`; we also require `1 <= k <= n` and at least one parity
    /// shard is optional (k == n is a valid no-parity split, though it offers no
    /// redundancy).
    pub fn validate(&self) -> Result<(), ErasureError> {
        if self.k == 0 || self.n == 0 || self.k > self.n || self.n > 255 {
            return Err(ErasureError::BadParams { k: self.k, n: self.n });
        }
        Ok(())
    }
}

/// Errors on the erasure path. Every variant on the fetch/reconstruct side maps
/// to the SAME caller action: fall back to whole-body pull (never wedge).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErasureError {
    /// `(k, n)` outside the supported range.
    BadParams { k: usize, n: usize },
    /// Fewer than `k` shards present ⇒ caller falls back to whole-body pull.
    InsufficientShards { have: usize, need: usize },
    /// A shard's Merkle proof did not verify against the committed root.
    ProofMismatch { index: usize },
    /// The RS backend rejected encode/decode (wrong shard count/lengths, etc.).
    Backend(String),
}

impl std::fmt::Display for ErasureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErasureError::BadParams { k, n } => write!(f, "bad erasure params k={k} n={n}"),
            ErasureError::InsufficientShards { have, need } => {
                write!(f, "insufficient shards: have {have}, need {need}")
            }
            ErasureError::ProofMismatch { index } => write!(f, "shard {index} proof mismatch"),
            ErasureError::Backend(e) => write!(f, "erasure backend: {e}"),
        }
    }
}

impl std::error::Error for ErasureError {}

/// A per-shard Merkle inclusion proof against [`EncodedBody::erasure_root`].
/// Sibling hashes bottom-up; the shard's own index fixes left/right ordering at
/// each level, so the proof needs no direction bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardProof {
    /// Sibling hashes from the leaf level upward to (but excluding) the root.
    pub siblings: Vec<B256>,
}

/// Output of [`encode`]: the `n` shards, the committed root, and the metadata a
/// reconstructing node needs (params + original body length for de-padding).
#[derive(Debug, Clone)]
pub struct EncodedBody {
    /// The `n` equal-length shards (indices `0..k` are systematic data shards).
    pub shards: Vec<Vec<u8>>,
    /// Merkle root over the `n` shards — bound to the body hash by the caller.
    pub erasure_root: B256,
    /// Per-shard byte length (all shards are padded to this).
    pub shard_len: usize,
    /// Original (pre-pad) body length — reconstruction truncates back to this.
    pub body_len: usize,
    /// The RS params used.
    pub params: ErasureParams,
}

impl EncodedBody {
    /// Merkle proof for shard `index` against [`erasure_root`](Self::erasure_root).
    pub fn proof(&self, index: usize) -> ShardProof {
        merkle_proof(&self.shards, index)
    }
}

/// Domain-separated leaf hash for shard `index`. Index is folded in so a peer
/// cannot present shard `j`'s bytes as shard `i` (which would corrupt the
/// systematic layout undetectably).
fn leaf_hash(index: usize, shard: &[u8]) -> B256 {
    let mut buf = Vec::with_capacity(1 + 4 + shard.len());
    buf.push(0x00); // leaf domain tag
    buf.extend_from_slice(&(index as u32).to_be_bytes());
    buf.extend_from_slice(shard);
    keccak256(&buf)
}

/// Domain-separated internal-node hash.
fn node_hash(left: B256, right: B256) -> B256 {
    let mut buf = [0u8; 1 + 32 + 32];
    buf[0] = 0x01; // internal domain tag
    buf[1..33].copy_from_slice(left.as_slice());
    buf[33..65].copy_from_slice(right.as_slice());
    keccak256(buf)
}

/// Build the Merkle levels bottom-up. Odd-width levels duplicate the last node
/// (the classic convention); [`merkle_proof`] mirrors it so verification is
/// convention-agnostic (the duplicated node appears as its own sibling).
fn merkle_levels(shards: &[Vec<u8>]) -> Vec<Vec<B256>> {
    let mut level: Vec<B256> = shards
        .iter()
        .enumerate()
        .map(|(i, s)| leaf_hash(i, s))
        .collect();
    let mut levels = vec![level.clone()];
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i < level.len() {
            let left = level[i];
            let right = if i + 1 < level.len() { level[i + 1] } else { level[i] };
            next.push(node_hash(left, right));
            i += 2;
        }
        levels.push(next.clone());
        level = next;
    }
    levels
}

/// Merkle root over the `n` shards. Empty input yields the zero hash (an
/// impossible root — encode always has `n >= 1` shards).
fn merkle_root(shards: &[Vec<u8>]) -> B256 {
    match merkle_levels(shards).pop() {
        Some(top) if !top.is_empty() => top[0],
        _ => B256::ZERO,
    }
}

/// Sibling path for `index` from the leaf level up to (excluding) the root.
fn merkle_proof(shards: &[Vec<u8>], index: usize) -> ShardProof {
    let levels = merkle_levels(shards);
    let mut siblings = Vec::with_capacity(levels.len().saturating_sub(1));
    let mut idx = index;
    for level in levels.iter().take(levels.len().saturating_sub(1)) {
        let sib = if idx % 2 == 0 {
            // right sibling, or self when this is a lone last node
            if idx + 1 < level.len() { level[idx + 1] } else { level[idx] }
        } else {
            level[idx - 1]
        };
        siblings.push(sib);
        idx /= 2;
    }
    ShardProof { siblings }
}

/// Verify shard `index`/`shard` against `erasure_root` using `proof`.
/// Call this BEFORE handing the shard to [`reconstruct`] (verify-then-reconstruct).
pub fn verify_shard(
    erasure_root: B256,
    index: usize,
    shard: &[u8],
    proof: &ShardProof,
) -> bool {
    let mut hash = leaf_hash(index, shard);
    let mut idx = index;
    for sib in &proof.siblings {
        hash = if idx % 2 == 0 {
            node_hash(hash, *sib)
        } else {
            node_hash(*sib, hash)
        };
        idx /= 2;
    }
    hash == erasure_root
}

/// Encode a body into `n` shards (first `k` systematic) and commit the erasure
/// root. Deterministic in `(body, k, n)`.
///
/// TODO(toolchain): confirm the `reed-solomon-erasure` v6 `encode(&mut [Vec<u8>])`
/// signature and that systematic shards are shards `0..k` (they are for this
/// crate's `galois_8` backend). If a future bump changes the shard container,
/// adjust here only — callers use [`EncodedBody`].
pub fn encode(body: &[u8], params: ErasureParams) -> Result<EncodedBody, ErasureError> {
    params.validate()?;
    let k = params.k;
    let n = params.n;

    // Pad the body up to a multiple of k; shard_len is the per-shard byte count.
    // max(1) so a zero-length body still yields well-formed 1-byte shards.
    let shard_len = body.len().div_ceil(k).max(1);

    let mut shards: Vec<Vec<u8>> = Vec::with_capacity(n);
    for i in 0..k {
        let start = i * shard_len;
        let mut shard = vec![0u8; shard_len];
        if start < body.len() {
            let end = (start + shard_len).min(body.len());
            shard[..end - start].copy_from_slice(&body[start..end]);
        }
        shards.push(shard);
    }
    // Parity shards start zeroed; encode() fills them.
    for _ in k..n {
        shards.push(vec![0u8; shard_len]);
    }

    let rs = reed_solomon_erasure::galois_8::ReedSolomon::new(k, n - k)
        .map_err(|e| ErasureError::Backend(e.to_string()))?;
    rs.encode(&mut shards)
        .map_err(|e| ErasureError::Backend(e.to_string()))?;

    let erasure_root = merkle_root(&shards);
    Ok(EncodedBody { shards, erasure_root, shard_len, body_len: body.len(), params })
}

/// Reconstruct the body from a sparse shard set (`None` = missing/unfetched).
/// Returns [`ErasureError::InsufficientShards`] when fewer than `k` are present
/// — the caller's signal to fall back to whole-body pull.
///
/// SECURITY: pass only shards that already passed [`verify_shard`]. This function
/// trusts its inputs; the body-hash backstop at the call site is the final gate.
///
/// TODO(toolchain): confirm v6 `reconstruct(&mut [Option<Vec<u8>>])` — `Option<Vec<u8>>`
/// implements `ReconstructShard`, so a missing shard is `None` and present ones
/// carry their bytes.
pub fn reconstruct(
    mut shards: Vec<Option<Vec<u8>>>,
    params: ErasureParams,
    body_len: usize,
) -> Result<Vec<u8>, ErasureError> {
    params.validate()?;
    let present = shards.iter().filter(|s| s.is_some()).count();
    if present < params.k {
        return Err(ErasureError::InsufficientShards { have: present, need: params.k });
    }
    // Normalize length: RS backends want exactly n slots.
    if shards.len() != params.n {
        return Err(ErasureError::Backend(format!(
            "expected {} shard slots, got {}",
            params.n,
            shards.len()
        )));
    }

    let rs = reed_solomon_erasure::galois_8::ReedSolomon::new(params.k, params.n - params.k)
        .map_err(|e| ErasureError::Backend(e.to_string()))?;
    rs.reconstruct(&mut shards)
        .map_err(|e| ErasureError::Backend(e.to_string()))?;

    // Concatenate the k systematic data shards, then strip the padding.
    let mut body = Vec::with_capacity(params.k.saturating_mul(shards.first().and_then(|s| s.as_ref()).map_or(0, |s| s.len())));
    for slot in shards.iter().take(params.k) {
        let shard = slot
            .as_ref()
            .ok_or_else(|| ErasureError::Backend("data shard still missing post-reconstruct".into()))?;
        body.extend_from_slice(shard);
    }
    body.truncate(body_len);
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + 3) as u8).collect()
    }

    /// RED (no erasure module on HEAD): encode→reconstruct round-trips from the
    /// FULL shard set. GREEN once the module + reed-solomon dep land.
    #[test]
    fn roundtrip_full_set() {
        let b = body(1000);
        let params = ErasureParams::new(2, 3);
        let enc = encode(&b, params).expect("encode");
        assert_eq!(enc.shards.len(), 3);
        let all: Vec<Option<Vec<u8>>> = enc.shards.iter().cloned().map(Some).collect();
        let out = reconstruct(all, params, enc.body_len).expect("reconstruct");
        assert_eq!(out, b, "full-set reconstruction must equal the original body");
    }

    /// Any k-of-n subset reconstructs: drop shard 0 (a DATA shard) so parity is
    /// actually exercised, keep exactly k=2 present.
    #[test]
    fn reconstruct_from_k_subset_missing_data_shard() {
        let b = body(777);
        let params = ErasureParams::new(2, 3);
        let enc = encode(&b, params).expect("encode");
        // Present: shard 1 (data) + shard 2 (parity); shard 0 missing.
        let sparse: Vec<Option<Vec<u8>>> =
            vec![None, Some(enc.shards[1].clone()), Some(enc.shards[2].clone())];
        let out = reconstruct(sparse, params, enc.body_len).expect("reconstruct k-subset");
        assert_eq!(out, b, "any k shards must rebuild the body");
    }

    /// verify-then-reconstruct: a valid shard verifies; a corrupted shard is
    /// REJECTED by its proof before it can poison reconstruction.
    #[test]
    fn proof_accepts_valid_rejects_corrupt() {
        let b = body(512);
        let params = ErasureParams::new(2, 4);
        let enc = encode(&b, params).expect("encode");
        for i in 0..params.n {
            let proof = enc.proof(i);
            assert!(
                verify_shard(enc.erasure_root, i, &enc.shards[i], &proof),
                "honest shard {i} must verify"
            );
            // Flip a byte: the proof must now FAIL (Byzantine-shard rejection).
            let mut bad = enc.shards[i].clone();
            bad[0] ^= 0xFF;
            assert!(
                !verify_shard(enc.erasure_root, i, &bad, &proof),
                "corrupted shard {i} must be rejected by its proof"
            );
        }
    }

    /// A shard presented under the WRONG index must fail (index is folded into
    /// the leaf hash) — stops a peer swapping systematic shards.
    #[test]
    fn proof_rejects_wrong_index() {
        let b = body(300);
        let params = ErasureParams::new(2, 4);
        let enc = encode(&b, params).expect("encode");
        let proof0 = enc.proof(0);
        assert!(
            !verify_shard(enc.erasure_root, 1, &enc.shards[0], &proof0),
            "shard 0's bytes must not verify as shard 1"
        );
    }

    /// < k shards ⇒ clean InsufficientShards signal (caller falls back to
    /// whole-body pull; NEVER wedges).
    #[test]
    fn under_k_falls_back() {
        let b = body(400);
        let params = ErasureParams::new(3, 5);
        let enc = encode(&b, params).expect("encode");
        // Only 2 present, need 3.
        let sparse: Vec<Option<Vec<u8>>> = vec![
            Some(enc.shards[0].clone()),
            None,
            Some(enc.shards[2].clone()),
            None,
            None,
        ];
        let err = reconstruct(sparse, params, enc.body_len).unwrap_err();
        assert_eq!(err, ErasureError::InsufficientShards { have: 2, need: 3 });
    }

    /// Determinism: identical (body, params) ⇒ identical shards + root on every
    /// node (state-root/consensus safety).
    #[test]
    fn encode_is_deterministic() {
        let b = body(650);
        let params = ErasureParams::new(3, 6);
        let a = encode(&b, params).expect("encode a");
        let c = encode(&b, params).expect("encode c");
        assert_eq!(a.erasure_root, c.erasure_root);
        assert_eq!(a.shards, c.shards);
    }

    #[test]
    fn bad_params_rejected() {
        assert!(ErasureParams::new(0, 3).validate().is_err());
        assert!(ErasureParams::new(4, 3).validate().is_err()); // k > n
        assert!(ErasureParams::new(2, 256).validate().is_err()); // n > 255
        assert!(ErasureParams::new(2, 3).validate().is_ok());
    }
}
