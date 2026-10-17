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
    assert_eq!(c.scenarios[0].request.method, "GET");
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
        p99: GoDuration(Duration::from_millis(5)),
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
