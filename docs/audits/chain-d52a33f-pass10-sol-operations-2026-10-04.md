# Pass 10 — ordinary build, launch and observability contracts

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, HEAD
`feb01b60f28e2111611c68947c7063eac9e06f41`. The HEAD commit adds audit documents
only; the reviewed production source remains
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`.

**Result: one P3 monitoring-consumer candidate, with narrower operational
qualifications below; no additional P1/P2 finding established.** This is a static candidate
under the [audit policy](README.md), not reproduced build/runtime failures or
verified fixes. The coordinating review owns final numbering and Torus records.
No chain safety, state-loss, exploited node or deployed monitoring failure is
claimed. The known emitted timeout metric spelling is explicitly acknowledged.

Scope was shipped ordinary Docker build/Compose launch paths, current node CLI
and TOML capabilities, telemetry startup/liveness, and shipped Prometheus/Grafana
consumers. No applicable `AGENTS.md` was found in the inspected repository and
ancestor paths. September [round 1](astra-round1-2026-09-24.md),
[round 2](astra-round2-2026-09-24.md) and progress, the October consolidated
catalogue through F40, [pass 8 integration](chain-d52a33f-pass8-sol-integration-2026-10-04.md)
and [pass 9 storage](chain-d52a33f-pass9-astra-storage-2026-10-04.md) were checked
for overlap. F19, F35, F36, F38 and the previously qualified SIGTERM procedure
are not counted again. Historical report search did not locate the exact
alert/panel consumer mismatch below; that is not a claim of global novelty.

Cargo/rustc were absent from PATH and were not installed. Docker Compose's
read-only `config --format json` succeeded for each shipped file; this parses
configuration without building images, launching containers or testing DNS.
No production code, Git state, dependencies, keys, services or live chain were
changed. No external network request or interrupted certificate/malformed-input
scope was pursued. Only this report was added by this worker.

## Build-wrapper lead rejected: provisioning alone does not establish consumption

The [fast image](../../devnet/Dockerfile#L1) offers `Dockerfile.full-build` as
an alternative to copying a host-built binary. The
[full recipe](../../devnet/Dockerfile.full-build#L3) installs `pkg-config`,
OpenSSL/libclang development files, `clang`, `cmake` and `mold`, but does not
explicitly provision `sccache`. It then copies the repository and invokes
`cargo build --release --bin torus-node` from `/build`. The copied
[.cargo configuration](../../.cargo/config.toml#L23) declares
`CC_librocksdb_sys = "sccache cc"` and
`CXX_librocksdb_sys = "sccache c++"`;
[.dockerignore](../../.dockerignore#L1) does not exclude that file. The
[README](../../README.md#L25) also describes `sccache` as a build prerequisite.

**The critical consumption link is not established.** Compiler override keys
in cc-rs have target/host/global forms; arbitrary crate-name suffixes require
proof of a reader. These exact declarations and their comment do not prove
that the pinned RocksDB/cc build scripts consume them. Local pinned dependency
build-script sources were not found in the inspected cache/temp paths. No
dependency was downloaded and no image was built. The initial apparent missing
wrapper candidate was withdrawn during coordinating review: dormant settings
could explain this configuration without any missing-wrapper failure. No
unusable full build, first failing dependency, missing base-image package or
runtime binary incompatibility is established here.

Inspect the exact pinned dependency's compiler-key lookup before changing the
recipe. If caching is intended, establish which supported override activates
it, then provision or deliberately disable that wrapper in the image stage.
A future clean full-image build should check the resulting binary's
`--help`/`--version` in its runtime stage. No existing full-image regression was
located. Do not infer an MSRV failure solely from `rust:1.82`: the copied
[rust-toolchain.toml](../../rust-toolchain.toml#L1) selects `stable`, so rustup
selection needs separate verification. Likewise, this report does not assert
that `perl` is absent from an uninspected base image.

## O02 — P3: timeout alert and panel select a different series from the exporter

[ConsensusTimeoutSpike](../../monitoring/alerts/consensus.yml#L69) evaluates
`rate(torus_consensus_timeout_total[5m]) > 0.1`. The
[consensus dashboard](../../monitoring/dashboards/consensus.json#L249) selects
the same metric with an instance filter. The actual
[exporter registration](../../crates/torus-telemetry/src/lib.rs#L1120) registers a
Prometheus `Counter` using the name `torus_consensus_timeout_total`; counter
sample encoding adds the `_total` suffix. The resulting sample name is
`torus_consensus_timeout_total_total`. The existing
[gap-attribution report](../perf/cap100-gap-attribution-2026-09-10.md#L23)
already uses that exact emitted name. This pass does not claim discovery of the
double suffix itself; the candidate is the shipped alert/panel consumer mismatch.

The [node callback](../../crates/torus-node/src/main.rs#L962) really increments
this counter on view timeout. A Prometheus installation scraping this node
and loading the shipped rules, with no custom alias/recording rule, therefore
has the emitted double-suffix samples but no single-suffix timeout samples.
The alert's selector supplies no timeout series to `rate`, and this alert does
not become firing merely because the real timeout counter rises. The dashboard
panel has no matching samples either. This needs no malformed requests or
adversarial traffic; ordinary repeated timeouts suffice. Other alerts can still
detect a stall, and the correct series remains available to a manual query.
P3 reflects a narrower warning/display loss. No Prometheus evaluation was run.

**Correction and focused validation.** Make the consumer and exporter naming
contract agree, with compatibility handling if the exported name changes.
Encode real `Metrics` after incrementing this counter and assert the exact
sample name, not a substring. Use that encoded series in a rule fixture above
the timeout threshold for the required duration and require
`ConsensusTimeoutSpike` to fire; cover a below-threshold fixture too. Validate
the panel selector against the same exported series. Existing
[encoding tests](../../crates/torus-telemetry/src/lib.rs#L2419) check selected
metrics and do not establish this specific exporter-to-alert contract; no
matching timeout-rule test was located. Tests were read, not run.

## Operational qualifications retained without additional P2 findings

- **Telemetry startup does not acknowledge binding.** The
  [node](../../crates/torus-node/src/main.rs#L1160) spawns `serve_metrics`, drops
  its join handle and immediately logs `telemetry server started`. The
  [server](../../crates/torus-telemetry/src/lib.rs#L2356) propagates bind/accept
  errors as `io::Result`. An ordinary occupied metrics port can therefore end
  the telemetry task while node startup proceeds without reporting that
  task's error. The internal `telemetry server listening` message is emitted
  only after a successful bind and is stronger evidence than the parent log.
  This is a P3 observability/startup qualification, not proof that consensus or
  RPC failed. Decide whether metrics is mandatory, then await initial binding
  or log/monitor failure explicitly. A local occupied-port regression should
  assert the chosen startup policy; none was run.
- **Standalone monitoring files need network wiring.** Read-only Compose
  expansion produced project `devnet`, network `devnet_torus-devnet`, with all
  five nodes attached only to that network. The separate monitoring expansion
  produced project `monitoring`, network `monitoring_default`, with Prometheus
  and Grafana attached only there. Its
  [scrape configuration](../../monitoring/prometheus.yml#L8) nevertheless targets
  `validator-0:9090` through `validator-3:9090`. The
  [monitoring Compose](../../monitoring/docker-compose.yml#L1) declares no shared
  or external devnet network. Separate ordinary launches of these unchanged
  files thus provide no shared service-name network for those targets. The
  [monitoring README](../../monitoring/README.md#L20) describes manual Prometheus
  configuration rather than promising this combined launch, so this is kept
  as a conditional integration qualification. Configure an explicit shared
  network or reachable host targets before relying on these files together.
  `localhost:9100` inside Prometheus likewise requires an exporter in that
  container's network namespace or an explicit target change; the exporter is
  optional, and its absence is not counted as a defect. No DNS/scrape probe or
  container launch was performed.
- **Printed devnet endpoints lag the current mapping.**
  [start.sh](../../devnet/start.sh#L19) reports validator 0 at RPC 8545/P2P 30333
  and metrics 9090. The actual
  [Compose ports](../../devnet/docker-compose.yml#L64), confirmed by read-only
  expansion, are host RPC 8645/P2P 30433 and metrics 9091. Other validators keep
  8546–8548. An operator following the printed endpoints may reach an existing
  host testnet process or receive connection failure. This is P3 printed
  integration guidance; no wrong-chain submission was attempted or claimed.
  Update the output and README endpoint range to agree with the mapping.

## Safeguards, rejected leads and bounded coverage

The default devnet commands use flags present in the current
[parser](../../crates/torus-node/src/main.rs#L91), including private peer
addresses and the optional-value boolean native-gossip flag. Each node supplies
explicit local peers, avoiding the
[default public bootstrap fallback](../../crates/torus-node/src/main.rs#L636).
The full-build alternative is distinct from the default fast image used by
[Compose](../../devnet/docker-compose.yml#L11); the normal `start.sh` does not
silently select the full recipe. Shared target-directory staging is already
documented by [cargo-bin.sh](../../testnet/lib/cargo-bin.sh#L4), so no new
target-directory issue is claimed. ELF/glibc compatibility is explicitly
qualified by the fast image's comments; no binary or ABI was tested.

[HTTP /health](../../crates/torus-telemetry/src/lib.rs#L2352) is explicitly a
liveness probe. It returns a static 200 after accepting the request, without
consensus synchronization or execution-frontier checks. Its source does not
promise readiness, so the absence of those checks is not itself promoted.
[net_listening/net_peerCount](../../crates/torus-rpc/src/net.rs#L26) return fixed
values; they should not be used as evidence of actual peer connectivity. The
[network metric](../../crates/torus-network/src/swarm.rs#L2032), in contrast,
has real connection-event updates. No documented readiness gate relying on
those fixed RPC values was established here.

RPC binding has a stronger startup contract: the dedicated
[RPC runtime](../../crates/torus-node/src/main.rs#L1082) returns its bind result
through a channel and startup waits for it. This narrows the unobserved
telemetry-error qualification; it is not a blanket claim that all listeners
start without checks. CLI metrics address is configurable despite the older
configuration reference's `Hardcoded` table entry. CLI/TOML scalar provenance
remains F35; fields absent from the TOML struct are not automatically advertised
TOML capabilities. Snapshot export wiring and archive-transition pruning
awareness remain the explicit pass-nine qualifications/F38, not new findings.

This bounded review ends here. It does not certify every historical benchmark
launcher, every metric registration/query, clean shutdown drains, external
dependency package inventories, arbitrary architectures, upgrades, production
container networking or deployed readiness policy. The P3 candidate needs its
focused exporter-consumer validation; no earlier finding is closed.
