//! OpenAY Mic jitter buffer.
//!
//! A lock-free single-producer/single-consumer ring buffer of `f32` samples.
//! The producer is the network receive task (pushing decoded audio frames),
//! the consumer is the audio output callback (Popping samples for playback).
//!
//! Design notes:
//!
//! - Capacity is rounded up to the next power of two (minimum 1024) so index
//!   arithmetic is a mask, never a division.
//! - All shared state is a pair of monotonic `usize` indices plus two u64
//!   counters, held in `std::sync::atomic` with `Acquire`/`Release` ordering,
//!   plus the ring storage behind an [`UnsafeCell`] (see [`JitterBuffer`] for
//!   the soundness argument). There are no locks and no allocations after
//!   construction.
//! - Occupancy is derived from the two monotonic indices (`head - tail`),
//!   never from a third shared counter: each thread writes **only its own**
//!   index, so no update can ever be lost to a read-modify-write race
//!   (a shared "length" counter that both sides store to would suffer lost
//!   updates when a load-store pair interleaves, which is why it is avoided).
//! - `push` is all-or-nothing: if the whole block does not fit, *nothing* is
//!   written and the block counts as an overrun. Dropping a whole frame is
//!   better than tearing it across the ring boundary, which would glitch the
//!   audio with half a frame of garbage.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Nominal target prebuffer in milliseconds (plan: 5–15 ms window).
pub const TARGET_PREBUFFER_MS: f32 = 10.0;
/// Minimum prebuffer in milliseconds.
pub const MIN_PREBUFFER_MS: f32 = 5.0;
/// Maximum prebuffer in milliseconds (Phase 6 may raise this ceiling).
pub const MAX_PREBUFFER_MS: f32 = 20.0;

/// Smallest power of two not smaller than `x`, clamped to at least 1024.
fn next_pow2(x: usize) -> usize {
    const MIN: usize = 1024;
    if x <= MIN {
        return MIN;
    }
    let mut v = x - 1;
    v |= v >> 1;
    v |= v >> 2;
    v |= v >> 4;
    v |= v >> 8;
    v |= v >> 16;
    v |= v >> 32;
    v + 1
}

/// A lock-free single-producer/single-consumer ring of `f32` samples.
///
/// `head` is the write index (producer only) and `tail` the read index
/// (consumer only); both advance **monotonically** — they are masked only on
/// access, never stored masked. This removes the classic full-vs-empty
/// ambiguity (a full ring is `head - tail == capacity`, an empty one is
/// `head - tail == 0`) and, because each thread writes only its own index,
/// no update can be lost to an interleaved read-modify-write.
///
/// # Soundness of the interior mutability
///
/// The ring storage is an [`UnsafeCell`]; access goes through raw pointers
/// only, under this protocol:
///
/// - There is exactly **one producer thread** (calls [`push`](Self::push))
///   and exactly **one consumer thread** (calls [`pop`](Self::pop)).
/// - Producer: `tail.load(Acquire)` (synchronizes with the consumer's
///   `tail.store(Release)`, which publishes that the cells below the new tail
///   were fully read and are free to reuse), free-space check, write cells
///   `[head, head + n)`, then `head.store(head + n, Release)`.
/// - Consumer: `head.load(Acquire)` (synchronizes with the producer's
///   `head.store(Release)`, publishing the data writes), read cells
///   `[tail, tail + take)`, then `tail.store(tail + take, Release)`.
/// - The producer writes only cells below `tail + capacity` (its free-space
///   check), all of which the consumer finished reading before publishing
///   that `tail`; the consumer reads only cells below the `head` it loaded,
///   which the producer finished writing before publishing it. The write
///   region and the read region are therefore always disjoint, so the
///   `&mut [f32]` / `&[f32]` slices the two threads create never alias the
///   same cell.
/// - [`reset`](Self::reset) must not run concurrently with `push`/`pop`.
pub struct JitterBuffer {
    /// Ring storage; only the data pointer is used, never resized or
    /// otherwise mutated after construction.
    buf: UnsafeCell<Vec<f32>>,
    /// `capacity - 1`, the ring mask.
    mask: usize,
    /// Producer's monotonic write index; written only by the producer.
    head: AtomicUsize,
    /// Consumer's monotonic read index; written only by the consumer.
    tail: AtomicUsize,
    overruns: AtomicU64,
    underruns: AtomicU64,
}

