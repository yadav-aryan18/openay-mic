//! Loss profiles and the per-datagram decision engine.
//!
//! # Profile semantics
//!
//! * `clean` — every datagram forwarded immediately.
//! * `loss2` — each datagram dropped independently with `p = 0.02`.
//! * `burst` — Gilbert–Elliott two-state Markov chain: a *good* state drops
//!   with `p = 0.01`, a *bad* state drops with `p = 0.95`; a good datagram
//!   moves to bad with `p = 0.005`, a bad datagram returns to good with
//!   `p = 0.05`. The stationary bad-state fraction is
//!   `0.005 / (0.005 + 0.05) ≈ 0.091`, giving a mean loss of ≈ 9% (inside
//!   the 5–10% target) and a mean consecutive-drop run of
//!   `1 / (1 - 0.95 · 0.95) ≈ 10` datagrams (typical bursts 5–15; see the
//!   [`BURST_*`] constants).
//! * `jitter30` — uniform 0–60 ms extra delay per datagram plus 1%
//!   duplicates: the duplicate is forwarded immediately and the original
//!   takes the delay path.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use crate::rng::SplitMix64;

/// Independent per-datagram drop probability for [`Profile::Loss2`].
pub const LOSS2_DROP_PROB: f64 = 0.02;

/// Gilbert–Elliott burst profile constants.
/// Drop probability while in the good state.
pub const BURST_GOOD_DROP_PROB: f64 = 0.01;
/// Drop probability while in the bad state.
pub const BURST_BAD_DROP_PROB: f64 = 0.95;
/// Probability that a good-state datagram moves to bad.
pub const BURST_GOOD_TO_BAD: f64 = 0.005;
/// Probability that a bad-state datagram returns to good.
pub const BURST_BAD_TO_GOOD: f64 = 0.05;

/// Maximum extra delay (milliseconds) for [`Profile::Jitter30`].
pub const JITTER_MAX_DELAY_MS: u64 = 60;
/// Per-datagram duplicate probability for [`Profile::Jitter30`].
pub const JITTER_DUP_PROB: f64 = 0.01;

/// Loss profile applied to the forwarded stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// Forward everything unchanged.
    Clean,
    /// Drop each datagram independently with `p = 0.02`.
    Loss2,
    /// Gilbert–Elliott bursty loss (mean ≈ 9% loss, mean run of ~10).
    Burst,
    /// Uniform 0–60 ms extra delay + 1% immediate duplicates.
    Jitter30,
}

impl Profile {
    /// The four profile names in CLI order.
    pub const ALL: [Profile; 4] = [
        Profile::Clean,
        Profile::Loss2,
        Profile::Burst,
        Profile::Jitter30,
    ];

    /// CLI/profile name (`clean`, `loss2`, `burst`, `jitter30`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Clean => "clean",
            Profile::Loss2 => "loss2",
            Profile::Burst => "burst",
            Profile::Jitter30 => "jitter30",
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Profile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "clean" => Ok(Profile::Clean),
            "loss2" => Ok(Profile::Loss2),
            "burst" => Ok(Profile::Burst),
            "jitter30" => Ok(Profile::Jitter30),
            other => Err(format!(
                "unknown profile `{other}` (expected one of: clean, loss2, burst, jitter30)"
            )),
        }
    }
}

/// What the decision engine says to do with one datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Send immediately.
    ForwardImmediate,
    /// Send once, after the delay.
    ForwardDelayed(Duration),
    /// Send one copy immediately (the duplicate) and the original later,
    /// after the delay.
    ForwardImmediatePlusDelayed(Duration),
    /// Do not forward.
    Drop,
}

impl Action {
    /// `true` when the datagram is dropped.
    #[must_use]
    pub fn is_drop(self) -> bool {
        matches!(self, Action::Drop)
    }

