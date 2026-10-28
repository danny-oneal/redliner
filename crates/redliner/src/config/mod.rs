//! Scenario-file model: a port of the Go `config.go`.
//!
//! Every struct is `deny_unknown_fields` (parity with Go's
//! `KnownFields(true)`): a typo'd key is a config bug, not silence.

pub mod duration;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub use duration::ConfigDuration;

/// Request encoder selector. Defaults to GraphQL when omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Graphql,
    Rest,
}

/// The whole scenario file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub version: i64,
    pub target: TargetConfig,
    pub load: LoadConfig,
    pub data: DataConfig,
    pub scenarios: Vec<Scenario>,
    /// Production-log replay; `Some` switches the run into replay mode.
    pub replay: Option<ReplayConfig>,
    pub checks: ChecksConfig,
    pub slos: SloConfig,
    pub report: ReportConfig,
}

/// The service under test and transport settings shared by every phase.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TargetConfig {
    pub url: String,
    pub protocol: Protocol,
    pub timeout: ConfigDuration,
    pub headers: BTreeMap<String, String>,
    pub auth: AuthConfig,
    /// Tri-state: `None` means "use default (true)".
    pub keep_alive: Option<bool>,
    pub max_idle_conns: i64,
    pub read_buffer_bytes: i64,
    pub write_buffer_bytes: i64,
    pub http2: bool,
    /// Skip TLS certificate verification, for staging or in-cluster services
    /// with self-signed certs. The run warns loudly when it's on.
    pub insecure_tls: bool,
}

/// Credentials come from the environment, never from the file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuthConfig {
    /// none | bearer | basic | header
    #[serde(rename = "type")]
    pub auth_type: String,
    pub token_env: String,
    pub header: String,
    pub username_env: String,
    pub password_env: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LoadConfig {
    /// constant | poisson | ramp | steps
    pub profile: String,
    pub rate: f64,
    pub duration: ConfigDuration,
    pub warmup: ConfigDuration,
    pub ramp: RampConfig,
    pub steps: Vec<Step>,
    pub workers: u64,
    pub max_workers: u64,
    /// 0 = derived from wall clock.
    pub seed: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RampConfig {
    pub from: f64,
    pub to: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Step {
    pub rate: f64,
    pub duration: ConfigDuration,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DataConfig {
    pub pools: BTreeMap<String, PoolConfig>,
    pub harvest: Vec<HarvestConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PoolConfig {
    /// list | file | range | uuid | sequence | timestamp
    #[serde(rename = "type")]
    pub pool_type: String,
    pub values: Vec<serde_json::Value>,
    pub path: String,
    pub min: i64,
    pub max: i64,
    /// For sequence pools: the first value (default 1).
    pub start: Option<i64>,
    /// For file pools: string (default) | int. For timestamp pools:
    /// rfc3339 (default) | unix | unix_ms.
    #[serde(rename = "as")]
    pub as_type: String,
}

/// One bootstrap request whose JSON response seeds pools.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HarvestConfig {
    pub request: RequestConfig,
    /// Pool name -> JSON path.
    pub extract: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Scenario {
    pub name: String,
    pub weight: f64,
    pub request: RequestConfig,
    /// Replay-mode rule selecting which logged requests this scenario
    /// serves. Ignored in profile mode.
    #[serde(rename = "match")]
    pub matcher: Option<MatchConfig>,
}

/// Which logged entries a scenario replays. GraphQL scenarios match on
/// `operation`; REST scenarios on `method` (optional) and `path`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MatchConfig {
    pub operation: String,
    pub method: String,
    pub path: String,
}

/// Replay a production request log at its recorded pace.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReplayConfig {
    /// Log file; a relative path resolves against the config directory.
    pub source: String,
    /// json | har (inferred from the extension when empty).
    pub format: String,
    /// Half-open `[start, end)` slice of the log.
    pub window: Option<WindowConfig>,
    /// Time compression: 2.0 replays twice as fast.
    pub speed: f64,
    /// Concurrent copies of the log.
    pub copies: u64,
    /// What to do with entries no scenario matches: skip | fail.
    pub on_unmatched: String,
    pub fields: ReplayFields,
}

/// RFC3339 timestamps bounding the replayed slice of the log.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WindowConfig {
    pub start: String,
    pub end: String,
}

/// Dot paths into each JSONL object (same syntax as `data::extract_path`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReplayFields {
    pub ts: String,
    pub method: String,
    pub path: String,
    pub operation: String,
}

