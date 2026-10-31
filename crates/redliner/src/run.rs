//! Run orchestration (Go `run()` in `main.go`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::watch;

use volley::{Attacker, AttackerConfig, ConstantPacer, Metrics, Pacer};

use crate::checks::Checker;
use crate::cli::{Args, Command, ConfigSource, DEFAULT_CONFIG};
use crate::compare;
use crate::config::{load_config_file_with, load_config_with, Config, Protocol};
use crate::data;
use crate::encode::{new_encoder, MixTargeter};
use crate::live::Live;
use crate::mix::WeightedMix;
use crate::phases::{build_phases, Phase};
use crate::replay::schedule::Schedule;
use crate::replay::targeter::{SchedulePacer, ScheduleTargeter};
use crate::replay::{build_replay, ReplaySummary};
use crate::report::{self, Collector, RedactedConfig, RunManifest};
use crate::slo::{self, fmt_dur, SloResult};

/// Stands in for the config file name when the run comes from `--url` alone.
const URL_CONFIG: &str = "(command line)";

/// Process exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Ok = 0,
    SloFailed = 1,
    Error = 2,
    /// Stopped by Ctrl-C; reports hold partial results (128 + SIGINT).
    Interrupted = 130,
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
        danger_accept_invalid_certs: cfg.target.insecure_tls,
    })
}

pub async fn run(args: Args) -> Exit {
    run_with_interrupt(args, ctrl_c()).await
}

/// Like [`run`], but stops the attack when `interrupt` resolves: no new hits
/// are fired, in-flight ones finish, and the partial results are reported
/// with exit [`Exit::Interrupted`].
pub async fn run_with_interrupt<F>(args: Args, interrupt: F) -> Exit
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if let Some(Command::Compare(c)) = &args.command {
        return compare::run(c);
    }
    match run_inner(args, interrupt).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("redliner: {e:#}");
            Exit::Error
        }
    }
}

/// Resolves on the first Ctrl-C; a second one exits at once, without reports.
async fn ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        // No handler could be installed, so there is nothing to wait for.
        std::future::pending::<()>().await;
    }
    eprintln!(
        "\ninterrupted: no new requests; waiting for in-flight ones (Ctrl-C again to quit now)"
    );
    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("interrupted again: exiting without reports");
            std::process::exit(Exit::Interrupted.code());
        }
    });
}

