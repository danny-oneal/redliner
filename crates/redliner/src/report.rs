//! Per-run report directory (Go `report.go` + `slo.go` `writeJUnit`).
//!
//! A run leaves `reports/<UTC timestamp>/` (plus a `latest` symlink) with a
//! text summary, latency histogram, overall `metrics.json`, per-operation and
//! per-phase summaries, a `run_manifest.json` audit record, `junit.xml` (when
//! enabled), and a raw `results.jsonl` stream (when `keep_raw`).

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use volley::{AttackResult, Metrics};

use crate::config::Config;
use crate::replay::ReplaySummary;
use crate::slo::{fmt_dur, SloResult};

/// Latency histogram with log-spaced buckets spanning sub-ms lookups to
/// multi-second responses under distress (Go `newHistogram`).
pub struct LatencyHistogram {
    edges: Vec<Duration>,
    counts: Vec<u64>,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyHistogram {
    pub fn new() -> Self {
        let edges = vec![
            Duration::ZERO,
            Duration::from_micros(500),
            Duration::from_millis(1),
            Duration::from_millis(2),
            Duration::from_millis(5),
            Duration::from_millis(10),
            Duration::from_millis(25),
            Duration::from_millis(50),
            Duration::from_millis(100),
            Duration::from_millis(250),
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(5),
        ];
        let counts = vec![0u64; edges.len()];
        Self { edges, counts }
    }

    /// Records a latency into the highest bucket whose edge is `<=` it.
    pub fn add(&mut self, latency: Duration) {
        let mut idx = 0;
        for (i, edge) in self.edges.iter().enumerate() {
            if latency >= *edge {
                idx = i;
            } else {
                break;
            }
        }
        self.counts[idx] += 1;
    }

    pub fn report(&self) -> String {
        let total: u64 = self.counts.iter().sum();
        let mut s = String::new();
        for (i, edge) in self.edges.iter().enumerate() {
            let range = match self.edges.get(i + 1) {
                Some(next) => format!("[{}, {})", fmt_dur(*edge), fmt_dur(*next)),
                None => format!("[{}, +Inf)", fmt_dur(*edge)),
            };
            let c = self.counts[i];
            let pct = if total > 0 {
                c as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            s.push_str(&format!("{range:<24} {c:>8}  {pct:5.1}%\n"));
        }
        s
    }
}

/// Accumulates results across phases: overall metrics, per-operation metrics,
/// per-phase metrics, the latency histogram, and the replayable raw stream
/// (Go `collector`).
pub struct Collector {
    overall: Metrics,
    per_op: BTreeMap<String, Metrics>,
    per_phase: BTreeMap<String, Metrics>,
    hist: LatencyHistogram,
    raw: Option<BufWriter<File>>,
}

impl Collector {
    /// Opens a collector; when `raw_path` is set, every recorded result is
    /// also appended as one JSON line to that file.
    pub fn new(raw_path: Option<&Path>) -> std::io::Result<Self> {
        let raw = match raw_path {
            Some(p) => Some(BufWriter::new(File::create(p)?)),
            None => None,
        };
        Ok(Self {
            overall: Metrics::new(),
            per_op: BTreeMap::new(),
            per_phase: BTreeMap::new(),
            hist: LatencyHistogram::new(),
            raw,
        })
    }

    /// Records one result into overall, the histogram, the named operation
    /// (when attributable), the phase, and the raw stream (Go `Add`).
    pub fn add(&mut self, r: &AttackResult, phase: &str, op: Option<&str>) {
        if let Some(w) = self.raw.as_mut() {
            if let Ok(line) = serde_json::to_string(&RawRecord::from(r)) {
                let _ = writeln!(w, "{line}");
            }
        }
        self.overall.add(r);
        self.hist.add(r.latency);
        if let Some(op) = op {
            if !op.is_empty() {
                self.per_op.entry(op.to_string()).or_default().add(r);
            }
        }
        self.per_phase.entry(phase.to_string()).or_default().add(r);
    }

    pub fn close(&mut self) {
        self.overall.close();
        for m in self.per_op.values_mut() {
            m.close();
        }
        for m in self.per_phase.values_mut() {
            m.close();
        }
        if let Some(w) = self.raw.as_mut() {
            let _ = w.flush();
        }
    }

    pub fn overall(&self) -> &Metrics {
        &self.overall
    }

    pub fn per_op(&self) -> &BTreeMap<String, Metrics> {
        &self.per_op
    }
}

/// Creates `base/<UTC timestamp>/` and best-effort refreshes `base/latest`
/// to point at it (Go `makeReportDir`). A symlink failure is swallowed.
pub fn make_report_dir(base: &str, start: SystemTime) -> std::io::Result<PathBuf> {
    let secs = start.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let (y, mo, d, h, mi, s) = ymd_hms(secs);
    let name = format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z");
    let dir = Path::new(base).join(&name);
    fs::create_dir_all(&dir)?;

    let link = Path::new(base).join("latest");
    let _ = fs::remove_file(&link);
    #[cfg(unix)]
    {
        let _ = std::os::unix::fs::symlink(&name, &link);
    }
    Ok(dir)
}

/// Writes the text/histogram/metrics/per-op/per-phase/manifest files
/// (Go `writeReports`). `per_phase.json` only when more than one phase ran.
pub fn write_reports(dir: &Path, c: &Collector, manifest: &RunManifest) -> std::io::Result<()> {
    fs::write(dir.join("text.txt"), text_summary(&c.overall))?;
    fs::write(dir.join("histogram.txt"), c.hist.report())?;
    write_json(&dir.join("metrics.json"), &MetricsJson::from(&c.overall))?;
    if !c.per_op.is_empty() {
        write_json(&dir.join("per_operation.json"), &summarize(&c.per_op))?;
    }
    if c.per_phase.len() > 1 {
        write_json(&dir.join("per_phase.json"), &summarize(&c.per_phase))?;
    }
    write_json(&dir.join("run_manifest.json"), manifest)
}

/// Writes just the manifest — used before the attack so an aborted run still
/// leaves an audit trail; `write_reports` overwrites it with the finalized one.
pub fn write_manifest(dir: &Path, manifest: &RunManifest) -> std::io::Result<()> {
    write_json(&dir.join("run_manifest.json"), manifest)
}

/// Writes SLO verdicts as a JUnit `<testsuite>` for CI ingestion
/// (Go `writeJUnit`).
pub fn write_junit(path: &Path, slos: &[SloResult], elapsed: Duration) -> std::io::Result<()> {
    let cases: Vec<JunitCase> = slos
        .iter()
        .map(|r| JunitCase {
            name: r.name.clone(),
            failure: (!r.passed)
                .then(|| format!("{}: target {}, observed {}", r.name, r.target, r.observed)),
        })
        .collect();
    write_junit_suite(path, "redliner.slos", &cases, elapsed)
}

/// One JUnit `<testcase>`; `failure` is the message of a failed one.
#[derive(Debug, Clone, PartialEq)]
pub struct JunitCase {
    pub name: String,
    pub failure: Option<String>,
}

/// Writes `cases` as one JUnit `<testsuite>` named `suite`.
pub fn write_junit_suite(
    path: &Path,
    suite: &str,
    cases: &[JunitCase],
    elapsed: Duration,
) -> std::io::Result<()> {
    let failures = cases.iter().filter(|c| c.failure.is_some()).count();
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" time=\"{:.3}\">\n",
        esc(suite),
        cases.len(),
        failures,
        elapsed.as_secs_f64()
    ));
    for c in cases {
        match &c.failure {
            None => xml.push_str(&format!(
                "  <testcase name=\"{}\" classname=\"redliner\"></testcase>\n",
                esc(&c.name)
            )),
            Some(msg) => xml.push_str(&format!(
                "  <testcase name=\"{}\" classname=\"redliner\">\n    <failure message=\"{}\"></failure>\n  </testcase>\n",
                esc(&c.name),
                esc(msg)
            )),
        }
    }
    xml.push_str("</testsuite>\n");
    fs::write(path, xml)
}