    /// The delay used for the original datagram, if any.
    #[must_use]
    pub fn delay(self) -> Option<Duration> {
        match self {
            Action::ForwardDelayed(d) | Action::ForwardImmediatePlusDelayed(d) => Some(d),
            Action::ForwardImmediate | Action::Drop => None,
        }
    }

    /// `true` when an immediate duplicate copy is sent.
    #[must_use]
    pub fn duplicates(self) -> bool {
        matches!(self, Action::ForwardImmediatePlusDelayed(_))
    }
}

/// Deterministic per-datagram decision generator.
///
/// Decisions are drawn from [`SplitMix64`] seeded with the profile seed, in
/// arrival order; the same seed therefore always yields the identical
/// decision sequence for the same arrival order, and there is no wall-clock
/// or OS entropy in the decision path.
#[derive(Debug, Clone)]
pub struct DecisionEngine {
    rng: SplitMix64,
    profile: Profile,
    /// Gilbert–Elliott current state (`true` = bad).
    bad: bool,
}

impl DecisionEngine {
    /// Create a decision engine for `profile` with a fixed seed.
    #[must_use]
    pub fn new(profile: Profile, seed: u64) -> Self {
        Self {
            rng: SplitMix64::new(seed),
            profile,
            bad: false,
        }
    }

    /// Decide the fate of the next datagram.
    #[must_use]
    pub fn decide(&mut self) -> Action {
        match self.profile {
            Profile::Clean => Action::ForwardImmediate,
            Profile::Loss2 => {
                if self.rng.next_f64() < LOSS2_DROP_PROB {
                    Action::Drop
                } else {
                    Action::ForwardImmediate
                }
            }
            Profile::Burst => {
                // State transition first, then the (state-dependent) drop.
                if self.bad {
                    if self.rng.next_f64() < BURST_BAD_TO_GOOD {
                        self.bad = false;
                    }
                } else if self.rng.next_f64() < BURST_GOOD_TO_BAD {
                    self.bad = true;
                }
                let p = if self.bad {
                    BURST_BAD_DROP_PROB
                } else {
                    BURST_GOOD_DROP_PROB
                };
                if self.rng.next_f64() < p {
                    Action::Drop
                } else {
                    Action::ForwardImmediate
                }
            }
            Profile::Jitter30 => {
                // Uniform 0..=JITTER_MAX_DELAY_MS milliseconds.
                let ms = (self.rng.next_f64() * (JITTER_MAX_DELAY_MS as f64 + 1.0)) as u64;
                let delay = Duration::from_millis(ms);
                if self.rng.next_f64() < JITTER_DUP_PROB {
                    Action::ForwardImmediatePlusDelayed(delay)
                } else {
                    Action::ForwardDelayed(delay)
                }
            }
        }
    }

