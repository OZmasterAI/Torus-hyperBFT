# Pass 51 — Oracle quorum after outlier rejection

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass covers only the quorum population used for a fresh aggregate.

Submissions are paired with known validator stake, outlier rejection runs first, and both the minimum reporter count and strict greater-than-two-thirds stake quorum are then checked over the filtered set. Total stake comes from the supplied validator stake list. If the filtered set is insufficient, the method returns a still-valid prior aggregate rather than writing a new aggregate.

Evidence: [stake pairing and filtering](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/oracle.rs#L297), [filtered-set count/quorum gate](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/oracle.rs#L320), [strict quorum arithmetic](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/oracle.rs#L545).

No mismatch between the quorum check and the price-setting set found. The security tradeoff of unweighted outlier filtering is separately documented in prior oracle reviews. Static only.
