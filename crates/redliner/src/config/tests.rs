use super::*;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Write `yaml` to a fresh temp dir and load it.
fn load_str(yaml: &str) -> Result<Config, ConfigError> {
    let dir = tempdir(yaml);
    load_config_file(&dir.join("c.yaml"))
}

fn tempdir(yaml: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "redliner-cfg-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("c.yaml"), yaml).unwrap();
    dir
}

fn err_text(r: Result<Config, ConfigError>) -> String {
    r.expect_err("expected an error").to_string()
}

const GQL_HEAD: &str = "target:\n  url: http://x/graphql\n";
const ONE_GQL: &str = "scenarios:\n  - {name: a, weight: 1, request: {query: q}}\n";

#[test]
fn defaults_applied() {
    let c = load_config_file(&fixture("minimal.yaml")).unwrap();
    assert_eq!(c.target.protocol, Protocol::Graphql);
    assert_eq!(c.target.timeout.as_std(), Duration::from_secs(10));
    assert_eq!(c.target.keep_alive, Some(true));
    assert_eq!(c.target.max_idle_conns, 10000);
    assert_eq!(c.target.read_buffer_bytes, 4096);
    assert_eq!(c.target.write_buffer_bytes, 4096);
    assert_eq!(c.load.profile, "poisson");
    assert_eq!(c.load.rate, 200.0);
    assert_eq!(c.load.duration.as_std(), Duration::from_secs(30));
    assert_eq!(c.load.workers, 10);
    assert_eq!(c.load.max_workers, 100_000);
    assert_ne!(c.load.seed, 0);
    assert_eq!(c.checks.graphql_errors, Some(true));
    assert_eq!(c.report.dir, "reports");
    assert_eq!(c.report.keep_raw, Some(true));
    assert_eq!(c.report.per_operation, Some(true));
    assert_eq!(c.report.live_stats, Some(ConfigDuration(Duration::from_secs(10))));
    assert_eq!(c.scenarios[0].request.method, "GET");
}

#[test]
fn slos_reject_out_of_range_thresholds() {
    // A percentage where a fraction belongs would fail every run.
    let y = format!("{GQL_HEAD}slos:\n  success_rate: 99.5\n  throughput_min: -1\n{ONE_GQL}");
    let e = err_text(load_str(&y));
    assert!(
        e.contains("slos.success_rate must be a fraction from 0 to 1 (e.g. 0.995), got 99.5"),
        "{e}"
    );
    assert!(e.contains("slos.throughput_min must be >= 0, got -1"), "{e}");
}

#[test]
fn slos_accept_the_edges() {
    let y = format!("{GQL_HEAD}slos:\n  success_rate: 1\n  throughput_min: 0\n{ONE_GQL}");
    assert!(load_str(&y).is_ok());
}

#[test]
fn live_stats_zero_turns_it_off() {
    let c = load_str(&format!("{GQL_HEAD}report:\n  live_stats: 0s\n{ONE_GQL}")).unwrap();
    assert_eq!(c.report.live_stats, Some(ConfigDuration(Duration::ZERO)));
}

#[test]
fn live_stats_below_100ms_rejected() {
    let y = format!("{GQL_HEAD}report:\n  live_stats: 50ms\n{ONE_GQL}");
    assert!(err_text(load_str(&y))
        .contains("report.live_stats must be 0s (off) or at least 100ms, got 50ms"));
}

#[test]
fn rest_disables_graphql_errors_check_by_default() {
    let c = load_str(
        "target:\n  url: http://x\n  protocol: rest\nscenarios:\n  - {name: a, weight: 1, request: {path: /a}}\n",
    )
    .unwrap();
    assert_eq!(c.checks.graphql_errors, Some(false));
}

#[test]
fn explicit_tristate_false_preserved() {
    let y = format!(
        "{GQL_HEAD}  keep_alive: false\nreport: {{keep_raw: false, per_operation: false}}\n{ONE_GQL}"
    );
    let c = load_str(&y).unwrap();
    assert_eq!(c.target.keep_alive, Some(false));
    assert_eq!(c.report.keep_raw, Some(false));
    assert_eq!(c.report.per_operation, Some(false));
}

