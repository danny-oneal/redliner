//! Redliner's config importer: drafts a scenario config from a GraphQL schema
//! (live introspection) or an OpenAPI/Swagger document. The YAML it writes is
//! the only interface to `redliner`; nothing here links it at runtime.

pub mod cli;
pub mod draft;
pub mod error;
pub mod fetch;
pub mod graphql;
pub mod openapi;

pub use error::ImportError;

use std::path::PathBuf;

use cli::{Cli, Command, CommonArgs};
use draft::Draft;
use fetch::{FetchOpts, Fetcher};

/// What a run produced, for the one-line report on stderr.
#[derive(Debug)]
pub struct Summary {
    /// Live (uncommented) scenarios.
    pub scenarios: usize,
    /// Scenarios written commented out.
    pub commented: usize,
    pub pools: usize,
    /// Pools holding placeholder values the user must replace.
    pub todo_pools: usize,
    /// The file written, or `None` for stdout.
    pub output: Option<PathBuf>,
}

/// Builds the draft `cli` asks for and writes it to `--output` or `stdout`.
pub async fn run(cli: Cli, stdout: &mut impl std::io::Write) -> Result<Summary, ImportError> {
    let (mut d, common) = match cli.command {
        Command::Graphql(a) => {
            let f = Fetcher::new(&fetch_opts(&a.common))?;
            let schema = graphql::fetch_schema(&f, &a.url).await?;
            let mut d = graphql::draft(&schema, &scrub(&a.url, true), a.max_depth as usize)?;
            d.target_url = a.url.clone(); // scrubbed below, with a note if it had credentials
            (d, a.common)
        }
        Command::Openapi(a) => {
            let is_url = a.spec.starts_with("http://") || a.spec.starts_with("https://");
            let (text, spec_url) = if is_url {
                // Only a fetched spec needs the fetcher (and so --bearer-env set).
                let f = Fetcher::new(&fetch_opts(&a.common))?;
                let url = reqwest::Url::parse(&a.spec).map_err(|e| ImportError::Decode {
                    what: "spec url".to_string(),
                    message: e.to_string(),
                })?;
                (f.get_text(&a.spec).await?, Some(url))
            } else {
                let text = std::fs::read_to_string(&a.spec)
                    .map_err(|source| ImportError::Io { path: PathBuf::from(&a.spec), source })?;
                (text, None)
            };
            let doc = openapi::doc::load_doc(&text)?;
            let opts = openapi::OpenapiOpts {
                source: format!("OpenAPI document {}", scrub(&a.spec, false)),
                spec_url,
                base_url: a.base_url,
                include_unsafe: a.include_unsafe,
            };
            (openapi::draft(&doc, &opts)?, a.common)
        }
    };
    let target = scrub(&d.target_url, true);
    if target != d.target_url {
        d.target_url = target;
        d.notes.push("the URL's user:password was not written; set target.auth (type: basic) to authenticate".into());
    }
    d.insecure_tls = common.insecure;
    d.bearer_env = common.bearer_env.clone();
    d.header_names = common.header.iter().map(|(k, _)| k.clone()).collect();
    let summary = summarize(&d, common.output.clone());
    let yaml = draft::render(&d);

    match &common.output {
        Some(path) => {
            if path.exists() && !common.force {
                return Err(ImportError::OutputExists(path.clone()));
            }
            std::fs::write(path, yaml).map_err(|source| ImportError::Io { path: path.clone(), source })?;
        }
        None => stdout
            .write_all(yaml.as_bytes())
            .map_err(|source| ImportError::Io { path: PathBuf::from("<stdout>"), source })?,
    }
    Ok(summary)
}

