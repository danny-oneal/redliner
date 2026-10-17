//! Attack pacing and target generation, modeled on vegeta's `Pacer`/`Targeter`.

use std::time::Duration;

use crate::{Error, Target};

/// Controls the rate at which hits are fired.
pub trait Pacer: Send + Sync {
    /// Returns `(wait, stop)`: how long to sleep before the next hit, and
    /// whether the attack should stop (`true` ends it).
    fn pace(&self, elapsed: Duration, hits: u64) -> (Duration, bool);

    /// The instantaneous hit rate (hits per second) at `elapsed`.
    fn rate(&self, elapsed: Duration) -> f64;
}

/// Fills in one request per hit.
pub trait Targeter: Send + Sync {
    fn target(&self, t: &mut Target) -> Result<(), Error>;
}

/// Fires `freq` hits every `per`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstantPacer {
    pub freq: u64,
    pub per: Duration,
}

impl Pacer for ConstantPacer {
    fn pace(&self, elapsed: Duration, hits: u64) -> (Duration, bool) {
        if self.freq == 0 || self.per.is_zero() {
            return (Duration::ZERO, true);
        }
        let per_ns = self.per.as_nanos();
        // Hits that should have been fired by now: freq * elapsed / per.
        let expected = u128::from(self.freq) * elapsed.as_nanos() / per_ns;
        if u128::from(hits) < expected {
            return (Duration::ZERO, false);
        }
        // Time at which the next hit is scheduled: (hits + 1) * per / freq.
        let next_ns = (u128::from(hits) + 1) * per_ns / u128::from(self.freq);
        let wait_ns = next_ns.saturating_sub(elapsed.as_nanos());
        match u64::try_from(wait_ns) {
            Ok(ns) => (Duration::from_nanos(ns), false),
            Err(_) => (Duration::ZERO, true),
        }
    }

    fn rate(&self, _elapsed: Duration) -> f64 {
        if self.freq == 0 || self.per.is_zero() {
            return 0.0;
        }
        self.freq as f64 / self.per.as_secs_f64()
    }
}

/// Ramps the rate linearly: `start_freq + slope_per_sec * t`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearPacer {
    pub start_freq: f64,
    pub slope_per_sec: f64,
}

impl Pacer for LinearPacer {
    fn pace(&self, elapsed: Duration, hits: u64) -> (Duration, bool) {
        let (a, s) = (self.start_freq, self.slope_per_sec);
        let t = elapsed.as_secs_f64();
        // Expected hits by now: integral of the rate, a*t + s*t^2/2.
        let expected = a * t + 0.5 * s * t * t;
        if (hits as f64) < expected {
            return (Duration::ZERO, false);
        }
        // Solve a*x + s*x^2/2 = hits + 1 for the time x of the next hit.
        let target = hits as f64 + 1.0;
        let next = if s.abs() < f64::EPSILON {
            if a <= 0.0 {
                return (Duration::ZERO, true);
            }
            target / a
        } else {
            let disc = a * a + 2.0 * s * target;
            if disc < 0.0 {
                return (Duration::ZERO, true);
            }
            let x = (-a + disc.sqrt()) / s;
            if !x.is_finite() || x <= 0.0 {
                return (Duration::ZERO, true);
            }
            x
        };
        if !next.is_finite() || next >= Duration::MAX.as_secs_f64() {
            return (Duration::ZERO, true);
        }
        let wait = (next - t).max(0.0);
        (Duration::from_secs_f64(wait), false)
    }

    fn rate(&self, elapsed: Duration) -> f64 {
        (self.start_freq + self.slope_per_sec * elapsed.as_secs_f64()).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn constant_pacer_targets_rate() {
        let p = ConstantPacer { freq: 100, per: Duration::from_secs(1) };
        let mut elapsed = Duration::ZERO;
        let mut hits = 0u64;
        while elapsed < Duration::from_secs(1) {
            let (wait, stop) = p.pace(elapsed, hits);
            assert!(!stop);
            elapsed += wait;
            hits += 1;
        }
        assert!((99..=101).contains(&hits), "hits = {hits}");
        assert!((p.rate(Duration::ZERO) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn linear_pacer_increases() {
        let p = LinearPacer { start_freq: 50.0, slope_per_sec: 45.0 };
        assert!(p.rate(Duration::from_secs(10)) > p.rate(Duration::ZERO));
        assert!((p.rate(Duration::from_secs(10)) - 500.0).abs() < 1e-9);
    }

    #[test]
    fn linear_pacer_hits_match_integral() {
        let p = LinearPacer { start_freq: 50.0, slope_per_sec: 45.0 };
        let mut elapsed = Duration::ZERO;
        let mut hits = 0u64;
        while elapsed < Duration::from_secs(10) {
            let (wait, stop) = p.pace(elapsed, hits);
            assert!(!stop);
            elapsed += wait;
            hits += 1;
        }
        // integral = 50*10 + 0.5*45*100 = 2750
        assert!((2745..=2755).contains(&hits), "hits = {hits}");
    }

    #[test]
    fn zero_freq_stops() {
        let p = ConstantPacer { freq: 0, per: Duration::from_secs(1) };
        assert!(p.pace(Duration::ZERO, 0).1);
        assert_eq!(p.rate(Duration::ZERO), 0.0);
    }

    #[test]
    fn traits_are_object_safe() {
        let _p: std::sync::Arc<dyn Pacer> =
            std::sync::Arc::new(ConstantPacer { freq: 1, per: Duration::from_secs(1) });
        fn _t(_: &dyn Targeter) {}
    }
}
