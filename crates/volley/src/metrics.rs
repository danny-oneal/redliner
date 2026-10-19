//! Aggregated attack metrics (Go `vegeta.Metrics`).
//!
//! Latencies are stored in the histogram in nanoseconds.

use crate::target::AttackResult;
use hdrhistogram::Histogram;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

/// Latency summary (Go `vegeta.LatencyMetrics`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Latencies {
    pub total: Duration,
    pub mean: Duration,
    pub p50: Duration,
    pub p90: Duration,
    pub p95: Duration,
    pub p99: Duration,
    pub max: Duration,
    pub min: Duration,
}

/// Aggregated results of an attack. Call [`Metrics::add`] per result, then
/// [`Metrics::close`] once to finalize derived fields.
#[derive(Debug, Clone)]
pub struct Metrics {
    pub requests: u64,
    /// Requests per second issued over the attack window.
    pub rate: f64,
    /// Successful requests per second over the window including final latency.
    pub throughput: f64,
    /// Fraction of requests with no error and a 2xx/3xx code.
    pub success: f64,
    pub latencies: Latencies,
    pub status_codes: BTreeMap<u16, u64>,
    /// Distinct error strings, sorted.
    pub errors: Vec<String>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub earliest: Option<SystemTime>,
    pub latest: Option<SystemTime>,
    pub end: Option<SystemTime>,
    successes: u64,
    latency_total: Duration,
    lat_min: Option<Duration>,
    lat_max: Duration,
    hist: Histogram<u64>,
    error_set: BTreeSet<String>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            requests: 0,
            rate: 0.0,
            throughput: 0.0,
            success: 0.0,
            latencies: Latencies::default(),
            status_codes: BTreeMap::new(),
            errors: Vec::new(),
            bytes_in: 0,
            bytes_out: 0,
            earliest: None,
            latest: None,
            end: None,
            successes: 0,
            latency_total: Duration::ZERO,
            lat_min: None,
            lat_max: Duration::ZERO,
            // 1ns .. 1h, 3 significant digits.
            hist: Histogram::new_with_bounds(1, 3_600_000_000_000, 3)
                .expect("valid histogram bounds"),
            error_set: BTreeSet::new(),
        }
    }

    /// Record one result.
    pub fn add(&mut self, r: &AttackResult) {
        self.requests += 1;
        self.bytes_in += r.bytes_in;
        self.bytes_out += r.bytes_out;
        *self.status_codes.entry(r.code).or_insert(0) += 1;

        let nanos = u64::try_from(r.latency.as_nanos()).unwrap_or(u64::MAX);
        self.hist.saturating_record(nanos.max(1));
        self.latency_total += r.latency;
        if self.lat_min.is_none_or(|m| r.latency < m) {
            self.lat_min = Some(r.latency);
        }
        if r.latency > self.lat_max {
            self.lat_max = r.latency;
        }

        if self.earliest.is_none_or(|e| r.timestamp < e) {
            self.earliest = Some(r.timestamp);
        }
        if self.latest.is_none_or(|l| r.timestamp > l) {
            self.latest = Some(r.timestamp);
        }
        let done = r.timestamp + r.latency;
        if self.end.is_none_or(|e| done > e) {
            self.end = Some(done);
        }

        if r.error.is_none() && (200..400).contains(&r.code) {
            self.successes += 1;
        }
        if let Some(e) = &r.error {
            if !e.is_empty() {
                self.error_set.insert(e.clone());
            }
        }
    }

    /// Finalize derived fields. Idempotent.
    pub fn close(&mut self) {
        self.errors = self.error_set.iter().cloned().collect();
        if self.requests == 0 {
            return;
        }
        let n = self.requests as f64;
        self.success = self.successes as f64 / n;

        let q = |p: f64| Duration::from_nanos(self.hist.value_at_quantile(p));
        self.latencies = Latencies {
            total: self.latency_total,
            mean: self.latency_total / u32::try_from(self.requests).unwrap_or(u32::MAX),
            p50: q(0.50),
            p90: q(0.90),
            p95: q(0.95),
            p99: q(0.99),
            max: self.lat_max,
            min: self.lat_min.unwrap_or_default(),
        };

        // Vegeta semantics: rate over [earliest, latest], throughput over [earliest, end].
        // A zero span leaves rate = requests and throughput = successes.
        self.rate = self.requests as f64;
        self.throughput = self.successes as f64;
        let (Some(first), Some(last), Some(end)) = (self.earliest, self.latest, self.end) else {
            return;
        };
        let issue = last.duration_since(first).unwrap_or_default().as_secs_f64();
        let total = end.duration_since(first).unwrap_or_default().as_secs_f64();
        if issue > 0.0 {
            self.rate = self.requests as f64 / issue;
        }
        if total > 0.0 {
            self.throughput = self.successes as f64 / total;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::AttackResult;
    use std::time::{Duration, SystemTime};

    fn results() -> Vec<AttackResult> {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        (0..100u64)
            .map(|i| {
                let ok = i < 90;
                AttackResult {
                    attack: "t".into(),
                    seq: i,
                    code: if ok { 200 } else { 0 },
                    timestamp: base + Duration::from_millis(i * 10),
                    latency: Duration::from_millis(1 + i % 10),
                    bytes_in: 5,
                    bytes_out: 2,
                    error: if ok { None } else { Some("boom".into()) },
                    method: "GET".into(),
                    url: "http://x".into(),
                    body: vec![],
                }
            })
            .collect()
    }

    fn closed() -> Metrics {
        let mut m = Metrics::default();
        for r in results() {
            m.add(&r);
        }
        m.close();
        m
    }

    #[test]
    fn metrics_success_and_percentiles() {
        let m = closed();
        assert_eq!(m.requests, 100);
        assert!((m.success - 0.9).abs() < 1e-9);
        assert!(m.latencies.p50 > Duration::ZERO);
        assert_eq!(m.status_codes[&200], 90);
        assert_eq!(m.errors.len(), 1);
        assert_eq!(m.bytes_in, 500);
        assert_eq!(m.bytes_out, 200);
    }

    #[test]
    fn throughput_counts_only_success() {
        let m = closed();
        // latest ts = 990ms, its latency = 10ms => end 1.0s; span 1s; 90 successes.
        assert!((m.throughput - 90.0).abs() < 0.5, "got {}", m.throughput);
        // vegeta: rate = requests / (latest - earliest) = 100 / 0.99s.
        assert!((m.rate - 100.0 / 0.99).abs() < 1e-6, "got {}", m.rate);
    }
}
