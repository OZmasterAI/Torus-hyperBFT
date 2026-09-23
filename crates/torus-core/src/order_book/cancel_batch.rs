//! Bounded cancel-all batching. The book's original loop remains the fallback.

use super::*;

/// A target's position in its untouched queue and in the trader's output list.
#[derive(Clone, Copy)]
struct Target {
    position: usize,
    output: usize,
}

/// Leave the largest target-free interval untouched. Targets before it move
/// to the front; targets after it move to the back. Ties prefer the first gap.
fn end_split(len: usize, targets: &[Target]) -> (usize, usize) {
    let mut split = 0;
    let mut largest_gap = targets[0].position;
    for i in 1..=targets.len() {
        let end = targets.get(i).map_or(len, |target| target.position);
        let gap = end - targets[i - 1].position - 1;
        if gap > largest_gap {
            largest_gap = gap;
            split = i;
        }
    }
    (split, len - largest_gap - targets.len())
}

/// Stable-compacts survivors toward the untouched middle, using only safe
/// swaps. Every swap exchanges a survivor with a cancelled slot. A survivor
/// is read once and moved at most once; no survivor crosses the middle gap.
/// Afterward the first `split` and last `targets.len() - split` slots hold
/// the cancelled values (in unspecified order). No allocation or cloning.
fn compact_to_ends<T>(queue: &mut VecDeque<T>, targets: &[Target], split: usize) {
    if split > 0 {
        let end = targets[split - 1].position + 1;
        let mut write = end;
        let mut target = split;
        for read in (0..end).rev() {
            if target > 0 && read == targets[target - 1].position {
                target -= 1;
            } else {
                write -= 1;
                queue.swap(read, write);
            }
        }
        debug_assert_eq!(write, split);
    }
    if split < targets.len() {
        let start = targets[split].position;
        let mut write = start;
        let mut target = split;
        for read in start..queue.len() {
            if target < targets.len() && read == targets[target].position {
                target += 1;
            } else {
                queue.swap(read, write);
                write += 1;
            }
        }
        debug_assert_eq!(write, queue.len() - (targets.len() - split));
    }
}

/// How one level's position-sorted, distinct targets leave its queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Removal {
    /// `compact_to_ends` with this split, then pop both ends.
    Compact(usize),
    /// `VecDeque::remove` from the highest position down.
    Reverse,
    /// `VecDeque::remove` from the lowest position up.
    Forward,
}

/// A swap moves up to three Order-sized values. Only compact when its
/// movement bound beats either direction of repeated removal. This is a work
/// bound, not a claim about measured CPU time.
fn choose_removal(len: usize, targets: &[Target]) -> Removal {
    let (split, affected_survivors) = end_split(len, targets);
    let forward_shifts = targets.iter().enumerate().fold(0usize, |sum, (i, target)| {
        sum.saturating_add((target.position - i).min(len - 1 - target.position))
    });
    let reverse_shifts = targets
        .iter()
        .enumerate()
        .rev()
        .fold(0usize, |sum, (i, target)| {
            let remaining = len - (targets.len() - 1 - i);
            sum.saturating_add(target.position.min(remaining - 1 - target.position))
        });
    if forward_shifts.min(reverse_shifts) > affected_survivors.saturating_mul(3) {
        Removal::Compact(split)
    } else if reverse_shifts < forward_shifts {
        Removal::Reverse
    } else {
        Removal::Forward
    }
}

/// Moves every target out of `queue` into `cancelled[target.output]`,
/// keeping survivors in FIFO order. Compaction leaves the cancelled values
/// at the ends in unspecified order; `output_of` maps each popped order id
/// back to its slot. No allocation, no per-survivor lookups.
fn remove_targets(
    queue: &mut VecDeque<Order>,
    targets: &[Target],
    cancelled: &mut [Option<Order>],
    output_of: impl Fn(OrderId) -> usize,
) {
    match choose_removal(queue.len(), targets) {
        Removal::Compact(split) => {
            compact_to_ends(queue, targets, split);
            for front in [true, false] {
                let count = if front { split } else { targets.len() - split };
                for _ in 0..count {
                    let order = if front {
                        queue.pop_front()
                    } else {
                        queue.pop_back()
                    }
                    .expect("compaction moved target to endpoint");
                    let slot = output_of(order.id);
                    cancelled[slot] = Some(order);
                }
            }
        }
        Removal::Reverse => {
            for target in targets.iter().rev() {
                cancelled[target.output] = queue.remove(target.position);
            }
        }
        Removal::Forward => {
            for (removed, target) in targets.iter().enumerate() {
                cancelled[target.output] = queue.remove(target.position - removed);
            }
        }
    }
}

