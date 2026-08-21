//! Packet statistics, rendered as the canonical `RECV ...` line.

/// Cumulative receive statistics for one direction of the stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PacketStats {
    /// Number of valid packets received and processed.
    pub received: u64,
    /// Packets estimated lost (sequence gaps).
    pub lost: u64,
    /// Duplicate sequence numbers.
    pub duplicate: u64,
    /// Out-of-order (reordered) sequence numbers.
    pub out_of_order: u64,
    /// Datagrams/bytes that failed to decode as a packet.
    pub malformed: u64,
    /// Packets that decoded fine but failed payload verification.
    pub content_errors: u64,
}

impl PacketStats {
    /// The exact stats line expected by the test harness:
    /// `RECV ok=<received> lost=<lost> dup=<duplicate> ooo=<out_of_order>
    /// malformed=<malformed> content_errors=<content_errors>`
    pub fn render(&self) -> String {
        format!(
            "RECV ok={} lost={} dup={} ooo={} malformed={} content_errors={}",
            self.received, self.lost, self.duplicate, self.out_of_order, self.malformed, self.content_errors
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_exact_line() {
        let s = PacketStats {
            received: 1000,
            lost: 2,
            duplicate: 1,
            out_of_order: 3,
            malformed: 4,
            content_errors: 0,
        };
        assert_eq!(
            s.render(),
            "RECV ok=1000 lost=2 dup=1 ooo=3 malformed=4 content_errors=0"
        );
    }
}