/// Compact per-operation / per-phase row teams actually read (Go `opSummary`).
#[derive(Debug, Serialize)]
pub struct OpSummary {
    pub name: String,
    pub requests: u64,
    pub success: f64,
    pub p50: String,
    pub p95: String,
    pub p99: String,
    pub max: String,
}

/// Summarizes a per-op or per-phase metric map, sorted by name.
pub fn summarize(ms: &BTreeMap<String, Metrics>) -> Vec<OpSummary> {
    ms.iter()
        .map(|(name, m)| OpSummary {
            name: name.clone(),
            requests: m.requests,
            success: m.success,
            p50: fmt_dur(m.latencies.p50),
            p95: fmt_dur(m.latencies.p95),
            p99: fmt_dur(m.latencies.p99),
            max: fmt_dur(m.latencies.max),
        })
        .collect()
}

/// Everything needed to reproduce or audit a run (Go `runManifest`). Written
/// before the attack (so aborted runs leave a trail) and finalized after.
#[derive(Serialize)]
pub struct RunManifest {
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub config_file: String,
    pub config: Config,
    pub seed: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requests: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub slo_results: Vec<SloResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passed: Option<bool>,
    /// What a replay run fired; absent in profile mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay: Option<ReplaySummary>,
    /// The run was interrupted (Ctrl-C) and its results are partial.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub aborted: bool,
}

