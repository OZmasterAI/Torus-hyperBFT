# Implementation Plan: ADL per-block budget (P0 before testnet)

Design: `docs/plans/adl-budget.md` (Q1-Q5 decided by the owner, 18c s96). Liquidation
semantics: `docs/plans/liquidation.md` (H1, H2, H3, M2, D4, D8, D9, D10, invariants).
Branch `perf/adl-budget` (worktree `/home/oz/projects/wt/adl-budget`, off main aae6b9b).
Build dir: `CARGO_TARGET_DIR=/home/oz/.cargo-target-adl-budget` (one cargo build at a time on
the host: check `pgrep -a cargo` first).

**Q6 is being re-discussed (owner, s18 follow-up) for Hyperliquid parity.** This plan therefore
has two phases. **Phase A** does not depend on Q6. **Phase B** holds every Q6-dependent part as
separate, swappable tasks that run last. Phase A puts the Q6-dependent choices behind one seam
(`adl_market`, task A4): Phase A keeps today's rule there, and Phase B or a snapshot variant
replaces only that function and the tasks marked Q6. Nothing Q6-specific is final until the
owner decides.

```bash
export CARGO_TARGET_DIR=/home/oz/.cargo-target-adl-budget
F="--cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar"
```

## Design Decision

* **Q1, C1: candidates come from point reads over the whole trader set.** A ranking of market `m`
  for side `s` reads `get_position(t, m)` for every trader `t` of
  `NativeExecutor::liq_traders_after(ctx, None, usize::MAX)`. That list is the E2 sorted set
  merged with the block's dirty traders when the records are attached. Without records it is
  the `liq::traders_after` walk. Reads go through `AccountReader::get_position` (the record for a
  clean trader, the overlay for a dirty one). The ranking keeps every holder on side `s`, and
  `adl_rank` stays unchanged. `ADL_MAX_SCAN_ROWS` and the 65,536-row window go away. The cost of
  one ranking is the number of traders examined.
* **Q4: a FIFO queue in `CF_NATIVE_LIQUIDATION`.** Rows are `0x07 ‖ height(8 BE) ‖ trader(20)`
  → `[1]` and `0x08 ‖ trader` → `height(8 BE)`.
  * The regular pass, when it classifies an account ADL, runs D4 (cancel orders and stops) and
    then **enqueues** the account.
  * The pass **skips** an account that is already queued. The skip still uses a slot in the scan
    window (open question 6).
  * The vault (D8) is enqueued when its AV < 0 and it is not queued yet.
  * The pass then **drains** the queue in key order under `W` work units. For each account it
    closes the marked positions in ascending market. When no marked position is left, it applies
    today's `adl_account` tail (non-vault: `move_collateral` to the vault, D9) and dequeues the
    account.
  * `liquidation_due` also checks the `0x07` prefix.
* **Q3: the budget.** One unit is one trader examined by a ranking, one close, or one queued
  account visited (the visit unit is open question 1). A step is one (account, market) ADL. It
  starts only while `used < W` and runs to the end once started, so a block overshoots by at most
  one step. The constant `ADL_WORK_PER_BLOCK` comes from measurement (task A6).
* **M2 and the queue.** A queued account holds **no** pending row: the queue row keeps the step
  due. On enqueue, `set_pending(false)`. On dequeue, a non-vault account gets
  `mark_pending(ctx, marks, l1, u)` (set iff the account can still be valued and is not healthy).
  The vault gets `set_pending(false)`, because the vault is exempt from stage 1 and backstop, so a
  pending row for it would keep every block due. The vault-only pending code
  (`liquidation_step.rs:247-250`) goes away.
* **Q6 seam (kept as today in Phase A).** At a deferred close, the price is the drain block's
  previous mark clamped to the bankruptcy price at that step (H1/D10). The ranking is computed
  when the step runs. In the common case the drain finishes in the enqueue block, and the result
  then equals today's rule except for one change: ADL now runs after the regular pass instead of
  inside it.
* **Where the Q2 cache lives (B1, Q6-dependent).** A local
  `BTreeMap<(MarketId, bool), Vec<AdlCandidate>>` in `adl_drain`, dropped when the drain returns.
  It is never node state or a context field. Steps that reuse it **re-walk the ranked list from
  the start**. `adl_close` already re-reads each candidate and skips a vanished or flipped
  position, so no index has to be stored. This is deterministic, and the extra reads are bounded:
  a fully consumed candidate was closed by an earlier step of the same block (already counted),
  and during the drain no position grows or appears, because only ADL transfers run and they only
  reduce sizes.
* **Q5, freeze (B3, Q6-dependent).** One `prefix_exists(0x07)` per context, held in
  `ctx.adl_queue_on: Option<bool>`. A membership read per action happens only when the queue is
  non-empty. The checks sit at the three entry points that do not share a path (see B3). The new
  code is `FailureReason::Liquidating = 9` (`"liquidating"`).

## Success Criteria

1. With more than 65,536 position rows, the top-ranked counterparty at a **high** address is
   closed first (fails on 7ec5eb2).
2. The C1 records path equals the fallback walk. This includes traders written in the block:
   the shadow check reports no mismatch, and the L1 seeded test stays green.
3. The budget stops at `W` (± one step). The next block resumes in FIFO order (height, then
   address). Two runs produce identical rows in every native-root CF, and the e2e native roots
   are equal.
4. Over a multi-block drain, after every block: OI is symmetric in each market, total value is
   conserved (Σ available + order margin + UPnL at the mark, vault included), and each ADL'd
   account ends at exactly 0 (`available + order_margin == 0`, flat).
5. The vault is enqueued when its AV < 0. `liquidation_due` is true while the queue is not empty.
6. Backstop and stage 1 are unchanged: every existing test in `tests/liquidation_tests.rs`, the L1
   seeded test, and `offmark_bad_debt_tests` pass with their expectations unchanged. GOLDEN_B is
   re-pinned only for ADL blocks (A4).
7. `ubench_adl`: per-block ADL time ≤ target (A6). `W` is written into `adl-budget.md` with the
   measurement.
8. Phase B (once Q6 is decided): re-classification dequeues on recovery; the freeze rejects every
   frozen action kind with `liquidating`, credits deposits, and does no membership read when the
   queue is empty; the gauges report queue size and deficit.

## Q6 dependency map

