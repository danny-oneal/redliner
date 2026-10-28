//! `redliner compare`: the change between two runs' reports.
//!
//! Reads each run's `metrics.json` (and `per_phase.json` and
//! `per_operation.json`, when present) and reports the change in
//! p50/p95/p99, throughput and success rate, overall, per phase and per
//! operation. With a threshold, a latency rise or throughput
//! drop beyond it is a regression, and the command exits 1, so CI can fail a
//! build that got slower than its baseline. `--junit` writes each gated
//! metric as a JUnit test case, so CI shows which ones regressed. The success rate is shown in
//! percentage points but not gated: `slos.success_rate` sets its floor.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::cli::CompareArgs;
use crate::config::ConfigDuration;
use crate::report::{write_junit_suite, JunitCase};
use crate::run::Exit;
use crate::slo::fmt_dur;

/// Runs whose attack rates differ by more than this fraction get a warning.
const RATE_TOLERANCE: f64 = 0.05;

#[derive(Debug, thiserror::Error)]
pub enum CompareError {
    #[error("read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// The numbers compared for the whole run or one operation.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
    pub success: f64,
    /// Successful requests per second; only reported for the whole run.
    pub throughput: Option<f64>,
}

/// One run's report, as far as `compare` reads it.
#[derive(Debug, Clone)]
pub struct RunReport {
    /// The rate requests were fired at.
    pub rate: f64,
    pub overall: Stats,
    /// Per load phase; only multi-phase runs (`steps`) write these.
    pub phases: BTreeMap<String, Stats>,
    pub ops: BTreeMap<String, Stats>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    /// Relative change, in percent of the baseline.
    Percent(f64),
    /// Absolute change in percentage points (for the success rate).
    Points(f64),
    /// The baseline is zero.
    NotApplicable,
}

#[derive(Debug, Clone)]
pub struct Row {
    /// `overall`, `phase <name>` or an operation name.
    pub scope: String,
    pub metric: &'static str,
    pub baseline: String,
    pub candidate: String,
    pub change: Change,
    pub regressed: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Comparison {
    pub rows: Vec<Row>,
    pub only_in_baseline: Vec<String>,
    pub only_in_candidate: Vec<String>,
    pub warnings: Vec<String>,
}

impl Comparison {
    pub fn regressions(&self) -> usize {
        self.rows.iter().filter(|r| r.regressed).count()
    }
}

/// `redliner compare`: prints the comparison (and writes `--junit`); exit 1
/// on a regression over `--max-regression`, 2 when a report can't be read or
/// the JUnit file can't be written.
pub fn run(args: &CompareArgs) -> Exit {
    let loaded = load(&args.baseline).and_then(|b| Ok((b, load(&args.candidate)?)));
    let (base, cand) = match loaded {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("redliner compare: {e}");
            return Exit::Error;
        }
    };
    let c = compare(&base, &cand, args.max_regression);
    for w in &c.warnings {
        eprintln!("warning: {w}");
    }
    print!("{}", render(&c, args.max_regression));
    if let Some(path) = &args.junit {
        let cases = junit_cases(&c, args.max_regression);
        if let Err(e) = write_junit_suite(path, "redliner.compare", &cases, Duration::ZERO) {
            eprintln!("redliner compare: write {}: {e}", path.display());
            return Exit::Error;
        }
    }
    if c.regressions() > 0 {
        Exit::SloFailed
    } else {
        Exit::Ok
    }
}

