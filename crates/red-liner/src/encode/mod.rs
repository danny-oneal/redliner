//! Request encoding: turns a picked scenario into a volley `Target`.

pub mod graphql;
pub mod headers;
pub mod rest;

use std::sync::Mutex;

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use volley::{Target, Targeter};

pub use graphql::GraphqlEncoder;
pub use rest::RestEncoder;

use crate::config::{Config, Protocol, Scenario};
use crate::data::Pools;
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
    #[error("unknown pool {0}")]
    UnknownPool(String),
    #[error("bad template: {0}")]
    BadTemplate(String),
}

/// A protocol encoder: turns a picked scenario into a volley `Target`.
pub trait Encoder: Send + Sync {
    fn encode(
        &self,
        t: &mut Target,
        sc: &Scenario,
        pools: &Pools,
        rng: &mut ChaCha8Rng,
    ) -> Result<(), EncodeError>;
    /// Recovers the operation name from a request's method and URL.
    fn operation(&self, method: &str, url: &str) -> Option<String>;
}

/// Builds the encoder selected by `target.protocol`.
pub fn new_encoder(cfg: &Config) -> Result<Box<dyn Encoder>, EncodeError> {
    Ok(match cfg.target.protocol {
        Protocol::Graphql => Box::new(GraphqlEncoder::new(&cfg.target)?),
        Protocol::Rest => Box::new(RestEncoder::new(&cfg.target, &cfg.scenarios)?),
    })
}

/// Picks a weighted scenario per call and encodes it via the configured
/// protocol encoder. Reproducible from a single seed.
pub struct MixTargeter {
    enc: Box<dyn Encoder>,
    mix: WeightedMix,
    pools: Pools,
    rng: Mutex<ChaCha8Rng>,
}

impl MixTargeter {
    pub fn new(enc: Box<dyn Encoder>, mix: WeightedMix, pools: Pools, seed: u64) -> Self {
        Self {
            enc,
            mix,
            pools,
            rng: Mutex::new(ChaCha8Rng::seed_from_u64(seed)),
        }
    }

    /// Recovers the operation name for a fired request, for report attribution.
    pub fn operation(&self, method: &str, url: &str) -> Option<String> {
        self.enc.operation(method, url)
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
            .encode(t, sc, &self.pools, &mut *rng)
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
                crate::data::Pools::new(),
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