async fn run_inner<F>(args: Args, interrupt: F) -> anyhow::Result<Exit>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let source = args
        .config_source(Path::new(DEFAULT_CONFIG).exists())
        .map_err(anyhow::Error::msg)?;
    let (cfg, config_label) = match source {
        ConfigSource::File(path) => {
            let cfg = load_config_file_with(&path, |c| args.apply_overrides(c))?;
            (cfg, path.display().to_string())
        }
        ConfigSource::Url => {
            let (origin, scenario) = args.url_request().map_err(anyhow::Error::msg)?;
            let label = Path::new(URL_CONFIG);
            let cfg = load_config_with(label, Config::default(), |c| {
                args.apply_overrides(c);
                c.target.url = origin;
                c.target.protocol = Protocol::Rest;
                c.scenarios = vec![scenario];
            })?;
            (cfg, URL_CONFIG.to_string())
        }
    };
    if cfg.target.insecure_tls {
        eprintln!(
            "warning: TLS certificate verification is off (target.insecure_tls / --insecure): \
             any server on the path can answer for {}. Use it only against test environments.",
            cfg.target.url
        );
    }
    // Replay knobs without a replay target are one clear error, not a cascade.
    args.check_replay_knobs(cfg.replay.is_some())
        .map_err(anyhow::Error::msg)?;
    // Replay reads and schedules the whole log up front, so a bad log fails
    // before any report dir or request exists.
    let mut replay = cfg
        .replay
        .is_some()
        .then(|| build_replay(&cfg))
        .transpose()?;

    if args.validate {
        let proto = serde_json::to_value(cfg.target.protocol)?;
        let mode = match &replay {
            Some(r) => format!("replay {}", r.summary.source),
            None => format!("profile {}", cfg.load.profile),
        };
        println!(
            "{}: valid ({}, {} scenarios, {mode})",
            config_label,
            proto.as_str().unwrap_or("?"),
            cfg.scenarios.len(),
        );
        if let Some(r) = &replay {
            print!("{}", r.summary.render());
        }
        return Ok(Exit::Ok);
    }

    match &replay {
        Some(r) => {
            let s = &r.summary;
            eprintln!(
                "target={} replay={} hits={} span={} speed={} copies={} seed={}",
                cfg.target.url, s.source, s.hits, s.span, s.speed, s.copies, cfg.load.seed
            );
        }
        None => eprintln!(
            "target={} profile={} seed={}",
            cfg.target.url, cfg.load.profile, cfg.load.seed
        ),
    }

    let enc = new_encoder(&cfg)?;
    let mut pools = data::build_pools(&cfg.data)?;
    data::harvest::run_harvest(&cfg, &mut pools).await?;
    let seed = cfg.load.seed as u64;
    let plan = match &replay {
        Some(r) => {
            let schedule = r.schedule.clone();
            let t =
                ScheduleTargeter::new(enc, cfg.scenarios.clone(), pools, schedule.clone(), seed);
            Plan::Replay(Arc::new(t), schedule)
        }
        None => {
            let mix = WeightedMix::new(cfg.scenarios.clone());
            let t = MixTargeter::new(enc, mix, pools, seed);
            Plan::Profile(Arc::new(t), build_phases(&cfg.load)?)
        }
    };

    // Report scaffolding: create the run dir and write an initial manifest
    // before any traffic, so an aborted run still leaves a trail.
    let wall_start = SystemTime::now();
    let report_dir = report::make_report_dir(&cfg.report.dir, wall_start)
        .map_err(|e| anyhow::anyhow!("report dir: {e}"))?;
    let mut manifest = RunManifest {
        started_at: report::utc_rfc3339(wall_start),
        finished_at: None,
        config_file: config_label,
        config: RedactedConfig::from(&cfg),
        seed: cfg.load.seed,
        requests: None,
        slo_results: Vec::new(),
        passed: None,
        replay: replay.as_ref().map(|r| r.summary.clone()),
        aborted: false,
    };
    report::write_manifest(&report_dir, &manifest)
        .map_err(|e| anyhow::anyhow!("write manifest: {e}"))?;

    // Armed only now: an interrupt before this point has no trail to finish,
    // so the default (kill the process) is right.
    let (stop_tx, stop) = watch::channel(false);
    tokio::spawn(async move {
        interrupt.await;
        let _ = stop_tx.send(true);
    });

    let start = Instant::now();

    // Results flow through the checker (which reclassifies app-layer
    // failures) into the collector.
    let checker = Checker::new(&cfg.checks);
    let raw = cfg
        .report
        .keep_raw
        .unwrap_or(true)
        .then(|| report_dir.join("results.jsonl"));
    let mut collector =
        Collector::new(raw.as_deref()).map_err(|e| anyhow::anyhow!("open results.jsonl: {e}"))?;
    let mut live = Live::new(cfg.report.live_stats.unwrap_or_default().as_std());
    let fired = match plan {
        Plan::Profile(t, phases) => {
            attack_profile(&cfg, t, &phases, &checker, &mut collector, &stop, &mut live).await?;
            None
        }
        Plan::Replay(t, schedule) => Some(
            attack_replay(&cfg, t, schedule, &checker, &mut collector, &stop, &mut live).await?,
        ),
    };
    collector.close();
    let elapsed = start.elapsed();
    let interrupted = *stop.borrow();

    let mut results = slo::evaluate(&cfg.slos, collector.overall());
    let mut passed = slo::passed(&results);

    // A replay that could not keep up silently dropped its tail, which would
    // otherwise let a short-changed run pass the SLO gate. Record the shortfall
    // and fail the run regardless of whether SLOs are enforced. The delivery
    // check joins the SLO results so JUnit and the summary show it too.
    let mut fell_behind = false;
    if let (Some(r), Some(fired)) = (replay.as_mut(), fired) {
        let hits = r.summary.hits;
        let dropped = hits.saturating_sub(fired);
        r.summary.fired = Some(fired);
        r.summary.dropped = Some(dropped);
        // An interrupt cuts the schedule short on purpose; that is not falling behind.
        if !interrupted {
            results.push(SloResult {
                name: "replay_delivery".into(),
                target: format!("{hits} hits"),
                observed: format!("{fired} fired"),
                passed: dropped == 0,
            });
            if dropped > 0 {
                eprintln!(
                    "warning: replay fell behind: fired {fired} of {hits} scheduled hits \
                     ({dropped} dropped); raise load.max_workers if the load generator, \
                     not the target, is the bottleneck"
                );
                fell_behind = true;
                passed = false;
            }
        }
    }

    // A run where nothing succeeded measured nothing (the target is down, or
    // every answer failed a check): fail it even when no SLO is enforced.
    let mut none_succeeded = false;
    if !interrupted {
        if let Some(r) = slo::no_success(collector.overall(), checker.accepted()) {
            eprintln!("{}", slo::no_success_warning(collector.overall()));
            results.push(r);
            none_succeeded = true;
            passed = false;
        }
    }

    manifest.finished_at = Some(report::utc_rfc3339(SystemTime::now()));
    manifest.requests = Some(collector.overall().requests);
    manifest.slo_results = results.clone();
    manifest.passed = Some(passed);
    manifest.replay = replay.as_ref().map(|r| r.summary.clone());
    manifest.aborted = interrupted;
    report::write_reports(&report_dir, &collector, &manifest)
        .map_err(|e| anyhow::anyhow!("write reports: {e}"))?;
    if cfg.report.junit {
        report::write_junit(&report_dir.join("junit.xml"), &results, elapsed)
            .map_err(|e| anyhow::anyhow!("write junit: {e}"))?;
    }

    print_summary(
        interrupted,
        collector.overall(),
        collector.per_op(),
        replay.as_ref().map(|r| &r.summary),
        &results,
        elapsed,
        &report_dir,
    );

    if interrupted {
        return Ok(Exit::Interrupted);
    }
    if fell_behind || none_succeeded || (cfg.slos.enforced() && !passed) {
        return Ok(Exit::SloFailed);
    }
    Ok(Exit::Ok)
}

