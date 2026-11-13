//! Attack pacing and target generation, modeled on vegeta's `Pacer`/`Targeter`.

use std::time::Duration;

use crate::{Error, Target};

/// Controls the rate at which hits are fired.
pub trait Pacer: Send + Sync {
    /// Returns `(wait, stop)` for hit number `hits` (0-based) at `elapsed`
    /// into the attack. `stop` ends the attack. Otherwise the hit fires when
    /// `wait` is zero; a nonzero `wait` is slept and the pacer asked again.
    /// So the answer must be idempotent: the same `(elapsed, hits)` gets the
    /// same answer, and `hits` never decreases.
    fn pace(&self, elapsed: Duration, hits: u64) -> (Duration, bool);

    /// The instantaneous hit rate (hits per second) at `elapsed`.
    fn rate(&self, elapsed: Duration) -> f64;
}

/// Fills in one request per hit.
pub trait Targeter: Send + Sync {
    /// `seq` is the 0-based hit index this request is for.
    fn target(&self, seq: u64, t: &mut Target) -> Result<(), Error>;
}

/// Fires `freq` hits every `per`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstantPacer {
    pub freq: u64,
    pub per: Duration,
}

impl ConstantPacer {
    /// Fires `rate` hits per second, fractions included, at a resolution of
    /// 10⁻⁶ hits/s (`rate` micro-hits every 10⁶ s, so an integral rate keeps
    /// exactly the `{ freq: rate, per: 1s }` schedule). A NaN, zero or negative
    /// rate gives `freq: 0`, which stops at once.
    pub fn from_rate(rate: f64) -> Self {
        const SCALE: u64 = 1_000_000;
        Self { freq: (rate * SCALE as f64).round() as u64, per: Duration::from_secs(SCALE) }
    }
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

/// Fires hits as a Poisson process with the given mean rate: the first hit at
/// once, then exponentially distributed gaps from a seeded RNG, so the
/// schedule is reproducible. A hit whose time has passed (the attack fell
/// behind) is due at once, so the attack catches up.
#[derive(Debug)]
pub struct PoissonPacer {
    mean_rate: f64,
    schedule: std::sync::Mutex<PoissonSchedule>,
}

/// The draws so far: hit number `hit` is due at `next`.
#[derive(Debug)]
struct PoissonSchedule {
    rng: rand_chacha::ChaCha8Rng,
    hit: u64,
    next: Duration,
}

impl PoissonPacer {
    /// `rate` is the mean hits per second; `seed` makes the sequence reproducible.
    pub fn new(rate: f64, seed: u64) -> Self {
        use rand::SeedableRng;
        Self {
            mean_rate: rate,
            schedule: std::sync::Mutex::new(PoissonSchedule {
                rng: rand_chacha::ChaCha8Rng::seed_from_u64(seed),
                hit: 0,
                next: Duration::ZERO,
            }),
        }
    }
}

impl Pacer for PoissonPacer {
    fn pace(&self, elapsed: Duration, hits: u64) -> (Duration, bool) {
        use rand::RngExt;
        if !(self.mean_rate > 0.0 && self.mean_rate.is_finite()) {
            return (Duration::ZERO, true);
        }
        let mut s = self.schedule.lock().unwrap_or_else(|e| e.into_inner());
        while s.hit < hits {
            // Map [0,1) to (0,1], so the log is finite.
            let u = 1.0 - s.rng.random::<f64>();
            let gap = Duration::try_from_secs_f64(-u.ln() / self.mean_rate);
            match gap.ok().and_then(|g| s.next.checked_add(g)) {
                Some(next) => s.next = next,
                None => return (Duration::ZERO, true),
            }
            s.hit += 1;
        }
        (s.next.saturating_sub(elapsed), false)
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

    /// When each hit a pacer fires in `dur` goes off, driving it like the
    /// attack loop does: a nonzero wait is slept and the pacer asked again, a
    /// zero wait fires a hit, and a wait reaching the deadline ends the run.
    fn fire_times(p: &impl Pacer, dur: Duration) -> Vec<Duration> {
        let mut elapsed = Duration::ZERO;
        let mut fired = Vec::new();
        loop {
            let (wait, stop) = p.pace(elapsed, fired.len() as u64);
            if stop || elapsed >= dur {
                break;
            }
            if wait.is_zero() {
                fired.push(elapsed);
                continue;
            }
            if wait >= dur - elapsed {
                break;
            }
            elapsed += wait;
        }
        fired
    }

    /// Hits a pacer fires in `secs`, driving it like the attack loop does.
    fn hits_in(p: &impl Pacer, secs: u64) -> u64 {
        fire_times(p, Duration::from_secs(secs)).len() as u64
    }

    #[test]
    fn from_rate_fractional_paces_exactly() {
        // The first hit is at 1/rate; the one due exactly at the end never fires.
        assert_eq!(hits_in(&ConstantPacer::from_rate(2.5), 10), 24);
        assert_eq!(hits_in(&ConstantPacer::from_rate(0.5), 10), 4);
        assert!((ConstantPacer::from_rate(2.7).rate(Duration::ZERO) - 2.7).abs() < 1e-9);
    }

    #[test]
    fn from_rate_integral_matches_legacy_schedule() {
        for rate in [100u64, 3] {
            let new = ConstantPacer::from_rate(rate as f64);
            let old = ConstantPacer { freq: rate, per: Duration::from_secs(1) };
            let mut elapsed = Duration::ZERO;
            for hits in 0..1000u64 {
                let got = new.pace(elapsed, hits);
                assert_eq!(got, old.pace(elapsed, hits), "rate {rate} hit {hits}");
                elapsed += got.0;
            }
        }
    }

    #[test]
    fn from_rate_nan_stops() {
        assert!(ConstantPacer::from_rate(f64::NAN).pace(Duration::ZERO, 0).1);
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
        let hits = hits_in(&PoissonPacer::new(200.0, POISSON_SEED), 10);
        let diff = (hits as f64 - 2000.0).abs();
        assert!(diff <= 100.0, "hits = {hits}, want 2000 +/- 5%");
    }

    #[test]
    fn poisson_counts_are_poisson_dispersed() {
        // 100 rps for 200 s. Per-second counts of a Poisson process have a
        // variance equal to their mean (index of dispersion ~1); a pacer that
        // hugs the mean line gives ~0.
        let p = PoissonPacer::new(100.0, POISSON_SEED);
        let mut counts = [0u32; 200];
        for t in fire_times(&p, Duration::from_secs(200)) {
            counts[t.as_secs() as usize] += 1;
        }
        let n = counts.len() as f64;
        let mean = counts.iter().map(|&c| f64::from(c)).sum::<f64>() / n;
        let var = counts.iter().map(|&c| (f64::from(c) - mean).powi(2)).sum::<f64>() / (n - 1.0);
        assert!((mean - 100.0).abs() < 3.0, "mean {mean}");
        let dispersion = var / mean;
        assert!((0.7..1.3).contains(&dispersion), "index of dispersion {dispersion}");
    }

    #[test]
    fn poisson_non_finite_rate_stops() {
        for rate in [f64::NAN, f64::INFINITY, -1.0] {
            let (_, stop) = PoissonPacer::new(rate, POISSON_SEED).pace(Duration::ZERO, 0);
            assert!(stop, "rate {rate}");
        }
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
            // Asking again before the hit fires gives the same answer.
            assert_eq!(wa, a.pace(elapsed, hits));
            elapsed += wa.0;
        }
    }
}
