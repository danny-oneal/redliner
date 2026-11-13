use rand::{Rng, RngExt};

use crate::config::Scenario;

/// Weighted scenario chooser: cumulative weights + one draw + binary search.
pub struct WeightedMix {
    scenarios: Vec<Scenario>,
    cum: Vec<f64>,
}

impl WeightedMix {
    /// Config validation guarantees `scenarios` is non-empty; an empty mix
    /// picks nothing.
    pub fn new(scenarios: Vec<Scenario>) -> Self {
        let mut total = 0.0;
        let cum = scenarios
            .iter()
            .map(|s| {
                total += s.weight;
                total
            })
            .collect();
        Self { scenarios, cum }
    }

    pub fn pick(&self, rng: &mut impl Rng) -> Option<&Scenario> {
        let total = *self.cum.last()?;
        let x = rng.random::<f64>() * total;
        let i = self
            .cum
            .partition_point(|&c| c <= x)
            .min(self.scenarios.len() - 1);
        self.scenarios.get(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn sc(name: &str, w: f64) -> Scenario {
        Scenario {
            name: name.into(),
            weight: w,
            request: Default::default(),
            matcher: None,
        }
    }

    #[test]
    fn weighted_mix_respects_weights() {
        let mix = WeightedMix::new(vec![sc("a", 90.0), sc("b", 10.0)]);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let a = (0..10_000)
            .filter(|_| mix.pick(&mut rng).unwrap().name == "a")
            .count();
        assert!((8500..=9500).contains(&a), "a picked {a} times");
    }

    #[test]
    fn single_scenario_always_returned() {
        let mix = WeightedMix::new(vec![sc("only", 3.0)]);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        for _ in 0..100 {
            assert_eq!(mix.pick(&mut rng).unwrap().name, "only");
        }
    }

    #[test]
    fn empty_mix_picks_nothing() {
        let mix = WeightedMix::new(Vec::new());
        assert!(mix.pick(&mut ChaCha8Rng::seed_from_u64(1)).is_none());
    }
}
