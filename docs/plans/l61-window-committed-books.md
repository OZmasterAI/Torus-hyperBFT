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
node-local window. A clean book's rewrite is the stored value, so the state (every CF, native root) is unchanged by L6.1;
only the hash preimage changes. The set "every book in the context" is the set of decodable stored blobs (the load
reads them all; the resident holder carries the same set, which the resident goldens already rely on).

**Versus L6.1 off (same blocks):** `W_on(n) = W_off(n) \ {id 3}` for n % 100 != 0; `W_on(n) = W_off(n) \ {id 3} ∪
{(3, market, blob) for every book after n}` for n % 100 == 0. Below activation nothing is hashed, as today.

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
* Boot: `running_hash::configure_book_window(db, on)` right after `configure_activation`, before the replay; stored in
  META `state_hash_book_window` (node-local key, never hashed). A change with a stored chain discards it, as an
  activation change does. The digest reads it (serial: META; flush worker: `ChainMemo`), E reads the same row
  (`book_window_full_save_at`), so both sides apply one rule.
* Refuse to start if L6.1 is on and the book mode is not Classic (`TORUS_BOOK_ROWS`): row modes write per-order rows to
  the same CF, which this rule would drop.
* All nodes agree because it is in genesis, like the activation height. When P0.0 lands it becomes
  `activations.book_window_commit` (height = the hash activation).

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
