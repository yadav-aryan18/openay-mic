//! VU metering: level -> segment mapping, ballistics decay, and packet-rate
//! computation. Pure functions with no GUI dependencies so they are fully
//! unit-testable headlessly.

/// Number of segments in the VU ladder.
pub const SEGMENTS: usize = 24;
/// Bottom segments are cream (the "VU face").
pub const CREAM_SEGMENTS: usize = 18;
/// The next segments are amber (warning zone).
pub const AMBER_SEGMENTS: usize = 3;
/// The top segments are tally red (clip zone).
pub const RED_SEGMENTS: usize = 3;
/// First amber segment (0-based).
pub const AMBER_START: usize = CREAM_SEGMENTS;
/// First red (clip) segment.
pub const RED_START: usize = SEGMENTS - RED_SEGMENTS;

/// Decay rate of the VU ballistics in dB per second (design.md: ~12 dB/s).
pub const DECAY_DB_PER_SEC: f32 = 12.0;
/// Peak-hold decays a third as fast as the main reading (design.md: a single
/// brighter segment at the recent max, decaying).
pub const HOLD_DECAY_DB_PER_SEC: f32 = DECAY_DB_PER_SEC / 3.0;
/// Levels below this dB floor snap to zero.
pub const FLOOR_DB: f32 = -60.0;

/// dB thresholds (level must be >= the threshold to light segment `i`).
///
/// Top-end weighted: the top three segments are ~1 dB wide each, widening to
/// 3 dB per segment toward the bottom. `SEGMENT_DB[i]` is the threshold for
/// segment `i` (0-based, bottom = 0). Segment 0 uses a finite floor of
/// -120 dB so that silence (level 0, -inf dB) lights nothing.
const SEGMENT_DB: [f32; SEGMENTS] = [
    -120.0, // 0: anything above silence
    -60.0, -57.0, -54.0, -51.0, -48.0, -45.0, -42.0, -39.0, -36.0, -33.0, -30.0, -27.0, -24.0,
    -21.0, -18.0, -15.0, -12.0, // 17: end of cream zone
    -9.0,  // 18: amber
    -7.0, -5.0, -3.0, // 21: red (clip zone begins at -3 dB)
    -2.0, -1.0,
];

/// Convert a linear level `0.0..=1.0` to dB (0 dB = full scale).
pub fn level_to_db(level: f32) -> f32 {
    if level <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * level.log10()
    }
}

/// Convert a dB value back to a linear level.
pub fn db_to_level(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

/// Number of lit VU segments for a linear level `0.0..=1.0` (`0..=SEGMENTS`).
///
/// Segment `i` is lit when `level_to_db(level) >= SEGMENT_DB[i]`.
pub fn vu_segments(level: f32) -> usize {
    let db = level_to_db(level);
    let mut lit = 0;
    for &t in SEGMENT_DB.iter() {
        if db >= t {
            lit += 1;
        } else {
            break;
        }
    }
    lit
}

/// Zone of the topmost lit segment (for the canvas coloring).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Off,
    Cream,
    Amber,
    Red,
}

/// Zone of the topmost lit segment for a linear level.
#[cfg_attr(not(test), allow(dead_code))]
pub fn zone_for(level: f32) -> Zone {
    let lit = vu_segments(level);
    if lit == 0 {
        Zone::Off
    } else if lit <= CREAM_SEGMENTS {
        Zone::Cream
    } else if lit <= CREAM_SEGMENTS + AMBER_SEGMENTS {
        Zone::Amber
    } else {
        Zone::Red
    }
}

/// VU ballistics state: instant attack, ~12 dB/s decay for the reading, and
/// a peak-hold marker that decays a third as fast (design.md: "a single
/// brighter segment at recent max, decaying").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VuBallistics {
    level: f32,
    peak_hold: f32,
}

impl Default for VuBallistics {
    fn default() -> Self {
        VuBallistics {
            level: 0.0,
            peak_hold: 0.0,
        }
    }
}

