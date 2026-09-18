# Deterministic C1 pending-parent qualification candidate

Fixture-verified candidate on `test/c1-pending-parent`, based on `90442ed`.
Root verification passed on 2026-09-18 under receipt
`70a3bccf-e6a4-4724-99fe-939ad6c4eafe`: 132 state tests passed (2 ignored),
152 consensus tests passed (1 ignored), and a separate release node build
passed. A positive binary byte check found `TORUS_C1_QUALIFICATION` in the
built node. The checks include the deterministic subprocess SIGKILL/replay
fixture described below; they do not constitute a three-validator C1 run.

`TORUS_EXEC_PIPELINE` stays default off. This is an induced-stall correctness
fixture, never a throughput promotion measurement. Torus issue
`7fda17dc-f16c-46db-ba21-96162e849343` is resolved for this fixture implementation;
three-validator qualification and economic-state dependency remain unverified.

The opt-in `TORUS_C1_QUALIFICATION` value is a JSON object containing exactly:

```json
{"run_id":"unique-run","validator":"64 lowercase hex Ed25519 public-key characters","parent":5,"nonce_key":"56 hex characters: sender20bytes + nonce8bytes big-endian","evidence":"/absolute/external/new-evidence.jsonl","timeout_s":30}
```

The literal placeholders above must be replaced. Parent must be at least 2 and
leave room for child N+1; deadline is 1–300 seconds. Only the selected validator
gets this environment value, and its actual signing public key must match.
Arming requires explicit pipeline enablement. The deadline begins at arming,
after serial startup replay. An existing evidence file, malformed marker,
wrong identity, or already-applied target refuses arming. A fresh DB may have
no marker at arming; qualification never treats that as a valid N−1 marker.
Evidence must be outside the RocksDB directory and its parent must exist.

W receives native Flush(N), checks the exact durable marker N−1 and persisted
native header/body, records W_PARKED, then waits before run_job or trie locks.
There is no release API. Timeout, teardown, or invalid evidence sets fail-stop,
unparks W only into its error/exit path, and never writes N.

E constructs N+1 with immediate parent pending(N). The actual execution nonce
replay guard, for one configured key only, calls a separate state read helper
which returns both the bytes used in the decision and the supplying parent
height. Ordinary state reads are untouched. The witness requires source N,
value equal to the nonce-consumption height N, and absence in the direct DB.
The resulting real decision skips the duplicated action. Diagnostic reads,
including overlay-marker checks, cannot manufacture this witness.

After successful engine and book saving, and after releasing resident books,
E requires the witness, W still parked, and DB marker N−1. It publishes READY
before writing the child marker, freezing its overlay, or handing it to W.
E then also waits for SIGKILL or invalidation. This proves a **nonce/replay-guard
dependency**, not an economic balance dependency or a crash literally inside a
market worker. Non-native/serial barriers at either target height invalidate.

Records contain monotonic sequence, runID, validator public key, PID, Linux
process-start ticks, kernel bootID, explicit wall/boot-clock deadlines, and
fixed N/N+1. Parent and child canonical hashes are populated as they become available: ARMED cannot know future blocks;
W_PARKED binds N; PARENT_READ and READY bind both. Only the complete ordered
ARMED/W_PARKED/PARENT_READ/READY chain qualifies. Controller state, rather than
log order, establishes the relationships. Evidence is external JSONL, flushed
with file sync; it does not change the database WAL or application fsync policy.
These synchronization syscalls retain ordinary filesystem failure/blocking
semantics; the deadline bounds controller waits, not a hung kernel I/O call.

The Linux subprocess fixture uses native blocks 5/6 from the existing pipeline
fixture: block 5 consumes a nonce and leaves a sell; block 6 contains its duplicate
and a matching buy. The parent validates evidence, PID/start identity, both
canonical block hashes and the actual read witness, SIGKILLs the child, then
requires observed death before both deadlines and rechecks the entire evidence
file so a timeout racing the kill cannot pass.
Restart explicitly removes the qualification environment and performs serial
replay from marker 4 through 6 without attaching W. All CF rows and the native
root must match an independent serial reference. Kill/replay artifacts identify
the processes and boundaries. Fixture evidence lives only in its new TempDir.

Negative fixtures cover config bounds, wrong validator/job/height/marker,
existing artifacts, wrong parent, diagnostic-read rejection, source precedence,
and deadline invalidation without writing the parked batch. Existing serial and
pipeline differential suites exercise the disabled path. A three-validator
harness is **not implemented here**: it must supply the controlled duplicate,
select matching N/N+1, verify fresh identity/evidence before kill, explicitly
unset the hook on restart, and require replay plus survivor digest agreement.
Random timing or a positive replay gap alone still does not qualify C1.

Focused verification commands (schedule only when no performance cell is active):

```sh
cargo test --release -p torus-state qualification_parent_source_matches_real_read_precedence
cargo test --release -p torus-consensus c1_
cargo test --release -p torus-consensus exec_pipeline_
```

The passing receipt also covers full release state and consensus library suites
and a separate node build. No throughput benefit, power-loss durability,
economic balance read dependency, exhaustive crash-window coverage, or survivor
digest agreement is established by these fixtures. Keep pipeline defaults and
commit/ancestry rules unchanged pending the remaining qualification.