#[test]
fn unknown_key_rejected() {
    let y = "targett:\n  url: http://x\nscenarios: []\n";
    assert!(matches!(load_str(y), Err(ConfigError::Parse { .. })));
}

#[test]
fn unknown_nested_key_rejected() {
    let y = format!("{GQL_HEAD}  timeoutt: 5s\n{ONE_GQL}");
    assert!(matches!(load_str(&y), Err(ConfigError::Parse { .. })));
}

#[test]
fn missing_url_fails() {
    assert!(err_text(load_str(ONE_GQL)).contains("target.url is required"));
}

#[test]
fn rest_scenario_requires_path() {
    let y = "target:\n  url: http://x\n  protocol: rest\nscenarios:\n  - {name: a, weight: 1, request: {method: GET}}\n";
    assert!(err_text(load_str(y)).contains("scenario \"a\": request.path is required for rest"));
}

#[test]
fn graphql_scenario_requires_query() {
    let y = format!("{GQL_HEAD}scenarios:\n  - {{name: a, weight: 1, request: {{}}}}\n");
    assert!(err_text(load_str(&y))
        .contains("scenario \"a\": request.query or request.query_file is required for graphql"));
}

#[test]
fn wrong_protocol_fields_rejected() {
    let y = format!(
        "{GQL_HEAD}scenarios:\n  - {{name: a, weight: 1, request: {{query: q, path: /p}}}}\n"
    );
    assert!(err_text(load_str(&y)).contains("request.path is a REST field; protocol is graphql"));
    let y = "target:\n  url: http://x\n  protocol: rest\nscenarios:\n  - {name: a, weight: 1, request: {path: /p, query: q}}\n";
    assert!(err_text(load_str(y))
        .contains("request.query/query_file are GraphQL fields; protocol is rest"));
}

#[test]
fn query_file_resolves_relative_to_config() {
    let c = load_config_file(&fixture("query_file.yaml")).unwrap();
    assert_eq!(c.scenarios[0].request.query, "query Q { hello }");
}

#[test]
fn query_and_query_file_mutually_exclusive() {
    let y = format!(
        "{GQL_HEAD}scenarios:\n  - {{name: a, weight: 1, request: {{query: q, query_file: q.graphql}}}}\n"
    );
    let e = load_str(&y).unwrap_err();
    assert!(matches!(e, ConfigError::QueryFile { .. }));
    assert!(e
        .to_string()
        .contains("query and query_file are mutually exclusive"));
}

#[test]
fn empty_query_file_fails() {
    let dir = tempdir(&format!(
        "{GQL_HEAD}scenarios:\n  - {{name: a, weight: 1, request: {{query_file: e.graphql}}}}\n"
    ));
    std::fs::write(dir.join("e.graphql"), "  \n").unwrap();
    let e = load_config_file(&dir.join("c.yaml")).unwrap_err();
    assert!(e.to_string().contains("query_file e.graphql is empty"));
}

#[test]
fn missing_query_file_fails() {
    let y = format!(
        "{GQL_HEAD}scenarios:\n  - {{name: a, weight: 1, request: {{query_file: nope.graphql}}}}\n"
    );
    assert!(matches!(load_str(&y), Err(ConfigError::QueryFile { .. })));
}

#[test]
fn file_pool_path_resolves_relative_to_config() {
    let y = format!(
        "{GQL_HEAD}data:\n  pools:\n    ids: {{type: file, path: ids.txt}}\n{ONE_GQL}"
    );
    let dir = tempdir(&y);
    let c = load_config_file(&dir.join("c.yaml")).unwrap();
    assert_eq!(Path::new(&c.data.pools["ids"].path), dir.join("ids.txt"));
}