impl VuBallistics {
    /// Create ballistics starting from a given displayed level (the hold
    /// starts at the same level).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(level: f32) -> Self {
        VuBallistics {
            level,
            peak_hold: level,
        }
    }

    /// Current displayed level.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// The peak-hold level: the most recent maximum, decaying at
    /// [`HOLD_DECAY_DB_PER_SEC`]. (Only the segment count is used by the
    /// canvas; the level is exposed for tests and debugging.)
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn hold_level(&self) -> f32 {
        self.peak_hold
    }

    /// Segments lit by the peak-hold (always >= `vu_segments(level())`).
    pub fn hold_segments(&self) -> usize {
        vu_segments(self.peak_hold)
    }

    /// Fold a fresh peak sample into the displayed level after `dt` seconds:
    /// rises instantly, decays at [`DECAY_DB_PER_SEC`] dB/s. The peak-hold
    /// rises instantly too but decays at [`HOLD_DECAY_DB_PER_SEC`].
    pub fn update(&mut self, peak: f32, dt: f32) -> f32 {
        let peak = peak.clamp(0.0, 1.0);
        if peak >= self.level {
            self.level = peak;
        } else if self.level > 0.0 {
            let db = level_to_db(self.level);
            let new_db = db - DECAY_DB_PER_SEC * dt.max(0.0);
            self.level = if new_db <= FLOOR_DB {
                0.0
            } else {
                db_to_level(new_db)
            };
        }
        if peak >= self.peak_hold {
            self.peak_hold = peak;
        } else if self.peak_hold > 0.0 {
            let db = level_to_db(self.peak_hold);
            let new_db = db - HOLD_DECAY_DB_PER_SEC * dt.max(0.0);
            self.peak_hold = if new_db <= FLOOR_DB {
                0.0
            } else {
                db_to_level(new_db)
            };
        }
        self.level
    }
}