| Part | Task | Why it depends on Q6 |
|---|---|---|
| Price at a deferred close (previous mark of the drain block, clamped to bankruptcy at the step) | A4 seam `adl_market` (today's lines kept) | A snapshot variant would use the enqueue block's price |
| Ranking timing (per step in Phase A, per (block, market) in B1) | B1 | A snapshot ranks once, at enqueue |
| Re-classification (AV ≥ 0 → dequeue) | B2 | A snapshot keeps the ADL decision final (HL) |
| Freeze semantics | B3 | Needed or not, and how strict, depends on how long accounts stay queued |
| Gauges (queue size, deficit) | B4 | The deficit definition (−AV now, or fixed at enqueue) |
| `W` target | A6 (measurement Q6-neutral; the chosen value is not) | The "W large enough to finish typical events in one block" option |

## Tasks

Phase A (Q6-independent): A1 → A2 → A3 → A4 → A5 → A6.
Phase B (Q6-dependent, swappable, last): B1-B4, each needs A4 only.
Phase C: the proof.

---

### Task A1: core `adl_candidates` over a trader set (Q1)

**Test first** (`crates/torus-core/tests/liquidation_tests.rs`). Replace
`adl_candidates_scan_at_most_max_rows` (:274-290) with:

```rust
/// Q1 (s96): ADL counterparties = every holder on side `want_long` among
/// `traders` (each read through `get`), in `traders` order; the bankrupt
/// account is never on the opposite side of its own position, the vault is an
/// ordinary holder. Reads every trader once (the ranking's work units).
#[test]
fn adl_candidates_are_every_opposite_holder_of_the_trader_set() {
    let (_d, pm) = setup();
    open_pair(&pm, &addr(1), &addr(2), 1, 3, 100);
    open_pair(&pm, &addr(5), &addr(3), 1, 1, 100);
    open_pair(&pm, &addr(4), &LIQUIDATOR_VAULT, 1, 2, 100);
    open_pair(&pm, &addr(7), &addr(6), 2, 1, 100); // market 2 only
    let traders = traders_after(pm.state(), None, usize::MAX).unwrap();
    let reads = std::cell::Cell::new(0);
    let shorts = adl_candidates(&traders, false, |t| { reads.set(reads.get() + 1); pm.get_position(t, 1) }, |_| Ok(fp(1)))
        .unwrap();
    assert_eq!(
        shorts.iter().map(|c| (c.trader, c.size)).collect::<Vec<_>>(),
        vec![(addr(2), fp(3)), (addr(3), fp(1)), (LIQUIDATOR_VAULT, fp(2))]
    );
    assert_eq!(reads.get(), traders.len(), "one read per trader");
    let longs = adl_candidates(&traders, true, |t| pm.get_position(t, 1), |_| Ok(fp(1))).unwrap();
    assert_eq!(longs.iter().map(|c| c.trader).collect::<Vec<_>>(), vec![addr(1), addr(4), addr(5)]);
}
```

It fails to compile on 7ec5eb2 (the signature changes), which is the RED step.

**Implementation** (`crates/torus-core/src/liquidation.rs`):
* Delete `ADL_MAX_SCAN_ROWS` (:41-45). Keep `SCAN_PAGE`, because `pending_count` uses it.
* Replace `adl_candidates` (:340-384) with the pure filter below. `exclude` goes away: the
  bankrupt account's own position is on the wanted side's opposite, and `adl_close` (:325)
  still skips `u`.

```rust
/// Q1 (s96, `docs/plans/adl-budget.md`): ADL counterparties = every position on
/// side `want_long` of `traders` (ascending, each once), `get` reading a
/// trader's position in the ADL market, `av` valuing a holder (ranking only,
/// C7). The caller's work units for the ranking = `traders.len()` (Q3).
pub fn adl_candidates(
    traders: &[Address],
    want_long: bool,
    mut get: impl FnMut(&Address) -> Result<Option<Position>, CoreError>,
    mut av: impl FnMut(&Address) -> Result<FixedPoint, CoreError>,
) -> Result<Vec<AdlCandidate>, CoreError> {
    let mut out = Vec::new();
    for t in traders {
        let Some(p) = get(t)? else { continue };
        if p.is_long != want_long || p.size <= FixedPoint::ZERO {
            continue;
        }
        out.push(AdlCandidate {
            trader: *t,
            is_long: p.is_long,
            size: p.size,
            entry_price: p.entry_price,
            account_value: av(t)?,
        });
    }
    Ok(out)
}
```

* Remove the `CF_NATIVE_POSITIONS` import if it becomes unused (`traders_after` still uses it).
* Update the doc on H3 in `docs/plans/liquidation.md` (the window paragraph, :63-68) and on *ADL*
  (:199). Write "every opposite-side holder (C1 point reads over the trader set, adl-budget
  Q1)".

**Verify:** `cargo nextest run -p torus-core liquidation $F` and `cargo check --workspace --tests -q`.
The bridge breaks at `liquidation_step.rs:505` and `ubench_adl.rs:34,189`. A2 fixes both; land
A1 and A2 together.
**Depends on:** none.

---

### Task A2: bridge C1 wiring + the fairness test (Q1)

**Test first.**

1. `crates/torus-bridge/tests/liquidation_tests.rs`, new section `// ---- adl-budget Q1 ----`.
   It fails on 7ec5eb2: the window ends before `top`, so the step closes against `pad(0..4)`.

```rust
/// Q1 (s96): ADL counterparties = EVERY opposite-side holder. 230 padding
/// traders (low addresses) x 301 rows = 69,230 rows > 65,536; each is short 1
/// in market 1 with a huge AV (ranks low). `top` (0xFF.., highest address) is
/// short 4 with AV 500 at 900 -> rank (1000/900) x (3600/500) = 8: first. U
/// long 4 @ 1,000, collateral 200: AV -200 at 900 -> ADL; closes 4 against top.
#[test]
fn adl_reaches_a_top_ranked_counterparty_past_65536_rows() {
    let (_d, db) = liq_db(&[1]);
    let ctx = ctx_at(db.clone(), 2);
    let pad = |i: u32| {
        let mut a = [0x01u8; 20];
        a[16..].copy_from_slice(&i.to_be_bytes());
        Address::new(a)
    };
    let (u, top, sink) = (addr(0x02), Address::new([0xFF; 20]), Address::new([0xEE; 20]));
    fund(&ctx, &sink, fp(1_000_000_000));
    for i in 0..230 {
        fund(&ctx, &pad(i), fp(1_000_000));
        open_pair(&ctx, &sink, &pad(i), 1, 1, 1_000);
        for m in 2..=301 {
            open_pair(&ctx, &pad(i), &sink, m, 1, 100); // unlisted: valued at entry
        }
    }
    fund(&ctx, &u, fp(200));
    fund(&ctx, &top, fp(100));
    open_pair(&ctx, &u, &top, 1, 4, 1_000);
    assert!(all_positions(&ctx).len() > 65_536);
    let mut c = ctx_at(db.clone(), 2);
    set_mark(&c, 1, fp(900));
    let before = total_value(&c, &marks(&[(1, 900)]));
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(pos(&c, &u, 1), FixedPoint::ZERO);
    assert_eq!(pos(&c, &top, 1), FixedPoint::ZERO, "top-ranked, highest address: closed first");
    assert!((0..230).all(|i| pos(&c, &pad(i), 1) == -fp(1)), "no padding trader touched");
    assert_eq!(oi(&c, 1), (fp(230), fp(230)));
    assert_eq!(total_value(&c, &marks(&[(1, 900)])), before);
}
```

   `total_value` indexes `marks[&p.market_id]`. For this test, a variant that values unmarked
   markets at entry (UPnL 0) is needed: add `.get(..).map_or(ZERO, ..)` to the helper, which
   keeps every existing caller unchanged.

2. `crates/torus-bridge/src/liquidation_l1_tests.rs`:
   * add `adl_rankings: usize` to `Stats` (:118-144) and collect it at :360-368;
   * change :426 to
     `assert_eq!(s.traders_slice, 6 * BLOCKS as usize + s.adl_rankings, "E2: every L1 block's candidates (pass + ADL rankings) from the slot: {s:?}");`
     and add `assert!(s.adl_rankings > 0, "ADL rankings ran through the slot: {s:?}");`.
     The seeded run then shadow-checks every ranking's trader list (`liq_traders_after`
     :284-292) and every record read (`AccountReader::get_position` NE:1499-1509), including
     traders the block wrote.
   * New focused test `adl_candidates_records_equal_walk_with_dirty_traders`. Two blocks through
     `begin_resident` / `end_resident`: block 1 opens shorts for `trader(1..6)` against a long
     `trader(0)` and freezes them into R. Block 2 changes `trader(2)` (reduce) and `trader(4)`
     (close), and opens `trader(7)` (new) in the overlay. Then
     `NativeExecutor::adl_candidates_of(&ctx, 1, false)` with R attached (shadow on) must equal
     the same call on a context built with `begin_resident(None, ..)` over the same DB and
     overlay rows. The test asserts the lists are equal, `shadow_mismatches` is empty, and
     `counters.records > 0`.

