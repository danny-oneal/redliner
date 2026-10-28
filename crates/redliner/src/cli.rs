//! Command-line flags (Go `main.go` flag set).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::{Config, ConfigDuration, RequestConfig, Scenario};

/// Read when `--config` is not given, if it exists.
pub const DEFAULT_CONFIG: &str = "redliner.yaml";

/// Where a run's config comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    File(PathBuf),
    /// No file: one REST request built from `--url`, `--method` and `--body`.
    Url,
}

/// Redliner: SLO-driven HTTP load testing.
#[derive(Parser, Debug, Clone)]
#[command(name = "redliner", version, args_conflicts_with_subcommands = true)]
pub struct Args {
    /// Without a subcommand, redliner runs a load test.
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Scenario config file (YAML) [default: redliner.yaml]. Without one,
    /// --url benchmarks that single endpoint.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Override target.url; without a config file, the endpoint to benchmark.
    #[arg(long)]
    pub url: Option<String>,
    /// HTTP method of the --url request (no config file only; default GET).
    #[arg(long)]
    pub method: Option<String>,
    /// Body of the --url request (no config file only).
    #[arg(long)]
    pub body: Option<String>,
    /// Add a header to every request: "Name: value" (repeatable).
    #[arg(long, short = 'H', value_name = "NAME: VALUE", value_parser = parse_header)]
    pub header: Vec<(String, String)>,
    /// Skip TLS certificate verification (sets target.insecure_tls); for
    /// self-signed test environments only.
    #[arg(long, short = 'k')]
    pub insecure: bool,
    /// Override load.rate (RPS).
    #[arg(long)]
    pub rate: Option<f64>,
    /// Override load.duration (Go syntax, e.g. 30s).
    #[arg(long)]
    pub duration: Option<ConfigDuration>,
    /// Override load.warmup (Go syntax, e.g. 5s).
    #[arg(long)]
    pub warmup: Option<ConfigDuration>,
    /// Override load.seed.
    #[arg(long)]
    pub seed: Option<i64>,
    /// Override report.dir.
    #[arg(long)]
    pub out: Option<String>,
    /// Override report.live_stats: progress line interval (e.g. 5s; 0s = off).
    #[arg(long)]
    pub live_stats: Option<ConfigDuration>,
    /// Override slos.success_rate: minimum fraction of successful requests (e.g. 0.995).
    #[arg(long, value_name = "FRACTION")]
    pub slo_success_rate: Option<f64>,
    /// Override slos.p50: maximum p50 latency (e.g. 50ms; 0s = not enforced).
    #[arg(long, value_name = "DURATION")]
    pub slo_p50: Option<ConfigDuration>,
    /// Override slos.p95: maximum p95 latency (e.g. 200ms; 0s = not enforced).
    #[arg(long, value_name = "DURATION")]
    pub slo_p95: Option<ConfigDuration>,
    /// Override slos.p99: maximum p99 latency (e.g. 1s; 0s = not enforced).
    #[arg(long, value_name = "DURATION")]
    pub slo_p99: Option<ConfigDuration>,
    /// Override slos.throughput_min: minimum successful requests per second.
    #[arg(long, value_name = "RPS")]
    pub slo_throughput_min: Option<f64>,
    /// Validate config and exit.
    #[arg(long)]
    pub validate: bool,
    /// Replay a production request log (JSONL or HAR) instead of a load profile.
    #[arg(long)]
    pub replay: Option<PathBuf>,
    /// Replay time compression: 2 replays twice as fast (needs replay).
    #[arg(long)]
    pub speed: Option<f64>,
    /// Concurrent copies of the replayed log (needs replay).
    #[arg(long)]
    pub copies: Option<u64>,
    /// Fail instead of skipping log entries no scenario matches.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Compare two runs' reports; with --max-regression, exit 1 when the
    /// candidate is slower (p50/p95/p99) or has lower throughput.
    Compare(CompareArgs),
}

