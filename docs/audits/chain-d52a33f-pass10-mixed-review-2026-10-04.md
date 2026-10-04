# Pass 10 — RPC, operations and independent recheck

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, starting at
`feb01b60f28e2111611c68947c7063eac9e06f41`. Production source remains
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`; the intervening commit contains
the earlier audit documents and supporting models. This round adds review
documents, not production fixes.

**Result: three additional source-supported candidates: two P2 RPC issues
and one P3 monitoring issue.** All four assigned reviews completed. Under the
[audit policy](README.md), candidates still need failing production-code
regressions. No Rust tests, builds, live-chain experiments or Prometheus rule
evaluations ran. Cargo/rustc were unavailable and were not installed.

## Completed reviews

| Reviewer | Scope | Report and outcome |
| --- | --- | --- |
| GPT-6.1-sol | Valid Ethereum RPC inputs, simulation and wire contracts | [RPC review](chain-d52a33f-pass10-sol-rpc-2026-10-04.md): F41 and F42 |
| GPT-6.1-sol | Ordinary builds, launch configuration and observability | [Operations review](chain-d52a33f-pass10-sol-operations-2026-10-04.md): local O02 mapped to F43; narrower qualifications retained |
| GPT-6-astra | Ordinary native-action failure and partial-batch contracts | [Action review](chain-d52a33f-pass10-astra-actions-2026-10-04.md): no additional defect established |
| GPT-6-astra | Independent falsification of F35–F40 and peer candidates | [Recheck](chain-d52a33f-pass10-astra-recheck-2026-10-04.md): bounded support retained, no additional count |

## Additional candidates

### F41 — P2: U256 Quantities retain a leading zero nibble

[`hex_u256`](../../crates/torus-rpc/src/types.rs#L21) trims zero bytes and then
hex-encodes full bytes. Consequently 1 becomes `0x01` and 256 becomes `0x0100`.
The balance and mined-transaction projections use this helper. These strings
violate [EIP-1474's minimal Quantity encoding](https://eips.ethereum.org/EIPS/eip-1474#quantity).
Zero is already correct; fixed-width Data fields have a different contract.
The numerical value is unchanged, but strict consumers can reject the result.
No actual external client failure was demonstrated.

Existing roundtrip tests parse with the project's permissive parser and do not
assert these exact strings. Repair only numeric Quantity encoding and regress
literal expected values through the public endpoint as well as the helper.
The [RPC report](chain-d52a33f-pass10-sol-rpc-2026-10-04.md) records affected
callers, counterexamples and independent expected values.

### F42 — P2: call and estimation discard supplied access lists

[`CallRequest`](../../crates/torus-rpc/src/types.rs#L220) has no `accessList`
field. The shared [environment builder](../../crates/torus-rpc/src/eth.rs#L255)
leaves the list empty for `eth_call` and every `eth_estimateGas` probe.
The official [method schemas](https://github.com/ethereum/execution-apis/blob/main/src/eth/execute.yaml)
accept `GenericTransaction`, whose
[schema](https://github.com/ethereum/execution-apis/blob/main/src/schemas/transaction.yaml)
includes that input. [EIP-2930](https://eips.ethereum.org/EIPS/eip-2930#specification)
defines its intrinsic gas and warming effects.

An ordinary zero-value call to an empty-code address with one listed address
and storage key loses 4,300 intrinsic gas from the requested simulation
contract. With-list and without-list objects currently produce the same
environment. This is a source inference, not an observed estimate. Arbitrary
contracts need not show the same gas direction because warming also matters.

This is separate from F17's mined-envelope execution typing: the simulation
request loses data before the executor receives it. Repairs still interact:
preserving the list must also select an appropriate simulation transaction type.
The [RPC report](chain-d52a33f-pass10-sol-rpc-2026-10-04.md) gives an ordinary
EOA estimate fixture, negative controls and the test gap.

### F43 — P3: timeout alert and dashboard query the wrong series

The [counter registration](../../crates/torus-telemetry/src/lib.rs#L1120)
produces `torus_consensus_timeout_total_total`, also recorded in the existing
[performance evidence](../perf/cap100-gap-attribution-2026-09-10.md#L23).
The shipped [alert](../../monitoring/alerts/consensus.yml#L69) and
[panel](../../monitoring/dashboards/consensus.json#L249) query the single-suffix
`torus_consensus_timeout_total` instead. Without a custom alias, these consumers
receive no matching series even when the real timeout counter rises. Other
alerts and manual queries can still work; no deployed incident is established.

This maps the operations report's O02 to F43. The emitted spelling itself was
already known; the finding concerns its consumers. Align naming and validate
real exporter output against a firing/nonfiring rule fixture and panel selector.
No current scrape or rule evaluation was performed.

## Counterchecks and retained limits

- The apparent full-image `sccache` failure was withdrawn. Configuration sets
  crate-suffixed compiler variables, but consumption by the pinned dependency
  build path was not established. Missing explicit installation alone does
  not prove that the build invokes the wrapper or fails.
- A proposed lower-block-gas simulation failure was not promoted. The ordinary
  genesis parent hardcodes 30M and later headers inherit it, so a lower genesis
  configuration alone does not establish the required lower applied header.
- Different default nonces in inner estimation probes did not establish a
  separate failure: simulation disables nonce checks, and CREATE uses the
  loaded account nonce in the inspected pinned execution path.
- Native-action review found explicit partial-batch semantics and useful
  rejection guards. Handler success differs from successful order-book
  outcome; queued business failure does not imply the item should run twice.
  Existing tests were read for their actual assertions. This does not close
  F04's storage-error issue or certify all atomicity paths.
- Independent review retained F35–F40 with their conditions. Permanent key
  loss requires absence of a recoverable old key; byte-budget overshoot needs
  the stated concurrent admission schedule; missing user-fill recovery needs
  a same-market gap beyond the capped window. Public block history is not an
  equivalent owner-attributed user-history fallback.
- The operations report retains qualified observations about telemetry bind
  reporting, separate Compose networks and stale printed endpoints. These
  are not additional numbered findings in this round. Static health is an
  explicit liveness contract, so missing readiness checks were not promoted.

## Verification and handoff

Both shipped Compose files passed read-only `docker compose ... config
--format json` expansion. This checks configuration structure and exposed
network/port mappings; it does not test service startup, DNS, scraping or image
builds. Repository links and source line bounds in these reports were checked,
along with documentation whitespace and the restriction to documentation
changes. The audit index links this round and the earlier consolidated passes.

F41, F42 and F43 were saved as discoveries in Torus, respectively:
`57e9bcad-349f-4d37-97ac-b8a00b588f9f`,
`bcc80c43-6ab7-4187-8e95-d8ee52cfd6e1`, and
`8f63def0-3de0-477d-bede-315792210275`.
No finding was resolved and no production regression was claimed to pass.
The interrupted pass-5 scope was not resumed. The next implementation work
should first reproduce the chosen candidate against production code, then
repair it and run the focused regression.