    /// The profile this engine implements.
    #[must_use]
    pub fn profile(&self) -> Profile {
        self.profile
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A compact, total-ordered fingerprint of an action for sequence tests.
    fn encode(action: Action) -> u64 {
        (u64::from(action.is_drop()) << 2)
            | (u64::from(action.delay().is_some()) << 1)
            | u64::from(action.duplicates())
    }

    fn sequence(profile: Profile, seed: u64, n: usize) -> Vec<u64> {
        let mut engine = DecisionEngine::new(profile, seed);
        (0..n).map(|_| encode(engine.decide())).collect()
    }

    #[test]
    fn same_seed_reproduces_sequence_for_every_profile() {
        for profile in Profile::ALL {
            assert_eq!(
                sequence(profile, 42, 2_000),
                sequence(profile, 42, 2_000),
                "profile {profile} not reproducible under the same seed"
            );
        }
    }

    #[test]
    fn different_seed_differs_for_every_profile() {
        for profile in [Profile::Loss2, Profile::Burst, Profile::Jitter30] {
            assert_ne!(
                sequence(profile, 1, 2_000),
                sequence(profile, 2, 2_000),
                "profile {profile} ignores the seed"
            );
        }
    }

    #[test]
    fn clean_always_forwards() {
        let mut engine = DecisionEngine::new(Profile::Clean, 7);
        for _ in 0..1_000 {
            assert_eq!(engine.decide(), Action::ForwardImmediate);
        }
    }

    #[test]
    fn loss2_rate_is_about_two_percent() {
        let n = 100_000;
        let mut engine = DecisionEngine::new(Profile::Loss2, 1234);
        let drops = (0..n).filter(|_| engine.decide().is_drop()).count();
        let rate = drops as f64 / n as f64;
        // ~18 sigma tolerance around 0.02; deterministic for this seed.
        assert!(
            (0.015..=0.025).contains(&rate),
            "loss2 drop rate {rate} outside [0.015, 0.025]"
        );
    }

    #[test]
    fn burst_has_stable_band_and_long_runs_for_default_seed() {
        let n = 200_000;
        let mut engine = DecisionEngine::new(Profile::Burst, crate::DEFAULT_SEED);
        let mut drops = 0usize;
        let mut run = 0usize;
        let mut max_run = 0usize;
        let mut prev_drop = false;
        let mut drop_after_drop = 0usize;
        let mut drop_after_ok = 0usize;
        let mut ok_after_drop = 0usize;
        let mut ok_after_ok = 0usize;
        for _ in 0..n {
            let drop = engine.decide().is_drop();
            if drop {
                drops += 1;
                run += 1;
                max_run = max_run.max(run);
                if prev_drop {
                    drop_after_drop += 1;
                } else {
                    drop_after_ok += 1;
                }
            } else {
                run = 0;
                if prev_drop {
                    ok_after_drop += 1;
                } else {
                    ok_after_ok += 1;
                }
            }
            prev_drop = drop;
        }

        let loss = drops as f64 / n as f64;
        assert!(
            (0.05..=0.10).contains(&loss),
            "burst mean loss {loss} outside band [0.05, 0.10]"
        );
        assert!(
            max_run > 15,
            "expected a consecutive-drop run longer than 15, got {max_run}"
        );

        // Burstiness signature: drops clump.
        let p_after_drop = drop_after_drop as f64 / (drop_after_drop + ok_after_drop) as f64;
        let p_after_ok = drop_after_ok as f64 / (drop_after_ok + ok_after_ok) as f64;
        assert!(p_after_drop > 0.35, "P(drop|drop) = {p_after_drop}");
        assert!(p_after_ok < 0.20, "P(drop|delivered) = {p_after_ok}");
    }

    #[test]
    fn jitter30_delays_are_bounded_and_dups_near_one_percent() {
        let n = 100_000;
        let mut engine = DecisionEngine::new(Profile::Jitter30, 7);
        let mut dups = 0usize;
        let mut max_ms = 0u64;
        for _ in 0..n {
            let action = engine.decide();
            if action.duplicates() {
                dups += 1;
            }
            let delay = action.delay().expect("jitter30 never forwards immediately");
            let ms = u64::try_from(delay.as_millis()).expect("delay fits u64");
            assert!(
                ms <= JITTER_MAX_DELAY_MS,
                "delay {ms} ms exceeds {JITTER_MAX_DELAY_MS} ms"
            );
            max_ms = max_ms.max(ms);
        }
        let dup_rate = dups as f64 / n as f64;
        assert!(
            (0.005..=0.015).contains(&dup_rate),
            "jitter30 dup rate {dup_rate} outside [0.005, 0.015]"
        );
        assert!(max_ms > 30, "no delay above 30 ms in {n} samples");
    }

    #[test]
    fn profile_names_parse_and_roundtrip() {
        for profile in Profile::ALL {
            let name = profile.as_str();
            assert_eq!(name.parse::<Profile>().unwrap(), profile);
        }
        assert!("lossy".parse::<Profile>().is_err());
    }
}
