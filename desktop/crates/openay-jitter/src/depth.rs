//! Adaptive depth controller for the jitter prebuffer.
//!
//! The engine's prebuffer target is adaptive, per the plan's xrun policy:
//! an underrun (the buffer ran dry while streaming) raises the target by
//! [`DepthParams::rise_ms`] (2 ms) toward [`MAX_PREBUFFER_MS`] (20 ms), and
//! every fully elapsed [`DepthParams::decay_window`] (60 s) of streaming
//! without an underrun lowers it by [`DepthParams::decay_step_ms`] (1 ms)
//! back toward the configured base depth — the user setting, which is both
//! the decay floor and the initial value.
//!
//! # Clock injection
//!
//! The controller needs wall-clock *durations* only (never timestamps), so
//! the clock is a single-method [`Clock`] trait. [`RealClock`] measures from
//! a process-wide monotonic anchor; tests inject a fake clock their test body
//! advances deterministically, so every decay/rise scenario is exact and
//! non-flaky.
//!
//! # Windowing model
//!
//! `on_tick` is called periodically (≈200 ms in the engine). The controller
//! tracks the start of the current underrun-free window:
//!
//! - While **stopped** (`on_tick(false)`), it is inert: the target does not
//!   change and the window is discarded, so no decay accrues across a stop;
//!   if an underrun happened previously it does not count toward the new
//!   stream. Accrual restarts from the first `on_tick(true)` after a stop.
//! - While **running**, each completely elapsed window without an underrun
//!   earns exactly one step toward the base. Multiple fully elapsed windows
//!   accumulated between two ticks each earn their step (a 180 s clean gap
//!   with the default 60 s window decays three steps at the next tick).
//! - An underrun recorded mid-window ([`on_underrun`](Self::on_underrun))
//!   raises the target and restarts the window at that instant: the streak
//!   is zeroed, so the next decay needs a full clean window *after* the
//!   underrun.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::{MAX_PREBUFFER_MS, MIN_PREBUFFER_MS};

/// A clock readable as an elapsed [`Duration`].
///
/// The value is arbitrary but must be monotonic (the controller only
/// measures *differences*); [`RealClock`] satisfies that. Tests inject a
/// fake clock whose time the test advances deterministically.
pub trait Clock {
    /// Time elapsed since an arbitrary, fixed, monotonic origin.
    fn now(&self) -> Duration;
}

/// Wall-clock [`Clock`]: `Instant` since a process-wide anchor.
///
/// A `static` anchor makes every instance of [`RealClock`] share one origin,
/// so durations are comparable across instances; `Instant` is monotonic and
/// immune to NTP steps.
pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> Duration {
        static ANCHOR: OnceLock<Instant> = OnceLock::new();
        ANCHOR.get_or_init(Instant::now).elapsed()
    }
}

/// Timing knobs of a [`DepthController`].
///
/// This is the documented override entry point for tests and scenario
/// validation (the integration scenarios shrink the 60 s decay window so a
/// full rise/decay cycle fits in a few seconds of test time). Production
/// uses [`DepthParams::default`] via [`DepthController::new`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DepthParams {
    /// Target increase per underrun, in ms.
    pub rise_ms: f32,
    /// Length of one underrun-free window that earns a decay step.
    pub decay_window: Duration,
    /// Target decrease per fully elapsed clean window, in ms.
    pub decay_step_ms: f32,
}

impl Default for DepthParams {
    fn default() -> Self {
        DepthParams {
            rise_ms: 2.0,
            decay_window: Duration::from_secs(60),
            decay_step_ms: 1.0,
        }
    }
}

impl DepthParams {
    /// Clamp the knobs to safe values (non-negative steps, hold ≥ 1 ms so
    /// the window always advances).
    fn sanitized(self) -> Self {
        let window = if self.decay_window.is_zero() {
            Duration::from_millis(1)
        } else {
            self.decay_window
        };
        DepthParams {
            rise_ms: self.rise_ms.max(0.0),
            decay_window: window,
            decay_step_ms: self.decay_step_ms.max(0.0),
        }
    }
}