**Implementation** (`crates/torus-bridge/src/liquidation_step.rs`; NE = `native_executor.rs`):
* NE:1100: add `adl_rankings: std::sync::atomic::AtomicUsize` to `SumsCounters` (doc: "C1: ADL
  rankings over the slot's trader set").
* New method next to `liq_traders_after` (:273):

```rust
/// adl-budget Q1 (C1): `m`'s counterparties on side `want_long` — a point
/// read of every trader of the positions CF (the slot's sorted set merged
/// with the block's dirty traders, else the walk), through the records for a
/// clean trader and the overlay for a dirty one. Returns the candidates and
/// the traders examined (the ranking's work units, Q3).
fn adl_candidates_of<T: StateBackend>(
    ctx: &NativeExecContext<T>,
    m: MarketId,
    want_long: bool,
) -> Result<(Vec<liq::AdlCandidate>, u64), CoreError> {
    let traders = Self::liq_traders_after(ctx, None, usize::MAX)?;
    let reader = AccountReader::of(ctx);
    // C7: ranking AV with entry fallback for unmarked markets. An
    // overflowing valuation ranks last (AV 0); a storage error stays an error.
    let cands = liq::adl_candidates(&traders, want_long, |t| reader.get_position(t, m), |t| {
        let bal = ctx.positions.get_native_balance(t)?;
        let v = match reader.view(t, &bal) {
            Ok(v) => v,
            Err(CoreError::Overflow(_)) => return Ok(FixedPoint::ZERO),
            Err(e) => return Err(e),
        };
        Ok(v.available.checked_add(v.order_margin).and_then(|x| x.checked_add(v.upnl)).unwrap_or(FixedPoint::ZERO))
    })?;
    #[cfg(test)]
    if let Some(s) = ctx.sums.as_ref() {
        bump(&s.counters.adl_rankings);
    }
    Ok((cands, traders.len() as u64))
}
```

* In `adl_account` (:501-517), replace the `liq::adl_candidates(.., ADL_MAX_SCAN_ROWS, ..)`
  call with `let (cands, _) = Self::adl_candidates_of(ctx, m, !p.is_long)?;`. That removes the
  local `reader` and the `av` closure there. Ranking and close are otherwise unchanged.
* `crates/torus-bridge/tests/ubench_adl.rs`: drop the `ADL_MAX_SCAN_ROWS` import (:34) and the
  window note (:129-133). In `parts` (:183-208), time
  `liq::adl_candidates(&liq::traders_after(&ctx.state, None, usize::MAX)?, false, |t| ctx.positions.get_position(t, m), |_| Ok(ZERO))`
  (the fallback walk cost, labelled "walk"). The records path is timed by the step itself.

**Verify:**
`cargo nextest run -p torus-bridge adl_reaches_a_top_ranked $F` (RED on 7ec5eb2, GREEN after),
`cargo nextest run -p torus-bridge liquidation $F`, and
`cargo nextest run -p torus-bridge scenario_b_liquidation_digests $F`. GOLDEN_B must stay
**unchanged** here: below 65,536 rows the candidate set is the same, and `adl_rank` is a total
order, so the closes are identical.
**Depends on:** A1.

---

### Task A3: queue rows + `liquidation_due` (Q4)

**Test first:**
* `crates/torus-core/tests/liquidation_tests.rs`:

```rust
/// Q4 (s96): `0x07 ‖ height ‖ trader` -> [1] (FIFO: height, then address) and
/// `0x08 ‖ trader` -> height. Enqueue is idempotent (keeps the first height);
/// dequeue deletes both rows; `adl_queue_next` pages in key order.
#[test]
fn adl_queue_is_fifo_by_height_then_address() {
    use torus_core::liquidation::{adl_dequeue, adl_enqueue, adl_queue_key, adl_queue_next, adl_queue_nonempty, adl_queued, ADL_QUEUE_TAG};
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    assert!(!adl_queue_nonempty(&db).unwrap());
    assert!(adl_enqueue(&db, &addr(9), 5).unwrap());
    assert!(adl_enqueue(&db, &addr(8), 5).unwrap());
    assert!(adl_enqueue(&db, &addr(1), 6).unwrap());
    assert!(!adl_enqueue(&db, &addr(9), 7).unwrap(), "already queued: height 5 kept");
    let mut order = Vec::new();
    let mut start = vec![ADL_QUEUE_TAG];
    while let Some((h, t)) = adl_queue_next(&db, &start).unwrap() {
        order.push((h, t));
        start = [adl_queue_key(h, &t).as_slice(), &[0]].concat();
    }
    assert_eq!(order, vec![(5, addr(8)), (5, addr(9)), (6, addr(1))]);
    assert!(adl_queued(&db, &addr(9)).unwrap());
    assert!(adl_dequeue(&db, &addr(9)).unwrap());
    assert!(!adl_dequeue(&db, &addr(9)).unwrap(), "absent: no write");
    assert!(!adl_queued(&db, &addr(9)).unwrap());
    assert_eq!(adl_queue_next(&db, &[ADL_QUEUE_TAG]).unwrap(), Some((5, addr(8))));
    assert!(db.get_cf_raw(CF_NATIVE_LIQUIDATION, &[[0x08u8].as_slice(), addr(9).as_slice()].concat()).unwrap().is_none());
}
```

* `crates/torus-bridge/tests/liquidation_tests.rs`: extend `liquidation_due_reads_cooldown_and_cursor_rows`
  (:773-780). After the cooldown row is deleted, `adl_enqueue(&db, &addr(7), 3)` must make it
  due, and `adl_dequeue` must make it not due again.

**Implementation** (`crates/torus-core/src/liquidation.rs`, after `PENDING_TAG` :54 and after
`pending_key` :462):

```rust
/// adl-budget Q4 (s96): `0x07 ‖ height(8, BE) ‖ trader(20)` -> `[1]` — the ADL
/// queue, FIFO by the height the account was classified ADL, then address.
pub const ADL_QUEUE_TAG: u8 = 0x07;
/// Q4: `0x08 ‖ trader(20)` -> height (8, BE) — queue membership.
pub const ADL_MEMBER_TAG: u8 = 0x08;

pub fn adl_queue_key(h: u64, t: &Address) -> [u8; 29] {
    let mut k = [0u8; 29];
    k[0] = ADL_QUEUE_TAG;
    k[1..9].copy_from_slice(&h.to_be_bytes());
    k[9..].copy_from_slice(t.as_slice());
    k
}

fn member_key(t: &Address) -> [u8; 21] {
    let mut k = [0u8; 21];
    k[0] = ADL_MEMBER_TAG;
    k[1..].copy_from_slice(t.as_slice());
    k
}

/// Q4: queue `t` at `height` unless queued (then nothing is written).
pub fn adl_enqueue<T: StateBackend>(state: &T, t: &Address, height: u64) -> Result<bool, CoreError> {
    if state.get_cf_raw(CF_NATIVE_LIQUIDATION, &member_key(t))?.is_some() {
        return Ok(false);
    }
    state.put_cf_raw(CF_NATIVE_LIQUIDATION, &adl_queue_key(height, t), &[1])?;
    state.put_cf_raw(CF_NATIVE_LIQUIDATION, &member_key(t), &height.to_be_bytes())?;
    Ok(true)
}

/// Q4: remove `t` from the queue (both rows); `false` when not queued.
pub fn adl_dequeue<T: StateBackend>(state: &T, t: &Address) -> Result<bool, CoreError> {
    let Some(v) = state.get_cf_raw(CF_NATIVE_LIQUIDATION, &member_key(t))? else {
        return Ok(false);
    };
    let h = u64::from_be_bytes(v.as_slice().try_into().map_err(|_| malformed("adl member"))?);
    state.delete_cf_raw(CF_NATIVE_LIQUIDATION, &adl_queue_key(h, t))?;
    state.delete_cf_raw(CF_NATIVE_LIQUIDATION, &member_key(t))?;
    Ok(true)
}

pub fn adl_queued<T: StateBackend>(state: &T, t: &Address) -> Result<bool, CoreError> {
    Ok(state.get_cf_raw(CF_NATIVE_LIQUIDATION, &member_key(t))?.is_some())
}

pub fn adl_queue_nonempty<T: StateBackend>(state: &T) -> Result<bool, CoreError> {
    Ok(state.prefix_exists(CF_NATIVE_LIQUIDATION, &[ADL_QUEUE_TAG])?)
}

/// Q4: the first queued account at or after `start` (the bare tag, or a
/// queue key followed by 0x00).
pub fn adl_queue_next<T: StateBackend>(state: &T, start: &[u8]) -> Result<Option<(u64, Address)>, CoreError> {
    let Some((k, _)) = state.iterate_cf_prefix_from(CF_NATIVE_LIQUIDATION, &[ADL_QUEUE_TAG], start, 1)?.pop() else {
        return Ok(None);
    };
    if k.len() != 29 {
        return Err(malformed("adl queue"));
    }
    Ok(Some((u64::from_be_bytes(k[1..9].try_into().expect("8 bytes")), Address::from_slice(&k[9..]))))
}
```

* `liquidation_step.rs:144-148` `liquidation_due`: add
  `|| state.prefix_exists(CF_NATIVE_LIQUIDATION, &[liq::ADL_QUEUE_TAG])?` and extend the doc
  comment. Update the table in `docs/plans/liquidation.md` (:220-227) with the `0x07` and `0x08`
  rows.

**Verify:** `cargo nextest run -p torus-core adl_queue $F`,
`cargo nextest run -p torus-bridge liquidation_due $F`, and `cargo check --workspace --tests -q`.
**Depends on:** none (independent of A1/A2; can land before them).

---

### Task A4: enqueue in the pass, drain under `W` (Q3, Q4, D8)

**Test first** (`crates/torus-bridge/tests/liquidation_tests.rs`, section `// ---- adl-budget Q3/Q4 ----`).
Shared fixture: listed markets `1..=4`, constant mark 900 in every block (the previous mark after
block 1 equals the mark, so total value at the mark is conserved across blocks).
`u1 = addr(0x29)` and `u2 = addr(0x28)` are long 1 @ 1,000 in every market with collateral 100
(AV = 100 − 400 < 0 → ADL in block 1). `u3 = addr(0x21)` has the same positions but collateral
1,000 (healthy at 900); block 2 sets its balance to 100 so it is classified at height 2. The
counterparties `c(i) = addr(0x40 + i)`, `i ∈ 0..4`, are each short 3 in every market, with
10,000,000 collateral. `sink = addr(0x60)` is long 9 in every market with 10,000,000 collateral,
which makes OI per market 3 + 9 = 12 long and 12 short. Trader count `A` = 8. The vault holds no
position here, because D9 only moves cash. A step costs `A + 1` units (one ranking, one close:
the top counterparty's 3 covers a size of 1) plus 1 per account visit.

```rust
/// Run block `h` of the queue fixture with budget `w`; returns the context.
fn queue_block(db: &StateDb, h: u64, w: u64) -> NativeExecContext {
    let mut c = ctx_at(db.clone(), h);
    set_mark_all(&c, &[1, 2, 3, 4], fp(900));
    NativeExecutor::run_liquidations_with(&mut c, 2_048, 64, w);
    c
}

fn queue(ctx: &NativeExecContext) -> Vec<(u64, Address)> {
    liq_rows(ctx, 0x07).iter().map(|(k, _)| (u64::from_be_bytes(k[1..9].try_into().unwrap()), Address::from_slice(&k[9..]))).collect()
}

fn invariants(ctx: &NativeExecContext, before: FixedPoint) {
    for m in 1..=4 { let (l, s) = oi(ctx, m); assert_eq!(l, s, "OI symmetric in {m}"); }
    assert_eq!(total_value(ctx, &marks(&[(1, 900), (2, 900), (3, 900), (4, 900)])), before, "value conserved (vault incl.)");
}
```

1. `adl_budget_stops_at_w_and_resumes_in_fifo_order`: `W = 2·(A+1) + 1` (one visit + two steps;
   the second step starts at `used = A+2 < W`).
   * Block 1: `queue == [(1,u2),(1,u1)]` before the drain, so u2 is drained first (same height,
     lower address). After block 1, u2 holds 2 of its 4 positions. In block 2 u3 becomes ADL and
     is enqueued at `(2,u3)`, behind u1 despite its lower address.
   * Per block, assert exactly two positions closed in total: the first queued account's lowest
     remaining markets.
   * Drain order across blocks: u2 (m1, m2 | m3, m4), then u1, then u3.
   * After each block, run `invariants`. At the end: queue empty; `ab(u) == (0, 0)` and no
     position for u1, u2, u3; `liquidation_due` false (counterparties healthy, so no pending
     row).
2. `adl_budget_overshoots_by_at_most_one_step`: `W = A + 3`. The first step starts (`1 < W`)
   and ends at `A + 2 < W`, so a second step starts and the block ends at `2A + 3`. Assert that
   exactly 2 markets closed. With `W = A + 2` the second step does not start, so assert that
   exactly 1 market closed.
3. `adl_queue_drain_is_deterministic`: run the whole fixture twice from fresh DBs (default `W`
   and `W = A+2`). After every block, `iterate_cf` of `CF_NATIVE_POSITIONS`,
   `CF_NATIVE_BALANCES` and `CF_NATIVE_LIQUIDATION` must be equal between the two runs of the
   same `W`.
4. `the_vault_is_enqueued_when_its_value_goes_negative`: reuse the setup of
   `the_vault_is_adld_when_its_value_goes_negative` (:711-732). Block 2 runs with `W = 1`, which
   allows only the visit and no step: the vault keeps long 10, `queue == [(2, LIQUIDATOR_VAULT)]`,
   and `liquidation_due` is true. Block 3 runs with the default `W` and the queue empties. The
   vault has no pending row, OI is `(0, 0)`, and total value is conserved. **The price is
   Q6-dependent.** In block 3 the previous mark is block 2's 900, and
   `adl_price(900, bankruptcy 970, long)` = 900. So the vault closes at 900:
   `available = 50 − 10 × 75 = −700`, and S is paid 10 × 100. The existing same-block test
   closes at 970. A snapshot or escrow variant would close at 970 here too. When Q6 is decided,
   this is the assertion that changes.
5. `an_adl_account_is_enqueued_after_its_orders_are_cancelled`: with `W = 1`, block 1 cancels the
   account's resting bid and its stop (D4: reservations released, `order_margin == 0`), and the
   account holds a `0x07` row and **no** `0x06` row. In block 2 the regular pass skips it: with
   `scan = 1`, the cursor moves past it, and no second `liquidations_adl` count happens.
6. Existing ADL tests stay unchanged and pass with the default `W`, because the drain finishes in
   the classification block: `adl_closes_against_ranked_counterparties_at_the_previous_mark`,
   `adl_without_a_previous_mark_uses_the_mark_and_the_deficit_goes_to_the_vault`,
   `the_liquidation_step_publishes_the_vault_deficit`, `the_vault_is_adld_when_its_value_goes_negative`,
   `telemetry_counts_an_adl_account`, `telemetry_counts_the_vault_adl_outside_the_act_budget`.
   (`set_mark_all` is a 3-line helper looping `set_mark`.)

**Implementation:**

* `crates/torus-core/src/liquidation.rs`: add the constant. Its value stays a placeholder until
  A6:

```rust
/// adl-budget Q3 (s96): ADL work units per block — traders examined by a
/// ranking + closes + queued accounts visited. Set from the A6 measurement
/// (ns / unit on the rig) so ADL adds <= ~20 ms per block.
pub const ADL_WORK_PER_BLOCK: u64 = 100_000; // placeholder, A6 sets it
```

* `liquidation_step.rs:44-67`: `run_liquidations` passes `liq::ADL_WORK_PER_BLOCK`.
  `run_liquidations_with(ctx, scan, act, work)` gains `work: u64` and threads it into
  `liquidation_pass`. That makes 12 mechanical call-site edits: `liquidation_l1_tests.rs:339`,
  `tests/liquidation_tests.rs` ×8, and `tests/perf_equivalence_golden.rs:218`
  (pass `liq::ADL_WORK_PER_BLOCK`).
* `liquidation_pass` (:150-266):
  * after `let cursor = …` (:170): `let queue_on = liq::adl_queue_nonempty(&ctx.state)?;`
  * :185-187 becomes `scanned += 1; last = Some(trader); if queue_on && liq::adl_queued(&ctx.state, &trader)? { continue; } stats.scanned += 1;`
    The skip uses a window slot. If it did not, a window full of queued accounts would end the
    loop without hitting a budget, the cursor would be deleted, and the traders after them would
    starve.
  * :219-237: after D4 (:219-220), `Health::Adl` now runs
    `liq::adl_enqueue(&ctx.state, &trader, ctx.block_height)?; stats.pending_changed |= liq::set_pending(&ctx.state, &trader, false)?; results.push(NativeActionResult::ok("liquidation", 3000)); continue;`.
    It skips `settle_flat_deficit`, `clear_cooldown` and `mark_pending` (the drain does them on
    dequeue). Backstop and stage 1 are untouched.
  * Replace the vault block :238-250 with:

```rust
        // D8 + adl-budget Q4: the vault is exempt from stage 1 / backstop and
        // enters the ADL queue like any account when its AV < 0.
        if !liq::adl_queued(&ctx.state, &LIQUIDATOR_VAULT)?
            && Self::liq_view(ctx, &marks, &LIQUIDATOR_VAULT, l1)?
                .is_some_and(|v| liq::classify(&v) == Some(Health::Adl))
        {
            liq::adl_enqueue(&ctx.state, &LIQUIDATOR_VAULT, ctx.block_height)?;
            stats.pending_changed |= liq::set_pending(&ctx.state, &LIQUIDATOR_VAULT, false)?;
            stats.adl += 1;
            stats.vault_adl = true;
        }
        if !marks.is_empty() {
            Self::adl_drain(ctx, &marks, &prev, l1, work, stats)?;
        }
```

    With no usable mark there is nothing to close. The queue waits, so an oracle outage never
    triggers a D9 that empties a queued account.
* Replace `adl_account` (:481-544) with `adl_drain` + `adl_market` + `adl_finish`:

```rust
    /// adl-budget Q3/Q4: drain the ADL queue in key order (FIFO: height, then
    /// address) under `work` units. Per account: its marked positions in
    /// ascending market, one step each (atomic; starts only while used < work,
    /// so a block overshoots by at most one step); with no marked position
    /// left: [`Self::adl_finish`]. Each market is stepped at most once per
    /// account per block (OI symmetry closes it; a position left open is a
    /// bug and waits for the next block instead of looping).
    fn adl_drain<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        marks: &Marks,
        prev: &Marks,
        l1: bool,
        work: u64,
        stats: &mut LiqStats,
    ) -> Result<(), CoreError> {
        let mut used = 0u64;
        let mut start = vec![liq::ADL_QUEUE_TAG];
        while used < work {
            let Some((h, u)) = liq::adl_queue_next(&ctx.state, &start)? else { break };
            start = [liq::adl_queue_key(h, &u).as_slice(), &[0]].concat();
            used += 1; // the visit (open question 1)
            // [B2 seam: re-classification]
            let mut after: Option<MarketId> = None;
            loop {
                let next = ctx.positions.positions_for_trader(&u)?.into_iter().find(|p| {
                    marks.contains_key(&p.market_id) && after.is_none_or(|a| p.market_id > a)
                });
                let Some(p) = next else { break };
                if used >= work {
                    stats.adl_work = used;
                    return Ok(());
                }
                used += Self::adl_market(ctx, marks, prev, &u, &p)?;
                after = Some(p.market_id);
            }
            let open = ctx.positions.positions_for_trader(&u)?.iter().any(|p| marks.contains_key(&p.market_id));
            if open {
                tracing::error!(account = %u, "liquidation: ADL left a marked position open (OI asymmetry?) — retried next block");
                continue;
            }
            Self::adl_finish(ctx, marks, l1, &u, stats)?;
        }
        stats.adl_work = used;
        Ok(())
    }

    /// Decision 5 + D10 + review H1: one ADL step — `u`'s position `p` closed
    /// against the ranked opposite holders. Returns its work units (traders
    /// examined + closes).
    /// Q6 SEAM: price = this block's previous mark (else the mark) clamped to
    /// `u`'s bankruptcy price now; ranking computed now (B1: cached per block
    /// and market). A snapshot variant replaces exactly these lines.
    fn adl_market<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        marks: &Marks,
        prev: &Marks,
        u: &Address,
        p: &Position,
    ) -> Result<u64, CoreError> {
        let m = p.market_id;
        let mark = marks[&m];
        let px = prev.get(&m).copied().unwrap_or(mark);
        let bankruptcy = Self::adl_rest(ctx, marks, u, m)?
            .and_then(|rest| liq::bankruptcy_price(rest, p.is_long, p.size, p.entry_price));
        let px = liq::adl_price(px, bankruptcy, mark, p.is_long);
        let (cands, examined) = Self::adl_candidates_of(ctx, m, !p.is_long)?;
        let closes = liq::adl_close(&ctx.positions, u, m, px, &liq::adl_rank(mark, cands))?;
        // telemetry: today's per-(account, market) info line + debug per close (:518-532)
        Ok(examined + closes.len() as u64)
    }

    /// The account has no marked position left (today's `adl_account` tail +
    /// the pass's post-action steps): a non-vault account hands its remaining
    /// collateral to the vault (H1/D9: it ends at exactly 0), loses its
    /// cooldown row when flat, leaves the queue; M2 pending row recomputed
    /// (never for the vault: exempt from stage 1 / backstop).
    fn adl_finish<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        marks: &Marks,
        l1: bool,
        u: &Address,
        stats: &mut LiqStats,
    ) -> Result<(), CoreError> {
        if *u != LIQUIDATOR_VAULT {
            liq::move_collateral(&ctx.positions, u, &LIQUIDATOR_VAULT)?;
        }
        if ctx.positions.positions_for_trader(u)?.is_empty() {
            liq::clear_cooldown(&ctx.state, u)?;
        }
        liq::adl_dequeue(&ctx.state, u)?;
        stats.pending_changed |= if *u == LIQUIDATOR_VAULT {
            liq::set_pending(&ctx.state, u, false)?
        } else {
            Self::mark_pending(ctx, marks, l1, u)?
        };
        Ok(())
    }
```

* `LiqStats` (:22-38): add `adl_work: u64` (log only). Add `adl_work` to the info line
  (:120-132).
* `perf_equivalence_golden.rs` GOLDEN_B (:546): scenario B contains an ADL (:441, :451). The
  queue rows are written and deleted in the same block, which leaves tombstones in the running
  hash, and ADL now runs after the pass. Re-capture with `GOLDEN_PRINT=1`, and only after
  checking that the first changed digest is the first ADL block. Note this in the commit message
  (a change made on purpose, adl-budget Q4).

**Verify:** `cargo nextest run -p torus-bridge adl_ $F`, `cargo nextest run -p torus-bridge liquidation $F`,
`cargo nextest run -p torus-bridge offmark_bad_debt $F`, `cargo nextest run -p torus-bridge perf_equivalence $F`,
`cargo nextest run -p torus-bridge storage_reads $F`.
**Depends on:** A2, A3.

---

### Task A5: e2e: the queue keeps blocks due; roots agree (Q4)

**Test first** (`crates/torus-consensus/src/app.rs`, after
`liquidation_e2e_partial_stage1_keeps_the_step_due` :17414):

```rust
/// adl-budget Q4: the ADL queue keeps the step DUE. T (71) long 10 @ 1,000,
/// collateral 50, plus 13 more accounts (seeds 80..=92) seeded the same way
/// against S (72, short 140). Mark 970 (block 1's submissions aggregate at
/// 2): AV 50 - 300 < 0 -> all 14 ADL at height 2 (FIFO = address order).
/// `test_adl_work = 2`: one visit + one step per block, so one account drains
/// per block in blocks 2..=15. The oracle rows are pruned at 12 (as in the M2
/// test), so blocks 13..=15 carry no action, no oracle row, no cursor and no
/// pending row: only the 0x07 rows run them.
#[test]
fn liquidation_e2e_adl_queue_drains_over_empty_blocks() { … }
```

The body follows `liquidation_e2e_partial_stage1_keeps_the_step_due` (:17415-17433):
`liq_fixture_db(10, 50)`, the 13 extra accounts seeded through `PositionManager`, and
`ctx.test_adl_work = Some(2)`. The assertions:
* after block 12: `oracle_sub_rows(&db).is_empty()`;
* per block h: exactly one more account is flat, in address order;
* after block 15: `liquidation_due == false`, S is flat, and the vault holds the summed deficit;
* no `exec_failed`. A second run of the same blocks must give the same `persisted_native_root` and
`dump_all_cfs` (the `liquidation_telemetry_does_not_change_block_results_or_state` pattern,
:17442-17462).

**Implementation:**
* `app.rs:645` (next to `test_engine_threads`): add
  `#[cfg(test)] test_adl_work: Option<u64>`, initialized `None` at :3984 and :10815.
* `app.rs:2339`: under `#[cfg(test)]`, if `self.test_adl_work` is `Some(w)`, call
  `NativeExecutor::run_liquidations_with(&mut ctx, liq::LIQ_SCAN_PER_BLOCK, liq::LIQ_ACT_PER_BLOCK, w)`.
  Production is unchanged. Use the same `#[cfg(test)]` / `#[cfg(not(test))]` shape as
  `engine_threads` at :2287-2291.
* `app.rs:1966-1969`: update the comment ("… cooldown, cursor, pending or ADL-queue row").

**Verify:** `cargo nextest run -p torus-consensus liquidation_e2e $F`.
**Depends on:** A4.

---

### Task A6: measure ns/unit, set `W` (Q3)

**Test first** (`crates/torus-bridge/tests/ubench_adl.rs`, still `#[ignore]`):
* After block 2, keep running blocks 3, 4, … with the same mark until `liq::adl_queue_nonempty`
  is false (cap 100,000 blocks). `UB_ADL_WORK` (default `liq::ADL_WORK_PER_BLOCK`) goes to
  `run_liquidations_with(ctx, LIQ_SCAN, LIQ_ACT, w)`.
* Per block, print: the ADL wall time (the step's timer minus the regular pass; the `adl_work`
  field of the info line goes through `AdlClock` (:47-63), extended to record that field) and the
  units used. Summary: blocks to drain, ms per block (p50/p90/max), ns/unit (Σ ms / Σ units).
* Assertions: `blocks_to_drain >= 1`; every block's units ≤ `W + A + K_max + 1` (at most one
  step of overshoot); after the drain, OI is symmetric in all 300 markets and each bankrupt
  account has `ab == (0, 0)`.
* `report` (:211-231): the "one ADL event per (account, market)" assertion holds over the
  **sum** of all blocks.

**Implementation / measurement:**
1. On a quiet box (ozarchy, release): run `UB_ADL_TRADERS=1000` and `5000`, with
   `UB_ADL_BANKRUPT=1` and `10`, and `UB_ADL_WORK=10^6` (unbounded per block). Take ns/unit from
   these runs.
2. `W = floor(20 ms / (ns_per_unit × rig_factor))`. `rig_factor` = rig ms/step ÷ quiet-box
   ms/step from the same build: ~26 / 13.8 ≈ 1.9 in s18; re-measure if possible, else use 2.
   Round down to a multiple of 1,000.
3. Re-run with `UB_ADL_WORK=W`: p90 ADL ms per block × rig_factor ≤ 20.
4. Write the constant in `liquidation.rs` and add a section "8. Budget measurement" to
   `docs/plans/adl-budget.md`: the table (N, K, ns/unit, W, ms/block p50/p90, blocks to drain)
   and the command lines. Hand the runs to the `bench-runner` agent (perf campaign rule).

**Verify:** `cargo nextest run -p torus-bridge liquidation $F` (green with the final `W`), then
`UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=10 cargo test -p torus-bridge --release --test ubench_adl -- --ignored --nocapture`
(bench-runner).
**Depends on:** A4. The final `W` value is Q6-sensitive: under the "W large enough to finish
typical events in one block" option, the target changes but the measurement does not.

---

## Phase B — Q6-dependent (swappable; do not start until Q6 is final)

Each B task is self-contained against A4 and can be dropped or replaced on its own.

### Task B1: rank once per (block, market) (Q2; ranking timing = Q6)

**Test first** (`liquidation_l1_tests.rs`, which has access to the counters):
`adl_ranks_each_market_once_per_block`. Three queued accounts are long in market 1. Per block,
`counters.adl_rankings` must equal the number of distinct `(market, side)` pairs stepped. The
closes must equal a reference run without the cache when every counterparty's AV is unchanged
in the block. The test also uses one counterparty whose AV changes between two steps: it is
ranked with the AV from the first step, as decided.
**Implementation:** in `adl_drain`, keep a local
`let mut ranked: BTreeMap<(MarketId, bool), Vec<liq::AdlCandidate>> = BTreeMap::new();` and pass
`&mut ranked` to `adl_market`. On a miss: `adl_candidates_of` + `adl_rank` + insert, and units +=
examined. On a hit: units += 0 before the closes. Then `adl_close(.., &ranked[&key])`, which
re-walks from the start and re-reads each candidate. Units += closes. Why re-walk and not a
stored index: see Design Decision.
**Verify:** `cargo nextest run -p torus-bridge adl_ $F`. Re-run A6's ubench and check that
ns/unit is unchanged.
**Depends on:** A4.

### Task B2: re-classification dequeues on recovery (Q4 step 2; Q6)

**Test first** (`tests/liquidation_tests.rs`): `a_queued_account_that_recovers_leaves_the_queue`.
u1 is queued with `W = 1` in block 1 (visit only). In block 2 the mark moves to 1,000:
AV ≥ 0 → dequeued. Its positions are untouched, no counterparty moved, and the regular pass of
block 2 or later classifies it. If AV < MM, `mark_pending` sets the `0x06` row. The vault
variant: a vault that recovers is dequeued and gets **no** pending row.
**Implementation:** at the `[B2 seam]` in `adl_drain`:

```rust
            match Self::liq_view(ctx, marks, &u, l1)?.map(|v| liq::classify(&v)) {
                Some(Some(Health::Adl)) => {}
                // AV >= 0 (marks moved): the regular pass handles it from now.
                Some(Some(_)) => {
                    liq::adl_dequeue(&ctx.state, &u)?;
                    stats.pending_changed |= if u == LIQUIDATOR_VAULT {
                        liq::set_pending(&ctx.state, &u, false)?
                    } else {
                        Self::mark_pending(ctx, marks, l1, &u)?
                    };
                    continue;
                }
                // No marked position / overflow / isolated: the close loop finds
                // nothing to step and `adl_finish` runs (today's tail).
                _ => {}
            }
```

**Verify:** `cargo nextest run -p torus-bridge adl_ $F`.
**Depends on:** A4.

### Task B3: freeze queued accounts (Q5; strictness = Q6)

**Signed-action entry points** (`native_executor.rs`, NE):

| Path | Where | Reaches `execute_action`? | Check |
|---|---|---|---|
| Single action (`execute`; crash-replay batches; RPC simulation `torus-rpc/src/torus.rs:2822`) | NE:5421 → `execute_action` NE:5450 | yes | top of `execute_action` |
| Batch Phase 1 non-place actions | NE:5831, :5872 → `execute` | yes | (covered) |
| CoreWriter drain (PlaceOrder, Cancel, CancelAll, Delegate, Undelegate, ClaimRewards, LockPermanent, ClaimUnbonded, LockboxWithdraw→TransferToSpot) | `drain_core_writer` NE:9936-9972 → `execute` NE:9965; mapping `core_writer_to_native` NE:~10362-10409 | yes | (covered) |
| Batch PlaceOrder / PlaceOrderBatch (Phase 2 serial and sharded prepare, Phase 3/4) | flatten NE:5771-5804 | **no** | at flatten |
| Cancel-all runs (`TORUS_CANCEL_BATCH`) | NE:5852-5869 → `exec_cancel_all_run` NE:8947 | **no** | at flatten |
| Individual handlers behind `execute_action` | place NE:8338, cancel NE:8814, cancel-all NE:8861, modify NE:9045, TransferToSpot NE:9831, Withdraw NE:9852, staking NE:9253-9306, governance NE:9722-9781, oracle NE:9544, validator NE:9316-9429, sessions NE:9450-9514 | via the above | — |
| Credits (allowed) | CoreWriter `LockboxDeposit` → `exec_settle_lockbox_deposit` NE:9978 (not via `execute`); `TransferToPerp` → `exec_deposit_to_native` NE:9816 (signed by the account: open question 3) | — | not frozen |

No leverage or margin-change action exists (`NativeAction`, `torus-types/src/lib.rs:609-…`), and
no native-to-native transfer exists. `Withdraw{to}` credits the **EVM** balance of `to`
(`lockbox.rs:146-190`), so there is no "incoming native transfer" path to keep open.

**Test first** (`tests/liquidation_tests.rs`, new section):
* `a_queued_account_is_frozen`: queue `u` (`W = 1`). In the next block run one `execute_batch`
  with u's PlaceOrder (plain and reduce-only), PlaceOrderBatch(2), CancelOrder,
  CancelAllOrders (once with `execute_batch_cancel_mode(.., true)`, once with `false`),
  ModifyOrder, TransferToSpot, Withdraw, Delegate. Every result has `!success`,
  `reason == FailureReason::Liquidating`, and an error that starts with "liquidating". The batch
  gives 2 results for the batch. Also queue a CoreWriter `CancelAll` and a `LockboxWithdraw`
  for u; after `drain_core_writer` both are rejected with `Liquidating`. `LockboxDeposit` and
  `TransferToPerp` credit u's `available`. Another sender's actions in the same batch succeed.
  u's rows in positions, balances (except credits) and books are unchanged.
* `no_freeze_read_when_the_queue_is_empty`: with `CountingBackend`
  (`tests/common/counting_backend.rs`), `execute_batch` of 100 actions from 10 senders on an
  empty queue performs exactly **one** `CF_NATIVE_LIQUIDATION` read (the `prefix_exists`) and
  no `0x08` reads.
* `crates/torus-state/src/action_status.rs:380-390` and `crates/torus-rpc/src/types.rs:660-670`:
  add `(FailureReason::Liquidating, 9, "liquidating")` to the round-trip tables (RED until the
  variant exists).

**Implementation:**
* `torus-state/src/action_status.rs:47-100`: `Liquidating = 9` (doc: "adl-budget Q5: the sender
  is in the ADL queue"), `from_u8` 9, `as_str` "liquidating".
* NE:3588: add the field `pub(crate) adl_queue_on: Option<bool>` (doc: one `prefix_exists(0x07)`
  per context; `run_liquidations` resets it to `None` at its end, because the step changes the
  queue). Initialize it `None` at NE:~4057.
* New `NativeExecutor::adl_frozen(ctx, sender) -> bool`. On first use it computes
  `liq::adl_queue_nonempty`; when that is true it reads `liq::adl_queued(sender)`. A read error
  sets `ctx.fatal_error` (fail-stop) and returns `false`.
* `execute_action` NE:5450: first line
  `if !matches!(action, NativeAction::TransferToPerp { .. }) && Self::adl_frozen(ctx, sender) { return Self::liquidating(); }`.
  The exempt list depends on open questions 2 and 3.
* Flatten NE:5771-5804: add `FlatAction::Frozen`. For a frozen sender, push `Frozen` instead of
  `Place` (one per order of a valid batch; an over-cap batch is still skipped first) and instead
  of `Other`, so a frozen cancel-all never joins a run. In both Phase 1 loops (NE:5827-5836 and
  5845-5878; in the run's inner loop treat it like `Place`: no state touched),
  `results[i] = Self::liquidating()`.
* `fn liquidating() -> NativeActionResult { NativeActionResult::rejected("liquidating", (FailureReason::Liquidating, "liquidating: account is in the ADL queue".into())) }`
* `docs/api/` action-status docs: add the reason (grep `price_band`).

**Verify:** `cargo nextest run -p torus-bridge frozen $F`,
`cargo nextest run -p torus-state action_status $F`, `cargo nextest run -p torus-rpc failure_reason $F`,
`cargo check --workspace --tests -q`.
**Depends on:** A4 (and A3 for the rows).

### Task B4: queue gauges (Q6)

**Test first** (`tests/liquidation_tests.rs`): `telemetry_reports_the_adl_queue`. Two accounts are
queued with `W = 1`. After the step, `torus_liquidation_adl_queue == 2` and
`torus_liquidation_adl_queue_deficit == Σ −AV` (exact tokens from the fixture). After the drain
both gauges are 0. Without metrics: identical rows (extend
`telemetry_does_not_change_results_or_state` :1143).
**Implementation:**
* `torus-telemetry/src/lib.rs`: fields next to `liquidation_deferred` (:91):
  `liquidation_adl_queue: Gauge` and
  `liquidation_adl_queue_deficit: Gauge<f64, AtomicU64>`. Register them at :1042-1047
  (`torus_liquidation_adl_queue`, `torus_liquidation_adl_queue_deficit`) and add them to the
  struct literal (:2335).
* `liquidation_telemetry` (:73-136): only with metrics and no fatal error, page the `0x07` rows
  (a 3-line loop over `adl_queue_next`). For each row: `liq_view_walk` → `max(0, −AV)`, summed
  in tokens. A read error skips the update, the same rule as the pending gauge.
* `docs/monitoring-setup.md` metric rows; `docs/plans/liquidation.md` telemetry paragraph.

**Verify:** `cargo nextest run -p torus-bridge telemetry_ $F`.
**Depends on:** A4.

---

## Verification end-to-end (Phase C)

1. Unit / integration: every task's Verify command, then
   `cargo nextest run -E 'rdeps(torus-core)' $F` (the core crate changed).
2. Before each commit:
   `cargo nextest run --workspace $F > /tmp/claude-1000/…/nextest.log 2>&1` (in the background)
   plus the doc-test command from `TESTING.md`. A `flaky` count above 0 is reported. Before
   merging to main: one full `cargo test --workspace -q` (TESTING.md).
3. ubench (bench-runner): `ubench_adl` before (7ec5eb2: ms/step 12-17 at N=5000) and after (ms
   per block at `W`, ns/unit, blocks to drain) for N ∈ {1000, 5000}, K ∈ {1, 10}, with R and
   without R.
4. **Proof phase** (separate, bench-runner, after Phase A merges; again after Phase B):
   * harness fixes from s17 first (`liq_stress.py` `_count` KeyError, stale test genesis in the
     worktree, reflink-seeded stale binaries);
   * **S=750**: liveness PASS (no commit gap above the cadence bound), liquidation step ≤ the W
     target in every block, **drain time in blocks** until the `0x07` prefix is empty, AGREE,
     vault deficit identical on all nodes, **conservation of total balances** (Σ available +
     order margin + UPnL at the mark over all accounts incl. the vault, sampled per block over
     the drain);
   * **S=400 unchanged**: 100 BACKSTOP, vault +19,984,975.69, step times in the same range.

## Rollback

* Docs and the plan only until A1 lands. Each task is its own commit on `perf/adl-budget`; a
  failing task is reverted with `git revert <sha>`, never with `reset --hard`.
* Phase B tasks are independent of each other. Dropping one (for example after a Q6 decision)
  reverts that commit only. The A4 seam keeps today's semantics.
* Consensus: every A/B task changes block results (fresh genesis, M1). No migration is needed
  because pre-testnet chains restart from genesis. Rolling back after a deploy means redeploying
  the previous binary and restarting from genesis.

## Q6 variants (not final; what each would change in this plan)

**V1: snapshot at enqueue.** In the enqueue block, for each marked position, store the
previous-mark price clamped to the bankruptcy price, and possibly the ranking, in the queue row.
* A3: the row value goes from `[1]` to a borsh `Vec<(MarketId, price)>` (~24 B per market; 270
  markets ≈ 6.5 KB per account, root-hashed). Storing the **ranking** is not feasible:
  S=750 is ~2,500 candidates × 20 B × 270 markets ≈ 13.5 MB per account in consensus state. The
  realistic V1 therefore stores **prices only** and still ranks at drain time (B1 stays).
* Bankruptcy prices for all markets have to be defined jointly at enqueue (each market with the
  others at the mark). Today, each step sees the state the previous closes left. The account
  may then not end at exactly 0; the D9 tail (`move_collateral`) absorbs the remainder.
* A4: `adl_market` reads the price from the row instead of `prev`/`adl_rest`. Computing the
  prices at enqueue costs one `adl_rest` per marked position (~P × O(P) reads, cheap) and is not
  budgeted.
* B2 is dropped, because the ADL decision is final, as in HL. B3 stays and gets stricter: the
  positions must not change before the close. B4's deficit is fixed at enqueue
  (Σ (entry − price) × size), not −AV.
* The A4 vault test closes at 970 instead of 900.

**V2: P2 escrow.** Memory `d88cc443` (18c, s96, 21:46) records this as **the owner decision**.
The coordinator's later message says Q6 is still under discussion, so it is not adopted here.
In the bankruptcy block, positions move at their ADL prices to a dedicated escrow account, the
account goes flat at that block, and the escrow drains by market under `W`. What changes:
* A1 and A2: unchanged, except that the escrow is excluded as a candidate (an explicit filter in
  `adl_candidates_of`).
* A3: the queue is keyed by **market** (`0x07 ‖ height ‖ market`, or simply the escrow's
  position rows act as the queue), not by account. `0x08` membership is dropped.
* A4: the pass replaces enqueue with a backstop-like transfer (`liq::transfer` at the clamped
  ADL price) to the escrow, then the D9 `move_collateral`. That reuses
  `backstop(.., |m| price(m))` with a new destination and price closure. The drain steps the
  escrow's positions in market order against ranked counterparties at the stored price. With
  several accounts in one market at different prices, the escrow's single netted position
  averages the entries. Matching HL's final balances per counterparty then needs per-lot rows
  (`0x09 ‖ market ‖ height ‖ account` → `(side, size, price)`), not the escrow's position.
  Open point to settle before coding.
* The escrow is exempt from scan, margin and stage 1 / backstop, like the vault: the
  `not_vault` filter (:174) generalizes to "protocol accounts".
* B2 and B3 are dropped (the account is flat at B; the freeze is not needed). B4 becomes the
  escrow's position count and UPnL. B1 is unchanged.
* A5 is unchanged in shape: escrow rows keep the step due.
* Proof adds: the escrow ends with 0 positions and 0 balance, and the S=750 26.5M vault deficit
  is explained under the clamp.

## Open questions (for the owner; recommendation in brackets)

1. **Units for a visit.** Re-classification and the visit cost 1 unit each. Without this, a queue
   of accounts that all recovered (B2) would be dequeued in a single block without bound. [Count
   1 per visited account, as written in A4.]
2. **Freeze scope.** The literal Q5 reading is "every signed action". That includes
   `SubmitOraclePrices`, `AttestStateHash`, `JailVote`, staking, governance and session
   actions, and none of them touches the perp account (staking has its own balance). Freezing
   them stops a validator's oracle and attestation duties while it is being ADL'd. [Freeze only
   the perp-account actions: orders (incl. batch and reduce-only), cancels, cancel-all, modify,
   TransferToSpot, Withdraw, and their CoreWriter forms.]
3. **`TransferToPerp`** is signed by the queued account, and it is a deposit. [Allowed:
   "deposits are credited".]
4. **Same-block order change.** ADL closes now run after the regular pass instead of inside it.
   An account classified later in the same pass therefore sees its counterparties' state before
   this block's ADL closes. [Accept; it is deterministic and GOLDEN_B is re-pinned.]
5. **Drain cost at S=750.** Rankings are per account in FIFO order. The Q2 cache saves work only
   when queued accounts share markets inside one block. With `W` ≈ 10 rankings per block,
   ~26.8k rankings take ~2,700 blocks. [Measure in A6 and the proof. If the drain is too slow,
   consider C2 (holders index) or a per-market drain order; the latter is a Q4/Q6 change.]
6. **Skipped queued accounts use a scan slot.** They do not count in
   `torus_liquidation_scanned`. [As written: otherwise the cursor logic breaks.]
7. **Unmarked remainder.** A queued account whose remaining positions are all unmarked goes
   through D9 (collateral to the vault) and is dequeued, which is today's `adl_account` tail.
   With no mark at all, the drain is skipped. [Keep.]
8. **Q6 status conflict.** Memory `d88cc443` says Q6 = P2 escrow (decided, with Q5's freeze
   dropped and the drain ordered by market). The coordinator says Q6 is still open. [Owner to
   confirm. If P2 is final, A3/A4 take the V2 shape before coding, and B2/B3 are dropped.]