/// `url` without a `user:password@`, and unless `keep_query` without its query
/// and fragment (a spec URL's query can carry a token); anything that is not
/// a URL, or has nothing to remove, comes back unchanged.
fn scrub(url: &str, keep_query: bool) -> String {
    let Ok(mut u) = reqwest::Url::parse(url) else { return url.to_string() };
    let drop_query = !keep_query && (u.query().is_some() || u.fragment().is_some());
    if u.username().is_empty() && u.password().is_none() && !drop_query {
        return url.to_string();
    }
    let _ = u.set_username("");
    let _ = u.set_password(None);
    if drop_query {
        u.set_query(None);
        u.set_fragment(None);
    }
    let s = u.to_string();
    // Url adds a trailing "/" to an empty path; keep the caller's form.
    if url.ends_with('/') { s } else { s.trim_end_matches('/').to_string() }
}

fn fetch_opts(c: &CommonArgs) -> FetchOpts {
    FetchOpts { headers: c.header.clone(), bearer_env: c.bearer_env.clone(), insecure: c.insecure }
}

fn summarize(d: &Draft, output: Option<PathBuf>) -> Summary {
    let commented = d.scenarios.iter().filter(|s| s.commented.is_some()).count();
    Summary {
        scenarios: d.scenarios.len() - commented,
        commented,
        pools: d.pools.len(),
        todo_pools: d.pools.values().filter(|p| p.todo.is_some()).count(),
        output,
    }
}