/// What the attack section fires: weighted phases, or a replayed log.
enum Plan {
    Profile(Arc<MixTargeter>, Vec<Phase>),
    Replay(Arc<ScheduleTargeter>, Arc<Schedule>),
}

/// Warmup, then one fresh attacker per phase.
async fn attack_profile(
    cfg: &Config,
    targeter: Arc<MixTargeter>,
    phases: &[Phase],
    checker: &Checker,
    collector: &mut Collector,
    stop: &watch::Receiver<bool>,
    live: &mut Live,
) -> anyhow::Result<()> {
    // Warmup: uniform, unmeasured, 10% of the first phase's rate.
    let warmup = cfg.load.warmup.as_std();
    if !warmup.is_zero() {
        let first_rate = phases.first().map_or(0.0, |p| p.rate);
        let freq = ((first_rate / 10.0) as u64).max(1);
        let pacer = Arc::new(ConstantPacer { freq, per: Duration::from_secs(1) });
        let attacker = new_attacker(cfg)?;
        let mut rx =
            attacker.attack_until(targeter.clone(), pacer, warmup, "warmup".into(), stop.clone());
        while rx.recv().await.is_some() {}
    }

    live.begin();
    for ph in phases {
        if *stop.borrow() {
            break;
        }
        eprintln!("attack: {} for {:?} (nominal {} rps)", ph.name, ph.duration, ph.rate);
        let attacker = new_attacker(cfg)?;
        let mut rx = attacker.attack_until(
            targeter.clone(),
            Arc::new(DynPacer(ph.pacer.clone())),
            ph.duration,
            ph.name.clone(),
            stop.clone(),
        );
        while let Some(mut r) = live.recv(&mut rx, &ph.name).await {
            checker.apply(&mut r);
            let ok = checker.accepts(&r);
            live.add(&r, ok);
            let op = targeter.operation(&r.method, &r.url);
            collector.add(&r, ok, &ph.name, op.as_deref());
        }
    }
    Ok(())
}