/// Reads a report directory, or the `metrics.json` inside one.
pub fn load(path: &Path) -> Result<RunReport, CompareError> {
    let (dir, metrics_path) = if path.is_file() {
        (path.parent().unwrap_or(Path::new(".")), path.to_path_buf())
    } else {
        (path, path.join("metrics.json"))
    };
    let m: MetricsFile = read_json(&metrics_path)?;
    let rows = |name: &str| -> Result<BTreeMap<String, Stats>, CompareError> {
        let path = dir.join(name);
        let rows: Vec<OpFile> = if path.exists() { read_json(&path)? } else { Vec::new() };
        Ok(rows
            .into_iter()
            .map(|o| {
                let stats = o.stats();
                (o.name, stats)
            })
            .collect())
    };
    Ok(RunReport {
        rate: m.rate,
        overall: Stats {
            p50: m.latencies.p50.as_std(),
            p95: m.latencies.p95.as_std(),
            p99: m.latencies.p99.as_std(),
            success: m.success,
            throughput: Some(m.throughput),
        },
        phases: rows("per_phase.json")?,
        ops: rows("per_operation.json")?,
    })
}

/// Compares `cand` against `base`. A latency rise or throughput drop of more
/// than `max_regression` percent is a regression; `None` gates nothing.
pub fn compare(base: &RunReport, cand: &RunReport, max_regression: Option<f64>) -> Comparison {
    let mut c = Comparison::default();
    if base.rate > 0.0 && ((cand.rate - base.rate) / base.rate).abs() > RATE_TOLERANCE {
        c.warnings.push(format!(
            "the runs fired at different rates ({:.1}/s vs {:.1}/s); latency and throughput may not be comparable",
            base.rate, cand.rate
        ));
    }
    push_rows(&mut c.rows, "overall", &base.overall, &cand.overall, max_regression);
    compare_named(&mut c, "phase ", &base.phases, &cand.phases, max_regression);
    compare_named(&mut c, "", &base.ops, &cand.ops, max_regression);
    c
}

/// Rows for the entries both runs have, matched by name; the rest are listed
/// as only in one run. `prefix` labels the group (`phase `).
fn compare_named(
    c: &mut Comparison,
    prefix: &str,
    base: &BTreeMap<String, Stats>,
    cand: &BTreeMap<String, Stats>,
    max_regression: Option<f64>,
) {
    for (name, b) in base {
        let scope = format!("{prefix}{name}");
        match cand.get(name) {
            Some(k) => push_rows(&mut c.rows, &scope, b, k, max_regression),
            None => c.only_in_baseline.push(scope),
        }
    }
    c.only_in_candidate
        .extend(cand.keys().filter(|n| !base.contains_key(*n)).map(|n| format!("{prefix}{n}")));
}

/// The comparison as a table, then the verdict on its last line.
pub fn render(c: &Comparison, max_regression: Option<f64>) -> String {
    let mut out = String::new();
    let mut scope = "";
    let _ = writeln!(out, "{:<14}{:>14}{:>14}{:>11}", "", "baseline", "candidate", "change");
    for r in &c.rows {
        if r.scope != scope {
            scope = &r.scope;
            let _ = writeln!(out, "{scope}");
        }
        let change = match r.change {
            Change::Percent(p) => format!("{p:+.1}%"),
            Change::Points(p) => format!("{p:+.3}pp"),
            Change::NotApplicable => "n/a".into(),
        };
        let flag = if r.regressed { "  REGRESSED" } else { "" };
        let _ = writeln!(
            out,
            "  {:<12}{:>14}{:>14}{:>11}{flag}",
            r.metric, r.baseline, r.candidate, change
        );
    }
    if !c.only_in_baseline.is_empty() {
        let _ = writeln!(out, "only in baseline: {}", c.only_in_baseline.join(", "));
    }
    if !c.only_in_candidate.is_empty() {
        let _ = writeln!(out, "only in candidate: {}", c.only_in_candidate.join(", "));
    }
    let _ = match max_regression {
        None => writeln!(out, "--max-regression not set; nothing gated"),
        Some(t) => {
            let regressed: Vec<String> = c
                .rows
                .iter()
                .filter(|r| r.regressed)
                .map(|r| format!("{} {}", r.scope, r.metric))
                .collect();
            match regressed.len() {
                0 => writeln!(out, "no regressions over {t}%"),
                1 => writeln!(out, "1 regression over {t}%: {}", regressed[0]),
                n => writeln!(out, "{n} regressions over {t}%: {}", regressed.join(", ")),
            }
        }
    };
    out
}

