//! Run orchestration (Go `run()` in `main.go`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use volley::{Attacker, AttackerConfig, ConstantPacer, Metrics, Pacer};

use crate::checks::Checker;
use crate::cli::Args;
use crate::config::{load_config_file, Config};
use crate::data;
use crate::encode::{new_encoder, MixTargeter};
use crate::mix::WeightedMix;
use crate::phases::build_phases;
use crate::report::{self, Collector, RunManifest};
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

    let enc = new_encoder(&cfg)?;
    let mix = WeightedMix::new(cfg.scenarios.clone());
    let mut pools = data::build_pools(&cfg.data)?;
    data::harvest::run_harvest(&cfg, &mut pools).await?;
    let targeter = Arc::new(MixTargeter::new(enc, mix, pools, cfg.load.seed as u64));
    let phases = build_phases(&cfg.load)?;

    // Report scaffolding: create the run dir and write an initial manifest
    // before any traffic, so an aborted run still leaves a trail.
    let wall_start = SystemTime::now();
    let report_dir = report::make_report_dir(&cfg.report.dir, wall_start)
        .map_err(|e| anyhow::anyhow!("report dir: {e}"))?;
    let mut manifest = RunManifest {
        started_at: report::utc_rfc3339(wall_start),
        finished_at: None,
        config_file: args.config.display().to_string(),
        config: cfg.clone(),
        seed: cfg.load.seed,
        requests: None,
        slo_results: Vec::new(),
        passed: None,
    };
    report::write_manifest(&report_dir, &manifest)
        .map_err(|e| anyhow::anyhow!("write manifest: {e}"))?;

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

    // One fresh attacker per phase; results flow through the checker (which
    // reclassifies app-layer failures) into the collector.
    let checker = Checker::new(&cfg.checks);
    let raw = cfg
        .report
        .keep_raw
        .unwrap_or(true)
        .then(|| report_dir.join("results.jsonl"));
    let mut collector =
        Collector::new(raw.as_deref()).map_err(|e| anyhow::anyhow!("open results.jsonl: {e}"))?;
    for ph in &phases {
        eprintln!("attack: {} for {:?} (nominal {} rps)", ph.name, ph.duration, ph.rate);
        let attacker = new_attacker(&cfg)?;
        let mut rx = attacker.attack(
            targeter.clone(),
            Arc::new(DynPacer(ph.pacer.clone())),
            ph.duration,
            ph.name.clone(),
        );
        while let Some(mut r) = rx.recv().await {
            checker.apply(&mut r);
            let op = targeter.operation(&r.method, &r.url);
            collector.add(&r, &ph.name, op.as_deref());
        }
    }
    collector.close();
    let elapsed = start.elapsed();

    let results = slo::evaluate(&cfg.slos, collector.overall());
    let passed = slo::passed(&results);

    manifest.finished_at = Some(report::utc_rfc3339(SystemTime::now()));
    manifest.requests = Some(collector.overall().requests);
    manifest.slo_results = results.clone();
    manifest.passed = Some(passed);
    report::write_reports(&report_dir, &collector, &manifest)
        .map_err(|e| anyhow::anyhow!("write reports: {e}"))?;
    if cfg.report.junit {
        report::write_junit(&report_dir.join("junit.xml"), &results, elapsed)
            .map_err(|e| anyhow::anyhow!("write junit: {e}"))?;
    }

    print_summary(collector.overall(), collector.per_op(), &results, elapsed, &report_dir);

    if cfg.slos.enforced() && !passed {
        return Ok(Exit::SloFailed);
    }
    Ok(Exit::Ok)
}

fn print_summary(
    m: &Metrics,
    per_op: &BTreeMap<String, Metrics>,
    slos: &[SloResult],
    elapsed: Duration,
    report_dir: &Path,
) {
    println!();
    println!("==== red-liner summary ====");
    println!("elapsed       : {}", fmt_dur(elapsed));
    println!("requests      : {}", m.requests);
    println!("rate (actual) : {:.1} rps", m.rate);
    println!("throughput    : {:.1} rps (successful)", m.throughput);
    println!("success       : {:.3}%", m.success * 100.0);
    println!("latency p50   : {}", fmt_dur(m.latencies.p50));
    println!("latency p95   : {}", fmt_dur(m.latencies.p95));
    println!("latency p99   : {}", fmt_dur(m.latencies.p99));
    println!("latency max   : {}", fmt_dur(m.latencies.max));

    if !per_op.is_empty() {
        println!("\nper operation:");
        for s in report::summarize(per_op) {
            println!(
                "  {:<24} n={:<8} ok={:6.2}%  p50={:<10} p95={:<10} p99={}",
                s.name, s.requests, s.success * 100.0, s.p50, s.p95, s.p99
            );
        }
    }

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

    println!("\nreports in    : {}", report_dir.display());
}