#[test]
fn validation_errors_are_aggregated() {
    let y = "target:\n  auth: {type: bogus}\nload: {profile: wat}\n";
    let t = err_text(load_str(y));
    assert!(t.contains("invalid config:\n  - target.url is required"));
    assert!(t.contains("target.auth.type \"bogus\" not supported (none|bearer|basic|header)"));
    assert!(t.contains("load.profile \"wat\" not supported (constant|poisson|ramp|steps)"));
    assert!(t.contains("at least one scenario is required"));
}

#[test]
fn auth_branches() {
    let y = format!("target:\n  url: http://x\n  auth: {{type: header}}\n{ONE_GQL}");
    let t = err_text(load_str(&y));
    assert!(t.contains("target.auth.token_env is required for auth type \"header\""));
    assert!(t.contains("target.auth.header is required for auth type header"));
    let y = format!("target:\n  url: http://x\n  auth: {{type: basic}}\n{ONE_GQL}");
    assert!(err_text(load_str(&y))
        .contains("target.auth.username_env and password_env are required for basic auth"));
}

#[test]
fn scenario_name_weight_duplicates() {
    let y = format!(
        "{GQL_HEAD}scenarios:\n  - {{weight: 1, request: {{query: q}}}}\n  - {{name: a, weight: 0, request: {{query: q}}}}\n  - {{name: a, weight: 1, request: {{query: q}}}}\n"
    );
    let t = err_text(load_str(&y));
    assert!(t.contains("scenarios[0]: name is required"));
    assert!(t.contains("scenario \"a\": weight must be > 0"));
    assert!(t.contains("scenarios[2]: duplicate name \"a\""));
}

#[test]
fn load_profiles_validated() {
    let t = err_text(load_str(&format!("{GQL_HEAD}load: {{profile: ramp}}\n{ONE_GQL}")));
    assert!(t.contains("load.ramp.from/to must be set for profile ramp"));
    let t = err_text(load_str(&format!("{GQL_HEAD}load: {{profile: steps}}\n{ONE_GQL}")));
    assert!(t.contains("load.steps must be non-empty for profile steps"));
    let t = err_text(load_str(&format!(
        "{GQL_HEAD}load: {{profile: steps, steps: [{{rate: 0, duration: 1s}}]}}\n{ONE_GQL}"
    )));
    assert!(t.contains("load.steps[0]: rate and duration must be > 0"));
    // steps profile does not get default rate/duration
    let c = load_str(&format!(
        "{GQL_HEAD}load: {{profile: steps, steps: [{{rate: 5, duration: 1s}}]}}\n{ONE_GQL}"
    ))
    .unwrap();
    assert_eq!(c.load.rate, 0.0);
    assert_eq!(c.load.duration.as_std(), Duration::ZERO);
}

#[test]
fn pools_and_harvest_validated() {
    let y = format!(
        "{GQL_HEAD}data:\n  pools:\n    a: {{type: list}}\n    b: {{type: file}}\n    c: {{type: range, min: 5, max: 1}}\n    d: {{type: nope}}\n  harvest:\n    - request: {{}}\n{ONE_GQL}"
    );
    let t = err_text(load_str(&y));
    assert!(t.contains("pool \"a\": list pool needs values"));
    assert!(t.contains("pool \"b\": file pool needs path"));
    assert!(t.contains("pool \"c\": range max < min"));
    assert!(t.contains("pool \"d\": type \"nope\" not supported (list|file|range)"));
    assert!(t.contains("data.harvest[0]: extract map is required"));
    assert!(t
        .contains("data.harvest[0]: request.query or request.query_file is required for graphql"));
}

#[test]
fn slo_enforced() {
    assert!(!SloConfig::default().enforced());
    let s = SloConfig {
        p99: ConfigDuration(Duration::from_millis(5)),
        ..Default::default()
    };
    assert!(s.enforced());
    let s = SloConfig {
        success_rate: 0.99,
        ..Default::default()
    };
    assert!(s.enforced());
}

