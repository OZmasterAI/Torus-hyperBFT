# L6.1: window-committed Classic order books

Design note (s131, owner order: Classic final form = Classic + L3.3 + L3.1 + L6.1, so Classic keeps L3.1's deep-book
gain with the running state hash ON). Consensus-changing and genesis-bound. Background: `docs/perf/8k-explore-ideas.md`
row L6.1 and footnote l61; `docs/plans/deep-book-save-options.md` section 3 (branch `perf/mode3-checkpoints-b0`).

## 1. Problem

`h_n = SHA-256(h_{n-1} ‖ n ‖ D_native(n) ‖ D_evm(n))`, and `D_native(n)` covers every write of block n to
`cf_native_order_books` (hash id 3, `running_hash.rs`). In Classic that is the whole blob of every dirty book, every
block. L3.1 skips that encode on pipelined blocks and pays it at the next checkpoint window close, which fires on
node-local wall time (interval, barriers, shutdown). So with the hash on, a coalesced block hashes no book bytes on one
node and the blobs on a node whose block ran serially: `h_n` forks. That is why L3.1 refuses any hash activation today.

## 2. Rule (consensus)

With L6.1 on, for every hashed height n (hash step Start / Continue, i.e. n >= activation):

1. **n % 100 != 0:** `D_native(n)` skips every hash-id-3 entry (`cf_native_order_books`), whatever the block's pending
   set holds. Every other CF is hashed as today.
2. **n % 100 == 0 (attest heights, `STATE_HASH_CHECKPOINT_INTERVAL`):** block n's Classic save writes the blob of
   **every** book in the executing context (all markets with a book, market ascending, not only the dirty ones), and
   `D_native(n)` hashes hash-id-3 entries as today. So `h_n` at attest heights commits to the full book state after n.
   The native path is forced at these heights (like epoch boundaries, `run_native`), so an empty block still commits.

Hence `h_n` is a function of block contents only: the book bytes enter at a height fixed by the chain, never by a
node-local window. The set "every book in the context" is the set of decodable stored blobs (the load reads them all;
the resident holder carries the same set, which the resident goldens already rely on).

**L6.1 changes the state transition, not only the preimage (review correction, s131).** A clean book's rewrite is the
stored value, so the book save itself leaves every CF as it was. But forcing the native phase at an attest height that
would otherwise skip it (an empty block with nothing due) runs every block-level step of that phase at n. The steps
that can then change state at n (instead of at the next native block, which may be much later):

