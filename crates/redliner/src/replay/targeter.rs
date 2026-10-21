//! Adapters that replay a [`Schedule`] through the volley attacker.

use std::sync::Arc;
use std::time::Duration;

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use volley::{Pacer, Target, Targeter};

use super::schedule::Schedule;
use crate::config::Scenario;
use crate::data::Pools;
use crate::encode::Encoder;

/// Fires each hit at its scheduled offset from the start of the attack.
pub struct SchedulePacer {
    schedule: Arc<Schedule>,
}

impl SchedulePacer {
    pub fn new(schedule: Arc<Schedule>) -> Self {
        Self { schedule }
    }
}

impl Pacer for SchedulePacer {
    fn pace(&self, elapsed: Duration, hits: u64) -> (Duration, bool) {
        let next = usize::try_from(hits)
            .ok()
            .and_then(|i| self.schedule.hits.get(i));
        match next {
            // A late hit has no wait and fires immediately.
            Some(hit) => (hit.at.saturating_sub(elapsed), false),
            None => (Duration::ZERO, true),
        }
    }

    fn rate(&self, _elapsed: Duration) -> f64 {
        let span = self.schedule.span().as_secs_f64();
        if span == 0.0 {
            return 0.0;
        }
        self.schedule.len() as f64 / span
    }
}

/// Encodes the scenario each scheduled hit was matched to.
///
/// Every call seeds its own rng from the hit index, so the bytes for a given
/// `seq` do not depend on call order or worker concurrency, and no lock is
/// needed.
pub struct ScheduleTargeter {
    enc: Box<dyn Encoder>,
    scenarios: Vec<Scenario>,
    pools: Pools,
    schedule: Arc<Schedule>,
    seed: u64,
}

impl ScheduleTargeter {
    pub fn new(
        enc: Box<dyn Encoder>,
        scenarios: Vec<Scenario>,
        pools: Pools,
        schedule: Arc<Schedule>,
        seed: u64,
    ) -> Self {
        Self {
            enc,
            scenarios,
            pools,
            schedule,
            seed,
        }
    }

    fn scenario(&self, seq: u64) -> Option<&Scenario> {
        let hit = self.schedule.hits.get(usize::try_from(seq).ok()?)?;
        self.scenarios.get(hit.scenario)
    }

    /// The scenario name hit `seq` replays, for report attribution.
    pub fn operation(&self, seq: u64) -> Option<&str> {
        self.scenario(seq).map(|sc| sc.name.as_str())
    }
}

impl Targeter for ScheduleTargeter {
    fn target(&self, seq: u64, t: &mut Target) -> Result<(), volley::Error> {
        let sc = self
            .scenario(seq)
            .ok_or_else(|| volley::Error::Build(format!("replay: hit {seq} beyond schedule")))?;
        let mut rng = ChaCha8Rng::seed_from_u64(self.seed);
        rng.set_stream(seq);
        self.enc
            .encode(t, sc, &self.pools, &mut rng)
            .map_err(|e| volley::Error::Build(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RequestConfig, TargetConfig};
    use crate::data::Pool;
    use crate::encode::GraphqlEncoder;
    use crate::replay::schedule::ScheduledHit;
    use volley::Target;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn schedule(hits: &[(Duration, usize)]) -> Arc<Schedule> {
        Arc::new(Schedule {
            hits: hits
                .iter()
                .map(|&(at, scenario)| ScheduledHit { at, scenario })
                .collect(),
        })
    }

    #[test]
    fn pacer_waits_for_each_scheduled_hit() {
        let p = SchedulePacer::new(schedule(&[
            (ms(0), 0),
            (ms(100), 0),
            (ms(100), 0),
            (Duration::from_secs(1), 0),
        ]));
        assert_eq!(p.pace(Duration::ZERO, 0), (Duration::ZERO, false));
        assert_eq!(p.pace(ms(50), 1), (ms(50), false));
        assert_eq!(p.pace(ms(100), 2), (Duration::ZERO, false));
        assert_eq!(p.pace(Duration::from_secs(2), 3), (Duration::ZERO, false));
        assert!(p.pace(Duration::from_secs(2), 4).1);
    }

    #[test]
    fn pacer_rate_is_hits_over_span() {
        let p = SchedulePacer::new(schedule(&[(ms(0), 0), (ms(500), 0), (ms(1000), 0)]));
        assert_eq!(p.rate(Duration::ZERO), 3.0);
        let flat = SchedulePacer::new(schedule(&[(ms(0), 0), (ms(0), 0)]));
        assert_eq!(flat.rate(Duration::ZERO), 0.0);
    }

    fn scenario(name: &str, var: &str) -> Scenario {
        Scenario {
            name: name.into(),
            weight: 1.0,
            request: RequestConfig {
                query: format!("query {name} {{ x }}"),
                variables: [("ids".to_string(), serde_json::json!(var))].into(),
                ..Default::default()
            },
            matcher: None,
        }
    }

    fn targeter(sched: Arc<Schedule>, seed: u64) -> ScheduleTargeter {
        let enc = GraphqlEncoder::new(&TargetConfig {
            url: "http://h/graphql".into(),
            ..Default::default()
        })
        .unwrap();
        let pools: Pools = [(
            "ids".to_string(),
            Pool::Range {
                min: 1,
                max: 1_000_000,
            },
        )]
        .into();
        ScheduleTargeter::new(
            Box::new(enc),
            vec![scenario("A", "{{ids}}"), scenario("B", "{{ids}}")],
            pools,
            sched,
            seed,
        )
    }

    fn body(t: &ScheduleTargeter, seq: u64) -> Vec<u8> {
        let mut target = Target::default();
        t.target(seq, &mut target).unwrap();
        target.body
    }

    #[test]
    fn targeter_is_deterministic_per_seq() {
        let sched = schedule(&[(ms(0), 0); 8]);
        let (a, b) = (targeter(sched.clone(), 42), targeter(sched, 42));
        let (a5, a2) = (body(&a, 5), body(&a, 2));
        let (b2, b5) = (body(&b, 2), body(&b, 5));
        assert_eq!(a5, b5);
        assert_eq!(a2, b2);
        assert_ne!(a5, a2);
    }

    #[test]
    fn targeter_uses_scheduled_scenario() {
        let t = targeter(schedule(&[(ms(0), 1), (ms(1), 0)]), 1);
        let v: serde_json::Value = serde_json::from_slice(&body(&t, 0)).unwrap();
        assert_eq!(v["operationName"], "B");
        let v: serde_json::Value = serde_json::from_slice(&body(&t, 1)).unwrap();
        assert_eq!(v["operationName"], "A");
        assert_eq!(t.operation(0), Some("B"));
        assert_eq!(t.operation(1), Some("A"));
        assert_eq!(t.operation(2), None);
    }

    #[test]
    fn targeter_rejects_seq_beyond_schedule() {
        let t = targeter(schedule(&[(ms(0), 0)]), 1);
        let err = t.target(1, &mut Target::default()).unwrap_err();
        assert!(
            matches!(&err, volley::Error::Build(m) if m == "replay: hit 1 beyond schedule"),
            "{err}"
        );
    }
}