/// An RFC3339-ish UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) for manifests.
pub fn utc_rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let (y, mo, d, h, mi, s) = ymd_hms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

// ---- internals ---------------------------------------------------------------

#[derive(Serialize)]
struct RawRecord {
    seq: u64,
    code: u16,
    latency_ns: u64,
    bytes_in: u64,
    bytes_out: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    method: String,
    url: String,
}

impl RawRecord {
    fn from(r: &AttackResult) -> Self {
        Self {
            seq: r.seq,
            code: r.code,
            latency_ns: u64::try_from(r.latency.as_nanos()).unwrap_or(u64::MAX),
            bytes_in: r.bytes_in,
            bytes_out: r.bytes_out,
            error: r.error.clone(),
            method: r.method.clone(),
            url: r.url.clone(),
        }
    }
}

#[derive(Serialize)]
struct LatenciesJson {
    mean: String,
    p50: String,
    p90: String,
    p95: String,
    p99: String,
    max: String,
    min: String,
}

#[derive(Serialize)]
struct MetricsJson {
    requests: u64,
    rate: f64,
    throughput: f64,
    success: f64,
    status_codes: BTreeMap<u16, u64>,
    errors: Vec<String>,
    bytes_in: u64,
    bytes_out: u64,
    latencies: LatenciesJson,
}

impl MetricsJson {
    fn from(m: &Metrics) -> Self {
        Self {
            requests: m.requests,
            rate: m.rate,
            throughput: m.throughput,
            success: m.success,
            status_codes: m.status_codes.clone(),
            errors: m.errors.clone(),
            bytes_in: m.bytes_in,
            bytes_out: m.bytes_out,
            latencies: LatenciesJson {
                mean: fmt_dur(m.latencies.mean),
                p50: fmt_dur(m.latencies.p50),
                p90: fmt_dur(m.latencies.p90),
                p95: fmt_dur(m.latencies.p95),
                p99: fmt_dur(m.latencies.p99),
                max: fmt_dur(m.latencies.max),
                min: fmt_dur(m.latencies.min),
            },
        }
    }
}