/// Binary-search a sorted `(id, output)` table for a cancelled order's slot.
fn output_slot(by_id: &[(OrderId, usize)], id: OrderId) -> usize {
    let slot = by_id
        .binary_search_by_key(&id, |&(id, _)| id)
        .expect("endpoint is a cancellation target");
    by_id[slot].1
}

/// `cancel_all_many`'s read-only plan over the untouched book.
struct ManyPlan {
    /// Per sender: its slice of the output slots — `None` for a repeat or a
    /// sender without a `trader_orders` entry (sequential early return).
    ranges: Vec<Option<std::ops::Range<usize>>>,
    /// Targets sorted by level, then queue position.
    targets: Vec<Target>,
    /// `(side_tag, price, end)`: level groups as exclusive ends into `targets`.
    levels: Vec<(u8, FixedPoint, usize)>,
    /// Sorted `(order id, output slot)`.
    by_id: Vec<(OrderId, usize)>,
}

impl OrderBook {
    /// Cheap, conservative admission sample. Do not allocate a batch plan for
    /// shallow or scattered targets just because unrelated levels are deep.
    /// At most 32 index reads and one level lookup; no queue-position probes.
    /// A shallow first target intentionally leaves even a deep tail on the
    /// original path, rather than searching the whole trader list to qualify.
    fn cancel_batch_has_concentrated_prefix(&self, order_ids: &[OrderId]) -> bool {
        let Some(first) = order_ids.first().and_then(|id| self.order_index.get(id)) else {
            return false;
        };
        let book = match first.side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        if !book
            .get(&first.price)
            .is_some_and(|queue| queue.len() >= 1_024)
        {
            return false;
        }
        let mut same_level = 1;
        for id in order_ids.iter().take(32).skip(1) {
            let Some(loc) = self.order_index.get(id) else {
                return false;
            };
            if loc.side == first.side && loc.price == first.price {
                same_level += 1;
                if same_level == 4 {
                    return true;
                }
            }
        }
        false
    }

    /// Return `None` without changing the book when batching is ineligible.
    /// All planning/storage is bounded by the ordinary 200-order trader cap;
    /// recovered above-limit states keep the original loop too.
    pub(super) fn try_cancel_all_batch(&mut self, order_ids: &[OrderId]) -> Option<Vec<Order>> {
        if !(32..=MAX_ORDERS_PER_TRADER_PER_MARKET).contains(&order_ids.len())
            || self.order_index.len() < 1_024
            || !self.cancel_batch_has_concentrated_prefix(order_ids)
        {
            return None;
        }

        // Read-only preflight makes malformed/stale indexes take the exact old
        // path, including its partial-error and debug-assert behavior.
        let mut output_by_id: Vec<_> = order_ids
            .iter()
            .copied()
            .enumerate()
            .map(|(output, id)| (id, output))
            .collect();
        output_by_id.sort_unstable_by_key(|&(id, _)| id);
        if output_by_id.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return None;
        }
        let mut levels: BTreeMap<(u8, FixedPoint), Vec<Target>> = BTreeMap::new();
        for (output, id) in order_ids.iter().enumerate() {
            let loc = self.order_index.get(id)?;
            let queue = match loc.side {
                Side::Buy => self.bids.get(&loc.price),
                Side::Sell => self.asks.get(&loc.price),
            }?;
            let seq = self.order_seq.get(id)?;
            let position = queue
                .partition_point(|order| self.order_seq.get(&order.id).is_some_and(|s| s < seq));
            if queue.get(position).is_none_or(|order| order.id != *id) {
                return None;
            }
            levels
                .entry((crate::book_rows::side_tag(loc.side), loc.price))
                .or_default()
                .push(Target { position, output });
        }
        for (&(tag, price), targets) in &mut levels {
            // Reordering mutation before bookkeeping must not change where
            // epoch overflow panics in debug builds. Fall back even in release
            // so the existing wrapping arithmetic is left exactly as before.
            if self.level_hash_cache.is_some()
                && self
                    .level_epoch
                    .get(&(tag, price.raw()))
                    .copied()
                    .unwrap_or(0)
                    .checked_add(targets.len() as u64)
                    .is_none()
            {
                return None;
            }
            targets.sort_unstable_by_key(|target| target.position);
        }