/// Tagged union across the two protocols; validation enforces the fields
/// required by the active one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RequestConfig {
    // GraphQL
    pub query: String,
    pub query_file: String,
    pub variables: BTreeMap<String, serde_json::Value>,
    // REST
    pub method: String,
    pub path: String,
    pub body: String,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ChecksConfig {
    /// Acceptable HTTP status codes; empty means 2xx/3xx.
    #[serde(rename = "status")]
    pub statuses: Vec<i64>,
    /// Tri-state: `None` defaults to on for graphql, off for rest.
    pub graphql_errors: Option<bool>,
}

/// CI gate. Zero values mean "not enforced".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SloConfig {
    pub success_rate: f64,
    pub p50: ConfigDuration,
    pub p95: ConfigDuration,
    pub p99: ConfigDuration,
    pub p999: ConfigDuration,
    pub throughput_min: f64,
}

impl SloConfig {
    pub fn enforced(&self) -> bool {
        self.success_rate > 0.0
            || !self.p50.as_std().is_zero()
            || !self.p95.as_std().is_zero()
            || !self.p99.as_std().is_zero()
            || !self.p999.as_std().is_zero()
            || self.throughput_min > 0.0
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReportConfig {
    pub dir: String,
    pub junit: bool,
    /// Tri-state; default true.
    pub keep_raw: Option<bool>,
    /// Tri-state; default true.
    pub per_operation: Option<bool>,
    /// How often to print a progress line on stderr; `0s` turns it off.
    /// Tri-state; default 10s.
    pub live_stats: Option<ConfigDuration>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
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
        source: serde_yaml_ng::Error,
    },
    /// All validation problems, aggregated.
    #[error("{}: invalid config:\n  - {}", path.display(), errors.join("\n  - "))]
    Invalid { path: PathBuf, errors: Vec<String> },
    /// `context` is e.g. `scenario "x"` or `data.harvest[0]`.
    #[error("{}: {context}: {message}", path.display())]
    QueryFile {
        path: PathBuf,
        context: String,
        message: String,
    },
}

/// Read, decode, default, resolve files, and validate a scenario file.
/// Fail-fast: every problem is reported before a request is fired.
pub fn load_config_file(path: &Path) -> Result<Config, ConfigError> {
    load_config_file_with(path, |_| {})
}

/// Like [`load_config_file`], but runs `overrides` (e.g. CLI flags) after
/// parsing and before defaults and validation, so overrides are validated
/// and can switch the config into replay mode.
pub fn load_config_file_with(
    path: &Path,
    overrides: impl FnOnce(&mut Config),
) -> Result<Config, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let cfg: Config = serde_yaml_ng::from_str(&raw).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    load_config_with(path, cfg, overrides)
}

/// The rest of [`load_config_file_with`] for an already-decoded config,
/// e.g. one built from CLI flags. `path` names it in errors and anchors
/// relative file paths.
pub fn load_config_with(
    path: &Path,
    mut cfg: Config,
    overrides: impl FnOnce(&mut Config),
) -> Result<Config, ConfigError> {
    overrides(&mut cfg);
    cfg.apply_defaults();
    let base = path.parent().unwrap_or_else(|| Path::new(""));
    cfg.resolve_files(base, path)?;
    let errors = cfg.validate();
    if !errors.is_empty() {
        return Err(ConfigError::Invalid {
            path: path.to_path_buf(),
            errors,
        });
    }
    Ok(cfg)
}