/// Packet rate from a counter delta over a time delta (packets per second).
pub fn pps(received_delta: u64, dt: f32) -> f32 {
    if dt <= 0.0 {
        return 0.0;
    }
    received_delta as f32 / dt
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full segment threshold table, one entry per segment.
    #[test]
    fn segment_table_has_exactly_24_entries() {
        assert_eq!(SEGMENT_DB.len(), SEGMENTS);
        assert_eq!(CREAM_SEGMENTS + AMBER_SEGMENTS + RED_SEGMENTS, SEGMENTS);
    }

    #[test]
    fn thresholds_are_weakly_decreasing() {
        for w in SEGMENT_DB.windows(2) {
            assert!(
                w[1] >= w[0],
                "threshold must not decrease: {} < {}",
                w[1],
                w[0]
            );
        }
    }

    #[test]
    fn clip_zone_begins_at_minus_3_db() {
        // Segment 20 (last amber) lights at -5 dB; segment 21 (first red)
        // lights at -3 dB.
        assert_eq!(RED_START, 21);
        assert_eq!(SEGMENT_DB[RED_START], -3.0, "red zone starts at -3 dB");
    }

    #[test]
    fn silence_lights_nothing() {
        assert_eq!(vu_segments(0.0), 0);
        assert_eq!(vu_segments(-1.0), 0);
        assert_eq!(zone_for(0.0), Zone::Off);
    }

    #[test]
    fn full_scale_lights_everything() {
        assert_eq!(vu_segments(1.0), SEGMENTS);
        assert_eq!(zone_for(1.0), Zone::Red);
    }

    #[test]
    fn boundary_just_below_threshold() {
        // -3.0 dB exactly lights segment 21 (>=); just below does not.
        let level = db_to_level(-3.0);
        assert_eq!(vu_segments(level), RED_START + 1);
        let just_below = db_to_level(-3.0 - 1e-4);
        assert_eq!(vu_segments(just_below), RED_START);
        assert_eq!(zone_for(just_below), Zone::Amber);
    }

    #[test]
    fn boundary_at_full_scale() {
        assert_eq!(vu_segments(1.0), SEGMENTS);
        assert_eq!(vu_segments(db_to_level(-1.0)), SEGMENTS - 1);
    }

    #[test]
    fn zone_transitions() {
        assert_eq!(zone_for(db_to_level(-12.0)), Zone::Cream);
        assert_eq!(zone_for(db_to_level(-9.0)), Zone::Amber);
        assert_eq!(zone_for(db_to_level(-5.0)), Zone::Amber);
        assert_eq!(zone_for(db_to_level(-2.0)), Zone::Red);
    }

    /// Decay: -6 dB peak decays by ~12 dB/s over 1 s, but a fresh peak that
    /// is higher than the displayed level attacks instantly.
    #[test]
    fn ballistics_attack_is_instant() {
        let mut b = VuBallistics::default();
        b.update(0.5, 1.0); // jump from 0 to 0.5 is instant
        assert_eq!(b.level(), 0.5);
    }

    #[test]
    fn ballistics_decay_is_12_db_per_second() {
        let mut b = VuBallistics::new(0.5); // -6.02 dB
        let before = level_to_db(b.level());
        let after_db = level_to_db(b.update(0.0, 1.0));
        let delta = before - after_db;
        assert!(
            (delta - 12.0).abs() < 0.1,
            "expected ~12 dB decay over 1 s, got {delta}"
        );
    }

    #[test]
    fn ballistics_decay_half_second() {
        let mut b = VuBallistics::new(0.5);
        let after_db = level_to_db(b.update(0.0, 0.5));
        assert!((after_db - (-12.02)).abs() < 0.1, "got {after_db}");
    }

    #[test]
    fn ballistics_never_goes_negative_and_snaps_to_floor() {
        let mut b = VuBallistics::new(db_to_level(-30.0)); // -30 dB
                                                           // 5 s at 12 dB/s = 60 dB of decay: reaches the -60 dB floor -> 0.
        let level = b.update(0.0, 5.0);
        assert_eq!(level, 0.0);
        // Subsequent updates keep it at 0.
        assert_eq!(b.update(0.0, 100.0), 0.0);
    }

    #[test]
    fn ballistics_clamps_peak_input() {
        let mut b = VuBallistics::default();
        b.update(2.0, 0.0);
        assert_eq!(b.level(), 1.0);
    }

    /// Peak-hold: attacks instantly with the peak, and after the level has
    /// decayed the hold still marks the recent maximum (a "brighter segment
    /// at recent max, decaying").
    #[test]
    fn ballistics_peak_hold_attacks_instantly_and_holds() {
        let mut b = VuBallistics::default();
        b.update(0.5, 1.0); // jump from 0 to 0.5 is instant
        assert_eq!(b.hold_level(), 0.5);
        assert_eq!(b.hold_segments(), vu_segments(0.5));

        // 0.1 s later the reading has barely moved; the hold matches it.
        b.update(0.5, 0.1);
        assert!(b.hold_level() >= b.level());

        // Level decays at 12 dB/s while the hold decays at 4 dB/s: after a
        // long silence the hold still marks a higher segment than the
        // reading (or both reach the floor together).
        let mut b = VuBallistics::new(0.5); // -6.02 dB
        b.update(0.0, 0.5); // level -12.02 dB, hold -8.02 dB
        let level_db = level_to_db(b.level());
        let hold_db = level_to_db(b.hold_level());
        assert!(
            (level_db - (-12.02)).abs() < 0.1,
            "reading decays at 12 dB/s, got {level_db}"
        );
        assert!(
            (hold_db - (-8.02)).abs() < 0.1,
            "hold decays at 4 dB/s, got {hold_db}"
        );
        assert!(hold_db > level_db, "hold must lag behind the reading");
        assert!(
            b.hold_segments() >= vu_segments(b.level()),
            "hold must never mark fewer segments than the reading"
        );
    }

    #[test]
    fn ballistics_peak_hold_never_goes_negative() {
        let mut b = VuBallistics::new(db_to_level(-30.0));
        b.update(0.0, 100.0); // long silence: both decay to the floor
        assert_eq!(b.level(), 0.0);
        assert_eq!(b.hold_level(), 0.0);
        assert_eq!(b.hold_segments(), 0);
    }

    #[test]
    fn pps_from_counter_deltas() {
        // 480 packets over 1 s.
        assert!((pps(480, 1.0) - 480.0).abs() < 1e-6);
        // 48 packets over 100 ms.
        assert!((pps(48, 0.1) - 480.0).abs() < 1e-6);
        // Zero time -> zero rate (no divide by zero).
        assert_eq!(pps(100, 0.0), 0.0);
        assert_eq!(pps(100, -1.0), 0.0);
        // No packets -> zero rate.
        assert_eq!(pps(0, 1.0), 0.0);
    }

    #[test]
    fn db_level_round_trip() {
        for &l in &[0.001, 0.1, 0.5, 1.0] {
            let db = level_to_db(l);
            let back = db_to_level(db);
            assert!((back - l).abs() < 1e-6, "round trip {l} -> {db} -> {back}");
        }
    }
}