/// One JUnit test case per gated row (latency and throughput; the success
/// rate isn't gated), named `<scope> <metric>`; a regression is a failure.
pub fn junit_cases(c: &Comparison, max_regression: Option<f64>) -> Vec<JunitCase> {
    c.rows
        .iter()
        .filter(|r| !matches!(r.change, Change::Points(_)))
        .map(|r| JunitCase {
            name: format!("{} {}", r.scope, r.metric),
            failure: match (r.regressed, r.change, max_regression) {
                (true, Change::Percent(p), Some(t)) => Some(format!(
                    "{} {}: {} -> {} ({p:+.1}%, over the {t}% limit)",
                    r.scope, r.metric, r.baseline, r.candidate
                )),
                _ => None,
            },
        })
        .collect()
}

/// Parses a threshold such as `10%` or `2.5` (percent).
pub fn parse_percent(s: &str) -> Result<f64, String> {
    let n: f64 = s
        .strip_suffix('%')
        .unwrap_or(s)
        .parse()
        .map_err(|_| format!("{s:?} is not a percentage (e.g. 10%)"))?;
    if !n.is_finite() || n < 0.0 {
        return Err(format!("{s:?} must be a percentage >= 0 (e.g. 10%)"));
    }
    Ok(n)
}

// ---- internals ---------------------------------------------------------------

/// The parts of `metrics.json` that `compare` reads; other fields are ignored
/// so reports can grow without breaking older baselines.
#[derive(Deserialize)]
struct MetricsFile {
    rate: f64,
    throughput: f64,
    success: f64,
    latencies: LatenciesFile,
}

#[derive(Deserialize)]
struct LatenciesFile {
    p50: ConfigDuration,
    p95: ConfigDuration,
    p99: ConfigDuration,
}

/// One `per_operation.json` or `per_phase.json` row.
#[derive(Deserialize)]
struct OpFile {
    name: String,
    success: f64,
    p50: ConfigDuration,
    p95: ConfigDuration,
    p99: ConfigDuration,
}

