//! SLO gate (Go `evaluateSLOs` / `slosPassed`; JUnit output deferred).

use std::time::Duration;

use volley::Metrics;

use crate::config::SloConfig;

#[derive(Debug, Clone, serde::Serialize)]
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
    lat("latency_p999", slos.p999.as_std(), m.latencies.p999);
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

/// The built-in check that some request succeeded, enforced with or without
/// SLOs: `Some` (a failed result) when none fired or none of them was
/// `accepted` by the checker.
pub fn no_success(m: &Metrics, accepted: u64) -> Option<SloResult> {
    if accepted > 0 {
        return None;
    }
    let observed = match m.requests {
        0 => "no requests fired".to_string(),
        n => format!("0 of {n}"),
    };
    Some(SloResult {
        name: "successful_requests".into(),
        target: ">= 1".into(),
        observed,
        passed: false,
    })
}

/// The stderr line for a run that [`no_success`] failed.
pub fn no_success_warning(m: &Metrics) -> String {
    if m.requests == 0 {
        return "warning: no requests fired; check load.rate and load.duration".into();
    }
    format!(
        "warning: no request succeeded (0 of {}; status: {}); the errors in the summary say why",
        m.requests,
        crate::report::status_classes(&m.status_codes)
    )
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

    fn hit(code: u16, error: Option<&str>) -> AttackResult {
        AttackResult {
            attack: "t".into(),
            seq: 0,
            code,
            timestamp: SystemTime::now(),
            latency: Duration::from_millis(1),
            bytes_in: 0,
            bytes_out: 0,
            error: error.map(Into::into),
            method: "GET".into(),
            url: "http://x".into(),
            body: vec![],
        }
    }

    #[test]
    fn no_success_flags_empty_and_all_failed_runs() {
        let mut empty = Metrics::new();
        empty.close();
        let r = no_success(&empty, 0).expect("empty run must fail");
        assert_eq!(
            (r.name.as_str(), r.target.as_str(), r.observed.as_str(), r.passed),
            ("successful_requests", ">= 1", "no requests fired", false)
        );

        let mut dead = Metrics::new();
        for _ in 0..20 {
            dead.add(&hit(0, Some("connection refused")));
        }
        dead.close();
        let r = no_success(&dead, 0).expect("all-failed run must fail");
        assert_eq!(r.observed, "0 of 20");
        assert!(!r.passed);

        let mut one = Metrics::new();
        for _ in 0..20 {
            one.add(&hit(0, Some("connection refused")));
        }
        one.add(&hit(200, None));
        one.close();
        assert!(no_success(&one, 1).is_none());
    }

    #[test]
    fn no_success_warning_names_the_cause() {
        let mut empty = Metrics::new();
        empty.close();
        let w = no_success_warning(&empty);
        assert!(w.contains("no requests fired") && w.contains("load.rate"), "{w}");

        let mut failed = Metrics::new();
        for _ in 0..3 {
            failed.add(&hit(0, Some("check: graphql errors (http 200): boom")));
        }
        failed.close();
        let w = no_success_warning(&failed);
        assert!(w.contains("0 of 3") && w.contains("errors"), "{w}");
        assert!(!w.contains("target up"), "{w}");
    }

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

        let p999 = SloConfig { p999: ConfigDuration(Duration::from_millis(100)), ..Default::default() };
        let res = evaluate(&p999, &m);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "latency_p999");
        assert!(!res[0].passed);
        assert!(p999.enforced());
    }
}
