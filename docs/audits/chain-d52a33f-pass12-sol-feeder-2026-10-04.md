# Pass 12 — ordinary price-feeder lifecycle

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, starting HEAD
`11756e8bdd8db4fe95a18a42a25d37e6aca536b8`. Production source remains
`d52a33f`; the scoped production diff from that revision to HEAD was empty.

**Result: no additional finding promoted.** Partial-submission health and the
batching/cap interaction below are operational qualifications and regression
suggestions, not additional finding counts or a demonstrated ordinary-market
starvation defect.

This is static inspection under the [audit convention](README.md#L3), not a
runtime reproduction or a verified fix. Neither `cargo` nor `rustc` was
available. No Rust tests, services, exchange requests, live-chain calls, key
operations, source fixes, Git mutations, or Torus writes were performed. Only
this report was written. No applicable `AGENTS.md` was found in the repository
or its containing directories. The incomplete pass-5 certificate, malformed
input and adversarial work was not resumed.

The September reports and chain passes 1–11 were checked for overlap, including
the [pass-8 feeder integration qualification](chain-d52a33f-pass8-sol-integration-2026-10-04.md#L174),
[oracle lifecycle review](chain-d52a33f-pass8-astra-lifecycle-2026-10-04.md#L76),
and [mark-consumer review](chain-d52a33f-pass8-sol-marks-2026-10-04.md#L28).
F01–F44, oracle signer policy, epoch scheduling, genesis fields, persistence and
previous mark/margin findings retain their provenance. The partial-submit
qualification does not extend F32's wallet schema mismatch to the feeder, nor
F43's timeout-series mismatch.

## Operational qualification — partial rejection does not degrade health

**Preconditions:** an Active validator with the correctly registered signer;
more than 256 configured, listed, matching markets with usable source prices;
healthy venue fetches; the node admits one chunk and explicitly rejects another
with an ordinary retryable error. The failure must recur for the persistent
example. This needs no unauthorized signer, bad price, exchange compromise,
malformed response, chain failure, or tool-generated live transaction.

**Source trace.** The public per-market contract describes `omitted = None` as
“submitted” in [MarketStatus](../../tools/price-feeder/src/feeder.rs#L24).
[aggregate_all](../../tools/price-feeder/src/feeder.rs#L199) installs that state
for every successful aggregate before the submission phase. The
[builder](../../tools/price-feeder/src/submit.rs#L12) creates ordered chunks of
up to 256 markets, and [run_cycle](../../tools/price-feeder/src/feeder.rs#L307)
submits them sequentially. A successful response updates the single global
`last_submit_ok_ms`; an error increments a result counter and appends the
message to `CycleReport`, but does not mark the affected chunk's markets as
omitted or record any per-market rejection.

The node's [single-submit semaphore gate](../../crates/torus-rpc/src/torus.rs#L1330)
can explicitly return `server overloaded, try again` **before** verification
and pooling. This gives a concrete non-admission result, unlike a client
timeout after admission with a lost response. The feeder
[extracts the RPC error message](../../tools/price-feeder/src/node.rs#L114)
and [classifies overloaded](../../tools/price-feeder/src/submit.rs#L65) as
Retryable. Oracle priority bypasses the backlog screen at the next stage; it
does not bypass this semaphore gate.

**Source-derived example:** take 257 successful market aggregates. Chunk A carries
markets 1–256 and receives a normal hash response. During the next RPC call a
transient burst saturates the submit permits; chunk B, market 257, receives
the explicit overload rejection. The resulting cycle is `{ok: 1, failed: 1}`
and its global retryable counter increases, but market 257 still has a price,
sources, weight and `omitted = None`. With all venue states successful and no
authorization error, [health::level](../../tools/price-feeder/src/health.rs#L32)
returns `Ok` because any recent success satisfies its global freshness check.
[health_json](../../tools/price-feeder/src/health.rs#L44) therefore responds
HTTP 200 with `status: "ok"` and no per-market omission for the rejected price.
Repeating this outcome each cycle keeps that status even while market 257 has
no newly admitted feeder submission.

**Impact and boundary:** health and per-market omission output cannot identify
partial admission failure. Logs, `CycleReport.errors`, the cycle failure count
and `feeder_submit_total{result="retryable"}` still expose it; this is not a
claim that all monitoring signals hide the error. No current
257-market deployment or recurring overload schedule was observed. Other
validators may still satisfy oracle quorum for that market.

The [README health contract](../../tools/price-feeder/README.md#L93) uses any
recent accepted submission plus market omission/venue failure state; it does
not explicitly require admission success for every market. The implementation
therefore satisfies its narrow documented level rule. The `None = submitted`
comment does not clearly promise acceptance for that market either. This is
retained as an unnumbered operational limitation and proposed monitoring
enhancement, with no P3 promotion. Health also measures **admission**, not block
execution or quorum. A recent successful whole-cycle submission followed by one
failed cycle is allowed its documented three-interval grace period and is not
separately reported as a defect.

**Regression/enhancement priority:** low operational priority. First pin the
current global-success rule with a real feeder cycle of 257 valid matching
markets, fixture quotes and deterministic node responses
`Ok(hash)` then `Err("server overloaded, try again")`. If the health policy is
expanded to cover partial failure, assert the second chunk's exact market ID
receives a submit-failure state and health is at least degraded, while
successfully admitted markets stay successful. Repeat across
cycles and include all-success and all-failure controls. A real-RPC version
should hold submit permits only for the second request and assert it never
enters the pool. Track chunk admission per market, or add a partial-cycle
failure state to health and make the `omitted` contract explicit; finality
polling is unnecessary for that monitoring enhancement.

The existing [busy retry test](../../tools/price-feeder/src/feeder.rs#L506)
uses one chunk and asserts retry classification, a higher subsequent nonce,
and result counters. [Health tests](../../tools/price-feeder/src/health.rs#L197)
construct status directly and test global recency, source/market omissions
and authorization errors. Neither composes partial chunk rejection with
per-market health. These tests were read, not run.

## Conditional scaling qualification — chunking versus the four-pending cap

The builder has no total-market limit; [config validation](../../tools/price-feeder/src/config.rs#L296)
requires a nonempty set, unique market IDs and enough enabled symbols rather
than a maximum count. [Market pagination](../../tools/price-feeder/src/node.rs#L79)
continues beyond the node's [500-row page cap](../../crates/torus-rpc/src/torus.rs#L1065).
[Requests](../../tools/price-feeder/src/fetch.rs#L211) deduplicate symbols, and
six venues use bulk unfiltered URLs in [url_for](../../tools/price-feeder/src/exchange.rs#L40).
The governance [listing writer](../../crates/torus-economics/src/governance.rs#L1088)
has no market-count ceiling or duplicate-base check. Thus the source itself
does not impose a four-chunk ceiling on a valid feeder configuration.

Nevertheless, the node allows only [four pending oracle submissions per
validator](../../crates/torus-mempool/src/rate_limit.rs#L180), shared across
the validator and its signer. [Admission](../../crates/torus-mempool/src/lib.rs#L766)
deliberately replaces the oldest pooled nonce with a newer one at that cap;
[the removal rule](../../crates/torus-mempool/src/native_pool.rs#L624) does not
compare the chunks' market coverage. For 1,025 usable markets, the builder
produces 256/256/256/256/1. If all five requests arrive at one pool before its
proposal selection, the fifth successful admission removes the first chunk
from that pool, leaving only markets 257–1,025 there. Every RPC can return a
success even though the first chunk is no longer pooled.

This finite local-pool sequence follows from source, but is **not promoted as
a P1/P2 ordinary-market finding** here. The recorded fixtures contain only
BTC/ETH/POL; they do not establish a supported live configuration of 1,025
distinct quoted assets. A synthetic configuration can reuse symbols across
otherwise valid distinct market IDs, but that alone does not establish a
normal deployment. Kraken's pair-list validation/body and response constraints
would also need checking for a real distinct-symbol set. No exchange coverage
or remote URL acceptance was tested.

Nor does eviction from this pool establish disappearance everywhere: an
earlier chunk may already be forwarded, gossiped or selected by another node.
The actual [proposer selection](../../crates/torus-consensus/src/app.rs#L5720)
admits oracle actions in ordinary and deepest-backlog tiers. Repeated loss of
the same prefix would additionally require each relevant producer pool to
receive the full batch before selection on each cycle. Neither that timing
nor indefinite starvation was observed. Keep the intended newest-submission
replacement safeguard; a regression should test four and five **disjoint**
chunks through one real node before considering coverage-aware batching,
bounded supported market counts, or round-robin chunk rotation.

The [600-market builder test](../../tools/price-feeder/src/submit.rs#L114)
asserts three chunks, and the [submission property test](../../tools/price-feeder/src/submit.rs#L125)
checks structural price/listing/uniqueness constraints with fewer than 700
generated entries. The [node cap test](../../crates/torus-mempool/src/lib.rs#L3588)
asserts intentional oldest replacement. None asserts end-to-end coverage of
more than four disjoint chunks. No replacement safeguard is called a new bug
solely because its isolated test passes.

## Safeguards, rejected leads and test boundaries

| Reviewed path | Source-supported conclusion and limit |
| --- | --- |
| Config and arithmetic | [Validation](../../tools/price-feeder/src/config.rs#L260) bounds weights, enforces timing ordering, unique IDs and source counts. [Integer aggregation](../../tools/price-feeder/src/price.rs#L92) uses configured enabled weights, both source/weight thresholds, checked USDT conversion and a lower weighted median. A runtime market sample supplies at most one sample per configured venue, so calling the public helper with duplicate venue samples is not an ordinary feeder path. |
| Parser compatibility | [Fixture assertions](../../tools/price-feeder/src/exchange.rs#L240) check the seven parsers' exact recorded bid/ask values. [Timestamp parsing](../../tools/price-feeder/src/exchange.rs#L356) retains OKX row and Bybit/KuCoin response times. This establishes the asserted recorded shapes, not present exchange compatibility or live timestamp accuracy. No changed external API was inferred without a call. |
| Freshness | [market_samples](../../tools/price-feeder/src/feeder.rs#L108) uses the venue timestamp capped at fetch time when present; [aggregation](../../tools/price-feeder/src/price.rs#L103) drops samples past the configured age. [Stale-cache cycle assertions](../../tools/price-feeder/src/feeder.rs#L490) check accepted cached quotes at 3,000 ms and omission at 5,001 ms. The [venue timestamp test](../../tools/price-feeder/src/feeder.rs#L575) asserts exclusion of a six-second-old OKX quote. These are real assertion bodies, not execution evidence. |
| Backoff and symbols | [Fetcher](../../tools/price-feeder/src/fetch.rs#L108) retains good quotes on failure and applies bounded exponential backoff. [Per-pair fallback](../../tools/price-feeder/src/fetch.rs#L164) salvages Kraken pairs after an unknown-pair response. [Canonical pair checking](../../tools/price-feeder/src/exchange.rs#L65) is intentionally fatal at startup. A startup Kraken outage can stop startup even if other venues would suffice; that is a documented startup gate rather than a new claim that runtime redundancy is absent. |
| Conversion dependency | `kraken_usdt` with disabled Kraken is accepted; [request construction](../../tools/price-feeder/src/fetch.rs#L221) does not fetch the rate, and [aggregation](../../tools/price-feeder/src/price.rs#L108) safely drops USDT sources. Add an operator-facing config warning or rejection for an impossible configured source quorum. This is fail-closed configuration feedback, not a claim that unconverted USDT is silently submitted. USD/USDC sources can still suffice. |
| Node wire and readiness | [Parsers/paging](../../tools/price-feeder/src/node.rs#L47) match current camelCase/hex wire values. [startup_check](../../tools/price-feeder/src/node.rs#L174) requires this signer's exact registration and Active readiness; [run_cycle](../../tools/price-feeder/src/feeder.rs#L225) retries check errors next cycle and delays inactive rechecks by 60 seconds. [Listing refresh](../../tools/price-feeder/src/feeder.rs#L255) runs every cycle and checks base/USD consistency. These remain the pass-8 safeguards, not new findings. |
| Key creation | [keygen](../../tools/price-feeder/src/keyfile.rs#L54) preserves its refusal to replace an existing signer keystore. Key files have [permission validation](../../tools/price-feeder/src/keyfile.rs#L33). This does not change F36's separate wallet/node writers. No key was loaded or created during review. |
| Admission versus execution | [Ingress](../../crates/torus-rpc/src/torus.rs#L292) checks price/listing/count constraints. [Execution](../../crates/torus-bridge/src/native_executor.rs#L7837) separately resolves Active reporter authority, limits sample/block skew and keeps newest samples. [Aggregation](../../crates/torus-core/src/oracle.rs#L279) additionally needs enough reporters and more than two-thirds Active stake after filtering. A feeder hash response proves none of these later quorum outcomes. The README's “never ... reject” wording needs this ordinary timing/listing/status qualification; it is not counted again as a failure of admission-only health. |

The [real-RPC feeder test](../../tools/price-feeder/tests/rpc_e2e.rs#L101)
asserts one real mempool entry, recovered signer and expected BTC/ETH prices,
including an admission-backlog configuration. It then drains that entry; it
does not execute a block or form an oracle quorum. Its
[fixture_at helper](../../tools/price-feeder/src/testing.rs#L28) rewrites venue
timestamps to the test clock, so the test cannot establish freshness of the
original recording or current live exchange responses. The separate
block/oracle and mark tests retain the bounded evidence described in pass 8;
their assertions were not rerun or generalized to process restart, validator
transition or complete feeder-to-quorum operation.

Follow-up should first specify partial-rejection health policy and its assertions,
then compose three real feeder submissions from sufficient Active stake with
block execution and an independently expected mark. Keep 5,000/5,001 ms
source-age and sample/block-skew controls, 10/11-second reporter-window controls
and 60/61-second aggregate-age controls distinct. These are proposed regression
expectations; no test implementation, pass result, closure or runtime defect
confirmation is claimed.