/// Adaptive prebuffer-depth controller.
///
/// The target is a live value in `[base_ms, MAX_PREBUFFER_MS]`:
///
/// - [`on_underrun`](Self::on_underrun): `target += rise_ms`, clamped to
///   [`MAX_PREBUFFER_MS`], and the underrun-free streak resets (the window
///   restarts at the current time).
/// - [`on_tick`](Self::on_tick): while running, one decay step per fully
///   elapsed clean window, floored at the configured `base_ms`; while
///   stopped, inert and the window is discarded.
///
/// `base_ms` is the user's setting: the initial target and the decay floor.
/// It is clamped into `[MIN_PREBUFFER_MS, MAX_PREBUFFER_MS]` on construction.
///
/// # Example
///
/// ```
/// use openay_jitter::{DepthController, DepthParams};
/// use std::time::Duration;
///
/// let mut c = DepthController::new(10.0);
/// c.on_underrun();
/// assert_eq!(c.target_ms(), 12.0, "one underrun raises by the default 2 ms");
///
/// // Shrink the window for tests: one full 100 ms clean window earns −1 ms.
/// let mut c = DepthController::with_params(
///     openay_jitter::RealClock,
///     10.0,
///     DepthParams { rise_ms: 2.0, decay_window: Duration::from_millis(100),
///                   decay_step_ms: 1.0, ..DepthParams::default() },
/// );
/// ```
pub struct DepthController<C: Clock = RealClock> {
    clock: C,
    params: DepthParams,
    /// Decay floor and initial target (the user's base setting).
    base_ms: f32,
    /// Current effective target, `base_ms <= target_ms <= MAX_PREBUFFER_MS`.
    target_ms: f32,
    /// Start of the current underrun-free window; `None` while stopped or
    /// before the first running tick.
    window_start: Option<Duration>,
}

impl DepthController<RealClock> {
    /// Create a controller with the default timings ([`DepthParams::default`]:
    /// +2 ms per underrun, −1 ms per fully elapsed 60 s clean window).
    pub fn new(base_ms: f32) -> Self {
        Self::with_params(RealClock, base_ms, DepthParams::default())
    }
}

impl<C: Clock> DepthController<C> {
    /// Create a controller with explicit timings.
    ///
    /// Intended for tests and scenario validation, which shrink the 60 s
    /// [`DepthParams::decay_window`] (and tune the step sizes) so a full
    /// rise/decay cycle fits in the test budget. Production code should use
    /// [`DepthController::new`]. `base_ms` is clamped into
    /// `[MIN_PREBUFFER_MS, MAX_PREBUFFER_MS]`; params are sanitized
    /// (non-negative steps, hold ≥ 1 ms).
    pub fn with_params(clock: C, base_ms: f32, params: DepthParams) -> Self {
        let base_ms = base_ms.clamp(MIN_PREBUFFER_MS, MAX_PREBUFFER_MS);
        DepthController {
            clock,
            params: params.sanitized(),
            base_ms,
            target_ms: base_ms,
            window_start: None,
        }
    }

    /// Record one underrun: raise the target by `rise_ms` (clamped to
    /// [`MAX_PREBUFFER_MS`]) and reset the underrun-free streak, so the next
    /// decay requires a full clean window *after* this underrun.
    pub fn on_underrun(&mut self) {
        self.target_ms = (self.target_ms + self.params.rise_ms).min(MAX_PREBUFFER_MS);
        self.window_start = Some(self.clock.now());
    }

