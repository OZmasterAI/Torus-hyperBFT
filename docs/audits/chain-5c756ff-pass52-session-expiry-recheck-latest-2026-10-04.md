# Pass 52 — Session expiry units on latest branch

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass rechecks one known issue against the newly fetched source head `5c756ffb8f9a95675e62064269abdc31f10eb703`: on-chain session expiry units.

Session creation stores expiry in milliseconds after converting the block's seconds timestamp. The committed-block app batch verifier receives the block timestamp directly, and its authorization predicate compares that value directly with `session.expiry`. Because the latter is milliseconds, an expired session can remain accepted well beyond its intended lifetime. This is the existing F03 finding, not a new issue; this pass confirms it remains on this revision.

Evidence: [session creation milliseconds conversion](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L8023), [batch expiry comparison](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-types/src/eip712.rs#L1228), [app verification call](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-consensus/src/app.rs#L1998).

Static recheck only. No duplicate issue opened and no runtime test run (`cargo` unavailable).
