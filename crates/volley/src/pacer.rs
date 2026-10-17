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

/// Fires hits as a Poisson process with the given mean rate. Seeded for
/// reproducibility; falls back to firing immediately when behind schedule.
#[derive(Debug)]
pub struct PoissonPacer {
    mean_rate: f64,
    rng: std::sync::Mutex<rand_chacha::ChaCha8Rng>,
}

impl PoissonPacer {
    /// `rate` is the mean hits per second; `seed` makes the sequence reproducible.
    pub fn new(rate: f64, seed: u64) -> Self {
        use rand::SeedableRng;
        Self {
            mean_rate: rate,
            rng: std::sync::Mutex::new(rand_chacha::ChaCha8Rng::seed_from_u64(seed)),
        }
    }
}

impl Pacer for PoissonPacer {
    fn pace(&self, elapsed: Duration, hits: u64) -> (Duration, bool) {
        use rand::Rng;
        if self.mean_rate <= 0.0 {
            return (Duration::ZERO, true);
        }
        // Behind schedule: fire immediately.
        if (hits as f64) < self.mean_rate * elapsed.as_secs_f64() {
            return (Duration::ZERO, false);
        }
        let mut u: f64 = {
            let mut rng = self.rng.lock().unwrap_or_else(|e| e.into_inner());
            // Map [0,1) to (0,1].
            1.0 - rng.random::<f64>()
        };
        if u <= 0.0 {
            u = f64::MIN_POSITIVE;
        }
        (Duration::from_secs_f64(-u.ln() / self.mean_rate), false)
    }

    fn rate(&self, _elapsed: Duration) -> f64 {
        self.mean_rate
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

    const POISSON_SEED: u64 = 42;

    #[test]
    fn poisson_mean_converges() {
        let p = PoissonPacer::new(200.0, POISSON_SEED);
        let mut elapsed = Duration::ZERO;
        let mut hits = 0u64;
        while elapsed < Duration::from_secs(10) {
            let (wait, stop) = p.pace(elapsed, hits);
            assert!(!stop);
            elapsed += wait;
            hits += 1;
        }
        let diff = (hits as f64 - 2000.0).abs();
        assert!(diff <= 100.0, "hits = {hits}, want 2000 +/- 5%");
    }

    #[test]
    fn poisson_catches_up() {
        let p = PoissonPacer::new(200.0, POISSON_SEED);
        let (wait, stop) = p.pace(Duration::from_secs(5), 10);
        assert_eq!(wait, Duration::ZERO);
        assert!(!stop);
    }

    #[test]
    fn poisson_zero_rate_stops() {
        let p = PoissonPacer::new(0.0, POISSON_SEED);
        let (_, stop) = p.pace(Duration::from_secs(1), 0);
        assert!(stop);
    }

    #[test]
    fn poisson_rate_is_mean_rate() {
        let p = PoissonPacer::new(123.0, POISSON_SEED);
        assert_eq!(p.rate(Duration::from_secs(7)), 123.0);
    }

    #[test]
    fn poisson_same_seed_is_deterministic() {
        let a = PoissonPacer::new(50.0, 7);
        let b = PoissonPacer::new(50.0, 7);
        let mut elapsed = Duration::ZERO;
        for hits in 0..100u64 {
            let wa = a.pace(elapsed, hits);
            assert_eq!(wa, b.pace(elapsed, hits));
            elapsed += wa.0;
        }
    }
}