    /// Tick the controller (≈200 ms in the engine) and return the effective
    /// target in ms.
    ///
    /// While `running`, applies one decay step per fully elapsed clean
    /// window since the last underrun (or since the last tick that advanced
    /// the window), floored at the configured base. While stopped, nothing
    /// changes and the window is discarded (accrual restarts on the next
    /// running tick).
    pub fn on_tick(&mut self, running: bool) -> f32 {
        if !running {
            self.window_start = None;
            return self.target_ms;
        }
        let now = self.clock.now();
        let mut window_start = match self.window_start {
            Some(start) => start,
            None => {
                // First running tick: arm the window at the current time;
                // time between Start and the first tick never counts.
                self.window_start = Some(now);
                return self.target_ms;
            }
        };
        while now.saturating_sub(window_start) >= self.params.decay_window {
            self.target_ms = (self.target_ms - self.params.decay_step_ms).max(self.base_ms);
            match window_start.checked_add(self.params.decay_window) {
                Some(next) => window_start = next,
                // Unreachable with sanitized params (window ≥ 1 ms); leave
                // the window at the current time rather than looping forever.
                None => break,
            }
        }
        self.window_start = Some(window_start);
        self.target_ms
    }

    /// The current effective target in ms
    /// (`base_ms <= target_ms <= MAX_PREBUFFER_MS`).
    pub fn target_ms(&self) -> f32 {
        self.target_ms
    }

