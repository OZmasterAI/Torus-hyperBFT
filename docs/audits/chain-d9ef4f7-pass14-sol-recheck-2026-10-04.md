# Pass 14 — independent challenge of findings and coverage

Reviewed 2026-10-04 using GPT-6.1-sol against read-only source
`merge/item6-c3-pf1` at `d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd`.
Only this report was written in the separate documentation worktree,
`audit/chain-findings-2026-10-04`, starting at `f39a352`. Source references
pin the full reviewed revision rather than the documentation branch's code.

**Result: independently support F47 as a P2 source-supported candidate;
no additional finding proposed.** The [audit convention](README.md) still
requires a failing production-code regression. This is static evidence, not
an observed faucet outage, successful exploit or passing Rust test.

I read the finding catalogue and consolidated October reports through pass 13,
particularly F44, R2, the prior independent recheck, and the current pass-14
coordinator, history, economics and storage drafts. Their historical evidence
was checked against the current source where discussed below. No applicable
ancestor or repository `AGENTS.md` was found. Read-only Torus status identified
the newer documentation checkpoint and the already saved F47 discovery
`09551e7c-d42e-4f61-89f1-1c538470e86f`; no duplicate memory write was made.
Cargo, rustc and promtool are unavailable. All Rust tests were **read, not run**.
No source/Git mutation, installation, service, database experiment, live-chain
request or key operation occurred. Interrupted pass-5 certificate,
malformed-input and adversarial-network assignments were not resumed.

## F47 survives the ordinary sequential counterexample

This is a production caller path. The [HTTP dispatcher][dispatch] routes
`POST /faucet` to `handle_faucet`; [main][initial] constructs a shared state
with an empty nonce cache. The handler checks cooldown and balance, obtains
the initial nonce through [eth_getTransactionCount latest][helpers], then
[increments the cache][reserve] before requesting gas price. The
[gas-price error return][gaserror] neither clears nor restores that cache.
It precedes signing and sending. The [send-error reset][senderror] therefore
cannot handle this branch.

An explicit ordinary fixture uses chain ID 7778, a faucet funded with 100 TRS,
executed nonce 7, no queued/in-flight faucet transactions, and recipient
`0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` outside cooldown. The drip is
10 TRS and the recovered gas quote is 1 gwei; funding comfortably covers value
and 21,000 gas. Admission capacity and submit rate limits are available.

1. The first valid request gets the balance and nonce 7 successfully. It
   stores `Some(8)`. One transient transport failure during `eth_gasPrice`
   returns HTTP 500. The request never calls `eth_sendRawTransaction`; nonce 7
   remains unsubmitted, and no recipient cooldown is recorded.
2. After RPC recovery, retry sequentially with the same recipient. The balance
   still suffices. The cached branch yields nonce 8 and stores `Some(9)`;
   it does not query the unchanged account nonce again.
3. [The signer][signer] puts that supplied nonce into the EIP-1559 transaction.
   [Node submission][submit] returns a hash after mempool admission, without
   waiting for execution. [Validation][validate] accepts a nonce gap of one
   when the other checks pass. The faucet's [success branch][success] can
   consequently return HTTP 200 and record the recipient cooldown.
4. Account nonce remains 7. The payment with nonce 8 cannot execute normally
   until that missing predecessor is supplied. Admission and a returned hash
   do not establish recipient payment. A prompt further retry instead hits
   the handler's cooldown branch, whose default duration is 86,400 seconds.

This fixture uses valid signed transactions and an ordinary RPC failure; it
needs neither malformed user input nor concurrent requests. The exact bug is
an unsent reservation consumed locally. It affects delivery/retry availability,
not consensus safety, unauthorized transfers or proven loss of principal.

## Controls limit the impact and separate the earlier findings

Balance lookup failure, insufficient balance, initial nonce-query failure and
cooldown rejection return before consuming a nonce. A successful gas lookup
and submission use the reservation normally. A send rejection clears the
cache, although its concurrency/unknown-outcome safety is a separate question.
These controls explain why not every failed request creates this hole.

The accepted nonce gap is capped at 64. The existing
[future-nonce test][gaptest] explicitly rejects 65 and accepts 64 at account
nonce zero. Continued requests can therefore eventually encounter a rejection
and reset; pool capacity or rate limiting can also reject sooner. Restart
reinitializes the cache, and an external transaction at nonce 7 can fill the
hole. None guarantees timely payment for the already cooled-down recipient.
The report does not claim a permanent wedge or inevitable loss of every later
payment. Whether skipped transactions remain available is also affected by
the already recorded R2 ownership problem.