impl Config {
    /// Load external file references before validation. Paths resolve
    /// relative to the config file's directory, not the process CWD.
    fn resolve_files(&mut self, base_dir: &Path, cfg_path: &Path) -> Result<(), ConfigError> {
        let wrap = |context: String, message: String| ConfigError::QueryFile {
            path: cfg_path.to_path_buf(),
            context,
            message,
        };
        for s in &mut self.scenarios {
            resolve_query_file(&mut s.request, base_dir)
                .map_err(|m| wrap(format!("scenario {:?}", s.name), m))?;
        }
        for (i, h) in self.data.harvest.iter_mut().enumerate() {
            resolve_query_file(&mut h.request, base_dir)
                .map_err(|m| wrap(format!("data.harvest[{i}]"), m))?;
        }
        for p in self.data.pools.values_mut() {
            if p.pool_type == "file" && !p.path.is_empty() && !Path::new(&p.path).is_absolute() {
                p.path = base_dir.join(&p.path).to_string_lossy().into_owned();
            }
        }
        if let Some(r) = &mut self.replay {
            if !r.source.is_empty() && !Path::new(&r.source).is_absolute() {
                r.source = base_dir.join(&r.source).to_string_lossy().into_owned();
            }
        }
        Ok(())
    }

    fn apply_defaults(&mut self) {
        let t = &mut self.target;
        if t.timeout.as_std().is_zero() {
            t.timeout = ConfigDuration(Duration::from_secs(10));
        }
        t.keep_alive.get_or_insert(true);
        if t.max_idle_conns == 0 {
            t.max_idle_conns = 10000;
        }
        if t.read_buffer_bytes == 0 {
            t.read_buffer_bytes = 4096;
        }
        if t.write_buffer_bytes == 0 {
            t.write_buffer_bytes = 4096;
        }
        let protocol = t.protocol;

        let l = &mut self.load;
        // Replay paces from the log, so the load-profile defaults would
        // only trip the "cannot be combined with replay" validation.
        if self.replay.is_none() {
            if l.profile.is_empty() {
                l.profile = "poisson".into();
            }
            if l.rate == 0.0 && l.steps.is_empty() && l.ramp.to == 0.0 {
                l.rate = 200.0;
            }
            if l.duration.as_std().is_zero() && l.steps.is_empty() {
                l.duration = ConfigDuration(Duration::from_secs(30));
            }
        }
        if l.workers == 0 {
            l.workers = 10;
        }
        if l.max_workers == 0 {
            l.max_workers = 100_000;
        }
        if l.seed == 0 {
            l.seed = wall_clock_seed();
        }

        if let Some(r) = &mut self.replay {
            if r.format.is_empty() {
                r.format = if r.source.to_ascii_lowercase().ends_with(".har") {
                    "har"
                } else {
                    "json"
                }
                .into();
            }
            if r.speed == 0.0 {
                r.speed = 1.0;
            }
            if r.copies == 0 {
                r.copies = 1;
            }
            if r.on_unmatched.is_empty() {
                r.on_unmatched = "skip".into();
            }
            let f = &mut r.fields;
            for (field, default) in [
                (&mut f.ts, "ts"),
                (&mut f.method, "method"),
                (&mut f.path, "path"),
                (&mut f.operation, "operation"),
            ] {
                if field.is_empty() {
                    *field = default.into();
                }
            }
        }

        self.checks
            .graphql_errors
            .get_or_insert(protocol == Protocol::Graphql);

        let r = &mut self.report;
        if r.dir.is_empty() {
            r.dir = "reports".into();
        }
        r.keep_raw.get_or_insert(true);
        r.per_operation.get_or_insert(true);
        r.live_stats
            .get_or_insert(ConfigDuration(Duration::from_secs(10)));

        for s in &mut self.scenarios {
            if s.request.method.is_empty() {
                s.request.method = "GET".into();
            }
        }
    }

