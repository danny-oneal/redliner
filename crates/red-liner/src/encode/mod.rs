//! Request encoding: turns a picked scenario into a volley `Target`.

pub mod graphql;
pub mod headers;

use std::sync::Mutex;

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use volley::{Target, Targeter};

pub use graphql::GraphqlEncoder;

use crate::config::Scenario;
use crate::mix::WeightedMix;

/// Failures while building headers or encoding a request.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("environment variable {0} is not set (required by target.auth)")]
    MissingEnv(String),
    #[error("invalid header: {0}")]
    InvalidHeader(String),
    #[error("encode body: {0}")]
    Body(String),
}

/// A protocol encoder: turns a picked scenario into a volley `Target`.
pub trait Encoder: Send + Sync {
    fn encode(&self, t: &mut Target, sc: &Scenario) -> Result<(), EncodeError>;
    /// Recovers the operation name from a request's method and URL.
    fn operation(&self, method: &str, url: &str) -> Option<String>;
}

/// Picks a weighted scenario per call and encodes it via the configured
/// protocol encoder. Reproducible from a single seed.
pub struct MixTargeter {
    enc: Box<dyn Encoder>,
    mix: WeightedMix,
    rng: Mutex<ChaCha8Rng>,
}

impl MixTargeter {
    pub fn new(enc: Box<dyn Encoder>, mix: WeightedMix, seed: u64) -> Self {
        Self {
            enc,
            mix,
            rng: Mutex::new(ChaCha8Rng::seed_from_u64(seed)),
        }
    }
}

impl Targeter for MixTargeter {
    fn target(&self, t: &mut Target) -> Result<(), volley::Error> {
        let mut rng = self
            .rng
            .lock()
            .map_err(|_| volley::Error::Build("rng lock poisoned".into()))?;
        let sc = self.mix.pick(&mut *rng);
        self.enc
            .encode(t, sc)
            .map_err(|e| volley::Error::Build(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RequestConfig, Scenario, TargetConfig};

    fn sc(name: &str, w: f64) -> Scenario {
        Scenario {
            name: name.into(),
            weight: w,
            request: RequestConfig {
                query: format!("query {name} {{ x }}"),
                ..Default::default()
            },
        }
    }

    #[test]
    fn targeter_is_reproducible_from_seed() {
        let cfg = TargetConfig {
            url: "http://x/graphql".into(),
            ..Default::default()
        };
        let mk = || {
            MixTargeter::new(
                Box::new(GraphqlEncoder::new(&cfg).unwrap()),
                WeightedMix::new(vec![sc("A", 1.0), sc("B", 1.0)]),
                7,
            )
        };
        let (a, b) = (mk(), mk());
        for _ in 0..50 {
            let (mut ta, mut tb) = (Target::default(), Target::default());
            a.target(&mut ta).unwrap();
            b.target(&mut tb).unwrap();
            assert_eq!(ta.url, tb.url);
        }
    }
}
