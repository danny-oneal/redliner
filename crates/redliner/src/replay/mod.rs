//! Production-log replay: read a request log, match it to scenarios, and
//! fire it at its recorded pace.

pub mod matcher;
pub mod parse;
pub mod schedule;
pub mod targeter;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use crate::config::Config;
use crate::slo::fmt_dur;

use self::matcher::match_events;
use self::parse::read_trace;
use self::schedule::{build_schedule, Schedule};

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("replay: read {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("replay: invalid HAR: {0}")]
    Har(String),
    #[error("replay: invalid window: {0}")]
    Window(String),
    #[error("replay: line {line} ({event}) matches both scenario {first:?} and {second:?}; make the match rules disjoint")]
    Ambiguous {
        line: usize,
        event: String,
        first: String,
        second: String,
    },
    #[error("replay: {count} log events matched no scenario (on_unmatched: fail); most frequent: {top}")]
    Unmatched { count: u64, top: String },
    #[error("replay: no events left to replay (parsed {parsed}, malformed {malformed}, outside window {filtered}, unmatched {unmatched})")]
    Empty {
        parsed: u64,
        malformed: u64,
        filtered: u64,
        unmatched: u64,
    },
    #[error("replay: schedule spans {span} (> 24h); check timestamp units or outlier timestamps, narrow replay.window, or raise --speed")]
    SpanTooLong { span: String },
    #[error("replay: config has no replay block")]
    NotConfigured,
}

/// What the replay will do, for the pre-run banner and the run manifest.
#[derive(Debug, Clone, Serialize)]
pub struct ReplaySummary {
    pub source: String,
    /// Events that parsed, including those the window later removed.
    pub parsed: u64,
    pub malformed: u64,
    pub malformed_examples: Vec<String>,
    /// Events outside the replay window.
    pub filtered: u64,
    /// Matched events per scenario (before `copies` amplification).
    pub matched: BTreeMap<String, u64>,
    pub unmatched: u64,
    pub unmatched_top: Vec<(String, u64)>,
    /// Requests the schedule will fire.
    pub hits: u64,
    pub span: String,
    pub speed: f64,
    pub copies: u64,
    pub peak_rps: u64,
    /// Hits actually fired; `None` until the run has executed (so `--validate`
    /// omits it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired: Option<u64>,
    /// Scheduled hits the run never reached (`hits - fired`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped: Option<u64>,
}

impl ReplaySummary {
    /// A compact multi-line block, aligned like the run summary.
    pub fn render(&self) -> String {
        let mut out = format!("replay: {}\n", self.source);
        let mut line = |label: &str, value: String| {
            out.push_str(&format!("  {label:<14}: {value}\n"));
        };
        line(
            "events",
            format!(
                "{} parsed, {} malformed, {} outside window",
                self.parsed, self.malformed, self.filtered
            ),
        );
        let matched: u64 = self.matched.values().sum();
        line(
            "matching",
            format!("{matched} matched, {} unmatched", self.unmatched),
        );
        line("hits", self.hits.to_string());
        if let Some(fired) = self.fired {
            let dropped = self.dropped.unwrap_or_else(|| self.hits.saturating_sub(fired));
            line("fired", format!("{fired} of {} ({dropped} dropped)", self.hits));
        }
        line("span", self.span.clone());
        line("speed", format!("{}x", self.speed));
        line("copies", self.copies.to_string());
        line("peak", format!("{} rps", self.peak_rps));
        out.push_str("  scenarios:\n");
        for (name, n) in &self.matched {
            out.push_str(&format!("    {name:<24} {n}\n"));
        }
        if !self.malformed_examples.is_empty() {
            out.push_str("  malformed:\n");
            for e in &self.malformed_examples {
                out.push_str(&format!("    {e}\n"));
            }
        }
        if !self.unmatched_top.is_empty() {
            out.push_str("  unmatched (top):\n");
            for (key, n) in &self.unmatched_top {
                out.push_str(&format!("    {key} ({n})\n"));
            }
        }
        out
    }
}

/// A validated, ready-to-fire replay.
#[derive(Debug, Clone)]
pub struct Replay {
    pub schedule: Arc<Schedule>,
    pub summary: ReplaySummary,
}