        let mut cancelled = vec![None; order_ids.len()];
        for ((tag, price), targets) in levels {
            let book = if tag == crate::book_rows::side_tag(Side::Buy) {
                &mut self.bids
            } else {
                &mut self.asks
            };
            let queue = book.get_mut(&price).expect("preflight found level");
            remove_targets(queue, &targets, &mut cancelled, |id| {
                output_slot(&output_by_id, id)
            });
            if queue.is_empty() {
                book.remove(&price);
            }
        }

        // Replay all effects in the original trader-index order, independent
        // of grouping, queue FIFO, or the compaction's cancelled-slot order.
        let cache_on = self.level_hash_cache.is_some();
        let chunked_on = self.level_hash_chunked;
        let cancelled: Vec<Order> = cancelled
            .into_iter()
            .map(|order| order.expect("preflight located every target"))
            .collect();
        for order in &cancelled {
            let loc = self
                .order_index
                .remove(&order.id)
                .expect("preflight found index");
            let seq = self.order_seq.remove(&order.id);
            let tag = crate::book_rows::side_tag(loc.side);
            self.row_journal.insert(order.id);
            self.level_journal.insert((tag, loc.price.raw()));
            Self::bump_level_epoch(cache_on, &mut self.level_epoch, tag, loc.price.raw());
            Self::mark_chunk_dirty(
                chunked_on,
                &mut self.dirty_chunks,
                tag,
                loc.price.raw(),
                seq,
            );
        }
        Some(cancelled)
    }

    /// Cancel-all for a run of senders, state-equivalent to
    /// `senders.iter().map(|s| self.cancel_all(*s, None)).collect()`: the
    /// same per-sender results (a repeated sender gets the empty result its
    /// second sequential call gets), the same survivor FIFO order, journals,
    /// epochs, dirty chunks and stops. Each touched level is compacted ONCE
    /// for the union of all senders' targets instead of once per sender.
    ///
    /// Falls back to the sequential loop — before any mutation — when fewer
    /// than two senders own orders or any index is stale/shared, so unusual
    /// states keep their exact sequential behavior.
    pub fn cancel_all_many(&mut self, senders: &[Address]) -> Vec<Vec<Order>> {
        match self.plan_cancel_all_many(senders) {
            Some(plan) => self.apply_cancel_all_many(senders, plan),
            None => senders.iter().map(|s| self.cancel_all(*s, None)).collect(),
        }
    }

    /// Read-only. Planning memory scales with the run's targets, never with
    /// queue depth; each target costs the same seq binary search the
    /// sequential path pays, and survivors are never looked up.
    fn plan_cancel_all_many(&self, senders: &[Address]) -> Option<ManyPlan> {
        // Only a sender's first occurrence can find a trader_orders entry.
        let mut first = vec![false; senders.len()];
        let mut by_sender: Vec<(Address, usize)> = senders
            .iter()
            .copied()
            .enumerate()
            .map(|(k, s)| (s, k))
            .collect();
        by_sender.sort_unstable();
        for (i, &(sender, k)) in by_sender.iter().enumerate() {
            first[k] = i == 0 || by_sender[i - 1].0 != sender;
        }
        let mut ranges = vec![None; senders.len()];
        let mut total = 0usize;
        let mut owners = 0usize;
        for (k, sender) in senders.iter().enumerate() {
            if let Some(ids) = self.trader_orders.get(sender).filter(|_| first[k]) {
                ranges[k] = Some(total..total + ids.len());
                total += ids.len();
                owners += usize::from(!ids.is_empty());
            }
        }
        if owners < 2 {
            return None;
        }

        let mut keyed: Vec<(u8, FixedPoint, Target)> = Vec::with_capacity(total);
        let mut by_id: Vec<(OrderId, usize)> = Vec::with_capacity(total);
        for (sender, range) in senders.iter().zip(&ranges) {
            let Some(range) = range else { continue };
            for (output, id) in (range.start..).zip(&self.trader_orders[sender]) {
                let loc = self.order_index.get(id)?;
                let queue = match loc.side {
                    Side::Buy => self.bids.get(&loc.price),
                    Side::Sell => self.asks.get(&loc.price),
                }?;
                let seq = self.order_seq.get(id)?;
                let position = queue.partition_point(|order| {
                    self.order_seq.get(&order.id).is_some_and(|s| s < seq)
                });
                if queue.get(position).is_none_or(|order| order.id != *id) {
                    return None;
                }
                keyed.push((
                    crate::book_rows::side_tag(loc.side),
                    loc.price,
                    Target { position, output },
                ));
                by_id.push((*id, output));
            }
        }
        by_id.sort_unstable_by_key(|&(id, _)| id);
        if by_id.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return None;
        }
        keyed.sort_unstable_by_key(|&(tag, price, target)| (tag, price, target.position));

        let mut levels = Vec::new();
        let mut start = 0;
        for i in 0..keyed.len() {
            let (tag, price, _) = keyed[i];
            if keyed
                .get(i + 1)
                .is_some_and(|next| (next.0, next.1) == (tag, price))
            {
                continue;
            }
            // Epoch overflow must hit the sequential path's exact
            // panic/wrap point, so leave it to that path.
            if self.level_hash_cache.is_some()
                && self
                    .level_epoch
                    .get(&(tag, price.raw()))
                    .copied()
                    .unwrap_or(0)
                    .checked_add((i + 1 - start) as u64)
                    .is_none()
            {
                return None;
            }
            levels.push((tag, price, i + 1));
            start = i + 1;
        }
        Some(ManyPlan {
            ranges,
            targets: keyed.into_iter().map(|(_, _, target)| target).collect(),
            levels,
            by_id,
        })
    }

    fn apply_cancel_all_many(&mut self, senders: &[Address], plan: ManyPlan) -> Vec<Vec<Order>> {
        let ManyPlan {
            ranges,
            targets,
            levels,
            by_id,
        } = plan;
        let mut cancelled: Vec<Option<Order>> = vec![None; by_id.len()];
        let mut start = 0;
        for (tag, price, end) in levels {
            let book = if tag == crate::book_rows::side_tag(Side::Buy) {
                &mut self.bids
            } else {
                &mut self.asks
            };
            let queue = book.get_mut(&price).expect("plan found level");
            remove_targets(queue, &targets[start..end], &mut cancelled, |id| {
                output_slot(&by_id, id)
            });
            if queue.is_empty() {
                book.remove(&price);
            }
            start = end;
        }

        // Per-order effects in the sequential order: sender, then its
        // trader_orders list — independent of grouping and compaction.
        let cache_on = self.level_hash_cache.is_some();
        let chunked_on = self.level_hash_chunked;
        let mut cancelled = cancelled
            .into_iter()
            .map(|order| order.expect("plan located every target"));
        let mut stop_owners = Vec::new();
        let mut out = Vec::with_capacity(senders.len());
        for (sender, range) in senders.iter().zip(ranges) {
            let Some(range) = range else {
                out.push(Vec::new());
                continue;
            };
            self.trader_orders.remove(sender);
            stop_owners.push(*sender);
            let orders: Vec<Order> = cancelled.by_ref().take(range.len()).collect();
            for order in &orders {
                let loc = self
                    .order_index
                    .remove(&order.id)
                    .expect("plan found index");
                let seq = self.order_seq.remove(&order.id);
                let tag = crate::book_rows::side_tag(loc.side);
                self.row_journal.insert(order.id);
                self.level_journal.insert((tag, loc.price.raw()));
                Self::bump_level_epoch(cache_on, &mut self.level_epoch, tag, loc.price.raw());
                Self::mark_chunk_dirty(
                    chunked_on,
                    &mut self.dirty_chunks,
                    tag,
                    loc.price.raw(),
                    seq,
                );
            }
            out.push(orders);
        }
        // Each owner's `retain` in the sequential loop, as one stable pass.
        stop_owners.sort_unstable();
        if !stop_owners.is_empty() {
            self.pending_stops
                .retain(|stop| stop_owners.binary_search(&stop.trader).is_err());
        }
        out
    }
}