fn text_summary(m: &Metrics) -> String {
    let mut s = String::new();
    s.push_str(&format!("requests   : {}\n", m.requests));
    s.push_str(&format!("rate       : {:.1} rps\n", m.rate));
    s.push_str(&format!("throughput : {:.1} rps\n", m.throughput));
    s.push_str(&format!("success    : {:.3}%\n", m.success * 100.0));
    s.push_str(&format!("p50        : {}\n", fmt_dur(m.latencies.p50)));
    s.push_str(&format!("p95        : {}\n", fmt_dur(m.latencies.p95)));
    s.push_str(&format!("p99        : {}\n", fmt_dur(m.latencies.p99)));
    s.push_str(&format!("max        : {}\n", fmt_dur(m.latencies.max)));
    s.push_str("status codes:\n");
    for (code, n) in &m.status_codes {
        s.push_str(&format!("  {code} : {n}\n"));
    }
    if !m.errors.is_empty() {
        s.push_str("errors:\n");
        for e in &m.errors {
            s.push_str(&format!("  - {e}\n"));
        }
    }
    s
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    let mut data = serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?;
    data.push(b'\n');
    fs::write(path, data)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Civil (year, month, day, hour, minute, second) in UTC from Unix seconds.
/// Howard Hinnant's days-from-civil inverse; no calendar dependency.
fn ymd_hms(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32, hh as u32, mm as u32, ss as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "redliner-report-{}-{tag}-{:?}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn res(code: u16, latency_ms: u64, url: &str) -> AttackResult {
        AttackResult {
            attack: "p".into(),
            seq: 0,
            code,
            timestamp: SystemTime::now(),
            latency: Duration::from_millis(latency_ms),
            bytes_in: 3,
            bytes_out: 1,
            error: None,
            method: "GET".into(),
            url: url.into(),
            body: vec![],
        }
    }

    #[test]
    fn collector_splits_overall_op_phase() {
        let mut c = Collector::new(None).unwrap();
        c.add(&res(200, 1, "http://h/a"), "phase1", Some("A"));
        c.add(&res(200, 1, "http://h/b"), "phase1", Some("B"));
        c.add(&res(200, 1, "http://h/a"), "phase2", Some("A"));
        c.add(&res(200, 1, "http://h/x"), "phase2", None); // matches no op
        c.close();

        assert_eq!(c.overall().requests, 4);
        let ops = c.per_op();
        assert_eq!(ops.len(), 2, "only named ops; no empty key: {:?}", ops.keys());
        assert!(ops.contains_key("A") && ops.contains_key("B"));
        assert!(!ops.contains_key(""));
        assert_eq!(ops["A"].requests, 2);
    }

    #[test]
    fn writes_core_files() {
        let base = tmp("core");
        let dir = make_report_dir(base.to_str().unwrap(), SystemTime::now()).unwrap();
        assert!(dir.is_dir());
        let link = base.join("latest");
        assert_eq!(fs::read_link(&link).unwrap(), Path::new(dir.file_name().unwrap()));

        let mut c = Collector::new(None).unwrap();
        c.add(&res(200, 2, "http://h/a"), "phase1", Some("A"));
        c.close();
        let manifest = RunManifest {
            started_at: utc_rfc3339(SystemTime::now()),
            finished_at: None,
            config_file: "cfg.yaml".into(),
            config: Config::default(),
            seed: 7,
            requests: Some(c.overall().requests),
            slo_results: vec![],
            passed: None,
            replay: None,
            aborted: false,
        };
        write_reports(&dir, &c, &manifest).unwrap();

        for f in ["text.txt", "histogram.txt", "metrics.json", "per_operation.json", "run_manifest.json"] {
            assert!(dir.join(f).is_file(), "{f} missing");
        }
        let m: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("metrics.json")).unwrap()).unwrap();
        assert_eq!(m["requests"], 1);
    }

    #[test]
    fn per_phase_only_when_multiphase() {
        let one_dir = tmp("one");
        let mut one = Collector::new(None).unwrap();
        one.add(&res(200, 1, "http://h/a"), "only", Some("A"));
        one.close();
        let mf = RunManifest {
            started_at: utc_rfc3339(SystemTime::now()),
            finished_at: None,
            config_file: "c".into(),
            config: Config::default(),
            seed: 0,
            requests: None,
            slo_results: vec![],
            passed: None,
            replay: None,
            aborted: false,
        };
        write_reports(&one_dir, &one, &mf).unwrap();
        assert!(!one_dir.join("per_phase.json").exists());

        let two_dir = tmp("two");
        let mut two = Collector::new(None).unwrap();
        two.add(&res(200, 1, "http://h/a"), "p1", Some("A"));
        two.add(&res(200, 1, "http://h/a"), "p2", Some("A"));
        two.close();
        write_reports(&two_dir, &two, &mf).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&fs::read(two_dir.join("per_phase.json")).unwrap()).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn keep_raw_writes_jsonl() {
        let dir = tmp("raw");
        let path = dir.join("results.jsonl");
        let mut c = Collector::new(Some(&path)).unwrap();
        for i in 0..3 {
            let mut r = res(200, 1, "http://h/a");
            r.seq = i;
            c.add(&r, "p1", Some("A"));
        }
        c.close();
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3);
        let last: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(last["seq"], 2);
        assert_eq!(last["code"], 200);

        let none_dir = tmp("noraw");
        let _ = Collector::new(None).unwrap();
        assert!(!none_dir.join("results.jsonl").exists());
    }

    #[test]
    fn junit_marks_failures() {
        let dir = tmp("junit");
        let path = dir.join("junit.xml");
        let slos = vec![
            SloResult {
                name: "success_rate".into(),
                target: ">= 0.9900".into(),
                observed: "1.0000".into(),
                passed: true,
            },
            SloResult {
                name: "latency_p95".into(),
                target: "<= 50ms".into(),
                observed: "200ms".into(),
                passed: false,
            },
        ];
        write_junit(&path, &slos, Duration::from_secs(1)).unwrap();
        let xml = fs::read_to_string(&path).unwrap();
        assert!(xml.contains("tests=\"2\""), "{xml}");
        assert!(xml.contains("failures=\"1\""), "{xml}");
        assert_eq!(xml.matches("<failure").count(), 1);
        assert!(xml.contains("latency_p95"));
    }

    #[test]
    fn junit_output_is_stable() {
        let dir = tmp("junit-exact");
        let path = dir.join("junit.xml");
        let slos = vec![
            SloResult { name: "ok".into(), target: "<= 1s".into(), observed: "1ms".into(), passed: true },
            SloResult { name: "p<95>".into(), target: "<= 50ms".into(), observed: "\"200ms\"".into(), passed: false },
        ];
        write_junit(&path, &slos, Duration::from_millis(1500)).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <testsuite name=\"redliner.slos\" tests=\"2\" failures=\"1\" time=\"1.500\">\n\
             \x20 <testcase name=\"ok\" classname=\"redliner\"></testcase>\n\
             \x20 <testcase name=\"p&lt;95&gt;\" classname=\"redliner\">\n\
             \x20   <failure message=\"p&lt;95&gt;: target &lt;= 50ms, observed &quot;200ms&quot;\"></failure>\n\
             \x20 </testcase>\n\
             </testsuite>\n"
        );
    }

    #[test]
    fn histogram_buckets_monotonic() {
        let mut h = LatencyHistogram::new();
        for ms in [0u64, 1, 3, 30, 1_500] {
            h.add(Duration::from_millis(ms));
        }
        let rep = h.report();
        // 14 bucket lines, counts summing to the 5 recorded samples.
        assert_eq!(rep.lines().count(), 14);
        let counts: u64 = rep
            .lines()
            .filter_map(|l| l.split_whitespace().rev().nth(1))
            .filter_map(|n| n.parse::<u64>().ok())
            .sum();
        assert_eq!(counts, 5, "{rep}");
    }
}