    /// Returns every validation problem (empty means valid).
    fn validate(&self) -> Vec<String> {
        let mut errs: Vec<String> = Vec::new();
        let t = &self.target;

        if t.url.is_empty() {
            errs.push("target.url is required".into());
        }
        // Go also checks target.protocol here; in Rust an unknown protocol
        // is rejected at parse time by the `Protocol` enum.
        let a = &t.auth;
        match a.auth_type.as_str() {
            "" | "none" => {}
            "bearer" | "header" => {
                if a.token_env.is_empty() {
                    errs.push(format!(
                        "target.auth.token_env is required for auth type {:?}",
                        a.auth_type
                    ));
                }
                if a.auth_type == "header" && a.header.is_empty() {
                    errs.push("target.auth.header is required for auth type header".into());
                }
            }
            "basic" => {
                if a.username_env.is_empty() || a.password_env.is_empty() {
                    errs.push(
                        "target.auth.username_env and password_env are required for basic auth"
                            .into(),
                    );
                }
            }
            other => errs.push(format!(
                "target.auth.type {other:?} not supported (none|bearer|basic|header)"
            )),
        }

        let l = &self.load;
        if let Some(r) = &self.replay {
            validate_load_for_replay(l, &mut errs);
            validate_replay(r, &mut errs);
        } else {
            match l.profile.as_str() {
                "constant" | "poisson" => {
                    let dur = l.duration.as_std();
                    if !positive_finite(l.rate) {
                        errs.push(format!(
                            "load.rate must be > 0 and finite for profile {:?}",
                            l.profile
                        ));
                    } else if fires_nothing(l.rate * dur.as_secs_f64(), l.profile == "constant") {
                        errs.push(format!(
                            "load.rate × load.duration fires no requests for profile {:?} \
                             ({} rps for {dur:?}); raise either",
                            l.profile, l.rate
                        ));
                    }
                }
                "ramp" => {
                    let (from, to) = (l.ramp.from, l.ramp.to);
                    let dur = l.duration.as_std();
                    if !(from.is_finite() && from >= 0.0 && positive_finite(to)) {
                        errs.push("load.ramp.from/to must be set for profile ramp \
                                   (finite, from >= 0, to > 0)".into());
                    } else if fires_nothing((from + to) / 2.0 * dur.as_secs_f64(), false) {
                        errs.push(format!(
                            "load.ramp averages {} rps over {dur:?}, which fires no requests",
                            (from + to) / 2.0
                        ));
                    }
                }
                "steps" => {
                    if l.steps.is_empty() {
                        errs.push("load.steps must be non-empty for profile steps".into());
                    }
                    for (i, s) in l.steps.iter().enumerate() {
                        let dur = s.duration.as_std();
                        if !positive_finite(s.rate) || dur.is_zero() {
                            errs.push(format!(
                                "load.steps[{i}]: rate and duration must be > 0 and finite"
                            ));
                        } else if fires_nothing(s.rate * dur.as_secs_f64(), true) {
                            errs.push(format!(
                                "load.steps[{i}]: {} rps for {dur:?} fires no requests",
                                s.rate
                            ));
                        }
                    }
                }
                other => errs.push(format!(
                    "load.profile {other:?} not supported (constant|poisson|ramp|steps)"
                )),
            }
        }

        if self.scenarios.is_empty() {
            errs.push("at least one scenario is required".into());
        }
        if self.replay.is_some() && self.scenarios.iter().all(|s| s.matcher.is_none()) {
            errs.push("at least one scenario needs a match rule for replay".into());
        }
        let mut seen = std::collections::HashSet::new();
        for (i, s) in self.scenarios.iter().enumerate() {
            if s.name.is_empty() {
                errs.push(format!("scenarios[{i}]: name is required"));
                continue;
            }
            if !seen.insert(s.name.as_str()) {
                errs.push(format!("scenarios[{i}]: duplicate name {:?}", s.name));
            }
            if self.replay.is_none() && s.weight <= 0.0 {
                errs.push(format!("scenario {:?}: weight must be > 0", s.name));
            }
            if self.replay.is_some() {
                if let Some(m) = &s.matcher {
                    validate_match(&s.name, m, t.protocol, &mut errs);
                }
            }
            match t.protocol {
                Protocol::Graphql => {
                    if s.request.query.is_empty() {
                        errs.push(format!(
                            "scenario {:?}: request.query or request.query_file is required for graphql",
                            s.name
                        ));
                    }
                    if !s.request.path.is_empty() {
                        errs.push(format!(
                            "scenario {:?}: request.path is a REST field; protocol is graphql",
                            s.name
                        ));
                    }
                }
                Protocol::Rest => {
                    if s.request.path.is_empty() {
                        errs.push(format!(
                            "scenario {:?}: request.path is required for rest",
                            s.name
                        ));
                    }
                    if !s.request.query.is_empty() || !s.request.query_file.is_empty() {
                        errs.push(format!(
                            "scenario {:?}: request.query/query_file are GraphQL fields; protocol is rest",
                            s.name
                        ));
                    }
                }
            }
        }

        for (name, p) in &self.data.pools {
            match p.pool_type.as_str() {
                "list" => {
                    if p.values.is_empty() {
                        errs.push(format!("pool {name:?}: list pool needs values"));
                    }
                }
                "file" => {
                    if p.path.is_empty() {
                        errs.push(format!("pool {name:?}: file pool needs path"));
                    }
                }
                "range" => {
                    if p.max < p.min {
                        errs.push(format!("pool {name:?}: range max < min"));
                    }
                }
                "uuid" | "sequence" => {}
                "timestamp" => {
                    if !matches!(p.as_type.as_str(), "" | "rfc3339" | "unix" | "unix_ms") {
                        errs.push(format!(
                            "pool {name:?}: timestamp as {:?} not supported (rfc3339|unix|unix_ms)",
                            p.as_type
                        ));
                    }
                }
                other => errs.push(format!(
                    "pool {name:?}: type {other:?} not supported \
                     (list|file|range|uuid|sequence|timestamp)"
                )),
            }
        }
        for (i, h) in self.data.harvest.iter().enumerate() {
            if h.extract.is_empty() {
                errs.push(format!("data.harvest[{i}]: extract map is required"));
            }
            if t.protocol == Protocol::Graphql && h.request.query.is_empty() {
                errs.push(format!(
                    "data.harvest[{i}]: request.query or request.query_file is required for graphql"
                ));
            }
            if t.protocol == Protocol::Rest && h.request.path.is_empty() {
                errs.push(format!(
                    "data.harvest[{i}]: request.path is required for rest"
                ));
            }
        }
        let s = &self.slos;
        if !(0.0..=1.0).contains(&s.success_rate) {
            errs.push(format!(
                "slos.success_rate must be a fraction from 0 to 1 (e.g. 0.995), got {}",
                s.success_rate
            ));
        }
        if !(s.throughput_min >= 0.0 && s.throughput_min.is_finite()) {
            errs.push(format!(
                "slos.throughput_min must be >= 0, got {}",
                s.throughput_min
            ));
        }
        if let Some(every) = self.report.live_stats.map(|d| d.as_std()) {
            if !every.is_zero() && every < Duration::from_millis(100) {
                errs.push(format!(
                    "report.live_stats must be 0s (off) or at least 100ms, got {every:?}"
                ));
            }
        }
        errs
    }
}