#[derive(clap::Args, Debug, Clone)]
pub struct CompareArgs {
    /// Baseline run: a report directory (e.g. reports/latest) or its metrics.json.
    pub baseline: PathBuf,
    /// Candidate run, in the same form.
    pub candidate: PathBuf,
    /// Exit 1 when a latency percentile rises, or throughput falls, by more
    /// than this percentage of the baseline (e.g. 10%).
    #[arg(long, value_name = "PERCENT", value_parser = crate::compare::parse_percent)]
    pub max_regression: Option<f64>,
    /// Also write each gated metric as a JUnit test case to this file.
    #[arg(long, value_name = "FILE")]
    pub junit: Option<PathBuf>,
}

impl Args {
    /// Applies only the flags the user set (`Some`), mirroring Go's `fs.Visit`.
    pub fn apply_overrides(&self, cfg: &mut Config) {
        if let Some(u) = &self.url {
            cfg.target.url = u.clone();
        }
        for (k, v) in &self.header {
            cfg.target.headers.insert(k.clone(), v.clone());
        }
        if self.insecure {
            cfg.target.insecure_tls = true;
        }
        if let Some(r) = self.rate {
            cfg.load.rate = r;
        }
        if let Some(d) = self.duration {
            cfg.load.duration = d;
        }
        if let Some(w) = self.warmup {
            cfg.load.warmup = w;
        }
        if let Some(s) = self.seed {
            cfg.load.seed = s;
        }
        if let Some(o) = &self.out {
            cfg.report.dir = o.clone();
        }
        if let Some(l) = self.live_stats {
            cfg.report.live_stats = Some(l);
        }
        let s = &mut cfg.slos;
        if let Some(r) = self.slo_success_rate {
            s.success_rate = r;
        }
        for (flag, slo) in [
            (self.slo_p50, &mut s.p50),
            (self.slo_p95, &mut s.p95),
            (self.slo_p99, &mut s.p99),
        ] {
            if let Some(d) = flag {
                *slo = d;
            }
        }
        if let Some(t) = self.slo_throughput_min {
            s.throughput_min = t;
        }
        if let Some(p) = &self.replay {
            // Absolute against the caller's cwd: the config-dir join in
            // `resolve_files` must not re-root a path typed on the command line.
            let abs = std::env::current_dir()
                .map(|d| d.join(p))
                .unwrap_or_else(|_| p.clone());
            cfg.replay.get_or_insert_with(Default::default).source =
                abs.to_string_lossy().into_owned();
        }
        // The knobs tune an existing replay block (from YAML or `--replay`); on
        // their own they must not switch the run into replay mode. When they are
        // passed without a target, `check_replay_knobs` reports one clear error.
        if let Some(r) = cfg.replay.as_mut() {
            if let Some(s) = self.speed {
                r.speed = s;
            }
            if let Some(c) = self.copies {
                r.copies = c;
            }
            if self.strict {
                r.on_unmatched = "fail".into();
            }
        }
    }

    /// Picks the config: `--config`, else `redliner.yaml` if it exists
    /// (`default_exists`), else zero-config from `--url`.
    pub fn config_source(&self, default_exists: bool) -> Result<ConfigSource, String> {
        let source = match &self.config {
            Some(p) => ConfigSource::File(p.clone()),
            None if default_exists => ConfigSource::File(PathBuf::from(DEFAULT_CONFIG)),
            None if self.url.is_some() => ConfigSource::Url,
            None => {
                return Err("no config: pass --config <file>, add a redliner.yaml here, or \
                            benchmark one endpoint with --url <url>"
                    .into())
            }
        };
        if matches!(source, ConfigSource::File(_)) && (self.method.is_some() || self.body.is_some())
        {
            return Err("--method and --body describe the --url request; with a config file, \
                        set them in a scenario"
                .into());
        }
        Ok(source)
    }

