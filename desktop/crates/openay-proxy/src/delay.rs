//! Deadline-ordered delay scheduler used by the [`jitter30`](crate::Profile::Jitter30)
//! profile.
//!
//! Items are released strictly in deadline order; ties (same deadline)
//! release in insertion order, which keeps the scheduler deterministic.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use tokio::time::Instant;

/// A scheduled item: deadline first, then insertion sequence for tie
/// stability, then the payload.
#[derive(Debug)]
struct Entry<T> {
    deadline: Instant,
    seq: u64,
    item: T,
}

/// Ordering (and equality, required by `Ord`'s supertraits) is defined over
/// `(deadline, seq)` only, so `T` itself needs no comparison traits.
impl<T> PartialEq for Entry<T> {
    fn eq(&self, other: &Self) -> bool {
        (self.deadline, self.seq) == (other.deadline, other.seq)
    }
}

impl<T> Eq for Entry<T> {}

impl<T> PartialOrd for Entry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Entry<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        // Deadlines are unique enough that seq is only consulted for ties;
        // the payload itself never participates in ordering.
        (self.deadline, self.seq).cmp(&(other.deadline, other.seq))
    }
}

/// A min-heap of items keyed by deadline.
///
/// The proxy polls [`DelayQueue::pop_expired`] whenever the sleep armed on
/// [`DelayQueue::deadline`] wakes up, so no item is released before its
/// deadline and earlier deadlines always go first.
#[derive(Debug)]
pub struct DelayQueue<T> {
    /// BinaryHeap is a max-heap; `Reverse` turns it into a min-heap.
    heap: BinaryHeap<Reverse<Entry<T>>>,
    next_seq: u64,
}

impl<T> Default for DelayQueue<T> {
    fn default() -> Self {
        Self {
            heap: BinaryHeap::new(),
            next_seq: 0,
        }
    }
}

impl<T> DelayQueue<T> {
    /// Create an empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedule `item` for release at `deadline`.
    pub fn push(&mut self, deadline: Instant, item: T) {
        self.next_seq += 1;
        self.heap.push(Reverse(Entry {
            deadline,
            seq: self.next_seq,
            item,
        }));
    }

    /// The earliest scheduled deadline, if any.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.heap.peek().map(|entry| entry.0.deadline)
    }

    /// Remove and return every item whose deadline is `<= now`, in deadline
    /// order (insertion order for ties).
    #[must_use]
    pub fn pop_expired(&mut self, now: Instant) -> Vec<T> {
        let mut expired = Vec::new();
        while let Some(entry) = self.heap.peek() {
            if entry.0.deadline > now {
                break;
            }
            expired.push(self.heap.pop().expect("just peeked").0.item);
        }
        expired
    }

    /// Number of scheduled items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// `true` when nothing is scheduled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn releases_earlier_deadline_first() {
        let mut queue = DelayQueue::new();
        let t0 = tokio::time::Instant::now();
        let ms = Duration::from_millis(1);
        queue.push(t0 + ms * 30, 30u64);
        queue.push(t0 + ms * 10, 10u64);
        queue.push(t0 + ms * 20, 20u64);

        assert_eq!(queue.deadline(), Some(t0 + ms * 10));
        assert_eq!(queue.len(), 3);

        // Nothing before the first deadline; items pop (10, 20, 30) in order.
        assert!(queue.pop_expired(t0).is_empty());
        assert_eq!(queue.pop_expired(t0 + ms * 15), vec![10u64]);
        assert_eq!(queue.pop_expired(t0 + ms * 20), vec![20u64]);
        // Still nothing due before the 30 ms deadline.
        assert!(queue.pop_expired(t0 + ms * 29).is_empty());
        assert_eq!(queue.pop_expired(t0 + ms * 30), vec![30u64]);
        assert!(queue.is_empty());
    }

    #[test]
    fn same_deadline_releases_in_insertion_order() {
        let mut queue = DelayQueue::new();
        let t0 = tokio::time::Instant::now();
        let deadline = t0 + Duration::from_millis(50);
        // Deliberately non-sorted insertion order.
        queue.push(deadline, "second");
        queue.push(deadline, "first");
        queue.push(deadline, "third");
        assert_eq!(
            queue.pop_expired(deadline),
            vec!["second", "first", "third"]
        );
    }

    #[test]
    fn interleaved_deadlines_keep_min_accurate() {
        let mut queue = DelayQueue::new();
        let t0 = tokio::time::Instant::now();
        let ms = Duration::from_millis(1);
        queue.push(t0 + ms * 100, 1u64);
        queue.push(t0 + ms * 20, 2u64);
        assert_eq!(queue.deadline(), Some(t0 + ms * 20));
        queue.push(t0 + ms * 5, 3u64);
        assert_eq!(queue.deadline(), Some(t0 + ms * 5));
        assert_eq!(queue.pop_expired(t0 + ms * 5), vec![3u64]);
        assert_eq!(queue.deadline(), Some(t0 + ms * 20));
    }
}
