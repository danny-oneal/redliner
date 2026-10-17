//! Load-profile to pacer phases (Go `buildPhases`).

use std::sync::Arc;
use std::time::Duration;

use volley::{ConstantPacer, LinearPacer, Pacer, PoissonPacer};

use crate::config::LoadConfig;

/// One measured segment of the run.
pub struct Phase {
    pub name: String,
    pub pacer: Arc<dyn Pacer>,
    pub duration: Duration,
    /// Nominal rate (rps), used for logging and warmup sizing.
    pub rate: f64,
}

fn per_sec(rate: f64) -> ConstantPacer {
    ConstantPacer { freq: rate as u64, per: Duration::from_secs(1) }
}

pub fn build_phases(load: &LoadConfig) -> anyhow::Result<Vec<Phase>> {
    let dur = load.duration.as_std();
    match load.profile.as_str() {
        "constant" => Ok(vec![Phase {
            name: "constant".into(),
            pacer: Arc::new(per_sec(load.rate)),
            duration: dur,
            rate: load.rate,
        }]),
        "poisson" => Ok(vec![Phase {
            name: "poisson".into(),
            pacer: Arc::new(PoissonPacer::new(load.rate, load.seed as u64)),
            duration: dur,
            rate: load.rate,
        }]),
        "ramp" => {
            let secs = dur.as_secs_f64();
            if secs <= 0.0 {
                anyhow::bail!("ramp profile needs load.duration > 0");
            }
            Ok(vec![Phase {
                name: "ramp".into(),
                pacer: Arc::new(LinearPacer {
                    start_freq: load.ramp.from.trunc(),
                    slope_per_sec: (load.ramp.to - load.ramp.from) / secs,
                }),
                duration: dur,
                rate: load.ramp.to,
            }])
        }
        "steps" => Ok(load
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| Phase {
                name: format!("step-{}({} rps)", i + 1, s.rate),
                pacer: Arc::new(per_sec(s.rate)),
                duration: s.duration.as_std(),
                rate: s.rate,
            })
            .collect()),
        other => anyhow::bail!("unsupported load profile {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GoDuration, RampConfig, Step};

    fn step(rate: f64, secs: u64) -> Step {
        Step { rate, duration: GoDuration(Duration::from_secs(secs)) }
    }

    #[test]
    fn build_phases_steps_expands() {
        let load = LoadConfig {
            profile: "steps".into(),
            steps: vec![step(10.0, 1), step(20.0, 2), step(30.0, 3)],
            ..Default::default()
        };
        let p = build_phases(&load).unwrap();
        assert_eq!(p.len(), 3);
        for (i, (r, d)) in [(10.0, 1), (20.0, 2), (30.0, 3)].iter().enumerate() {
            assert_eq!(p[i].rate, *r);
            assert_eq!(p[i].duration, Duration::from_secs(*d));
        }
        assert_eq!(p[0].name, "step-1(10 rps)");

        let poisson = LoadConfig {
            profile: "poisson".into(),
            rate: 5.0,
            duration: GoDuration(Duration::from_secs(1)),
            ..Default::default()
        };
        assert_eq!(build_phases(&poisson).unwrap().len(), 1);

        let ramp = LoadConfig {
            profile: "ramp".into(),
            ramp: RampConfig { from: 1.0, to: 11.0 },
            duration: GoDuration(Duration::from_secs(10)),
            ..Default::default()
        };
        let p = build_phases(&ramp).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].rate, 11.0);

        let bad = LoadConfig { profile: "ramp".into(), ..Default::default() };
        assert!(build_phases(&bad).is_err());
    }
}