[F44](chain-d52a33f-pass11-mixed-review-2026-10-04.md) concerns the wallet
choosing the same latest nonce while a first transaction remains pooled.
Here the missing transaction was never sent at all. Changing the faucet's
initial query to `pending` alone leaves this sequential cached branch intact.
The pool's [pending-nonce walker][pending] stops at the first missing nonce;
it does not synthesize the absent transaction. [R2 selection][drain] can
drain a future-nonce transaction because it starts from the sender's smallest
pooled nonce without consulting account state. The [executor][execute]
skips invalid transactions when configured to do so. These explain a possible
additional consequence, but F47's client-created hole exists even with a pool
that safely retains future transactions. No extra number is assigned to R2.

Move gas-price lookup and other fallible pre-send preparation before nonce
reservation. That removes the demonstrated unsent gap without reclaiming a
reservation already handed to another request. For broader recovery, serialize
reservation/submission or explicitly track outstanding nonce ownership and
reconcile submitted and uncertain outcomes. Blind decrement is unsafe:
request A can reserve 7, B reserve 8, then A's failure decrement the shared
next nonce onto B's reservation. Blind reset can similarly discard knowledge
of other submissions. A transport error after sending also does not prove the
node rejected the transaction; that case needs distinct reconciliation.

The focused regression should drive the real async handler with controlled
RPC responses, fail gas price exactly once after successful nonce lookup,
assert zero first-request send calls and no cooldown, then decode the next
signed envelope and require nonce 7. Add a real admission/execution check for
recipient balance, plus successful-send, initial-query failure, preexisting
reservation, send rejection and concurrent-caller controls. The current
[signing test][sigtest] asserts only prefix `0x02` and length above 100;
[cooldown testing][cooltest] manipulates an isolated HashMap. Neither exercises
this production failure sequence or proves decoded nonce identity.

## Crosschecks support the companions' bounded conclusions

**History:** [Explorer upsert][upsert] selects `IGNORE` for skipped outcomes
and `REPLACE` for executed outcomes. The [skipped-body caller][skipped]
actually uses that path. Therefore a skipped-first transaction can later
replace its row after execution; a later skipped replay cannot demote an
executed row. This independently rejects permanent hash suppression, without
closing F26's separate partial-indexing issue. The
[user-history endpoint][usertrades] still caps results at 1,000 with no
block/trade cursor, retaining F40 even when packed-row encoding is correct.

**Storage:** [TorusApp Drop][appdrop] closes its execution sender and joins;
[FlushWorker Drop][flushdrop] closes its channel and joins; the
[history writer][writerdrop] likewise closes and joins. The
[queued-batch test][writertest] enqueues five batches, drops the writer and
asserts exact rows using the retained open database. This is useful healthy
drain coverage, not a complete process-signal/cold-reopen or power-loss proof.
The storage companion correctly leaves F19/F30/F31/F38 open. A fresh runtime
regression must release all DB owners, reopen and continue execution.

**Economics:** [Wallet delegation][walletstake] parses wei, and the
[native delegate caller][delegate] passes its U256 unchanged. The
[conversion comment][unitcomment] incorrectly groups Delegate with native
eight-decimal transfers. Correcting that prose does not imply a production
factor-of-10^10 transfer defect. The [close/flip test][filltest] supplies
independent +32.5 and −82.5 PnL constants and checks the short remainder;
its assertions are stronger than differential equality alone, but omit signed
application cash settlement. The [lockbox EVM controls][lockboxtest] assert
all-dust/nonround calls revert, balances remain unchanged and the queue stays
empty. These support the companion's boundary qualifications.

Finally, [application fee computation][fees] still precedes
[EVM-bundle seeding][seed] and [native distribution][distribution]. The
[fee-split test][feetest] supplies synthetic fee revenue rather than debiting
an executed sender. It cannot close F02's combined-ledger accounting seam.
No companion correction changes the earlier finding count or establishes a
new C3/PF1 execution defect. The next evidence should be production regressions
at these caller seams, with runtime execution and persistence limits stated.

[dispatch]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L307
[initial]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L622
[helpers]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L152
[reserve]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L446
[gaserror]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L470
[senderror]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L516
[signer]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L238
[submit]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/eth.rs#L702
[validate]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/validate.rs#L102
[success]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L496
[gaptest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L3182
[pending]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/evm_pool.rs#L227
[drain]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/evm_pool.rs#L280
[execute]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-evm/src/executor.rs#L298
[sigtest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L718
[cooltest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/faucet/src/main.rs#L736
[upsert]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/db.rs#L358
[skipped]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L188
[usertrades]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/torus.rs#L1913
[appdrop]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L4644
[flushdrop]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/exec_pipeline.rs#L267
[writerdrop]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/bg_writer.rs#L304
[writertest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/bg_writer.rs#L587
[walletstake]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/wallet/src/commands/staking.rs#L10
[delegate]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L7822
[unitcomment]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/lockbox.rs#L270
[filltest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/tests/position_cache_tests.rs#L162
[lockboxtest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-evm/tests/evm_tests.rs#L1352
[fees]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L1825
[seed]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2142
[distribution]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2306
[feetest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-integration-tests/tests/fee_flow.rs#L41