// SAFETY: the type is `Send` via `Vec<f32>: Send`. `Sync` is justified by the
// SPSC protocol documented on the struct: the atomic `len` handshake
// guarantees that concurrent `push` and `pop` only ever touch disjoint cells,
// and the counters are atomic. Multiple simultaneous producers or consumers
// would violate the contract (the API doc names the single-thread
// restrictions); `Arc<JitterBuffer>` between one network task and one audio
// callback is the intended, safe use.
unsafe impl Sync for JitterBuffer {}

impl JitterBuffer {
    /// Create a buffer whose capacity is `capacity_samples` rounded up to the
    /// next power of two (minimum 1024 samples). Allocates exactly once.
    pub fn new(capacity_samples: usize) -> Self {
        let cap = next_pow2(capacity_samples);
        JitterBuffer {
            buf: UnsafeCell::new(vec![0.0f32; cap]),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            overruns: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
        }
    }

    /// The actual (power-of-two) capacity in samples.
    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Push a whole block of samples.
    ///
    /// If the entire block fits in the free space it is written (possibly
    /// wrapping around the ring) and `samples.len()` is returned. If it does
    /// not fit, **nothing** is written, the overrun counter is incremented,
    /// and `0` is returned: a frame is either stored whole or dropped whole,
    /// never torn.
    pub fn push(&self, samples: &[f32]) -> usize {
        let n = samples.len();
        if n == 0 {
            return 0;
        }
        // Acquire syncs with the consumer's Release `tail.store`: everything
        // below the loaded tail was fully read, so the cells we are about to
        // write (all of which are below `tail + capacity`) are provably not
        // being read concurrently.
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Relaxed);
        if n > self.capacity() - (head - tail) {
            self.overruns.fetch_add(1, Ordering::Relaxed);
            return 0;
        }
        let start = head & self.mask;
        let first = (self.capacity() - start).min(n);
        // SAFETY: SPSC protocol — the producer is the only thread writing the
        // ring, and every cell in `[start, start + n)` was released by the
        // consumer (its reads were published by the `tail.store(Release)`
        // that the `tail.load(Acquire)` above synchronized with), so the
        // write slices never alias cells the consumer is concurrently
        // reading.
        unsafe {
            let ptr = (*self.buf.get()).as_mut_ptr();
            std::slice::from_raw_parts_mut(ptr.add(start), first)
                .copy_from_slice(&samples[..first]);
            if first < n {
                std::slice::from_raw_parts_mut(ptr, n - first).copy_from_slice(&samples[first..]);
            }
        }
        // Release publishes both the data writes and the new head position.
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }

    /// Pop up to `out.len()` samples into `out` (contiguous, linear copy —
    /// any internal wraparound is hidden from the caller).
    ///
    /// Returns the number of samples actually written (0 when empty).
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let n = out.len();
        if n == 0 {
            return 0;
        }
        // Acquire syncs with the producer's Release `head.store`, so the
        // samples we are about to read were fully written before being
        // published.
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Relaxed);
        let take = n.min(head - tail);
        if take == 0 {
            return 0;
        }
        let start = tail & self.mask;
        let first = (self.capacity() - start).min(take);
        // SAFETY: SPSC protocol — the `head.load(Acquire)` above synchronized
        // with the producer's `head.store(Release)`, so the samples in
        // `[start, start + take)` were fully written and published before the
        // consumer reads them; the consumer is the only thread reading them,
        // so the read slices never alias cells the producer is concurrently
        // writing.
        unsafe {
            let ptr = (*self.buf.get()).as_mut_ptr();
            out[..first].copy_from_slice(std::slice::from_raw_parts(ptr.add(start), first));
            if first < take {
                out[first..take].copy_from_slice(std::slice::from_raw_parts(ptr, take - first));
            }
        }
        // Release publishes the cells as read (and hence reusable by the
        // producer's next `tail.load(Acquire)`).
        self.tail.store(tail.wrapping_add(take), Ordering::Release);
        take
    }

    /// Samples currently buffered and ready to pop.
    pub fn available(&self) -> usize {
        self.head
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail.load(Ordering::Relaxed))
    }

    /// Free samples (capacity minus available).
    pub fn free(&self) -> usize {
        self.capacity() - self.available()
    }

    /// Fraction of the capacity currently occupied, in `0.0..=1.0`.
    pub fn fill_ratio(&self) -> f32 {
        self.available() as f32 / self.capacity() as f32
    }

    /// Number of whole blocks dropped by [`push`](Self::push) for lack of
    /// space.
    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// Number of consumer underruns (times a read callback had to zero-fill).
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Count one underrun. Called by the consumer when it had to emit silence
    /// because the buffer ran dry.
    pub fn note_underrun(&self) {
        self.underruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Reset to empty; counters are zeroed too.
    ///
    /// Must not be called concurrently with `push`/`pop` on the same buffer
    /// (it is meant for stream restart points, not live operation).
    pub fn reset(&self) {
        self.head.store(0, Ordering::Release);
        self.tail.store(0, Ordering::Release);
        self.overruns.store(0, Ordering::Relaxed);
        self.underruns.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    /// Deterministic xorshift64 PRNG used by the SPSC stress test.
    struct XorShift64(u64);

    impl XorShift64 {
        fn new(seed: u64) -> Self {
            XorShift64(seed.max(1))
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        /// A deterministic f32 in roughly `[-1.0, 1.0)` — mantissa from the
        /// PRNG, exponent pinned to 1.0, sign from the PRNG.
        fn next_f32(&mut self) -> f32 {
            let bits = self.next_u64() as u32;
            let magnitude = f32::from_bits((bits & 0x007F_FFFF) | 0x3F80_0000) - 1.0;
            if bits & 0x8000_0000 != 0 {
                -magnitude
            } else {
                magnitude
            }
        }
    }

    fn block_of(start: f32, len: usize) -> Vec<f32> {
        (0..len).map(|i| start + i as f32).collect()
    }

    #[test]
    fn capacity_rounds_up_to_pow2_min_1024() {
        assert_eq!(JitterBuffer::new(1).capacity(), 1024);
        assert_eq!(JitterBuffer::new(1024).capacity(), 1024);
        assert_eq!(JitterBuffer::new(1025).capacity(), 2048);
        assert_eq!(JitterBuffer::new(4800).capacity(), 8192);
        assert_eq!(JitterBuffer::new(65536).capacity(), 65536);
    }

    #[test]
    fn push_pop_linear() {
        let jb = JitterBuffer::new(1024);
        assert_eq!(jb.available(), 0);
        let block = block_of(0.0, 480);
        assert_eq!(jb.push(&block), 480);
        assert_eq!(jb.available(), 480);
        assert_eq!(jb.free(), 1024 - 480);
        assert!((jb.fill_ratio() - 480.0 / 1024.0).abs() < 1e-6);

        let mut out = vec![0.0f32; 480];
        assert_eq!(jb.pop(&mut out), 480);
        for (i, s) in out.iter().enumerate() {
            assert_eq!(*s, i as f32, "sample {i}");
        }
        assert_eq!(jb.available(), 0);
        // Popping an empty buffer returns 0.
        assert_eq!(jb.pop(&mut out), 0);
    }

    /// Push/pop interleaved so the ring index wraps across the capacity
    /// boundary; the caller must still see a linear, in-order stream.
    #[test]
    fn wraparound_across_capacity_boundary() {
        let jb = JitterBuffer::new(1024);
        let a = block_of(0.0, 512); // fills [0..512)
        assert_eq!(jb.push(&a), 512);
        let mut scratch = vec![0.0f32; 512];
        // Pop 256: ring now [256..512) occupied.
        assert_eq!(jb.pop(&mut scratch[..256]), 256);
        for (i, s) in scratch[..256].iter().enumerate() {
            assert_eq!(*s, i as f32);
        }
        // Push 768: 256 free at the tail + 768 free after head wrap -> the
        // write spans 256..1024 then 0..256 (wraps the boundary).
        let b = block_of(1000.0, 768);
        assert_eq!(jb.push(&b), 768);
        assert_eq!(jb.available(), 1024, "buffer is now exactly full");

        // Pop 512: reads 256 leftover of `a` (256..512) then wraps to 256
        // samples of `b` (0..256).
        assert_eq!(jb.pop(&mut scratch), 512);
        for (i, s) in scratch[..256].iter().enumerate() {
            assert_eq!(*s, (256 + i) as f32, "leftover of block a");
        }
        for (i, s) in scratch[256..].iter().enumerate() {
            assert_eq!(*s, 1000.0 + i as f32, "first part of block b");
        }
        // Pop the remaining 512 of `b` (256..768) — again crossing the ring
        // boundary at 1024.
        assert_eq!(jb.pop(&mut scratch), 512);
        for (i, s) in scratch.iter().enumerate() {
            assert_eq!(*s, 1256.0 + i as f32, "rest of block b");
        }
        assert_eq!(jb.available(), 0);
        assert_eq!(jb.overruns(), 0);
        assert_eq!(jb.underruns(), 0);
    }

    /// A block that does not fit must be dropped whole: nothing written,
    /// overrun counted, prior contents untouched.
    #[test]
    fn overrun_drops_entire_block() {
        let jb = JitterBuffer::new(1024);
        let fill = vec![1.0f32; 1024];
        assert_eq!(jb.push(&fill), 1024, "exact fill fits");
        assert_eq!(jb.free(), 0);

        let extra = vec![2.0f32; 8];
        assert_eq!(jb.push(&extra), 0, "no space -> whole block dropped");
        assert_eq!(jb.overruns(), 1);
        assert_eq!(jb.available(), 1024, "prior contents intact");

        // Leave exactly one free sample: 8 samples still cannot fit, so the
        // block is dropped whole again.
        let mut out = vec![0.0f32; 1];
        assert_eq!(jb.pop(&mut out), 1);
        assert_eq!(jb.free(), 1);
        assert_eq!(jb.push(&extra), 0);
        assert_eq!(jb.overruns(), 2);

        // The surviving data is exactly what was pushed: 1023 samples of 1.0.
        let mut rest = vec![0.0f32; 1023];
        assert_eq!(jb.pop(&mut rest), 1023);
        assert!(out.iter().all(|&s| s == 1.0));
        assert!(rest.iter().all(|&s| s == 1.0));
        assert_eq!(jb.available(), 0);
    }

    #[test]
    fn underrun_counter() {
        let jb = JitterBuffer::new(1024);
        assert_eq!(jb.underruns(), 0);
        jb.note_underrun();
        assert_eq!(jb.underruns(), 1);
        jb.note_underrun();
        jb.note_underrun();
        assert_eq!(jb.underruns(), 3);
    }

    #[test]
    fn reset_clears_state() {
        let jb = JitterBuffer::new(1024);
        jb.push(&block_of(0.0, 512));
        jb.note_underrun();
        jb.reset();
        assert_eq!(jb.available(), 0);
        assert_eq!(jb.overruns(), 0);
        assert_eq!(jb.underruns(), 0);
        // Fully usable again.
        let block = block_of(7.0, 480);
        assert_eq!(jb.push(&block), 480);
        let mut out = vec![0.0f32; 480];
        assert_eq!(jb.pop(&mut out), 480);
        assert_eq!(out[0], 7.0);
    }

    /// Concurrent SPSC stress: producer streams xorshift-derived blocks while
    /// the consumer verifies the exact sequence order and values. Runs >= 2 s.
    #[test]
    fn concurrent_spsc_stress() {
        const BLOCK: usize = 480;
        const SEED: u64 = 0x9E37_79B9_7F4A_7C15;
        const RUN: Duration = Duration::from_secs(2);

        let jb = Arc::new(JitterBuffer::new(16384));
        let stop = Arc::new(AtomicBool::new(false));
        let produced = Arc::new(AtomicU64::new(0));
        let producer_done = Arc::new(AtomicBool::new(false));

        let prod_jb = jb.clone();
        let prod_stop = stop.clone();
        let prod_cnt = produced.clone();
        let prod_done = producer_done.clone();
        let producer = thread::spawn(move || {
            let mut rng = XorShift64::new(SEED);
            let mut block = [0.0f32; BLOCK];
            while !prod_stop.load(Ordering::Relaxed) {
                for s in &mut block {
                    *s = rng.next_f32();
                }
                let n = prod_jb.push(&block);
                if n > 0 {
                    prod_cnt.fetch_add(n as u64, Ordering::Relaxed);
                } else {
                    // The ring must never be full: pacing keeps the producer
                    // far slower than the consumer, so a full ring (which
                    // would force a drop-whole-block overrun and legitimately
                    // break the exact sequence) is impossible.
                    panic!(
                        "producer outran the consumer: ring full, overruns={}",
                        prod_jb.overruns()
                    );
                }
                // 150 us/block: ~6.6k blocks/s, far below what the verifying
                // consumer can drain, yet enough to exercise the ring and its
                // atomics continuously for the whole run.
                thread::sleep(Duration::from_micros(150));
            }
            // Last action: publish that no further pushes will happen, so the
            // consumer can stop exactly when the ring is drained.
            prod_done.store(true, Ordering::Release);
        });

        let cons_jb = jb.clone();
        let cons_stop = stop.clone();
        let cons_done = producer_done.clone();
        let consumer = thread::spawn(move || {
            let mut rng = XorShift64::new(SEED);
            let mut block = [0.0f32; BLOCK];
            let mut verified: u64 = 0;
            loop {
                let n = cons_jb.pop(&mut block);
                if n > 0 {
                    for (i, s) in block[..n].iter().enumerate() {
                        let expected = rng.next_f32();
                        assert_eq!(
                            s.to_bits(),
                            expected.to_bits(),
                            "SPSC mismatch at global offset {}",
                            verified + i as u64
                        );
                    }
                    verified += n as u64;
                } else if cons_stop.load(Ordering::Relaxed) && cons_done.load(Ordering::Acquire) {
                    break;
                } else {
                    thread::yield_now();
                }
            }
            verified
        });

        let t0 = Instant::now();
        // Let it run for the full window.
        while t0.elapsed() < RUN {
            thread::sleep(Duration::from_millis(20));
        }
        stop.store(true, Ordering::Relaxed);

        let _ = producer.join();
        let verified = consumer.join().expect("consumer thread panicked");

        let produced_total = produced.load(Ordering::Relaxed);
        eprintln!(
            "SPSC stress: produced={produced_total} verified={verified} in {} ms",
            t0.elapsed().as_millis()
        );
        // Every sample pushed by the producer must have been verified in
        // order by the consumer; the consumer drains until the ring is empty
        // *and* the producer has published completion, so nothing is lost.
        assert_eq!(verified, produced_total, "consumer must see every sample");
        // Floor: 100k samples (~2 s of audio at 48 kHz) is reached even on a
        // slow debug build; the point is that the ring was exercised heavily
        // with exact-order verification, not a throughput benchmark.
        assert!(
            verified >= 100_000,
            "stress must exercise the ring: {verified}"
        );
        assert_eq!(jb.overruns(), 0, "producer must never overrun a big ring");
    }
}
