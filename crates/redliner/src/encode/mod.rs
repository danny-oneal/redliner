//! Request encoding: turns a picked scenario into a volley `Target`.

pub mod graphql;
pub mod headers;
pub mod rest;

use std::sync::Arc;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC};
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

/// What a value spliced into a URL is percent-encoded with: everything but
/// the RFC 3986 unreserved characters `A-Za-z0-9-._~` (as Go's
/// `url.QueryEscape`), so it stays one path segment or one query value.
pub const URL_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// A protocol encoder: turns a picked scenario into a volley `Target`.
pub trait Encoder: Send + Sync {
    fn encode(
        &self,
        t: &mut Target,
        sc: &Scenario,
        pools: &Pools,
        rng: &mut ChaCha8Rng,
    ) -> Result<(), EncodeError>;
}

/// Builds the encoder selected by `target.protocol`.
pub fn new_encoder(cfg: &Config) -> Result<Box<dyn Encoder>, EncodeError> {
    Ok(match cfg.target.protocol {
        Protocol::Graphql => Box::new(GraphqlEncoder::new(&cfg.target, &cfg.scenarios)?),
        Protocol::Rest => Box::new(RestEncoder::new(&cfg.target, &cfg.scenarios)?),
    })
}

/// Picks a weighted scenario per hit and encodes it via the configured
/// protocol encoder.
///
/// Each hit draws from its own rng stream, keyed by the attack it belongs to
/// and its `seq`, so a seed reproduces every request whatever the call order
/// or worker concurrency, the scenario a hit fired can be recomputed for
/// attribution, and no lock is needed.
pub struct MixTargeter {
    shared: Arc<Shared>,
    attack: u64,
}

struct Shared {
    enc: Box<dyn Encoder>,
    mix: WeightedMix,
    pools: Pools,
    seed: u64,
}

impl MixTargeter {
    pub fn new(enc: Box<dyn Encoder>, mix: WeightedMix, pools: Pools, seed: u64) -> Self {
        Self {
            shared: Arc::new(Shared { enc, mix, pools, seed }),
            attack: 0,
        }
    }

    /// This targeter for attack `n` of a run (the warmup, then each phase).
    /// `seq` restarts at 0 in every attack; the attack number keeps their
    /// streams apart, so a phase doesn't repeat the values the one before it
    /// sent.
    pub fn attack(&self, n: u64) -> Self {
        Self {
            shared: self.shared.clone(),
            attack: n,
        }
    }

    /// Hit `seq`'s rng: the seed, then a stream of its own. Streams stay
    /// distinct for up to 2^48 hits per attack and 2^16 attacks.
    fn rng(&self, seq: u64) -> ChaCha8Rng {
        let mut rng = ChaCha8Rng::seed_from_u64(self.shared.seed);
        rng.set_stream((self.attack << 48) | seq);
        rng
    }

    /// The scenario name hit `seq` fires, for report attribution.
    pub fn operation(&self, seq: u64) -> Option<&str> {
        self.shared.mix.pick(&mut self.rng(seq)).map(|sc| sc.name.as_str())
    }
}

impl Targeter for MixTargeter {
    fn target(&self, seq: u64, t: &mut Target) -> Result<(), volley::Error> {
        let mut rng = self.rng(seq);
        let sc = self
            .shared
            .mix
            .pick(&mut rng)
            .ok_or_else(|| volley::Error::Build("no scenarios to pick from".into()))?;
        self.shared
            .enc
            .encode(t, sc, &self.shared.pools, &mut rng)
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
            matcher: None,
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
                Box::new(GraphqlEncoder::new(&cfg, &[sc("A", 1.0), sc("B", 1.0)]).unwrap()),
                WeightedMix::new(vec![sc("A", 1.0), sc("B", 1.0)]),
                crate::data::Pools::new(),
                7,
            )
        };
        let (a, b) = (mk(), mk());
        for _ in 0..50 {
            let (mut ta, mut tb) = (Target::default(), Target::default());
            a.target(0, &mut ta).unwrap();
            b.target(0, &mut tb).unwrap();
            assert_eq!(ta.url, tb.url);
        }
    }

    /// Scenarios A and B, each drawing `id` from a wide range pool.
    fn sampling_targeter(seed: u64) -> MixTargeter {
        let scenarios: Vec<Scenario> = ["A", "B"]
            .iter()
            .map(|n| {
                let mut s = sc(n, 1.0);
                s.request.variables = [("id".to_string(), serde_json::json!("{{ids}}"))].into();
                s
            })
            .collect();
        let cfg = TargetConfig {
            url: "http://x/graphql".into(),
            ..Default::default()
        };
        let pools: Pools = [("ids".to_string(), crate::data::Pool::Range { min: 1, max: 1_000_000 })].into();
        MixTargeter::new(
            Box::new(GraphqlEncoder::new(&cfg, &scenarios).unwrap()),
            WeightedMix::new(scenarios),
            pools,
            seed,
        )
    }

    fn request(t: &MixTargeter, seq: u64) -> (String, Vec<u8>) {
        let mut target = Target::default();
        t.target(seq, &mut target).unwrap();
        (target.url, target.body)
    }

    #[test]
    fn each_seq_draws_the_same_request_whatever_the_call_order() {
        // Workers call the targeter concurrently, in any order: the request
        // for a given seq must not depend on which hit asked first.
        let (a, b) = (sampling_targeter(7), sampling_targeter(7));
        let (a5, a2) = (request(&a, 5), request(&a, 2));
        let (b2, b5) = (request(&b, 2), request(&b, 5));
        assert_eq!(a5, b5);
        assert_eq!(a2, b2);
        assert_ne!(a5, a2);
    }

    #[test]
    fn attacks_draw_apart_though_seq_restarts() {
        let t = sampling_targeter(7);
        let (warm, one, two) = (t.attack(0), t.attack(1), t.attack(2));
        assert_eq!(request(&one, 3), request(&t.attack(1), 3), "reproducible");
        assert_ne!(request(&one, 3).1, request(&two, 3).1);
        assert_ne!(request(&warm, 3).1, request(&one, 3).1);
    }

    #[test]
    fn operation_names_the_scenario_each_seq_fired() {
        let t = sampling_targeter(7).attack(1);
        let mut seen = std::collections::BTreeSet::new();
        for seq in 0..50 {
            let (url, _) = request(&t, seq);
            let op = t.operation(seq).unwrap();
            assert_eq!(url, format!("http://x/graphql?op={op}"), "seq {seq}");
            seen.insert(op.to_string());
        }
        assert_eq!(seen.len(), 2, "both scenarios fired: {seen:?}");
    }

    /// Pins the requests a seeded profile run draws, so a change to the
    /// per-hit stream layout is a deliberate one.
    #[test]
    fn seeded_profile_draws_are_pinned() {
        let t = sampling_targeter(11).attack(1);
        let drawn: Vec<String> = (0..4)
            .map(|seq| {
                let v: serde_json::Value = serde_json::from_slice(&request(&t, seq).1).unwrap();
                format!("{}:{}", v["operationName"].as_str().unwrap(), v["variables"]["id"])
            })
            .collect();
        assert_eq!(drawn, ["A:390937", "A:494380", "B:494718", "A:1987"]);
    }
}
