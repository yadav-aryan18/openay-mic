//! Mod-2^16 sequence tracking.

/// Outcome of feeding a sequence number to a [`SeqTracker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqEvent {
    /// `seq == expected` (exactly `last + 1` mod 2^16).
    InOrder,
    /// `seq` jumped forward by `n` (mod-2^16 distance in `1..32768`); `n`
    /// packets are presumed lost.
    Gap(u16),
    /// `seq == last`; same packet received again.
    Duplicate,
    /// `seq` is behind `expected` beyond the duplicate window (mod-2^16
    /// distance >= 32768): a reordering or a very stale packet.
    Reorder,
}

/// Tracks the expected next sequence number of a per-direction packet stream.
///
/// Constructed with `expected = 0`; every successful classification advances
/// the expected counter by one (mod 2^16). Classification follows the spec:
///
/// - `seq == expected`                -> in-order
/// - `seq - expected (mod 2^16) < 32768` -> gap of that many lost packets
/// - `seq == last`                    -> duplicate
/// - otherwise                        -> reorder
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeqTracker {
    expected: u16,
    last: Option<u16>,
}

impl Default for SeqTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqTracker {
    /// New tracker whose first expected sequence number is 0.
    pub fn new() -> Self {
        SeqTracker {
            expected: 0,
            last: None,
        }
    }

    /// Feed the next received sequence number; returns the classification and
    /// advances internal state.
    pub fn update(&mut self, seq: u16) -> SeqEvent {
        let event = if seq == self.expected {
            SeqEvent::InOrder
        } else if Some(seq) == self.last {
            SeqEvent::Duplicate
        } else {
            let distance = seq.wrapping_sub(self.expected);
            if distance < 32768 {
                SeqEvent::Gap(distance)
            } else {
                SeqEvent::Reorder
            }
        };
        self.last = Some(seq);
        self.expected = seq.wrapping_add(1);
        event
    }

    /// The next expected sequence number.
    pub fn expected(&self) -> u16 {
        self.expected
    }

    /// The most recently seen sequence number, if any.
    pub fn last(&self) -> Option<u16> {
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_run() {
        let mut t = SeqTracker::new();
        for seq in 0..1000u16 {
            assert_eq!(t.update(seq), SeqEvent::InOrder, "seq {seq}");
        }
    }

    #[test]
    fn injected_gap() {
        let mut t = SeqTracker::new();
        for seq in 0..10u16 {
            assert_eq!(t.update(seq), SeqEvent::InOrder);
        }
        // Jump from 9 to 14: 4 lost.
        assert_eq!(t.update(14), SeqEvent::Gap(4));
        // Stream continues from 15.
        assert_eq!(t.update(15), SeqEvent::InOrder);
        // A larger jump across the 32768 threshold is a reorder, not a gap.
        assert!(!t.update(16).is_gap());
    }

    #[test]
    fn duplicate() {
        let mut t = SeqTracker::new();
        for seq in 0..5u16 {
            assert_eq!(t.update(seq), SeqEvent::InOrder);
        }
        // Re-send seq 4.
        assert_eq!(t.update(4), SeqEvent::Duplicate);
        // Next expected is still 5.
        assert_eq!(t.update(5), SeqEvent::InOrder);
    }

    #[test]
    fn reorder() {
        let mut t = SeqTracker::new();
        for seq in 0..10u16 {
            assert_eq!(t.update(seq), SeqEvent::InOrder);
        }
        // A late packet that is not the immediate duplicate: seq 8 after 9.
        assert_eq!(t.update(8), SeqEvent::Reorder);
        // A packet far behind is also a reorder (mod-2^16 distance >= 32768).
        assert_eq!(t.update(3), SeqEvent::Reorder);
    }

    #[test]
    fn wraparound_in_order() {
        let mut t = SeqTracker::new();
        // Feed the full run 0..=0xFFFD so the counter is just before wrap.
        for seq in 0u16..=0xFFFD {
            assert_eq!(t.update(seq), SeqEvent::InOrder, "seq {seq}");
        }
        // The wraparound continuation must stay in-order throughout.
        for seq in [0xFFFEu16, 0xFFFF, 0x0000, 0x0001] {
            assert_eq!(t.update(seq), SeqEvent::InOrder, "seq {seq}");
        }
        // And a duplicate just after wrap is still detected.
        assert_eq!(t.update(0x0001), SeqEvent::Duplicate);
    }

    #[test]
    fn gap_across_wraparound() {
        let mut t = SeqTracker::new();
        for seq in 0u16..=0xFFFE {
            assert_eq!(t.update(seq), SeqEvent::InOrder);
        }
        // Expected is 0xFFFF; jump to 0x0003 -> 4 lost.
        assert_eq!(t.update(0x0003), SeqEvent::Gap(4));
    }

    impl SeqEvent {
        fn is_gap(&self) -> bool {
            matches!(self, SeqEvent::Gap(_))
        }
    }
}