fn positive_finite(x: f64) -> bool {
    x.is_finite() && x > 0.0
}

/// Whether a load expected to send `expected` requests sends none. Constant
/// pacing puts the first hit at `1/rate` and never fires one due exactly at
/// the deadline, so it needs more than one; Poisson and ramp need one.
fn fires_nothing(expected: f64, constant_pacing: bool) -> bool {
    if constant_pacing {
        expected <= 1.0
    } else {
        expected < 1.0
    }
}

/// Parse an RFC3339 timestamp (any offset) into UTC. Err is the message only.
pub(crate) fn parse_rfc3339(s: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .map_err(|e| format!("{s:?} is not an RFC3339 timestamp: {e}"))
}

/// Replay paces from the log; any explicit load pacing is a conflict.
fn validate_load_for_replay(l: &LoadConfig, errs: &mut Vec<String>) {
    let conflicts = [
        ("profile", !l.profile.is_empty()),
        ("rate", l.rate != 0.0),
        ("duration", !l.duration.as_std().is_zero()),
        ("warmup", !l.warmup.as_std().is_zero()),
        ("ramp", l.ramp.from != 0.0 || l.ramp.to != 0.0),
        ("steps", !l.steps.is_empty()),
    ];
    for (field, set) in conflicts {
        if set {
            errs.push(format!(
                "load.{field} cannot be combined with replay (replay paces from the log)"
            ));
        }
    }
}

