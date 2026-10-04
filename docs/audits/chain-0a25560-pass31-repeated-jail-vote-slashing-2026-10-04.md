# Pass 31 — repeated jail votes and slash application

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. Focus: the threshold transition
from JailVote accumulation through slash, jail duration and unjailing.

**New static finding candidate: a reached JailVote threshold can repeatedly
slash an already-jailed validator.** The vote handler rejects a tombstoned
target but permits a target whose status is already `Jailed`. Every vote is
stored under a voter/target key, and whenever the non-expired votes exceed
two-thirds of active stake, the code applies another downtime slash and calls
`jail_validator` again. It neither consumes the threshold votes nor guards the
slash branch against an already-jailed target. Since jailing removes the target
from the active-stake denominator, the same vote set can remain sufficient
(and can become a larger fraction of the remaining active set).

A source-derived schedule: start with four active validators of equal stake.
Three validators vote against the fourth; the 75% tally exceeds the 2/3
threshold, slashing the target by 10 bps and setting it to Jailed. The remaining
three voters then represent 100% of active stake, while their unexpired votes
remain stored. A subsequent `JailVote` by one of them against the same target
again satisfies the threshold, applies another 10-bps slash to the reduced
remaining stake, and resets `jailed_until` to current height plus two days.
Because vote expiry is one day and jail duration is two days, the active voters
can refresh their existing votes before they expire and repeat this transition.
The target may therefore lose stake repeatedly and be prevented from completing
the cooldown needed to unjail; repeated 10-bps slashes can eventually push
self-stake below the unjail minimum.

The target guard and repeated action are in
[`record_jail_vote`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/staking.rs#L597).
It tests only `Tombstoned`, stores the vote, tallies, then calls both `slash`
and `jail_validator` on each qualifying call (L611-L645). The
[`JailVote` native handler](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/native_executor.rs#L7885)
provides the ordinary signed transaction path. The two-day jail and one-day
vote expiry are defined at
[`types.rs`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/types.rs#L395).
Unjailing requires the cooldown to expire and then clears votes at
[`staking.rs`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/staking.rs#L815).

A suitable fix would apply the slash once per threshold episode, reject or
no-op votes against a currently jailed target, or consume/reset votes when the
threshold action occurs. A regression should execute a threshold, then submit a
second vote before expiry and assert that slash amount and `jailed_until` do not
advance again absent fresh evidence or a new jail cycle. This has been saved as
Torus hypothesis issue `1fb71507-86cf-48c7-a006-a542680c4421`.

## Limits

The finding follows the source call path and deterministic stake example; no
Rust test or multi-validator chain execution was run. It is a candidate pending
production regression. No source changes were made.