/// One attacker that fires each scheduled hit at its offset, attributing
/// every result to the scenario its log event matched.
/// Returns the number of hits actually fired (results received).
async fn attack_replay(
    cfg: &Config,
    targeter: Arc<ScheduleTargeter>,
    schedule: Arc<Schedule>,
    checker: &Checker,
    collector: &mut Collector,
    stop: &watch::Receiver<bool>,
    live: &mut Live,
) -> anyhow::Result<u64> {
    let attacker = new_attacker(cfg)?;
    // Give a slow-but-responding target room to answer the last hits: one
    // request timeout past the last offset, plus a drain second.
    let duration = schedule.span() + cfg.target.timeout.as_std() + Duration::from_secs(1);
    let pacer = Arc::new(SchedulePacer::new(schedule));
    let mut rx =
        attacker.attack_until(targeter.clone(), pacer, duration, "replay".into(), stop.clone());
    let mut fired = 0u64;
    live.begin();
    while let Some(mut r) = live.recv(&mut rx, "replay").await {
        checker.apply(&mut r);
        let ok = checker.accepts(&r);
        live.add(&r, ok);
        collector.add(&r, ok, "replay", targeter.operation(r.seq));
        fired += 1;
    }
    Ok(fired)
}

fn print_summary(
    interrupted: bool,
    m: &Metrics,
    per_op: &BTreeMap<String, Metrics>,
    replay: Option<&ReplaySummary>,
    slos: &[SloResult],
    elapsed: Duration,
    report_dir: &Path,
) {
    println!();
    println!("==== redliner summary ====");
    if interrupted {
        println!("status        : INTERRUPTED (partial results)");
    }
    println!("elapsed       : {}", fmt_dur(elapsed));
    println!("requests      : {}", m.requests);
    println!("rate (actual) : {:.1} rps", m.rate);
    println!("throughput    : {:.1} rps (successful)", m.throughput);
    println!("success       : {:.3}%", m.success * 100.0);
    println!("status        : {}", report::status_classes(&m.status_codes));
    println!("latency p50   : {}", fmt_dur(m.latencies.p50));
    println!("latency p95   : {}", fmt_dur(m.latencies.p95));
    println!("latency p99   : {}", fmt_dur(m.latencies.p99));
    println!("latency p99.9 : {}", fmt_dur(m.latencies.p999));
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

    if let Some(r) = replay {
        print!("\n{}", r.render());
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

    if slos.iter().any(|r| !r.passed) {
        println!("{}", redline_banner());
    }

    println!("\nreports in    : {}", report_dir.display());
}

/// A redline tachometer bar, printed when a run violates its SLOs.
fn redline_banner() -> String {
    let bar = format!("{}{}◉", "█".repeat(38), "▓".repeat(7)); // 46 cells
    format!(
        "\n  {rule}\n  {bar}\n  {foot}\n  ⚠  R E D L I N E  ·  SLO violated",
        rule = "▄".repeat(46),
        foot = "▀".repeat(46),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redline_banner_marks_the_red() {
        let b = redline_banner();
        assert!(b.contains("R E D L I N E"), "{b}");
        assert!(b.contains("SLO violated"));
        assert!(b.contains('◉'), "needle missing");
        // three bar rows line up at the same width (46 cells each).
        for row in [&"▄".repeat(46), &"▀".repeat(46)] {
            assert!(b.contains(row.as_str()));
        }
    }
}
