//! SLO gate (Go `evaluateSLOs` / `slosPassed`; JUnit output deferred).

use std::time::Duration;

use volley::Metrics;

use crate::config::SloConfig;

#[derive(Debug, Clone)]
pub struct SloResult {
    pub name: String,
    pub target: String,
    pub observed: String,
    pub passed: bool,
}

/// Rounds to microseconds and formats the duration.
pub fn fmt_dur(d: Duration) -> String {
    let d = Duration::from_micros(d.as_micros() as u64);
    format!("{d:?}")
}

pub fn evaluate(slos: &SloConfig, m: &Metrics) -> Vec<SloResult> {
    let mut out = Vec::new();
    if slos.success_rate > 0.0 {
        out.push(SloResult {
            name: "success_rate".into(),
            target: format!(">= {:.4}", slos.success_rate),
            observed: format!("{:.4}", m.success),
            passed: m.success >= slos.success_rate,
        });
    }
    let mut lat = |name: &str, target: Duration, observed: Duration| {
        if !target.is_zero() {
            out.push(SloResult {
                name: name.into(),
                target: format!("<= {target:?}"),
                observed: fmt_dur(observed),
                passed: observed <= target,
            });
        }
    };
    lat("latency_p50", slos.p50.as_std(), m.latencies.p50);
    lat("latency_p95", slos.p95.as_std(), m.latencies.p95);
    lat("latency_p99", slos.p99.as_std(), m.latencies.p99);
    if slos.throughput_min > 0.0 {
        out.push(SloResult {
            name: "throughput".into(),
            target: format!(">= {:.1} rps", slos.throughput_min),
            observed: format!("{:.1} rps", m.throughput),
            passed: m.throughput >= slos.throughput_min,
        });
    }
    out
}

pub fn passed(results: &[SloResult]) -> bool {
    results.iter().all(|r| r.passed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigDuration;
    use std::time::SystemTime;
    use volley::AttackResult;

    #[test]
    fn slo_fails_on_latency() {
        let mut m = Metrics::new();
        for _ in 0..10 {
            m.add(&AttackResult {
                attack: "t".into(),
                seq: 0,
                code: 200,
                timestamp: SystemTime::now(),
                latency: Duration::from_millis(200),
                bytes_in: 0,
                bytes_out: 0,
                error: None,
                method: "POST".into(),
                url: "http://x".into(),
                body: vec![],
            });
        }
        m.close();
        let slos = SloConfig {
            p95: ConfigDuration(Duration::from_millis(50)),
            ..Default::default()
        };
        let res = evaluate(&slos, &m);
        assert_eq!(res.len(), 1);
        assert!(!res[0].passed);
        assert!(!passed(&res));

        let ok = SloConfig { p95: ConfigDuration(Duration::from_secs(1)), ..Default::default() };
        assert!(passed(&evaluate(&ok, &m)));
    }
}
