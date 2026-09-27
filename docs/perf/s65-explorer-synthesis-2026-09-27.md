# s65 explorer synthesis (s68, 2026-09-27)

Source: `docs/perf/s65-explorers/` in the main checkout (untracked). It holds
193 ideas from 56 agents and 49 angles that were never run. An explore agent
synthesized it against what s61-s68 established. Items marked "verified" were
checked in `main` code. Everything else is as reported.

## Context
Cutting leader consensus-thread time twice left throughput flat: the s67
vote-state DB, and the s68 DA-rewrite skip (block_build −30%). Leader build
time is not the limiter at cap 200.

## Top lead: pacemaker deadline grid never rebases when behind
- Verified: `hotstuff_rs/src/pacemaker/implementation.rs` (around L610)
  rebases the entered view's deadline only when it lies far in the future
  (`scheduled > now + 2·max_view_time·mult`). It never rebases a deadline
  that has already passed. Back-off only applies at multiplier > 1.
- Consequence: a view entered late times out immediately. At n=3, one
  TimeoutVote pulls the others out.
- Explorer numbers: about 21% of loaded proposals were orphaned (99 of 475),
  and about 44 of 300 s went to re-proposing.
- The s55 `timeout_base_ms` null was measured at cap 100 with n=3–4 and was
  underpowered. It has never been re-run at cap 200.
- The harness runs a non-default `TORUS_COMMIT_LAG_BACKOFF_CAP=8`
  (`run-cell.sh`).

## Other view-length and variance leads
1. The follower sends its body request only after persisting and sending its
   vote (`implementation.rs` ~2103 → 2155). About 50 ms of that sits on the
   next leader's critical path.
2. Votes queue behind the 4 MB body response at the next leader (unverified).
3. Duplicate body fetches: 61% extra requests. `BODY_RETRY_INTERVAL` is
   100 ms, while header→body p90 is 227 ms.
4. Variance: gossipsub "Send Queue full" logs whole payloads (3.8 GB in one
   blown cell). There is no RUST_LOG directive in the harness.
5. Variance: the jsonrpsee 64-connection cap sits in front of CONC=256, so
   offered load is uncontrolled (acked 305–424/s across identical controls).
6. The swarm task decodes and encodes 4 MB bodies inline, and there are 18
   tokio workers per node.
7. Exec back-pressure. The trade writer uses a blocking `sync_channel(256)`
   (verified). Cancel cost grows with level depth. Neither binds in 300 s
   cells.

## Open bugs
- Verified: session expiry is never enforced at exec. The header timestamp is
  in seconds (`app.rs` `as_secs`), and `timestamp <= session.expiry` compares
  it against an expiry in milliseconds (`eip712.rs`).
- Agent-verified:
  - ModifyOrder has no ownership check (`native_executor.rs` ~3138).
  - Triggered stop fills are dropped (`order_book.rs` ~1272–1327).
  - The header-first path skips TC/NEC/reproposal validation and never
    writes LOCAL_TIP.
  - `/metrics` dies permanently on its first accept error.
  - A failed direct send waits for an unrelated reconnect.

## Counts
193 ideas: about 16 repeat s64, about 10 are done or measured null, about 167
remain (about 80 themes). Of those, about 25 bear on view length and variance,
about 60 are CPU efficiency (deprioritised), and about 45 are correctness or
product.

Unfinished angles worth relaunching: channel capacity, partition/loss (one
slow validator), and protocol alternatives (relative-deadline pacemakers).