    /// The configured base depth (the user setting: decay floor and start).
    pub fn base_ms(&self) -> f32 {
        self.base_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Deterministic fake clock the tests advance by hand. `Rc` so the
    /// controller's clone and the test's handle share one time cell.
    #[derive(Clone)]
    struct FakeClock(Rc<RefCell<Duration>>);

    impl FakeClock {
        fn new() -> Self {
            FakeClock(Rc::new(RefCell::new(Duration::ZERO)))
        }

        fn advance(&self, d: Duration) {
            *self.0.borrow_mut() += d;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            *self.0.borrow()
        }
    }

    fn controller_60s(base: f32) -> (FakeClock, DepthController<FakeClock>) {
        let clock = FakeClock::new();
        let c = DepthController::with_params(clock.clone(), base, DepthParams::default());
        (clock, c)
    }

    fn controller_100ms(base: f32) -> (FakeClock, DepthController<FakeClock>) {
        let clock = FakeClock::new();
        let c = DepthController::with_params(
            clock.clone(),
            base,
            DepthParams {
                rise_ms: 2.0,
                decay_window: Duration::from_millis(100),
                decay_step_ms: 1.0,
            },
        );
        (clock, c)
    }

    #[test]
    fn starts_at_base_and_underrun_raises_by_rise() {
        let (_clock, mut c) = controller_60s(10.0);
        assert_eq!(c.target_ms(), 10.0, "initial target is the base");
        c.on_underrun();
        assert_eq!(c.target_ms(), 12.0, "+2 ms per underrun by default");
        c.on_underrun();
        assert_eq!(c.target_ms(), 14.0);
    }

    #[test]
    fn underrun_clamps_at_ceiling() {
        let (_clock, mut c) = controller_60s(19.0);
        c.on_underrun();
        assert_eq!(c.target_ms(), 20.0, "19 + 2 clamps to the 20 ms ceiling");
        c.on_underrun();
        assert_eq!(c.target_ms(), 20.0, "ceiling holds");
    }

    #[test]
    fn base_is_clamped_into_range() {
        let (_clock, c) = controller_60s(50.0);
        assert_eq!(c.base_ms(), MAX_PREBUFFER_MS);
        assert_eq!(c.target_ms(), MAX_PREBUFFER_MS);
        let (_clock, c) = controller_60s(0.5);
        assert_eq!(c.base_ms(), MIN_PREBUFFER_MS);
        assert_eq!(c.target_ms(), MIN_PREBUFFER_MS);
    }

    #[test]
    fn no_decay_before_window_elapses() {
        let (clock, mut c) = controller_60s(10.0);
        c.on_underrun(); // raise to 12 and arm the window at t=0
        assert_eq!(c.on_tick(true), 12.0, "first tick just arms the window");
        clock.advance(Duration::from_secs(59));
        assert_eq!(c.on_tick(true), 12.0, "59 s < 60 s window: no decay yet");
        clock.advance(Duration::from_secs(1));
        assert_eq!(c.on_tick(true), 11.0, "the 60th second completes the window");
    }

    #[test]
    fn exact_one_step_per_fully_elapsed_window() {
        let (clock, mut c) = controller_60s(10.0);
        for _ in 0..3 {
            c.on_underrun(); // target 16, window start t=0
        }
        assert_eq!(c.target_ms(), 16.0);
        clock.advance(Duration::from_secs(60));
        assert_eq!(c.on_tick(true), 15.0, "first clean window: −1");
        clock.advance(Duration::from_secs(60));
        assert_eq!(c.on_tick(true), 14.0, "second consecutive clean window: −1 again");
        // A long tick spanning several windows earns one step per window.
        clock.advance(Duration::from_secs(180));
        assert_eq!(c.on_tick(true), 11.0, "3 more windows → exact −1 each");
        clock.advance(Duration::from_secs(360));
        assert_eq!(c.on_tick(true), 10.0, "floor holds at the base, never below");
    }

    #[test]
    fn decay_never_goes_below_base_floor() {
        let (clock, mut c) = controller_100ms(7.0);
        c.on_underrun();
        c.on_underrun();
        assert_eq!(c.target_ms(), 11.0, "raised above the base");
        for _ in 0..50 {
            clock.advance(Duration::from_millis(100));
            c.on_tick(true);
        }
        assert_eq!(c.target_ms(), 7.0, "decay floors at the configured base");
    }

    #[test]
    fn underrun_mid_window_resets_the_streak() {
        let (clock, mut c) = controller_100ms(10.0);
        c.on_tick(true); // arm at t=0
        clock.advance(Duration::from_millis(30));
        c.on_underrun(); // t=30: +2 and the window restarts at t=30
        assert_eq!(c.target_ms(), 12.0);
        clock.advance(Duration::from_millis(30));
        // t=60: only 30 ms elapsed since the underrun.
        assert_eq!(c.on_tick(true), 12.0, "streak starts after the underrun, not before");
        clock.advance(Duration::from_millis(70));
        // t=130: a full 100 ms window elapsed since t=30.
        assert_eq!(c.on_tick(true), 11.0, "decay resumes one window after the underrun");
    }

    #[test]
    fn tick_reports_effective_target() {
        let (_clock, mut c) = controller_60s(10.0);
        assert_eq!(c.on_tick(true), 10.0);
        c.on_underrun();
        assert_eq!(c.on_tick(true), 12.0, "on_tick returns the live target");
        assert_eq!(c.target_ms(), 12.0, "getter agrees");
    }

    #[test]
    fn inert_while_stopped() {
        let (clock, mut c) = controller_60s(10.0);
        c.on_underrun();
        assert_eq!(c.target_ms(), 12.0);
        // A long stop changes nothing and discards the window.
        clock.advance(Duration::from_secs(60_000));
        assert_eq!(c.on_tick(false), 12.0, "stopped: no decay");
        assert_eq!(c.target_ms(), 12.0);
    }

    #[test]
    fn accrual_restarts_after_restart() {
        let (clock, mut c) = controller_60s(10.0);
        c.on_underrun(); // target 12, window armed at t=0
        c.on_tick(true);
        clock.advance(Duration::from_secs(30));
        assert_eq!(c.on_tick(true), 12.0, "30 s < 60 s window: no decay");
        // Stop: the partial 30 s window is discarded; stop time never counts.
        clock.advance(Duration::from_secs(50));
        c.on_tick(false);
        clock.advance(Duration::from_secs(61));
        assert_eq!(c.on_tick(false), 12.0, "stop time never accrues");
        // Restart: the window is re-armed at the first running tick; only a
        // full clean window after that decays.
        assert_eq!(c.on_tick(true), 12.0, "restart arms a fresh window at t=141");
        clock.advance(Duration::from_secs(60));
        assert_eq!(c.on_tick(true), 11.0, "accrual resumes after the restart");
    }
}
