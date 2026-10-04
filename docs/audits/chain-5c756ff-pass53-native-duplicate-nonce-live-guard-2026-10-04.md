# Pass 53 — Duplicate native nonce within a committed block

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass checks only whether the live committed-block path rejects repeated `(sender, nonce)` pairs within one block.

The app maintains a `seen_in_block` set and skips an action if either its nonce already exists in the overlay or insertion into that set fails. Thus a block containing the same signed action twice executes it at most once on the live path, even though the persisted nonce row is written after action selection. The parallel block-validator helper checks persisted nonces but does not maintain this in-block set; its callers and relationship to catch-up execution require separate review before inferring replica divergence.

Evidence: [live in-block set](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-consensus/src/app.rs#L2061), [duplicate branch](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-consensus/src/app.rs#L2089), [validator persisted-nonce check](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/validator.rs#L384).

No live-path replay defect found. The difference from the validator helper is recorded as a call-path question, not a finding. Static only.
