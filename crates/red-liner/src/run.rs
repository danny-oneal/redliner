//! Run orchestration (Go `run()` in `main.go`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use volley::{Attacker, AttackerConfig, ConstantPacer, Metrics, Pacer};

use crate::cli::Args;
use crate::config::{load_config_file, Config};
use crate::encode::{GraphqlEncoder, GraphqlTargeter};
use crate::mix::WeightedMix;
use crate::phases::build_phases;
use crate::slo::{self, fmt_dur, SloResult};

/// Process exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Ok = 0,
    SloFailed = 1,
    Error = 2,
}

impl Exit {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// Adapter: `Attacker::attack` wants a sized pacer, phases hold `Arc<dyn Pacer>`.
struct DynPacer(Arc<dyn Pacer>);

impl Pacer for DynPacer {
    fn pace(&self, elapsed: Duration, hits: u64) -> (Duration, bool) {
        self.0.pace(elapsed, hits)
    }
    fn rate(&self, elapsed: Duration) -> f64 {
        self.0.rate(elapsed)
    }
}

fn new_attacker(cfg: &Config) -> Result<Attacker, volley::Error> {
    Attacker::new(AttackerConfig {
        timeout: cfg.target.timeout.as_std(),
        workers: cfg.load.workers as usize,
        max_workers: cfg.load.max_workers as usize,
        max_idle_conns: cfg.target.max_idle_conns.max(0) as usize,
        keep_alive: cfg.target.keep_alive.unwrap_or(true),
        http2: cfg.target.http2,
    })
}

pub async fn run(args: Args) -> Exit {
    match run_inner(args).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("red-liner: {e:#}");
            Exit::Error
        }
    }
}

async fn run_inner(args: Args) -> anyhow::Result<Exit> {
    let mut cfg = load_config_file(&args.config)?;
    args.apply_overrides(&mut cfg);

    if args.validate {
        let proto = serde_json::to_value(cfg.target.protocol)?;
        println!(
            "{}: valid ({}, {} scenarios, profile {})",
            args.config.display(),
            proto.as_str().unwrap_or("?"),
            cfg.scenarios.len(),
            cfg.load.profile
        );
        return Ok(Exit::Ok);
    }

    eprintln!(
        "target={} profile={} seed={}",
        cfg.target.url, cfg.load.profile, cfg.load.seed
    );

    let enc = GraphqlEncoder::new(&cfg.target)?;
    let mix = WeightedMix::new(cfg.scenarios.clone());
    let targeter = Arc::new(GraphqlTargeter::new(enc, mix, cfg.load.seed as u64));
    let phases = build_phases(&cfg.load)?;

    let start = Instant::now();

    // Warmup: uniform, unmeasured, 10% of the first phase's rate.
    let warmup = cfg.load.warmup.as_std();
    if !warmup.is_zero() {
        let first_rate = phases.first().map_or(0.0, |p| p.rate);
        let freq = ((first_rate / 10.0) as u64).max(1);
        let pacer = Arc::new(ConstantPacer { freq, per: Duration::from_secs(1) });
        let attacker = new_attacker(&cfg)?;
        let mut rx = attacker.attack(targeter.clone(), pacer, warmup, "warmup".into());
        while rx.recv().await.is_some() {}
    }

    // One fresh attacker per phase, all results drained into one Metrics.
    let mut metrics = Metrics::new();
    for ph in &phases {
        eprintln!("attack: {} for {:?} (nominal {} rps)", ph.name, ph.duration, ph.rate);
        let attacker = new_attacker(&cfg)?;
        let mut rx = attacker.attack(
            targeter.clone(),
            Arc::new(DynPacer(ph.pacer.clone())),
            ph.duration,
            ph.name.clone(),
        );
        while let Some(r) = rx.recv().await {
            metrics.add(&r);
        }
    }
    metrics.close();
    let elapsed = start.elapsed();

    let results = slo::evaluate(&cfg.slos, &metrics);
    print_summary(&metrics, &results, elapsed);

    if cfg.slos.enforced() && !slo::passed(&results) {
        return Ok(Exit::SloFailed);
    }
    Ok(Exit::Ok)
}

fn print_summary(m: &Metrics, slos: &[SloResult], elapsed: Duration) {
    println!();
    println!("==== capacity_bench summary ====");
    println!("elapsed       : {}", fmt_dur(elapsed));
    println!("requests      : {}", m.requests);
    println!("rate (actual) : {:.1} rps", m.rate);
    println!("throughput    : {:.1} rps (successful)", m.throughput);
    println!("success       : {:.3}%", m.success * 100.0);
    println!("latency p50   : {}", fmt_dur(m.latencies.p50));
    println!("latency p95   : {}", fmt_dur(m.latencies.p95));
    println!("latency p99   : {}", fmt_dur(m.latencies.p99));
    println!("latency max   : {}", fmt_dur(m.latencies.max));

    if !m.errors.is_empty() {
        println!("\nerrors        : {} distinct", m.errors.len());
        for (i, e) in m.errors.iter().enumerate() {
            if i >= 5 {
                println!("  ... +{} more", m.errors.len() - 5);
                break;
            }
            println!("  - {e}");
        }
    }

    if !slos.is_empty() {
        println!("\nSLOs:");
        for r in slos {
            let mark = if r.passed { "PASS" } else { "FAIL" };
            println!(
                "  [{mark}] {:<14} target {:<12} observed {}",
                r.name, r.target, r.observed
            );
        }
    }
}