/// Reads the log, matches it to scenarios, and builds the schedule. Every
/// failure surfaces here, before any request is fired.
pub fn build_replay(cfg: &Config) -> Result<Replay, ReplayError> {
    let rc = cfg.replay.as_ref().ok_or(ReplayError::NotConfigured)?;
    let trace = read_trace(rc)?;
    let res = match_events(&trace, &cfg.scenarios, cfg.target.protocol)?;
    let parsed = trace.events.len() as u64 + trace.filtered;

    if rc.on_unmatched == "fail" && res.unmatched > 0 {
        let top = res
            .unmatched_top
            .iter()
            .take(3)
            .map(|(key, n)| format!("{key} ({n})"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ReplayError::Unmatched {
            count: res.unmatched,
            top,
        });
    }
    if res.matched.is_empty() {
        return Err(ReplayError::Empty {
            parsed,
            malformed: trace.malformed,
            filtered: trace.filtered,
            unmatched: res.unmatched,
        });
    }

    let schedule = build_schedule(&res.matched, rc.speed, rc.copies);
    // After speed compression, an absurd span (bad timestamp units, an outlier,
    // or a tiny speed) would run for days; reject it up front.
    const MAX_SPAN: Duration = Duration::from_secs(24 * 60 * 60);
    if schedule.span() > MAX_SPAN {
        return Err(ReplayError::SpanTooLong {
            span: fmt_dur(schedule.span()),
        });
    }
    let summary = ReplaySummary {
        source: rc.source.clone(),
        parsed,
        malformed: trace.malformed,
        malformed_examples: trace.malformed_examples,
        filtered: trace.filtered,
        matched: res.per_scenario,
        unmatched: res.unmatched,
        unmatched_top: res.unmatched_top,
        hits: schedule.len() as u64,
        span: fmt_dur(schedule.span()),
        speed: rc.speed,
        copies: rc.copies,
        peak_rps: schedule.peak_rps(),
        fired: None,
        dropped: None,
    };
    Ok(Replay {
        schedule: Arc::new(schedule),
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MatchConfig, ReplayConfig, ReplayFields, RequestConfig, Scenario};

    fn temp_log(tag: &str, lines: &[&str]) -> String {
        let path = std::env::temp_dir().join(format!(
            "redliner-replay-{tag}-{}-{:?}.jsonl",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, lines.join("\n")).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn scenario(name: &str, operation: &str) -> Scenario {
        Scenario {
            name: name.into(),
            weight: 1.0,
            request: RequestConfig {
                query: format!("query {name} {{ x }}"),
                ..Default::default()
            },
            matcher: Some(MatchConfig {
                operation: operation.into(),
                ..Default::default()
            }),
        }
    }

    fn config(source: String, on_unmatched: &str) -> Config {
        let mut cfg = Config::default();
        cfg.target.url = "http://h/graphql".into();
        cfg.scenarios = vec![scenario("A", "OpA"), scenario("B", "OpB")];
        cfg.replay = Some(ReplayConfig {
            source,
            format: "json".into(),
            speed: 1.0,
            copies: 1,
            on_unmatched: on_unmatched.into(),
            fields: ReplayFields {
                ts: "ts".into(),
                method: "method".into(),
                path: "path".into(),
                operation: "operation".into(),
            },
            ..ReplayConfig::default()
        });
        cfg
    }

    const LOG: [&str; 6] = [
        r#"{"ts":1760968800,"method":"POST","path":"/graphql","operation":"OpA"}"#,
        r#"{"ts":1760968801,"method":"POST","path":"/graphql","operation":"OpB"}"#,
        r#"{"ts":1760968802,"method":"POST","path":"/graphql","operation":"OpA"}"#,
        r#"{"ts":1760968803,"method":"POST","path":"/graphql","operation":"Nope"}"#,
        r#"{"ts":1760968804,"method":"POST","path":"/graphql","operation":"Nope"}"#,
        "not json",
    ];

    #[test]
    fn build_replay_summarizes_a_skip_run() {
        let r = build_replay(&config(temp_log("skip", &LOG), "skip")).unwrap();
        let s = &r.summary;
        assert_eq!(r.schedule.len(), 3);
        assert_eq!((s.parsed, s.malformed, s.filtered), (5, 1, 0));
        assert_eq!(s.matched.get("A"), Some(&2));
        assert_eq!(s.matched.get("B"), Some(&1));
        assert_eq!((s.unmatched, s.hits), (2, 3));
        assert_eq!(s.unmatched_top, vec![("POST /graphql op=Nope".to_string(), 2)]);
        assert_eq!(s.span, "2s");
        assert_eq!((s.speed, s.copies), (1.0, 1));
        assert_eq!(s.malformed_examples.len(), 1);
    }

    #[test]
    fn build_replay_fail_mode_rejects_unmatched() {
        let err = build_replay(&config(temp_log("fail", &LOG), "fail")).unwrap_err();
        assert!(
            matches!(err, ReplayError::Unmatched { count: 2, .. }),
            "{err}"
        );
        let msg = err.to_string();
        assert!(msg.contains("2 log events matched no scenario"), "{msg}");
        assert!(msg.contains("POST /graphql op=Nope (2)"), "{msg}");
    }

    #[test]
    fn build_replay_empty_is_error() {
        let log = [LOG[3], LOG[4], LOG[5]];
        let err = build_replay(&config(temp_log("empty", &log), "skip")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "replay: no events left to replay (parsed 2, malformed 1, outside window 0, unmatched 2)"
        );
    }

    #[test]
    fn build_replay_rejects_span_over_24h() {
        // A `ts:0` outlier stretches the span to ~decades; that must be a clean
        // exit-2 error, not a multi-year run.
        let log = [
            r#"{"ts":0,"method":"POST","path":"/graphql","operation":"OpA"}"#,
            r#"{"ts":1760968800,"method":"POST","path":"/graphql","operation":"OpB"}"#,
        ];
        let err = build_replay(&config(temp_log("outlier", &log), "skip")).unwrap_err();
        assert!(matches!(err, ReplayError::SpanTooLong { .. }), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("(> 24h)"), "{msg}");
        assert!(msg.contains("narrow replay.window"), "{msg}");
    }

    #[test]
    fn build_replay_requires_replay_block() {
        let mut cfg = config(String::new(), "skip");
        cfg.replay = None;
        let err = build_replay(&cfg).unwrap_err();
        assert_eq!(err.to_string(), "replay: config has no replay block");
    }

    #[test]
    fn summary_render_lists_scenarios_and_unmatched() {
        let s = ReplaySummary {
            source: "prod.jsonl".into(),
            parsed: 10,
            malformed: 2,
            malformed_examples: vec!["line 3: bad".into()],
            filtered: 1,
            matched: [("A".to_string(), 5), ("B".to_string(), 2)].into(),
            unmatched: 3,
            unmatched_top: vec![("GET /x".to_string(), 3)],
            hits: 14,
            span: "2s".into(),
            speed: 2.0,
            copies: 2,
            peak_rps: 9,
            fired: Some(12),
            dropped: Some(2),
        };
        let out = s.render();
        assert!(out.starts_with("replay: prod.jsonl"), "{out}");
        for want in [
            "parsed", "malformed", "outside window", "unmatched", "14", "2s", "2x", "9",
            "A", "B", "line 3: bad", "GET /x", "(3)", "fired", "12 of 14", "2 dropped",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
    }

    #[test]
    fn summary_render_small_speed_not_rounded_away() {
        let mut s = ReplaySummary {
            source: "p.jsonl".into(),
            parsed: 1,
            malformed: 0,
            malformed_examples: vec![],
            filtered: 0,
            matched: [("A".to_string(), 1)].into(),
            unmatched: 0,
            unmatched_top: vec![],
            hits: 1,
            span: "1s".into(),
            speed: 0.04,
            copies: 1,
            peak_rps: 1,
            fired: None,
            dropped: None,
        };
        assert!(s.render().contains("0.04x"), "{}", s.render());
        // --validate (no fired count) omits the fired line.
        assert!(!s.render().contains("fired"), "{}", s.render());
        s.fired = Some(1);
        s.dropped = Some(0);
        assert!(s.render().contains("1 of 1 (0 dropped)"), "{}", s.render());
    }
}