* **Governance** (`process_governance(n)`): a proposal whose vote ended before n finalizes at n; a passed proposal
  whose timelock expired executes at n (its payload's rows, e.g. a parameter change).
* **Liquidation pass** (`run_liquidations`): it walks the positioned traders from its cursor. With more traders than
  one pass scans (`LIQ_SCAN_PER_BLOCK`) the pass at n writes the cursor, which makes n + 1, ... due until the walk
  ends; it also rewrites the ADL mark-base rows and acts on any account that a pass would classify differently now
  (e.g. a mark that aged out since the last native block).
* **Native-phase bookkeeping rows**: a chain whose native phase never ran yet gets them at n (seen in the test:
  `cf_native_markets` `__book_mode__` and `__next_global_order_id__`, written by the first native phase).
* No change from the others when they are not due: the oracle step aggregates only with submission rows (which make the
  block due anyway), the CoreWriter drain only due rows, fees only with revenue, the epoch step only at boundaries, the
  EVM gas counter only with gas; the nonce-window prune runs on both paths.

So the execution rule is consensus in its own right. Every node takes it from its genesis chain config
(`ChainConfig::book_window_activation`, carried on `ExecutionContext`), never from node-local META, and with L6.1 on a
node that cannot apply the running-hash META rows at boot refuses to start (the digest reads them). Test:
`l61_forced_attest_block_runs_the_same_transition_on_every_node` (an empty attest height, not an epoch boundary, with a
proposal expiring before it and more traders than one pass scans: every node variant, including stale META, runs the
same transition at 100).

**Activation.** Both rules require `n >= H_l61` explicitly (`running_hash::book_window_active`); today `H_l61` is the
hash activation height, so below it nothing is hashed. With P0.0 `H_l61` may be later: below it the digest hashes books
as without L6.1 and no height is forced.

**Versus L6.1 off (same blocks):** where the attest block runs the native phase in both (it has work, or a step is due),
`W_on(n) = W_off(n) \ {id 3}` for n % 100 != 0 and `W_on(n) = W_off(n) \ {id 3} ∪ {(3, market, blob) for every book
after n}` for n % 100 == 0. Where only L6.1 forces it, the state can differ from n on (above). Below activation nothing
is hashed, as today.

## 3. L3.1 with the hash on

`configure_book_coalesce` allows the hash iff L6.1 is on. Per block: at a non-attest pipelined height L3.1 coalesces as
today (the digest skips id 3 anyway). At an attest height the block does not coalesce: its save writes every book into
its own pending set, then the window's debt (`CkptState::unsaved`) is cleared, because the block now holds the newest
blob of every book. The Attest close (always a checkpoint on the pipelined path) then has nothing owed. Other closes pay
the debt into the closing layer exactly as L3.1 does (those bytes are never hashed). Fail-stops are L3.1's, unchanged.

## 4. Durable DB, crash, replay, readers

* At every checkpoint (and so at every attest height n) the DB equals the per-block run's DB at that height: L3.1's
  invariant, plus the attest block writing every blob itself. The checkpoint at n carries `h_n` and its META rows.
* Crash mid-window: replay from S serially; serial blocks save per block and the digest applies the same rule (skip id 3
  off attest heights, full save at attest heights), so the replay reaches the same `h_n`. No new persisted state.
* Boot verify: Classic has none; unchanged. RPC `torus_getOrderBook` reads the DB (lags to the last close, as L3.1).
  No EVM reader of books. The native root is unchanged (same bytes).

## 5. Switch (genesis-bound)

* Genesis `consensus.book_window_commit: bool`, default false (absent = off = today's hash preimage). Copied into
  `ChainConfig.book_window_commit`. `validate`: requires `state_hash_activation_height` (inert otherwise).
* Boot: `running_hash::configure_book_window(db, ChainConfig::book_window_activation())` right after
  `configure_activation`, before the replay; stored in META `state_hash_book_window` (the L6.1 activation height, 8 BE;
  node-local key, never hashed). A change with a stored chain discards it, as an activation change does. With L6.1 on,
  a failure of either write refuses the boot. The digest reads the row (serial: META; flush worker: `ChainMemo`). E
  does NOT: `ExecutionContext::book_window` is set from the chain config, and `book_window_full_save_at(activation, n)`
  is a pure function of it and n, because the forced native phase is a state transition (section 2).
* Refuse to start if L6.1 is on and the book mode is not Classic (genesis `consensus.book_mode`, since the s131 book-mode genesis field; `ChainConfig::validate` refuses the same genesis): row modes write per-order rows to
  the same CF, which this rule would drop.
* All nodes agree because it is in genesis, like the activation height. When P0.0 lands it becomes
  `activations.book_window_commit` (height = the hash activation).
* The L3.1 gate keys on the hash in one place, `l31_hash_blocker(hash_activation, book_window_activation)` (app.rs):
  L3.1 runs iff no hash or L6.1 is active from the hash's first height. Its input is
  `ChainConfig::book_window_activation()`, so P0.0 switches it there; an L6.1 activation after the hash keeps L3.1 off
  for the run (a per-height rule is P0.0's to add).

## 6. Cost (estimates)

Removes per-block SHA-256 of blobs on W (~155 MB per block at P1M, ~80-100 ms) and, with L3.1, the per-block encode.
Adds one full save of every book per 100 blocks on E (~66 ms at P1M, ~56 with L3.3; at standard load one ~3.6 ms save)
and its SHA on W. At deep load every book is dirty in 100 blocks anyway, so the extra over L3.1 is ~0.

## 7. Tests (tests first; RED shown before the code)

Fixture: L3.1's 260-block deep P2 stream (attest heights 100, 200), hash on from 1, L6.1 on.
1. Golden: per-block pipelined run (no L3.1) with a dump at every height. L3.1 on under every 1 / 5 / 7, never (barriers),
   never + epoch fast path, 1 ms interval, seeded random: per-height write sets (so every `h_n`), final hash, retained
   checkpoint hashes, and the DB dump + native root at every durable height equal the golden. Serial and non-resident
   serial runs give the same write sets and hash.
2. Shape: L6.1-on write sets equal the L6.1-off write sets transformed as in section 2; the final dump equals off's
   except the hash rows.
3. Crash mid-window (six crashes, three with the checkpoint in flight): equal to the golden at every durable height.
4. Forced attest commit: an empty block at height 100 runs the native path and hashes every book.
5. Mutations must fail 1: the digest hashing books at one non-attest height; the digest never filtering (with
   barriers); an attest height skipping the full save; an attest height not hashing books.
6. Config: genesis parse / validate, META configure and chain reset, L3.1 gate (hash on allowed iff L6.1), boot refusal
   in row modes.