#[test]
fn durations_and_slos_parse() {
    let y = format!(
        "{GQL_HEAD}load: {{warmup: 3s}}\nslos: {{p95: 250ms, success_rate: 0.995}}\n{ONE_GQL}"
    );
    let c = load_str(&y).unwrap();
    assert_eq!(c.load.warmup.as_std(), Duration::from_secs(3));
    assert_eq!(c.slos.p95.as_std(), Duration::from_millis(250));
    assert!(c.slos.enforced());
}

// ---- replay mode ----

const REPLAY_GQL: &str = "replay: {source: log.jsonl}\nscenarios:\n  - {name: a, request: {query: q}, match: {operation: a}}\n";

#[test]
fn replay_defaults_applied() {
    let y = format!("{GQL_HEAD}{REPLAY_GQL}");
    let c = load_str(&y).unwrap();
    let r = c.replay.expect("replay block");
    assert_eq!(r.speed, 1.0);
    assert_eq!(r.copies, 1);
    assert_eq!(r.on_unmatched, "skip");
    assert_eq!(r.format, "json");
    assert_eq!(r.fields.ts, "ts");
    assert_eq!(r.fields.method, "method");
    assert_eq!(r.fields.path, "path");
    assert_eq!(r.fields.operation, "operation");
    assert!(r.window.is_none());
    assert_eq!(c.load.profile, "");
    assert_eq!(c.load.workers, 10);
    assert_eq!(c.load.max_workers, 100_000);
    assert_ne!(c.load.seed, 0);
    assert!(Path::new(&r.source).is_absolute(), "{}", r.source);
    assert!(r.source.ends_with("log.jsonl"), "{}", r.source);
}

#[test]
fn replay_har_format_inferred() {
    let y = format!(
        "{GQL_HEAD}replay: {{source: x.har}}\nscenarios:\n  - {{name: a, request: {{query: q}}, match: {{operation: a}}}}\n"
    );
    let c = load_str(&y).unwrap();
    assert_eq!(c.replay.unwrap().format, "har");
}

#[test]
fn replay_rejects_load_pacing() {
    let y = format!("{GQL_HEAD}load: {{profile: constant, rate: 5}}\n{REPLAY_GQL}");
    let e = err_text(load_str(&y));
    assert!(
        e.contains("load.profile cannot be combined with replay (replay paces from the log)"),
        "{e}"
    );
    assert!(
        e.contains("load.rate cannot be combined with replay (replay paces from the log)"),
        "{e}"
    );
}

#[test]
fn replay_requires_a_match_rule() {
    let y = format!("{GQL_HEAD}replay: {{source: log.jsonl}}\n{ONE_GQL}");
    let e = err_text(load_str(&y));
    assert!(
        e.contains("at least one scenario needs a match rule for replay"),
        "{e}"
    );
}

#[test]
fn replay_graphql_match_needs_operation() {
    let y = format!(
        "{GQL_HEAD}replay: {{source: log.jsonl}}\nscenarios:\n  - {{name: a, request: {{query: q}}, match: {{path: /x}}}}\n"
    );
    let e = err_text(load_str(&y));
    assert!(
        e.contains("scenario \"a\": match.operation is required for graphql"),
        "{e}"
    );
    assert!(
        e.contains("scenario \"a\": match.method/path are REST fields; protocol is graphql"),
        "{e}"
    );
}

#[test]
fn replay_rest_match_needs_path() {
    let head = "target:\n  url: http://x\n  protocol: rest\nreplay: {source: log.jsonl}\n";
    let y = format!(
        "{head}scenarios:\n  - {{name: a, request: {{path: /a}}, match: {{operation: op}}}}\n"
    );
    let e = err_text(load_str(&y));
    assert!(
        e.contains("scenario \"a\": match.path is required for rest and must start with /"),
        "{e}"
    );
    assert!(
        e.contains("scenario \"a\": match.operation is a GraphQL field; protocol is rest"),
        "{e}"
    );
    let y = format!(
        "{head}scenarios:\n  - {{name: a, request: {{path: /a}}, match: {{path: users}}}}\n"
    );
    assert!(
        err_text(load_str(&y)).contains("match.path is required for rest and must start with /")
    );
    let y = format!(
        "{head}scenarios:\n  - {{name: a, request: {{path: /a}}, match: {{method: GET, path: /a}}}}\n"
    );
    load_str(&y).unwrap();
}