#[cfg(test)]
mod many_tests;

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }
    pub(super) fn fp(n: i64) -> FixedPoint {
        FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
    }

    pub(super) fn order(id: u64, trader: Address, side: Side, price: i64) -> Order {
        Order {
            id: id as OrderId,
            trader,
            side,
            price: fp(price),
            remaining_qty: fp(3),
            original_qty: fp(5),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            timestamp: id * 7,
            reduce_only: id % 2 == 0,
            client_order_id: Some(id + 11),
        }
    }

    /// Exhaust every target subset and every physical deque wrap for small
    /// queues. The independent oracle is a simple stable filter of integers.
    #[test]
    fn end_compaction_exhaustive_wrapped_subsets() {
        for len in 1..=9usize {
            for mask in 1..(1usize << len) {
                let targets: Vec<_> = (0..len)
                    .filter(|i| mask & (1 << i) != 0)
                    .map(|position| Target {
                        position,
                        output: position,
                    })
                    .collect();
                let survivors: Vec<_> = (0..len).filter(|i| mask & (1 << i) == 0).collect();
                let removed: Vec<_> = targets.iter().map(|t| t.position).collect();
                for wrap in 0..len {
                    let mut queue: VecDeque<_> = (0..len).collect();
                    queue.rotate_left(wrap);
                    for (i, value) in queue.iter_mut().enumerate() {
                        *value = i;
                    }
                    let (split, _) = end_split(len, &targets);
                    compact_to_ends(&mut queue, &targets, split);
                    let mut got: Vec<_> = (0..split).map(|_| queue.pop_front().unwrap()).collect();
                    got.extend((split..targets.len()).map(|_| queue.pop_back().unwrap()));
                    got.sort_unstable();
                    assert_eq!(got, removed, "len={len} mask={mask} wrap={wrap}");
                    assert_eq!(
                        queue.into_iter().collect::<Vec<_>>(),
                        survivors,
                        "len={len} mask={mask} wrap={wrap}"
                    );
                }
            }
        }
    }

    /// Two near-end groups must leave the deep middle untouched: compaction
    /// work depends on the affected ends, rather than the whole level depth.
    #[test]
    fn end_compaction_skips_the_largest_middle_gap() {
        let targets = [
            Target {
                position: 2,
                output: 0,
            },
            Target {
                position: 3,
                output: 1,
            },
            Target {
                position: 9996,
                output: 2,
            },
            Target {
                position: 9997,
                output: 3,
            },
        ];
        let (split, moved) = end_split(10_000, &targets);
        assert_eq!((split, moved), (2, 4));
        let mut queue: VecDeque<_> = (0..10_000).collect();
        compact_to_ends(&mut queue, &targets, split);
        assert!(queue
            .iter()
            .enumerate()
            .skip(4)
            .take(9992)
            .all(|(i, v)| i == *v));
    }

    fn fixture(depth: usize, per_level: usize, shape: usize, mode: usize) -> OrderBook {
        let mut book = OrderBook::new(1, fp(1), fp(1));
        book.set_next_seq(1_000_000);
        book.set_next_order_id(1_000_000);
        if mode == 2 {
            book.ensure_level_hash_cache(1 << 20);
        }
        if mode == 3 {
            book.set_level_hash_chunked(true);
        }
        for level in 0..2 {
            let side = if level == 0 { Side::Buy } else { Side::Sell };
            let price = if level == 0 { 100 } else { 110 };
            let positions: BTreeSet<_> = (0..per_level)
                .map(|i| match shape {
                    0 => i * depth / per_level,
                    1 => depth / 2 + i,
                    2 => i,
                    3 => depth - per_level + i,
                    _ => {
                        if i < per_level / 2 {
                            i * 2
                        } else {
                            depth - (per_level - i) * 2
                        }
                    }
                })
                .collect();
            for position in 0..depth {
                let id = (level * depth + position + 1) as u64;
                let trader = if positions.contains(&position) {
                    addr(1)
                } else {
                    addr(2)
                };
                book.insert_loaded_order(order(id, trader, side, price), id + 100);
            }
        }
        // Make receipt order neither level order nor FIFO order. Recovery may
        // restore trader order lists differently; cancellation must honor it.
        book.trader_orders.get_mut(&addr(1)).unwrap().reverse();
        book.pending_stops.push(StopOrder {
            id: 900_000,
            trader: addr(1),
            market_id: 1,
            side: Side::Buy,
            trigger_price: fp(200),
            limit_price: None,
            quantity: fp(1),
            time_in_force: TimeInForce::GTC,
            timestamp: 0,
            reduce_only: false,
            client_order_id: None,
        });
        book.pending_stops.push(StopOrder {
            id: 900_001,
            trader: addr(3),
            market_id: 1,
            side: Side::Sell,
            trigger_price: fp(50),
            limit_price: None,
            quantity: fp(1),
            time_in_force: TimeInForce::GTC,
            timestamp: 0,
            reduce_only: false,
            client_order_id: None,
        });
        // Prime persistence state and caches before the first cancellation.
        book.full_row_ops();
        book.full_level_ops();
        let keys: Vec<_> = book.level_exists.iter().copied().collect();
        book.level_journal.extend(keys);
        book.take_level_ops();
        book
    }

    pub(super) fn assert_same(a: &mut OrderBook, b: &mut OrderBook) {
        assert_eq!(borsh::to_vec(a).unwrap(), borsh::to_vec(b).unwrap());
        assert_eq!(a.next_seq, b.next_seq);
        assert_eq!(a.order_seq, b.order_seq);
        assert_eq!(a.trader_orders, b.trader_orders);
        let index = |book: &OrderBook| -> BTreeMap<_, _> {
            book.order_index
                .iter()
                .map(|(&id, loc)| (id, (crate::book_rows::side_tag(loc.side), loc.price)))
                .collect()
        };
        assert_eq!(index(a), index(b));
        assert_eq!(a.row_journal, b.row_journal);
        assert_eq!(a.level_journal, b.level_journal);
        assert_eq!(a.level_epoch, b.level_epoch);
        assert_eq!(a.dirty_chunks, b.dirty_chunks);
        assert_eq!(a.take_row_ops(), b.take_row_ops());
        assert_eq!(a.take_level_ops(), b.take_level_ops());
        assert_eq!(a.row_exists, b.row_exists);
        assert_eq!(a.level_exists, b.level_exists);
        assert_eq!(a.level_chunks, b.level_chunks);
        // Recompute from scratch as well as comparing incremental drains.
        assert_eq!(a.full_level_ops(), b.full_level_ops());
    }

    #[test]
    fn cancel_batch_matches_original_across_shapes_and_persistence_modes() {
        for mode in 0..=3 {
            for shape in 0..5 {
                let mut a = fixture(2048, 40, shape, mode);
                let mut b = fixture(2048, 40, shape, mode);
                let expected = a.trader_orders[&addr(1)].clone();
                assert!(a.cancel_batch_has_concentrated_prefix(&expected));
                let got = a.cancel_all(addr(1), Some(999));
                assert_eq!(got, b.cancel_all_original(addr(1), Some(999)));
                assert_eq!(got.iter().map(|o| o.id).collect::<Vec<_>>(), expected);
                assert_same(&mut a, &mut b);
                // Subsequent append and cancellation exercise invalidated hash
                // prefixes, chunk boundaries and the preserved seq allocator.
                for book in [&mut a, &mut b] {
                    book.insert_order(order(1_000_001, addr(1), Side::Buy, 100));
                    book.insert_order(order(1_000_002, addr(1), Side::Sell, 110));
                }
                assert_same(&mut a, &mut b);
                assert_eq!(
                    a.cancel_all(addr(1), None),
                    b.cancel_all_original(addr(1), None)
                );
                assert_same(&mut a, &mut b);
            }
        }
    }

    #[test]
    fn cancel_batch_preserves_modified_priority_and_partial_fills() {
        let mut a = fixture(2048, 40, 0, 3);
        let mut b = fixture(2048, 40, 0, 3);
        for book in [&mut a, &mut b] {
            let ids = book.trader_orders[&addr(1)].clone();
            book.modify_order(ids[0], None, Some(fp(2))).unwrap();
            book.modify_order(ids[1], None, Some(fp(7))).unwrap();
            book.modify_order(ids[2], Some(fp(111)), None).unwrap();
            let result = book.place_order(
                PlaceOrderParams {
                    market_id: 1,
                    is_buy: false,
                    price: fp(100),
                    quantity: fp(5),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::IOC,
                    reduce_only: false,
                    client_order_id: None,
                },
                addr(9),
                99,
            );
            assert_eq!(result.fills.len(), 2);
        }
        assert_eq!(
            a.cancel_all(addr(1), None),
            b.cancel_all_original(addr(1), None)
        );
        assert_same(&mut a, &mut b);
    }

    #[test]
    fn cancel_batch_empties_levels_and_preserves_later_recreation() {
        let mut a = fixture(1024, 40, 1, 2);
        let mut b = fixture(1024, 40, 1, 2);
        for book in [&mut a, &mut b] {
            for i in 0..32 {
                book.insert_order(order(1_000_100 + i, addr(1), Side::Buy, 99));
            }
        }
        assert_eq!(
            a.cancel_all(addr(1), None),
            b.cancel_all_original(addr(1), None)
        );
        assert!(!a.bids.contains_key(&fp(99)));
        assert_same(&mut a, &mut b);
        for book in [&mut a, &mut b] {
            book.insert_order(order(1_000_200, addr(1), Side::Buy, 99));
        }
        assert_same(&mut a, &mut b);
    }

    #[test]
    fn cancel_batch_falls_back_for_small_large_and_stale_indexes() {
        for case in 0..7 {
            let per_level = match case {
                0 => 8,
                1 => 101,
                _ => 40,
            };
            let mut a = fixture(1024, per_level, 0, 0);
            let mut b = fixture(1024, per_level, 0, 0);
            for book in [&mut a, &mut b] {
                let id = book.trader_orders[&addr(1)][3];
                match case {
                    2 => {
                        book.order_index.remove(&id);
                    }
                    3 => {
                        book.order_seq.remove(&id);
                    }
                    4 => {
                        book.order_index.get_mut(&id).unwrap().price = fp(999);
                    }
                    5 => {
                        book.trader_orders.get_mut(&addr(1)).unwrap().push(id);
                    }
                    6 => {
                        book.trader_orders
                            .get_mut(&addr(1))
                            .unwrap()
                            .insert(0, 999_999);
                    }
                    _ => {}
                }
                let ids = book.trader_orders[&addr(1)].clone();
                assert!(book.try_cancel_all_batch(&ids).is_none());
            }
            assert_eq!(
                a.cancel_all(addr(1), None),
                b.cancel_all_original(addr(1), None)
            );
            assert_same(&mut a, &mut b);
        }
    }

    #[test]
    fn cancel_batch_rejects_scattered_levels_before_planning() {
        fn scattered(depth: usize) -> OrderBook {
            let mut book = OrderBook::new(1, fp(1), fp(1));
            book.set_next_seq(1_000_000);
            for level in 0..32 {
                for position in 0..depth {
                    let id = (level * depth + position + 1) as u64;
                    let trader = if position == depth / 2 {
                        addr(1)
                    } else {
                        addr(2)
                    };
                    book.insert_loaded_order(order(id, trader, Side::Buy, 100 + level as i64), id);
                }
            }
            // Unrelated depth must not qualify singleton target levels.
            for i in 0..1024u64 {
                let id = 100_000 + i;
                book.insert_loaded_order(order(id, addr(2), Side::Buy, 99), id);
            }
            book
        }
        for depth in [1, 32, 1024] {
            let mut a = scattered(depth);
            let mut b = scattered(depth);
            let ids = a.trader_orders[&addr(1)].clone();
            assert_eq!(ids.len(), 32);
            assert!(a.order_count() >= 1024);
            assert!(!a.cancel_batch_has_concentrated_prefix(&ids));
            assert!(a.try_cancel_all_batch(&ids).is_none());
            assert_same(&mut a, &mut b);
            assert_eq!(
                a.cancel_all(addr(1), None),
                b.cancel_all_original(addr(1), None)
            );
            assert_same(&mut a, &mut b);
        }
    }

    #[test]
    fn cancel_batch_shallow_prefix_leaves_deep_tail_on_original_path() {
        let mut a = fixture(1024, 40, 1, 3);
        let mut b = fixture(1024, 40, 1, 3);
        for book in [&mut a, &mut b] {
            book.insert_order(order(1_000_100, addr(1), Side::Buy, 99));
            book.trader_orders
                .get_mut(&addr(1))
                .unwrap()
                .rotate_right(1);
        }
        let ids = a.trader_orders[&addr(1)].clone();
        assert!(!a.cancel_batch_has_concentrated_prefix(&ids));
        assert!(a.try_cancel_all_batch(&ids).is_none());
        assert_same(&mut a, &mut b);
        assert_eq!(
            a.cancel_all(addr(1), None),
            b.cancel_all_original(addr(1), None)
        );
        assert_same(&mut a, &mut b);
    }

    #[test]
    fn cancel_batch_preserves_stop_only_early_return() {
        let mut a = fixture(1024, 40, 0, 0);
        let mut b = fixture(1024, 40, 0, 0);
        assert_eq!(
            a.cancel_all(addr(3), None),
            b.cancel_all_original(addr(3), None)
        );
        assert_eq!(a.pending_stop_count(), 2);
        assert_same(&mut a, &mut b);
    }

    #[test]
    fn cancel_batch_preserves_epoch_overflow_and_partial_state() {
        let mut a = fixture(1024, 40, 0, 2);
        let mut b = fixture(1024, 40, 0, 2);
        for book in [&mut a, &mut b] {
            book.level_epoch.insert(
                (crate::book_rows::SIDE_TAG_ASK, fp(110).raw()),
                u64::MAX - 1,
            );
            let ids = book.trader_orders[&addr(1)].clone();
            assert!(book.try_cancel_all_batch(&ids).is_none());
        }
        let got =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.cancel_all(addr(1), None)));
        let want = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            b.cancel_all_original(addr(1), None)
        }));
        assert_eq!(got.is_err(), want.is_err());
        assert_eq!(got.is_err(), cfg!(debug_assertions));
        if let (Ok(got), Ok(want)) = (got, want) {
            assert_eq!(got, want);
        }
        assert_same(&mut a, &mut b);
    }
}