impl OpFile {
    fn stats(&self) -> Stats {
        Stats {
            p50: self.p50.as_std(),
            p95: self.p95.as_std(),
            p99: self.p99.as_std(),
            success: self.success,
            throughput: None,
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CompareError> {
    let data = fs::read(path).map_err(|source| CompareError::Io { path: path.to_path_buf(), source })?;
    serde_json::from_slice(&data).map_err(|source| CompareError::Parse { path: path.to_path_buf(), source })
}

fn push_rows(rows: &mut Vec<Row>, scope: &str, b: &Stats, c: &Stats, max_regression: Option<f64>) {
    // A rise beyond the threshold regresses latency; a fall regresses throughput.
    let over = |change: Change, worse_when_higher: bool| match (change, max_regression) {
        (Change::Percent(p), Some(t)) => if worse_when_higher { p > t } else { -p > t },
        _ => false,
    };
    for (metric, bv, cv) in [("p50", b.p50, c.p50), ("p95", b.p95, c.p95), ("p99", b.p99, c.p99)] {
        let change = percent(bv.as_secs_f64(), cv.as_secs_f64());
        rows.push(Row {
            scope: scope.into(),
            metric,
            baseline: fmt_dur(bv),
            candidate: fmt_dur(cv),
            change,
            regressed: over(change, true),
        });
    }
    if let (Some(bt), Some(ct)) = (b.throughput, c.throughput) {
        let change = percent(bt, ct);
        rows.push(Row {
            scope: scope.into(),
            metric: "throughput",
            baseline: format!("{bt:.1}/s"),
            candidate: format!("{ct:.1}/s"),
            change,
            regressed: over(change, false),
        });
    }
    rows.push(Row {
        scope: scope.into(),
        metric: "success",
        baseline: format!("{:.3}%", b.success * 100.0),
        candidate: format!("{:.3}%", c.success * 100.0),
        change: Change::Points((c.success - b.success) * 100.0),
        regressed: false,
    });
}

fn percent(base: f64, cand: f64) -> Change {
    if base == 0.0 {
        Change::NotApplicable
    } else {
        Change::Percent((cand - base) / base * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "redliner-compare-{}-{tag}-{:?}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A report dir shaped like `report::write_reports` output.
    fn write_run(dir: &Path, rate: f64, overall: (&str, &str, &str, f64, f64), ops: &[(&str, &str, &str, &str, f64)]) {
        let (p50, p95, p99, throughput, success) = overall;
        let metrics = serde_json::json!({
            "requests": 500, "rate": rate, "throughput": throughput, "success": success,
            "status_codes": {"200": 500}, "errors": [], "bytes_in": 1, "bytes_out": 1,
            "latencies": {"mean": p50, "p50": p50, "p90": p95, "p95": p95, "p99": p99, "max": p99, "min": "1ms"}
        });
        fs::write(dir.join("metrics.json"), serde_json::to_vec_pretty(&metrics).unwrap()).unwrap();
        if !ops.is_empty() {
            let rows: Vec<_> = ops
                .iter()
                .map(|(name, p50, p95, p99, success)| {
                    serde_json::json!({"name": name, "requests": 100, "success": success,
                        "p50": p50, "p95": p95, "p99": p99, "max": p99})
                })
                .collect();
            fs::write(dir.join("per_operation.json"), serde_json::to_vec(&rows).unwrap()).unwrap();
        }
    }

    fn report(rate: f64, overall: (&str, &str, &str, f64, f64), ops: &[(&str, &str, &str, &str, f64)]) -> RunReport {
        let dir = tmp("r");
        write_run(&dir, rate, overall, ops);
        load(&dir).unwrap()
    }

    /// A `per_phase.json` beside a run's other reports.
    fn write_phases(dir: &Path, phases: &[(&str, &str, &str, &str, f64)]) {
        let rows: Vec<_> = phases
            .iter()
            .map(|(name, p50, p95, p99, success)| {
                serde_json::json!({"name": name, "requests": 100, "success": success,
                    "p50": p50, "p95": p95, "p99": p99, "max": p99})
            })
            .collect();
        fs::write(dir.join("per_phase.json"), serde_json::to_vec(&rows).unwrap()).unwrap();
    }

    fn report_with_phases(phases: &[(&str, &str, &str, &str, f64)]) -> RunReport {
        let dir = tmp("phases");
        write_run(&dir, 100.0, BASE, &[]);
        write_phases(&dir, phases);
        load(&dir).unwrap()
    }

    fn row<'a>(c: &'a Comparison, scope: &str, metric: &str) -> &'a Row {
        c.rows
            .iter()
            .find(|r| r.scope == scope && r.metric == metric)
            .unwrap_or_else(|| panic!("no {scope} {metric} row in {:?}", c.rows))
    }

    fn pct(r: &Row) -> f64 {
        match r.change {
            Change::Percent(p) => p,
            ref other => panic!("expected a percent change, got {other:?}"),
        }
    }

    const BASE: (&str, &str, &str, f64, f64) = ("10ms", "100ms", "200ms", 100.0, 0.999);

    #[test]
    fn load_reads_a_report_dir_or_its_metrics_file() {
        let dir = tmp("load");
        write_run(&dir, 100.0, BASE, &[("GetUser", "5ms", "50ms", "80ms", 1.0)]);
        for path in [dir.clone(), dir.join("metrics.json")] {
            let r = load(&path).unwrap();
            assert_eq!(r.rate, 100.0);
            assert_eq!(r.overall.p95, Duration::from_millis(100));
            assert_eq!(r.overall.throughput, Some(100.0));
            assert_eq!(r.overall.success, 0.999);
            let op = &r.ops["GetUser"];
            assert_eq!(op.p99, Duration::from_millis(80));
            assert_eq!(op.throughput, None);
        }
    }

    #[test]
    fn load_without_per_operation_has_no_ops() {
        let dir = tmp("noops");
        write_run(&dir, 100.0, BASE, &[]);
        assert!(load(&dir).unwrap().ops.is_empty());
    }

    #[test]
    fn load_errors_name_the_file() {
        let dir = tmp("missing");
        let err = load(&dir).unwrap_err().to_string();
        assert!(err.contains("metrics.json"), "{err}");

        fs::write(dir.join("metrics.json"), "{not json").unwrap();
        let err = load(&dir).unwrap_err().to_string();
        assert!(err.starts_with("parse "), "{err}");
    }

    #[test]
    fn latency_rise_over_the_threshold_regresses() {
        let base = report(100.0, BASE, &[]);
        let cand = report(100.0, ("10ms", "115ms", "205ms", 100.0, 0.999), &[]);
        let c = compare(&base, &cand, Some(10.0));
        let p95 = row(&c, "overall", "p95");
        assert!((pct(p95) - 15.0).abs() < 1e-9);
        assert!(p95.regressed);
        let p99 = row(&c, "overall", "p99");
        assert!((pct(p99) - 2.5).abs() < 1e-9);
        assert!(!p99.regressed, "within the threshold");
        assert!(!row(&c, "overall", "p50").regressed, "unchanged");
        assert_eq!(c.regressions(), 1);
    }

    #[test]
    fn faster_is_never_a_regression() {
        let base = report(100.0, BASE, &[]);
        let cand = report(100.0, ("1ms", "10ms", "20ms", 100.0, 0.999), &[]);
        let c = compare(&base, &cand, Some(0.0));
        assert!((pct(row(&c, "overall", "p95")) + 90.0).abs() < 1e-9);
        assert_eq!(c.regressions(), 0);
    }

    #[test]
    fn throughput_drop_over_the_threshold_regresses() {
        let base = report(100.0, BASE, &[]);
        let cand = report(100.0, ("10ms", "100ms", "200ms", 85.0, 0.999), &[]);
        let c = compare(&base, &cand, Some(10.0));
        let t = row(&c, "overall", "throughput");
        assert!((pct(t) + 15.0).abs() < 1e-9);
        assert!(t.regressed);

        let more = report(100.0, ("10ms", "100ms", "200ms", 150.0, 0.999), &[]);
        assert_eq!(compare(&base, &more, Some(10.0)).regressions(), 0);
    }

    #[test]
    fn identical_runs_pass_a_zero_threshold() {
        let base = report(100.0, BASE, &[("GetUser", "5ms", "50ms", "80ms", 1.0)]);
        let c = compare(&base, &base, Some(0.0));
        assert_eq!(c.regressions(), 0);
    }

    #[test]
    fn without_a_threshold_nothing_regresses() {
        let base = report(100.0, BASE, &[]);
        let cand = report(100.0, ("50ms", "900ms", "2s", 10.0, 0.5), &[]);
        let c = compare(&base, &cand, None);
        assert!(pct(row(&c, "overall", "p95")) > 0.0);
        assert_eq!(c.regressions(), 0);
    }

    #[test]
    fn success_change_is_in_points_and_not_gated() {
        let base = report(100.0, BASE, &[]);
        let cand = report(100.0, ("10ms", "100ms", "200ms", 100.0, 0.95), &[]);
        let c = compare(&base, &cand, Some(0.0));
        let s = row(&c, "overall", "success");
        match s.change {
            Change::Points(p) => assert!((p + 4.9).abs() < 1e-9, "{p}"),
            ref other => panic!("expected points, got {other:?}"),
        }
        assert!(!s.regressed, "success rate is gated by slos.success_rate, not compare");
    }

    #[test]
    fn a_zero_baseline_has_no_percent_change() {
        let base = report(100.0, ("0s", "0s", "0s", 0.0, 0.0), &[]);
        let cand = report(100.0, BASE, &[]);
        let c = compare(&base, &cand, Some(10.0));
        assert_eq!(row(&c, "overall", "p95").change, Change::NotApplicable);
        assert_eq!(c.regressions(), 0);
    }

    #[test]
    fn operations_are_compared_by_name() {
        let base = report(
            100.0,
            BASE,
            &[("GetUser", "5ms", "50ms", "80ms", 1.0), ("Old", "1ms", "1ms", "1ms", 1.0)],
        );
        let cand = report(
            100.0,
            BASE,
            &[("GetUser", "5ms", "50ms", "120ms", 1.0), ("New", "1ms", "1ms", "1ms", 1.0)],
        );
        let c = compare(&base, &cand, Some(10.0));
        let p99 = row(&c, "GetUser", "p99");
        assert!((pct(p99) - 50.0).abs() < 1e-9);
        assert!(p99.regressed);
        assert!(c.rows.iter().all(|r| r.scope != "GetUser" || r.metric != "throughput"));
        assert_eq!(c.only_in_baseline, vec!["Old".to_string()]);
        assert_eq!(c.only_in_candidate, vec!["New".to_string()]);
        assert_eq!(c.regressions(), 1);
    }

    #[test]
    fn load_reads_per_phase_when_present() {
        let r = report_with_phases(&[("step-1(10 rps)", "1ms", "2ms", "3ms", 1.0), ("step-2(50 rps)", "2ms", "4ms", "6ms", 0.99)]);
        assert_eq!(r.phases.len(), 2);
        assert_eq!(r.phases["step-2(50 rps)"].p95, Duration::from_millis(4));
        assert_eq!(r.phases["step-2(50 rps)"].throughput, None);
        assert!(report(100.0, BASE, &[]).phases.is_empty(), "single-phase runs have no per_phase.json");
    }

    #[test]
    fn phases_are_compared_by_name_and_gated() {
        let base = report_with_phases(&[
            ("step-1(10 rps)", "1ms", "2ms", "3ms", 1.0),
            ("step-2(50 rps)", "2ms", "40ms", "90ms", 1.0),
            ("step-3(90 rps)", "2ms", "40ms", "90ms", 1.0),
        ]);
        let cand = report_with_phases(&[
            ("step-1(10 rps)", "1ms", "2ms", "3ms", 1.0),
            ("step-2(50 rps)", "2ms", "88ms", "310ms", 1.0),
            ("step-3(100 rps)", "2ms", "40ms", "90ms", 1.0),
        ]);
        let c = compare(&base, &cand, Some(10.0));
        assert!(!row(&c, "phase step-1(10 rps)", "p95").regressed);
        let p99 = row(&c, "phase step-2(50 rps)", "p99");
        assert!((pct(p99) - 244.44444444444446).abs() < 1e-6, "{p99:?}");
        assert!(p99.regressed);
        assert!(row(&c, "phase step-2(50 rps)", "p95").regressed);
        assert!(c.rows.iter().all(|r| !r.scope.starts_with("phase ") || r.metric != "throughput"));
        assert_eq!(c.only_in_baseline, vec!["phase step-3(90 rps)".to_string()]);
        assert_eq!(c.only_in_candidate, vec!["phase step-3(100 rps)".to_string()]);
        assert_eq!(c.regressions(), 2);
    }

    #[test]
    fn phases_come_after_overall_and_before_operations() {
        let dir = tmp("order");
        write_run(&dir, 100.0, BASE, &[("GetUser", "5ms", "50ms", "80ms", 1.0)]);
        write_phases(&dir, &[("step-1(10 rps)", "1ms", "2ms", "3ms", 1.0), ("step-2(50 rps)", "1ms", "2ms", "3ms", 1.0)]);
        let r = load(&dir).unwrap();
        let out = render(&compare(&r, &r, Some(10.0)), Some(10.0));
        let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("no {needle:?} in {out}"));
        assert!(at("overall") < at("phase step-1(10 rps)"), "{out}");
        assert!(at("phase step-1(10 rps)") < at("phase step-2(50 rps)"), "{out}");
        assert!(at("phase step-2(50 rps)") < at("GetUser"), "{out}");
    }

    #[test]
    fn different_rates_warn() {
        let base = report(100.0, BASE, &[]);
        assert!(compare(&base, &report(103.0, BASE, &[]), None).warnings.is_empty());
        let c = compare(&base, &report(200.0, BASE, &[]), None);
        assert_eq!(c.warnings.len(), 1);
        assert!(c.warnings[0].contains("100.0/s") && c.warnings[0].contains("200.0/s"), "{:?}", c.warnings);
    }

    #[test]
    fn render_shows_the_changes_and_the_verdict() {
        let base = report(100.0, BASE, &[("GetUser", "5ms", "50ms", "80ms", 1.0)]);
        let cand = report(100.0, ("10ms", "115ms", "200ms", 100.0, 0.998), &[("GetUser", "5ms", "50ms", "80ms", 1.0)]);
        let out = render(&compare(&base, &cand, Some(10.0)), Some(10.0));
        let p95 = out.lines().find(|l| l.trim_start().starts_with("p95")).unwrap();
        assert!(p95.contains("100ms") && p95.contains("115ms") && p95.contains("+15.0%"), "{out}");
        assert!(p95.contains("REGRESSED"), "{out}");
        assert!(out.contains("-0.100pp"), "{out}");
        assert!(out.contains("GetUser"), "{out}");
        assert!(out.trim_end().ends_with("1 regression over 10%: overall p95"), "{out}");

        let out = render(&compare(&base, &base, Some(10.0)), Some(10.0));
        assert!(out.trim_end().ends_with("no regressions over 10%"), "{out}");
        let out = render(&compare(&base, &cand, None), None);
        assert!(!out.contains("REGRESSED"), "{out}");
        assert!(out.trim_end().ends_with("--max-regression not set; nothing gated"), "{out}");
    }

    #[test]
    fn junit_has_a_case_per_gated_row() {
        let base = report(100.0, BASE, &[("GetUser", "5ms", "50ms", "80ms", 1.0)]);
        let cand = report(100.0, ("10ms", "115ms", "200ms", 100.0, 0.9), &[("GetUser", "5ms", "50ms", "80ms", 1.0)]);
        let cases = junit_cases(&compare(&base, &cand, Some(10.0)), Some(10.0));
        let names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["overall p50", "overall p95", "overall p99", "overall throughput", "GetUser p50", "GetUser p95", "GetUser p99"],
            "success rows aren't gated, so they aren't test cases"
        );
        let failed: Vec<_> = cases.iter().filter_map(|c| c.failure.as_deref()).collect();
        assert_eq!(failed, ["overall p95: 100ms -> 115ms (+15.0%, over the 10% limit)"]);
    }

    #[test]
    fn junit_without_a_threshold_passes_every_case() {
        let base = report(100.0, BASE, &[]);
        let cand = report(100.0, ("50ms", "900ms", "2s", 10.0, 0.5), &[]);
        let cases = junit_cases(&compare(&base, &cand, None), None);
        assert_eq!(cases.len(), 4);
        assert!(cases.iter().all(|c| c.failure.is_none()));
    }

    #[test]
    fn parse_percent_accepts_a_trailing_percent_sign() {
        assert_eq!(parse_percent("10%"), Ok(10.0));
        assert_eq!(parse_percent("2.5"), Ok(2.5));
        assert_eq!(parse_percent("0%"), Ok(0.0));
        for bad in ["-1%", "abc", "%", "", "NaN", "inf"] {
            assert!(parse_percent(bad).is_err(), "{bad} accepted");
        }
    }
}
