# Pass 74 — Treasury spend recipient balance arithmetic

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: overflow behavior in governance treasury transfers.

**Hypothesis: recipient balance addition is unchecked in the governance treasury-spend branch.** The branch bounds the spend against the treasury balance, debits the treasury, reads the recipient account, then performs `recipient_acct.balance += amount` before writing. It does not use checked addition there. A recipient balance close enough to `U256::MAX` could overflow or panic depending on build arithmetic settings; practical state reachability and consensus handling were not tested. This is a separate treasury-transfer arithmetic check, not a claim of demonstrated exploitability.

Evidence: [governance.rs L1061](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/governance.rs#L1061), [governance.rs L1077](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/governance.rs#L1077), [governance.rs L1083](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/governance.rs#L1083).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