#[test]
fn replay_rejects_bad_knobs() {
    let scen = "scenarios:\n  - {name: a, request: {query: q}, match: {operation: a}}\n";
    let bad = |replay: &str| err_text(load_str(&format!("{GQL_HEAD}replay: {replay}\n{scen}")));

    let e = bad("{source: l.jsonl, speed: -1}");
    assert!(e.contains("replay.speed must be > 0"), "{e}");
    let e = bad("{source: l.jsonl, on_unmatched: maybe}");
    assert!(
        e.contains("replay.on_unmatched \"maybe\" not supported (skip|fail)"),
        "{e}"
    );
    let e = bad("{source: l.jsonl, format: xml}");
    assert!(
        e.contains("replay.format \"xml\" not supported (json|har)"),
        "{e}"
    );
    let e = bad("{speed: 2}");
    assert!(e.contains("replay.source is required"), "{e}");
    let e = bad("{source: l.jsonl, window: {start: x, end: y}}");
    assert!(
        e.contains("replay.window.start/end must both be RFC3339 timestamps"),
        "{e}"
    );
    let e = bad("{source: l.jsonl, window: {start: '2026-01-01T00:00:00Z'}}");
    assert!(
        e.contains("replay.window.start/end must both be RFC3339 timestamps"),
        "{e}"
    );
    let e = bad(
        "{source: l.jsonl, window: {start: '2026-01-02T00:00:00Z', end: '2026-01-01T00:00:00Z'}}",
    );
    assert!(e.contains("replay.window.start must be before end"), "{e}");
    load_str(&format!(
        "{GQL_HEAD}replay: {{source: l.jsonl, window: {{start: '2026-01-01T00:00:00Z', end: '2026-01-02T00:00:00+01:00'}}}}\n{scen}"
    ))
    .unwrap();
}

#[test]
fn profile_mode_ignores_match_blocks() {
    let y = format!(
        "{GQL_HEAD}load: {{profile: constant, rate: 5, duration: 1s}}\nscenarios:\n  - {{name: a, weight: 1, request: {{query: q}}, match: {{operation: a}}}}\n"
    );
    let c = load_str(&y).unwrap();
    assert!(c.replay.is_none());
    assert_eq!(c.scenarios[0].matcher.as_ref().unwrap().operation, "a");
}

#[test]
fn replay_override_reports_profile_conflict_not_missing_match() {
    // A constant-profile config whose scenario already has a `match` block:
    // injecting a replay source via overrides must fail only for the profile
    // conflict, pinning that the match rule is recognized (Review Focus #5).
    let dir = tempdir(&format!(
        "{GQL_HEAD}load: {{profile: constant, rate: 5, duration: 1s}}\nscenarios:\n  - {{name: a, weight: 1, request: {{query: q}}, match: {{operation: a}}}}\n"
    ));
    let e = load_config_file_with(&dir.join("c.yaml"), |c| {
        c.replay.get_or_insert_with(Default::default).source = "x.jsonl".into();
    })
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("load.profile cannot be combined with replay"),
        "{e}"
    );
    assert!(!e.contains("needs a match rule"), "{e}");
}

#[test]
fn replay_source_absolute_kept() {
    let y = format!(
        "{GQL_HEAD}replay: {{source: /var/log/x.jsonl}}\nscenarios:\n  - {{name: a, request: {{query: q}}, match: {{operation: a}}}}\n"
    );
    assert_eq!(
        load_str(&y).unwrap().replay.unwrap().source,
        "/var/log/x.jsonl"
    );
}