// Frozen080c4fa oracle: keep the old loop independent of the batch helpers.
#[cfg(test)]
impl OrderBook {
    fn cancel_all_original(&mut self, trader: Address, _market_id: Option<MarketId>) -> Vec<Order> {
        let order_ids = match self.trader_orders.remove(&trader) {
            Some(ids) => ids,
            None => return vec![],
        };

        let mut cancelled = Vec::with_capacity(order_ids.len());
        let cache_on = self.level_hash_cache.is_some();
        let chunked_on = self.level_hash_chunked;
        for order_id in order_ids {
            if let Some(loc) = self.order_index.remove(&order_id) {
                let book = match loc.side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                if let Some(queue) = book.get_mut(&loc.price) {
                    if let Some(pos) = Self::queue_position(queue, &self.order_seq, order_id) {
                        cancelled.push(queue.remove(pos).unwrap());
                        let seq = self.order_seq.remove(&order_id);
                        self.row_journal.insert(order_id);
                        self.level_journal
                            .insert((crate::book_rows::side_tag(loc.side), loc.price.raw()));
                        // L3: removal invalidates the cached level-hash prefix.
                        Self::bump_level_epoch(
                            cache_on,
                            &mut self.level_epoch,
                            crate::book_rows::side_tag(loc.side),
                            loc.price.raw(),
                        );
                        // Mode 3: the removed frame's chunk is dirty.
                        Self::mark_chunk_dirty(
                            chunked_on,
                            &mut self.dirty_chunks,
                            crate::book_rows::side_tag(loc.side),
                            loc.price.raw(),
                            seq,
                        );
                    }
                    if queue.is_empty() {
                        book.remove(&loc.price);
                    }
                }
            }
        }

        // Also remove pending stops for this trader
        self.pending_stops.retain(|s| s.trader != trader);

        cancelled
    }
}