fn validate_replay(r: &ReplayConfig, errs: &mut Vec<String>) {
    if r.source.is_empty() {
        errs.push("replay.source is required".into());
    }
    if !matches!(r.format.as_str(), "json" | "har") {
        errs.push(format!(
            "replay.format {:?} not supported (json|har)",
            r.format
        ));
    }
    if !(r.speed.is_finite() && r.speed > 0.0) {
        errs.push("replay.speed must be > 0".into());
    }
    if !matches!(r.on_unmatched.as_str(), "skip" | "fail") {
        errs.push(format!(
            "replay.on_unmatched {:?} not supported (skip|fail)",
            r.on_unmatched
        ));
    }
    if let Some(w) = &r.window {
        match (parse_rfc3339(&w.start), parse_rfc3339(&w.end)) {
            (Ok(start), Ok(end)) => {
                if start >= end {
                    errs.push("replay.window.start must be before end".into());
                }
            }
            _ => errs.push("replay.window.start/end must both be RFC3339 timestamps".into()),
        }
    }
}

fn validate_match(name: &str, m: &MatchConfig, protocol: Protocol, errs: &mut Vec<String>) {
    match protocol {
        Protocol::Graphql => {
            if m.operation.is_empty() {
                errs.push(format!(
                    "scenario {name:?}: match.operation is required for graphql"
                ));
            }
            if !m.method.is_empty() || !m.path.is_empty() {
                errs.push(format!(
                    "scenario {name:?}: match.method/path are REST fields; protocol is graphql"
                ));
            }
        }
        Protocol::Rest => {
            if !m.path.starts_with('/') {
                errs.push(format!(
                    "scenario {name:?}: match.path is required for rest and must start with /"
                ));
            }
            if !m.operation.is_empty() {
                errs.push(format!(
                    "scenario {name:?}: match.operation is a GraphQL field; protocol is rest"
                ));
            }
        }
    }
}

fn wall_clock_seed() -> i64 {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);
    // Never 0 (0 means "derive").
    i64::try_from(n).unwrap_or(i64::MAX).max(1)
}

/// Inline `query_file` contents into `query`. Err is the message only.
fn resolve_query_file(rc: &mut RequestConfig, base_dir: &Path) -> Result<(), String> {
    if rc.query_file.is_empty() {
        return Ok(());
    }
    if !rc.query.is_empty() {
        return Err("query and query_file are mutually exclusive".into());
    }
    let p = Path::new(&rc.query_file);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    };
    let raw = std::fs::read_to_string(&full).map_err(|e| format!("query_file: {e}"))?;
    let q = raw.trim();
    if q.is_empty() {
        return Err(format!("query_file {} is empty", rc.query_file));
    }
    rc.query = q.to_string();
    Ok(())
}

#[cfg(test)]
mod tests;