#[cfg(test)]
mod testutil;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use crate::testutil::{assert_contract, spawn_mock, INTROSPECTION_JSON};
    use clap::Parser;
    use std::path::PathBuf;

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("redliner-import-{}-{tag}.yaml", std::process::id()))
    }

    /// Runs the importer with `args` (after the binary name); returns the
    /// result and what it wrote to stdout.
    async fn run_args(args: &[&str]) -> (Result<Summary, ImportError>, String) {
        let cli = Cli::try_parse_from(std::iter::once("redliner-import").chain(args.iter().copied()))
            .unwrap();
        let mut out = Vec::new();
        let r = run(cli, &mut out).await;
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn cli_parses_both_subcommands() {
        let cli = Cli::try_parse_from([
            "redliner-import", "graphql", "http://h/graphql", "-H", "X-A: 1", "-H", "X-B: 2",
            "--bearer-env", "T", "-k", "-o", "out.yaml", "--force",
        ])
        .unwrap();
        let Command::Graphql(a) = cli.command else { panic!("expected graphql") };
        assert_eq!(a.url, "http://h/graphql");
        assert_eq!(a.max_depth, 3);
        assert_eq!(
            a.common.header,
            [("X-A".to_string(), "1".to_string()), ("X-B".to_string(), "2".to_string())]
        );
        assert_eq!(a.common.bearer_env.as_deref(), Some("T"));
        assert!(a.common.insecure);
        assert_eq!(a.common.output, Some(PathBuf::from("out.yaml")));
        assert!(a.common.force);

        let cli = Cli::try_parse_from([
            "redliner-import", "openapi", "spec.yaml", "--base-url", "http://x", "--include-unsafe",
        ])
        .unwrap();
        let Command::Openapi(a) = cli.command else { panic!("expected openapi") };
        assert_eq!(a.spec, "spec.yaml");
        assert_eq!(a.base_url.as_deref(), Some("http://x"));
        assert!(a.include_unsafe);

        assert!(Cli::try_parse_from(["redliner-import", "graphql", "http://h", "--max-depth", "0"]).is_err());
        assert!(Cli::try_parse_from(["redliner-import", "graphql", "http://h", "-H", "nocolon"]).is_err());
    }

    #[tokio::test]
    async fn run_writes_file_and_refuses_overwrite() {
        let out = tmp("overwrite");
        let _ = std::fs::remove_file(&out);
        let spec = fixture("petstore3.yaml");
        let args = ["openapi", spec.as_str(), "--base-url", "http://h", "-o", out.to_str().unwrap()];

        let (r, stdout) = run_args(&args).await;
        let s = r.unwrap();
        assert_eq!((s.scenarios, s.commented), (3, 2));
        assert_eq!(s.output.as_deref(), Some(out.as_path()));
        assert!(stdout.is_empty(), "{stdout}");
        assert_contract(&std::fs::read_to_string(&out).unwrap());

        let (r, _) = run_args(&args).await;
        assert!(matches!(&r, Err(ImportError::OutputExists(p)) if *p == out), "{r:?}");
        let mut forced = args.to_vec();
        forced.push("--force");
        assert!(run_args(&forced).await.0.is_ok());
        let _ = std::fs::remove_file(&out);
    }

    #[tokio::test]
    async fn run_graphql_end_to_end_never_writes_secrets() {
        std::env::set_var("REDLINER_IMPORT_T6_TOKEN", "t0k3n-secret");
        let m = spawn_mock(200, "application/json", INTROSPECTION_JSON.to_string()).await;
        let url = format!("{}/graphql", m.url);
        let (r, yaml) = run_args(&[
            "graphql", &url, "-H", "X-Api-Key: k3y-secret", "--bearer-env", "REDLINER_IMPORT_T6_TOKEN", "-k",
        ])
        .await;
        r.unwrap();
        assert!(yaml.contains("token_env: \"REDLINER_IMPORT_T6_TOKEN\""), "{yaml}");
        assert!(yaml.contains("insecure_tls: true"), "{yaml}");
        assert!(yaml.contains("X-Api-Key"), "{yaml}");
        assert!(!yaml.contains("k3y-secret") && !yaml.contains("t0k3n-secret"), "{yaml}");
        assert_contract(&yaml);
        assert!(m.requests.lock().unwrap()[0].contains("k3y-secret"));
    }

    #[tokio::test]
    async fn openapi_from_url_resolves_relative_servers() {
        let doc = r#"{"openapi":"3.0.0","servers":[{"url":"/v1"}],"paths":{"/ping":{"get":{"operationId":"ping"}}}}"#;
        let m = spawn_mock(200, "application/json", doc.to_string()).await;
        let (r, yaml) = run_args(&["openapi", &format!("{}/openapi.json", m.url)]).await;
        r.unwrap();
        assert_eq!(assert_contract(&yaml).target.url, format!("{}/v1", m.url));
    }

    #[tokio::test]
    async fn credentials_in_urls_are_never_written() {
        let doc = r#"{"openapi":"3.0.0","servers":[{"url":"/v1"}],"paths":{"/ping":{"get":{"operationId":"ping"}}}}"#;
        let m = spawn_mock(200, "application/json", doc.to_string()).await;
        let spec = format!("{}/openapi.json?access_token=SECRETTOKEN", m.url.replace("http://", "http://alice:hunter2@"));
        let (r, yaml) = run_args(&["openapi", &spec]).await;
        r.unwrap();
        assert!(!yaml.contains("alice") && !yaml.contains("hunter2") && !yaml.contains("SECRETTOKEN"), "{yaml}");
        assert!(yaml.contains("user:password was not written"), "{yaml}");
        assert_eq!(assert_contract(&yaml).target.url, format!("{}/v1", m.url));

        let g = spawn_mock(200, "application/json", INTROSPECTION_JSON.to_string()).await;
        let url = format!("{}/graphql", g.url.replace("http://", "http://bob:s3cret@"));
        let (r, yaml) = run_args(&["graphql", &url]).await;
        r.unwrap();
        assert!(!yaml.contains("bob:") && !yaml.contains("s3cret"), "{yaml}");
        assert!(yaml.contains("user:password was not written"), "{yaml}");
        assert_eq!(assert_contract(&yaml).target.url, format!("{}/graphql", g.url));
    }

    #[tokio::test]
    async fn openapi_file_with_bearer_env_does_not_need_the_variable() {
        std::env::remove_var("REDLINER_IMPORT_T6_UNSET");
        let spec = fixture("petstore3.yaml");
        let (r, yaml) = run_args(&[
            "openapi", &spec, "--base-url", "http://h", "--bearer-env", "REDLINER_IMPORT_T6_UNSET",
        ])
        .await;
        r.unwrap();
        assert!(yaml.contains("token_env: \"REDLINER_IMPORT_T6_UNSET\""), "{yaml}");
    }
}