    /// The zero-config request: `--url` split into the origin (`target.url`)
    /// and one scenario for its path and query.
    pub fn url_request(&self) -> Result<(String, Scenario), String> {
        let raw = self.url.as_deref().unwrap_or_default();
        let url = reqwest::Url::parse(raw).map_err(|e| format!("--url {raw:?}: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("--url {raw:?}: only http and https are supported"));
        }
        // The URL would land in reports; don't echo it either.
        if !url.username().is_empty() || url.password().is_some() {
            return Err("--url: credentials in the URL are not supported; use --header or a \
                        config with target.auth"
                .into());
        }
        let method = self.method.as_deref().unwrap_or("GET").to_ascii_uppercase();
        let mut path = url.path().to_string();
        let name = format!("{method} {path}");
        if let Some(q) = url.query() {
            path.push('?');
            path.push_str(q);
        }
        let scenario = Scenario {
            name,
            weight: 1.0,
            request: RequestConfig {
                method,
                path,
                body: self.body.clone().unwrap_or_default(),
                ..Default::default()
            },
            matcher: None,
        };
        Ok((url.origin().ascii_serialization(), scenario))
    }

    /// One clear error when a replay knob was passed with no replay target,
    /// instead of the pile of validation errors a synthesized empty replay
    /// block would raise. `has_replay` is whether the loaded config is in
    /// replay mode.
    pub fn check_replay_knobs(&self, has_replay: bool) -> Result<(), String> {
        if !has_replay && (self.speed.is_some() || self.copies.is_some() || self.strict) {
            return Err(
                "--speed, --copies and --strict need --replay <log> or a replay block in the config"
                    .into(),
            );
        }
        Ok(())
    }
}

/// `"Name: value"`, trimmed; the name must not be empty.
fn parse_header(s: &str) -> Result<(String, String), String> {
    match s.split_once(':') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.trim().to_string())),
        _ => Err(format!("expected \"Name: value\", got {s:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_set_flags_override() {
        let mut cfg = Config::default();
        cfg.load.rate = 5.0;
        cfg.target.url = "http://a".into();
        let args = Args::parse_from(["redliner", "--rate", "9", "--duration", "2s"]);
        args.apply_overrides(&mut cfg);
        assert_eq!(cfg.load.rate, 9.0);
        assert_eq!(cfg.load.duration.as_std().as_secs(), 2);
        assert_eq!(cfg.target.url, "http://a");
    }

    #[test]
    fn live_stats_flag_overrides() {
        let mut cfg = Config::default();
        Args::parse_from(["redliner", "--live-stats", "0s"]).apply_overrides(&mut cfg);
        assert_eq!(cfg.report.live_stats, Some(ConfigDuration(std::time::Duration::ZERO)));
    }

    #[test]
    fn replay_flags_switch_mode() {
        let mut cfg = Config::default();
        let args = Args::parse_from([
            "redliner",
            "--replay",
            "prod.jsonl",
            "--speed",
            "2",
            "--copies",
            "3",
            "--strict",
        ]);
        args.apply_overrides(&mut cfg);
        let r = cfg.replay.expect("replay set by flags");
        assert_eq!(
            r.source,
            std::env::current_dir()
                .unwrap()
                .join("prod.jsonl")
                .to_string_lossy()
        );
        assert_eq!(r.speed, 2.0);
        assert_eq!(r.copies, 3);
        assert_eq!(r.on_unmatched, "fail");
    }

    #[test]
    fn no_replay_flags_leave_replay_unset() {
        let mut cfg = Config::default();
        Args::parse_from(["redliner"]).apply_overrides(&mut cfg);
        assert!(cfg.replay.is_none());
    }

    #[test]
    fn knobs_without_replay_do_not_create_block() {
        // --speed/--copies/--strict alone must not switch the run into replay
        // mode (which produced a pile of confusing validation errors).
        let mut cfg = Config::default();
        Args::parse_from(["redliner", "--speed", "2", "--strict"]).apply_overrides(&mut cfg);
        assert!(cfg.replay.is_none(), "knobs alone must not create a replay block");
    }

    #[test]
    fn replay_knobs_without_target_is_one_error() {
        let msg = "--speed, --copies and --strict need --replay <log> or a replay block in the config";
        let e = Args::parse_from(["redliner", "--speed", "2"])
            .check_replay_knobs(false)
            .unwrap_err();
        assert_eq!(e, msg);
        assert_eq!(
            Args::parse_from(["redliner", "--copies", "3"])
                .check_replay_knobs(false)
                .unwrap_err(),
            msg
        );
        assert_eq!(
            Args::parse_from(["redliner", "--strict"])
                .check_replay_knobs(false)
                .unwrap_err(),
            msg
        );
        // With a replay target present, or no knobs, it is fine.
        assert!(Args::parse_from(["redliner", "--speed", "2"])
            .check_replay_knobs(true)
            .is_ok());
        assert!(Args::parse_from(["redliner"]).check_replay_knobs(false).is_ok());
    }

    #[test]
    fn explicit_config_wins() {
        let args = Args::parse_from(["redliner", "--config", "x.yaml", "--url", "http://h/"]);
        assert_eq!(
            args.config_source(true).unwrap(),
            ConfigSource::File(PathBuf::from("x.yaml"))
        );
    }

    #[test]
    fn redliner_yaml_is_still_the_default() {
        let args = Args::parse_from(["redliner", "--url", "http://h/"]);
        assert_eq!(
            args.config_source(true).unwrap(),
            ConfigSource::File(PathBuf::from(DEFAULT_CONFIG))
        );
    }

    #[test]
    fn url_without_a_config_file_is_zero_config() {
        let args = Args::parse_from(["redliner", "--url", "http://h/"]);
        assert_eq!(args.config_source(false).unwrap(), ConfigSource::Url);
    }

    #[test]
    fn no_config_and_no_url_is_one_error() {
        let e = Args::parse_from(["redliner"]).config_source(false).unwrap_err();
        assert_eq!(
            e,
            "no config: pass --config <file>, add a redliner.yaml here, or benchmark one endpoint with --url <url>"
        );
    }

    #[test]
    fn method_and_body_need_zero_config() {
        let msg = "--method and --body describe the --url request; with a config file, set them in a scenario";
        for extra in [["--method", "POST"], ["--body", "{}"]] {
            let mut v = vec!["redliner", "--config", "x.yaml"];
            v.extend_from_slice(&extra);
            assert_eq!(Args::parse_from(v).config_source(false).unwrap_err(), msg);
        }
    }

    #[test]
    fn url_request_splits_origin_and_path() {
        let args = Args::parse_from([
            "redliner",
            "--url",
            "http://127.0.0.1:8080/api/items?limit=5#frag",
        ]);
        let (origin, sc) = args.url_request().unwrap();
        assert_eq!(origin, "http://127.0.0.1:8080");
        assert_eq!(sc.name, "GET /api/items");
        assert_eq!(sc.weight, 1.0);
        assert_eq!(sc.request.method, "GET");
        assert_eq!(sc.request.path, "/api/items?limit=5");
        assert!(sc.request.body.is_empty());
    }

    #[test]
    fn url_request_takes_method_and_body() {
        let args = Args::parse_from([
            "redliner",
            "--url",
            "https://api.example.com",
            "--method",
            "post",
            "--body",
            r#"{"a":1}"#,
        ]);
        let (origin, sc) = args.url_request().unwrap();
        assert_eq!(origin, "https://api.example.com");
        assert_eq!(sc.name, "POST /");
        assert_eq!(sc.request.method, "POST");
        assert_eq!(sc.request.path, "/");
        assert_eq!(sc.request.body, r#"{"a":1}"#);
    }

    #[test]
    fn url_request_rejects_bad_urls() {
        for (url, want) in [
            ("not a url", "--url \"not a url\": relative URL without a base"),
            ("ftp://h/x", "--url \"ftp://h/x\": only http and https are supported"),
            (
                "http://user:pw@h/x",
                "--url: credentials in the URL are not supported; use --header or a config with target.auth",
            ),
        ] {
            let e = Args::parse_from(["redliner", "--url", url]).url_request().unwrap_err();
            assert_eq!(e, want);
        }
    }

    #[test]
    fn header_flags_add_target_headers() {
        let mut cfg = Config::default();
        cfg.target.headers.insert("X-Keep".into(), "k".into());
        Args::parse_from(["redliner", "-H", "X-A: 1", "--header", "X-B:two words "])
            .apply_overrides(&mut cfg);
        assert_eq!(cfg.target.headers["X-Keep"], "k");
        assert_eq!(cfg.target.headers["X-A"], "1");
        assert_eq!(cfg.target.headers["X-B"], "two words");
    }

    #[test]
    fn slo_flags_override() {
        let mut cfg = Config::default();
        Args::parse_from([
            "redliner",
            "--slo-success-rate",
            "0.995",
            "--slo-p50",
            "50ms",
            "--slo-p95",
            "200ms",
            "--slo-p99",
            "1s",
            "--slo-throughput-min",
            "100",
        ])
        .apply_overrides(&mut cfg);
        let s = &cfg.slos;
        assert_eq!(s.success_rate, 0.995);
        assert_eq!(s.p50.as_std(), std::time::Duration::from_millis(50));
        assert_eq!(s.p95.as_std(), std::time::Duration::from_millis(200));
        assert_eq!(s.p99.as_std(), std::time::Duration::from_secs(1));
        assert_eq!(s.throughput_min, 100.0);
    }

    #[test]
    fn slo_flag_zero_turns_off_a_file_slo() {
        let mut cfg = Config::default();
        cfg.slos.p99 = ConfigDuration(std::time::Duration::from_millis(5));
        cfg.slos.success_rate = 0.99;
        Args::parse_from(["redliner", "--slo-p99", "0s"]).apply_overrides(&mut cfg);
        assert!(cfg.slos.p99.as_std().is_zero());
        assert_eq!(cfg.slos.success_rate, 0.99, "unset flags leave the file alone");
    }

    #[test]
    fn header_without_a_colon_is_rejected() {
        assert!(Args::try_parse_from(["redliner", "-H", "nocolon"]).is_err());
        assert!(Args::try_parse_from(["redliner", "-H", ": v"]).is_err());
    }

    #[test]
    fn insecure_flag_turns_off_certificate_checks() {
        let mut cfg = Config::default();
        Args::parse_from(["redliner"]).apply_overrides(&mut cfg);
        assert!(!cfg.target.insecure_tls);
        cfg.target.insecure_tls = true;
        Args::parse_from(["redliner"]).apply_overrides(&mut cfg);
        assert!(cfg.target.insecure_tls, "no flag keeps the file's setting");
        for flag in ["-k", "--insecure"] {
            let mut cfg = Config::default();
            Args::parse_from(["redliner", flag]).apply_overrides(&mut cfg);
            assert!(cfg.target.insecure_tls, "{flag}");
        }
    }

    #[test]
    fn compare_is_a_subcommand() {
        let a = Args::parse_from(["redliner", "compare", "base", "cand", "--max-regression", "10%"]);
        match a.command {
            Some(Command::Compare(c)) => {
                assert_eq!(c.baseline, PathBuf::from("base"));
                assert_eq!(c.candidate, PathBuf::from("cand"));
                assert_eq!(c.max_regression, Some(10.0));
            }
            other => panic!("expected compare, got {other:?}"),
        }
        let a = Args::parse_from(["redliner", "compare", "base", "cand"]);
        assert!(matches!(a.command, Some(Command::Compare(ref c)) if c.max_regression.is_none()));
        assert!(Args::parse_from(["redliner", "--rate", "5"]).command.is_none());
    }

    #[test]
    fn compare_takes_a_junit_path() {
        let a = Args::parse_from(["redliner", "compare", "a", "b", "--junit", "out/compare.xml"]);
        assert!(matches!(a.command, Some(Command::Compare(ref c)) if c.junit == Some(PathBuf::from("out/compare.xml"))));
        let a = Args::parse_from(["redliner", "compare", "a", "b"]);
        assert!(matches!(a.command, Some(Command::Compare(ref c)) if c.junit.is_none()));
    }

    #[test]
    fn compare_rejects_run_flags_and_bad_thresholds() {
        assert!(Args::try_parse_from(["redliner", "--rate", "5", "compare", "a", "b"]).is_err());
        assert!(Args::try_parse_from(["redliner", "compare", "a"]).is_err());
        assert!(Args::try_parse_from(["redliner", "compare", "a", "b", "--max-regression", "-5%"]).is_err());
    }
}
