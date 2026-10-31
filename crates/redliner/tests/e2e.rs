use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use clap::Parser;
use redliner::{run, run_with_interrupt, Args, Exit};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Mock {
    url: String,
    hits: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<String>>>,
    lines: Arc<Mutex<Vec<String>>>,
    /// Each request's header block (spawn_mock / spawn_mock_seeded only).
    heads: Arc<Mutex<Vec<String>>>,
}

async fn spawn_mock() -> Mock {
    spawn_mock_seeded(None).await
}

/// Like `spawn_mock`, but when `seed` is set the FIRST request received
/// (the harvest probe, which runs before any attack traffic) is answered
/// with that body; every later request gets `{"data":{}}`.
async fn spawn_mock_seeded(seed: Option<&'static str>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let heads = Arc::new(Mutex::new(Vec::new()));
    let (h, b, l, hd) = (hits.clone(), bodies.clone(), lines.clone(), heads.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let (h, b, l, hd) = (h.clone(), b.clone(), l.clone(), hd.clone());
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&data).to_string();
                    if let Some(idx) = text.find("\r\n\r\n") {
                        let len = text[..idx]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if data.len() >= idx + 4 + len {
                            let nth = h.fetch_add(1, Ordering::SeqCst);
                            let body = text[idx + 4..].to_string();
                            let first = text.split("\r\n").next().unwrap_or("").to_string();
                            l.lock().unwrap().push(first);
                            hd.lock().unwrap().push(text[..idx].to_string());
                            let is_post = text.starts_with("POST");
                            b.lock().unwrap().push(format!("{is_post}|{body}"));
                            let payload = match seed {
                                Some(s) if nth == 0 => s,
                                _ => "{\"data\":{}}",
                            };
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                payload.len()
                            );
                            let _ = sock.write_all(resp.as_bytes()).await;
                            return;
                        }
                    }
                }
            });
        }
    });
    Mock { url: format!("http://{addr}/graphql"), hits, bodies, lines, heads }
}

/// Like `spawn_mock`, but answers EVERY request with a GraphQL `errors`
/// payload (HTTP 200) so the app-layer checker reclassifies every hit.
async fn spawn_error_mock() -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let (h, b, l) = (hits.clone(), bodies.clone(), lines.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let (h, b, l) = (h.clone(), b.clone(), l.clone());
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&data).to_string();
                    if let Some(idx) = text.find("\r\n\r\n") {
                        let len = text[..idx]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if data.len() >= idx + 4 + len {
                            h.fetch_add(1, Ordering::SeqCst);
                            let body = text[idx + 4..].to_string();
                            let first = text.split("\r\n").next().unwrap_or("").to_string();
                            l.lock().unwrap().push(first);
                            let is_post = text.starts_with("POST");
                            b.lock().unwrap().push(format!("{is_post}|{body}"));
                            let payload = "{\"errors\":[{\"message\":\"boom\"}]}";
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                payload.len()
                            );
                            let _ = sock.write_all(resp.as_bytes()).await;
                            return;
                        }
                    }
                }
            });
        }
    });
    Mock { url: format!("http://{addr}/graphql"), hits, bodies, lines, heads: Arc::default() }
}

/// Like `spawn_mock`, but sleeps `delay` before answering each request, so a
/// single worker can only clear a handful of hits per second.
async fn spawn_slow_mock(delay: std::time::Duration) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let (h, b, l) = (hits.clone(), bodies.clone(), lines.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let (h, b, l) = (h.clone(), b.clone(), l.clone());
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&data).to_string();
                    if let Some(idx) = text.find("\r\n\r\n") {
                        let len = text[..idx]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if data.len() >= idx + 4 + len {
                            h.fetch_add(1, Ordering::SeqCst);
                            let body = text[idx + 4..].to_string();
                            let first = text.split("\r\n").next().unwrap_or("").to_string();
                            l.lock().unwrap().push(first);
                            let is_post = text.starts_with("POST");
                            b.lock().unwrap().push(format!("{is_post}|{body}"));
                            tokio::time::sleep(delay).await;
                            let payload = "{\"data\":{}}";
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                payload.len()
                            );
                            let _ = sock.write_all(resp.as_bytes()).await;
                            return;
                        }
                    }
                }
            });
        }
    });
    Mock { url: format!("http://{addr}/graphql"), hits, bodies, lines, heads: Arc::default() }
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("redliner-e2e-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_cfg(dir: &std::path::Path, url: &str, slos: &str) -> std::path::PathBuf {
    let p = dir.join("cfg.yaml");
    std::fs::write(
        &p,
        // The scenario carries a `match` rule (ignored in profile mode) so that
        // when `--replay` switches the run on, it fails only for the profile
        // conflict, not for a missing match rule.
        format!(
            "target:\n  url: {url}\nload:\n  profile: constant\n  rate: 50\n  duration: 1s\n\
             scenarios:\n  - name: ping\n    weight: 1\n    request:\n      query: \"{{ ping }}\"\n    match:\n      operation: ping\n{slos}"
        ),
    )
    .unwrap();
    p
}

fn args(cfg: &std::path::Path, extra: &[&str]) -> Args {
    let mut v = vec!["redliner", "--config", cfg.to_str().unwrap()];
    v.extend_from_slice(extra);
    Args::parse_from(v)
}

/// The newest timestamped report dir under `base` (follows `latest`).
fn newest_report(base: &std::path::Path) -> std::path::PathBuf {
    let target = std::fs::read_link(base.join("latest")).expect("latest symlink");
    base.join(target)
}

#[tokio::test]
async fn e2e_graphql_runs_and_exits_zero() {
    let mock = spawn_mock().await;
    let dir = tempdir("ok");
    let cfg = write_cfg(&dir, &mock.url, "");
    assert_eq!(run(args(&cfg, &["--out", dir.to_str().unwrap()])).await, Exit::Ok);
    let n = mock.hits.load(Ordering::SeqCst);
    assert!(n > 10, "expected requests, saw {n}");
    let bodies = mock.bodies.lock().unwrap();
    assert!(bodies.iter().all(|b| b.starts_with("true|") && b.contains("\"query\"")));
}

#[tokio::test]
async fn e2e_rest_runs_and_exits_zero() {
    let mock = spawn_mock().await;
    let dir = tempdir("rest");
    let base = mock.url.trim_end_matches("/graphql");
    let p = dir.join("cfg.yaml");
    std::fs::write(
        &p,
        format!(
            "target:\n  url: {base}\n  protocol: rest\nload:\n  profile: constant\n  rate: 50\n  duration: 1s\n\
             scenarios:\n  - name: ping\n    weight: 1\n    request:\n      method: GET\n      path: /ping\n"
        ),
    )
    .unwrap();
    assert_eq!(run(args(&p, &["--out", dir.to_str().unwrap()])).await, Exit::Ok);
    let n = mock.hits.load(Ordering::SeqCst);
    assert!(n > 10, "expected requests, saw {n}");
    let lines = mock.lines.lock().unwrap();
    assert!(lines.iter().any(|l| l.starts_with("GET /ping")), "lines: {lines:?}");
}

#[tokio::test]
async fn e2e_slo_violation_exits_one() {
    let mock = spawn_mock().await;
    let dir = tempdir("slo");
    let cfg = write_cfg(&dir, &mock.url, "slos:\n  throughput_min: 1000000\n");
    assert_eq!(run(args(&cfg, &["--out", dir.to_str().unwrap()])).await, Exit::SloFailed);
}

#[tokio::test]
async fn e2e_graphql_errors_fail_slo() {
    let mock = spawn_error_mock().await;
    let dir = tempdir("gqlerr");
    // GraphQL default checks.graphql_errors = true; every 200 carries an
    // errors payload, so the checker drops success to ~0 and the enforced
    // success_rate SLO fails the run.
    let cfg = write_cfg(&dir, &mock.url, "slos:\n  success_rate: 0.99\n");
    assert_eq!(run(args(&cfg, &["--out", dir.to_str().unwrap()])).await, Exit::SloFailed);
    assert!(mock.hits.load(Ordering::SeqCst) > 10);
}

#[tokio::test]
async fn e2e_writes_report_dir() {
    let mock = spawn_mock().await;
    let dir = tempdir("reportdir");
    let cfg = write_cfg(&dir, &mock.url, "report:\n  junit: true\n");
    assert_eq!(run(args(&cfg, &["--out", dir.to_str().unwrap()])).await, Exit::Ok);

    let report = newest_report(&dir);
    for f in [
        "text.txt",
        "histogram.txt",
        "metrics.json",
        "per_operation.json",
        "run_manifest.json",
        "junit.xml",
        "results.jsonl",
    ] {
        assert!(report.join(f).is_file(), "{f} missing in {report:?}");
    }
    let manifest = std::fs::read_to_string(report.join("run_manifest.json")).unwrap();
    assert!(manifest.contains("\"passed\": true"), "manifest not finalized: {manifest}");
    assert!(manifest.contains("\"requests\":"), "manifest missing requests: {manifest}");
    assert!(!manifest.contains("aborted"), "complete run marked aborted: {manifest}");
    // results.jsonl carries one line per recorded request.
    let raw = std::fs::read_to_string(report.join("results.jsonl")).unwrap();
    assert!(raw.lines().filter(|l| !l.is_empty()).count() > 10, "raw stream short");
}

#[tokio::test]
async fn validate_flag_exits_zero_without_firing() {
    let mock = spawn_mock().await;
    let dir = tempdir("val");
    let cfg = write_cfg(&dir, &mock.url, "");
    assert_eq!(run(args(&cfg, &["--validate", "--out", dir.to_str().unwrap()])).await, Exit::Ok);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(mock.hits.load(Ordering::SeqCst), 0);
    // --validate returns before the attack, so no report dir is created.
    assert!(!dir.join("latest").exists());
}

#[tokio::test]
async fn missing_config_exits_error() {
    let a = Args::parse_from(["redliner", "--config", "/nonexistent/x.yaml"]);
    assert_eq!(run(a).await, Exit::Error);
}

#[tokio::test]
async fn e2e_harvest_seeds_pool_and_attacks() {
    let mock = spawn_mock_seeded(Some("{\"data\":{\"products\":[{\"id\":7}]}}")).await;
    let dir = tempdir("harvest");
    let base = mock.url.trim_end_matches("/graphql");
    let p = dir.join("cfg.yaml");
    std::fs::write(
        &p,
        format!(
            "target:\n  url: {base}\n  protocol: rest\nload:\n  profile: constant\n  rate: 50\n  duration: 1s\n\
             data:\n  harvest:\n    - request:\n        method: GET\n        path: /seed\n      extract:\n        ids: data.products[].id\n\
             scenarios:\n  - name: product\n    weight: 1\n    request:\n      method: GET\n      path: \"/products/{{{{ids}}}}\"\n"
        ),
    )
    .unwrap();
    assert_eq!(run(args(&p, &["--out", dir.to_str().unwrap()])).await, Exit::Ok);
    let lines = mock.lines.lock().unwrap();
    assert_eq!(
        lines[0], "GET /seed HTTP/1.1",
        "harvest must go first: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l == "GET /products/7 HTTP/1.1"),
        "lines: {lines:?}"
    );
}

/// Writes `log.jsonl` (ops a,b,a,b,a 150ms apart, plus one unmatched
/// `health` event) and a replay config with scenarios `a` and `b`.
/// Returns `(config path, log path)`.
fn write_replay(dir: &std::path::Path, url: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let log = dir.join("log.jsonl");
    let ev = |ms: u32, method: &str, path: &str, op: &str| {
        format!(
            r#"{{"ts":"2026-10-21T10:00:00.{ms:03}Z","method":"{method}","path":"{path}","operation":"{op}"}}"#
        )
    };
    let events = [
        ev(0, "POST", "/graphql", "a"),
        ev(75, "GET", "/health", "health"),
        ev(150, "POST", "/graphql", "b"),
        ev(300, "POST", "/graphql", "a"),
        ev(450, "POST", "/graphql", "b"),
        ev(600, "POST", "/graphql", "a"),
    ];
    std::fs::write(&log, events.join("\n")).unwrap();
    let cfg = dir.join("replay.yaml");
    std::fs::write(
        &cfg,
        format!(
            "target:\n  url: {url}\nreplay:\n  source: log.jsonl\n\
             scenarios:\n\
             \x20 - name: a\n    request:\n      query: \"query a {{ x }}\"\n    match:\n      operation: a\n\
             \x20 - name: b\n    request:\n      query: \"query b {{ x }}\"\n    match:\n      operation: b\n"
        ),
    )
    .unwrap();
    (cfg, log)
}

/// The `operationName` of each POST body the mock received, in arrival order.
fn operation_names(mock: &Mock) -> Vec<String> {
    mock.bodies
        .lock()
        .unwrap()
        .iter()
        .map(|b| {
            let json = b.strip_prefix("true|").expect("POST body");
            let v: serde_json::Value = serde_json::from_str(json).unwrap();
            v["operationName"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

fn read_manifest(base: &std::path::Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(newest_report(base).join("run_manifest.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[tokio::test]
async fn e2e_replay_fires_each_matched_event_in_order() {
    let mock = spawn_mock().await;
    let dir = tempdir("replay-order");
    let (cfg, _) = write_replay(&dir, &mock.url);
    assert_eq!(run(args(&cfg, &["--out", dir.to_str().unwrap()])).await, Exit::Ok);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 5);
    assert_eq!(operation_names(&mock), ["a", "b", "a", "b", "a"]);
    let manifest = read_manifest(&dir);
    assert_eq!(manifest["replay"]["unmatched"], 1, "manifest: {manifest:#}");
}

#[tokio::test]
async fn e2e_replay_paces_like_the_log() {
    let mock = spawn_mock().await;
    let dir = tempdir("replay-pace");
    let (cfg, _) = write_replay(&dir, &mock.url);
    let start = std::time::Instant::now();
    assert_eq!(run(args(&cfg, &["--out", dir.to_str().unwrap()])).await, Exit::Ok);
    let wall = start.elapsed();
    assert!(wall >= std::time::Duration::from_millis(600), "too fast: {wall:?}");
    assert!(wall < std::time::Duration::from_secs(3), "too slow: {wall:?}");
}

#[tokio::test]
async fn e2e_replay_copies_and_speed() {
    let mock = spawn_mock().await;
    let dir = tempdir("replay-copies");
    let (cfg, _) = write_replay(&dir, &mock.url);
    let start = std::time::Instant::now();
    let extra = ["--copies", "2", "--speed", "2", "--out", dir.to_str().unwrap()];
    assert_eq!(run(args(&cfg, &extra)).await, Exit::Ok);
    let wall = start.elapsed();
    assert_eq!(mock.hits.load(Ordering::SeqCst), 10);
    let manifest = read_manifest(&dir);
    assert_eq!(manifest["replay"]["copies"], 2, "manifest: {manifest:#}");
    assert_eq!(manifest["replay"]["speed"], 2.0, "manifest: {manifest:#}");
    assert!(wall < std::time::Duration::from_millis(1500), "too slow: {wall:?}");
}

#[tokio::test]
async fn e2e_replay_strict_exits_error_without_firing() {
    let mock = spawn_mock().await;
    let dir = tempdir("replay-strict");
    let (cfg, _) = write_replay(&dir, &mock.url);
    assert_eq!(
        run(args(&cfg, &["--strict", "--out", dir.to_str().unwrap()])).await,
        Exit::Error
    );
    assert_eq!(mock.hits.load(Ordering::SeqCst), 0);
    // build_replay fails before the report dir is created.
    assert!(!dir.join("latest").exists());
}

#[tokio::test]
async fn e2e_replay_validate_prints_schedule_without_firing() {
    let mock = spawn_mock().await;
    let dir = tempdir("replay-validate");
    let (cfg, _) = write_replay(&dir, &mock.url);
    assert_eq!(
        run(args(&cfg, &["--validate", "--out", dir.to_str().unwrap()])).await,
        Exit::Ok
    );
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(mock.hits.load(Ordering::SeqCst), 0);
    assert!(!dir.join("latest").exists());
}

#[tokio::test]
async fn e2e_replay_falling_behind_drops_and_fails() {
    // A log of 5 ops 20ms apart, amplified x10 => 50 scheduled hits inside an
    // 80ms span. The mock answers in 200ms and max_workers is 1, so a single
    // worker clears only a handful before the deadline; the rest drop.
    let mock = spawn_slow_mock(std::time::Duration::from_millis(200)).await;
    let dir = tempdir("replay-behind");
    let log = dir.join("log.jsonl");
    let events: Vec<String> = (0..5)
        .map(|i| {
            format!(
                r#"{{"ts":"2026-10-21T10:00:00.{:03}Z","method":"POST","path":"/graphql","operation":"a"}}"#,
                i * 20
            )
        })
        .collect();
    std::fs::write(&log, events.join("\n")).unwrap();
    let cfg = dir.join("replay.yaml");
    std::fs::write(
        &cfg,
        format!(
            "target:\n  url: {}\n  timeout: 300ms\nload:\n  max_workers: 1\nreplay:\n  source: log.jsonl\n\
             scenarios:\n  - name: a\n    request:\n      query: \"query a {{ x }}\"\n    match:\n      operation: a\n\
             report:\n  junit: true\n",
            mock.url
        ),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let extra = ["--copies", "10", "--out", dir.to_str().unwrap()];
    assert_eq!(run(args(&cfg, &extra)).await, Exit::SloFailed);
    let wall = start.elapsed();
    assert!(wall < std::time::Duration::from_secs(3), "too slow: {wall:?}");
    let served = mock.hits.load(Ordering::SeqCst);
    assert!(served < 50, "expected drops, served {served} of 50");
    let manifest = read_manifest(&dir);
    assert_eq!(manifest["replay"]["hits"], 50, "manifest: {manifest:#}");
    assert!(
        manifest["replay"]["dropped"].as_u64().unwrap_or(0) > 0,
        "manifest: {manifest:#}"
    );
    assert_eq!(manifest["passed"], false, "manifest: {manifest:#}");
    // CI reading junit.xml must see the shortfall, not an all-pass suite.
    let junit = std::fs::read_to_string(newest_report(&dir).join("junit.xml")).unwrap();
    assert!(
        junit.contains("name=\"replay_delivery\"") && junit.contains("<failure"),
        "junit: {junit}"
    );
}

#[tokio::test]
async fn e2e_replay_flag_rejects_profile_config() {
    let mock = spawn_mock().await;
    let dir = tempdir("replay-profile");
    let (_, log) = write_replay(&dir, &mock.url);
    let cfg = write_cfg(&dir, &mock.url, "");
    let extra = ["--replay", log.to_str().unwrap(), "--out", dir.to_str().unwrap()];
    assert_eq!(run(args(&cfg, &extra)).await, Exit::Error);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 0);
    // Rejected during config load, before any report dir exists.
    assert!(!dir.join("latest").exists());
}

#[tokio::test]
async fn e2e_interrupt_writes_partial_report_and_exits_130() {
    let mock = spawn_mock().await;
    let dir = tempdir("interrupt");
    let cfg = write_cfg(&dir, &mock.url, "report:\n  junit: true\n");
    let start = std::time::Instant::now();
    let exit = run_with_interrupt(
        args(&cfg, &["--out", dir.to_str().unwrap(), "--duration", "60s"]),
        tokio::time::sleep(std::time::Duration::from_millis(500)),
    )
    .await;

    assert_eq!(exit, Exit::Interrupted);
    assert_eq!(exit.code(), 130);
    assert!(start.elapsed() < std::time::Duration::from_secs(10), "ran {:?}", start.elapsed());
    let report = newest_report(&dir);
    for f in ["text.txt", "metrics.json", "junit.xml", "results.jsonl"] {
        assert!(report.join(f).is_file(), "{f} missing in {report:?}");
    }
    let manifest = read_manifest(&dir);
    assert_eq!(manifest["aborted"], true, "manifest: {manifest:#}");
    assert!(manifest["finished_at"].is_string(), "manifest: {manifest:#}");
    let n = manifest["requests"].as_u64().unwrap();
    // ~25 hits in 500ms @ 50 rps; a full run would be 3000.
    assert!((1..200).contains(&n), "requests {n}");
}

#[tokio::test]
async fn e2e_interrupted_replay_is_not_a_delivery_failure() {
    let mock = spawn_mock().await;
    let dir = tempdir("replay-interrupt");
    let (cfg, _) = write_replay(&dir, &mock.url);
    // At 0.1x the log's 600ms span stretches to 6s; stop after the first hit.
    let exit = run_with_interrupt(
        args(&cfg, &["--out", dir.to_str().unwrap(), "--speed", "0.1"]),
        tokio::time::sleep(std::time::Duration::from_millis(500)),
    )
    .await;

    assert_eq!(exit, Exit::Interrupted);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 1);
    let manifest = read_manifest(&dir);
    assert_eq!(manifest["aborted"], true, "manifest: {manifest:#}");
    assert_eq!(manifest["replay"]["fired"], 1, "manifest: {manifest:#}");
    let slos = manifest["slo_results"].as_array().cloned().unwrap_or_default();
    assert!(
        slos.iter().all(|r| r["name"] != "replay_delivery"),
        "an interrupt is not a delivery failure: {manifest:#}"
    );
}

/// Runs the real binary (so stderr can be read) against a mock, returning
/// the exit code and stderr.
async fn run_binary(cfg: &std::path::Path, extra: &[&str]) -> (Option<i32>, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_redliner"));
    cmd.arg("--config").arg(cfg).args(extra);
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_live_stats_print_progress_lines() {
    let mock = spawn_mock().await;
    let dir = tempdir("live");
    let cfg = write_cfg(&dir, &mock.url, "");
    let (code, stderr) =
        run_binary(&cfg, &["--out", dir.to_str().unwrap(), "--live-stats", "200ms"]).await;

    assert_eq!(code, Some(0), "stderr: {stderr}");
    let lines: Vec<&str> = stderr.lines().filter(|l| l.starts_with("[live ")).collect();
    // A 1s run at 200ms ticks: ~4 lines.
    assert!((3..=6).contains(&lines.len()), "lines: {lines:?}");
    assert!(lines.iter().all(|l| l.contains(" constant · ")), "lines: {lines:?}");
    assert!(lines.last().unwrap().contains(" ok, p95 "), "lines: {lines:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_live_stats_off() {
    let mock = spawn_mock().await;
    let dir = tempdir("live-off");
    let cfg = write_cfg(&dir, &mock.url, "");
    let (code, stderr) =
        run_binary(&cfg, &["--out", dir.to_str().unwrap(), "--live-stats", "0s"]).await;

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(!stderr.contains("[live "), "stderr: {stderr}");
}

/// The mock's origin, without the `/graphql` path.
fn origin(mock: &Mock) -> String {
    mock.url.trim_end_matches("/graphql").to_string()
}

#[tokio::test]
async fn e2e_zero_config_url_benchmarks_one_endpoint() {
    let mock = spawn_mock().await;
    let dir = tempdir("zero");
    let url = format!("{}/items?limit=5", origin(&mock));
    let out = dir.to_str().unwrap();
    let args = Args::parse_from([
        "redliner", "--url", &url, "--rate", "50", "--duration", "1s", "--live-stats", "0s",
        "--out", out,
    ]);
    assert_eq!(run(args).await, Exit::Ok);

    let lines = mock.lines.lock().unwrap().clone();
    assert!(lines.len() > 10, "expected requests, saw {}", lines.len());
    assert!(lines.iter().all(|l| l == "GET /items?limit=5 HTTP/1.1"), "{lines:?}");
    let manifest = read_manifest(&dir);
    assert_eq!(manifest["config_file"], "(command line)");
    assert_eq!(manifest["config"]["scenarios"][0]["name"], "GET /items");
}

#[tokio::test]
async fn e2e_manifest_redacts_header_credentials() {
    let mock = spawn_mock().await;
    let dir = tempdir("redact");
    let url = format!("{}/items", origin(&mock));
    let out = dir.to_str().unwrap();
    let args = Args::parse_from([
        "redliner", "--url", &url, "--rate", "20", "--duration", "1s", "--live-stats", "0s",
        "--out", out, "-H", "Authorization: Bearer SUPERSECRET", "-H", "X-Api-Key: k123",
        "-H", "Accept: application/json",
    ]);
    assert_eq!(run(args).await, Exit::Ok);

    // The target still gets the real credential...
    let heads = mock.heads.lock().unwrap().clone();
    assert!(!heads.is_empty() && heads[0].contains("SUPERSECRET"), "{heads:?}");
    // ...but the audit trail on disk does not.
    let raw = std::fs::read_to_string(newest_report(&dir).join("run_manifest.json")).unwrap();
    assert!(!raw.contains("SUPERSECRET") && !raw.contains("k123"), "{raw}");
    let manifest = read_manifest(&dir);
    let headers = &manifest["config"]["target"]["headers"];
    assert_eq!(headers["Authorization"], "<redacted>");
    assert_eq!(headers["X-Api-Key"], "<redacted>");
    assert_eq!(headers["Accept"], "application/json");
}

#[tokio::test]
async fn e2e_fractional_constant_rate_fires() {
    let mock = spawn_mock().await;
    let dir = tempdir("fractional");
    let cfg = write_cfg(&dir, &mock.url, "");
    // 2.5 rps for 2s: hits due at 0.4, 0.8, 1.2 and 1.6s; the one due at 2.0s
    // falls on the deadline. Truncating to 2 rps fired 3.
    let extra = ["--out", dir.to_str().unwrap(), "--rate", "2.5", "--duration", "2s"];
    assert_eq!(run(args(&cfg, &extra)).await, Exit::Ok);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 4);
}

/// `--url` args aimed at a port nothing listens on.
fn dead_target_args(out: &str, duration: &str) -> Args {
    Args::parse_from([
        "redliner", "--url", "http://127.0.0.1:1/", "--rate", "20", "--duration", duration,
        "--live-stats", "0s", "--out", out,
    ])
}

fn has_slo(manifest: &serde_json::Value, name: &str) -> bool {
    manifest["slo_results"]
        .as_array()
        .is_some_and(|rs| rs.iter().any(|r| r["name"] == name))
}

#[tokio::test]
async fn e2e_dead_target_without_slos_exits_one() {
    let dir = tempdir("dead");
    assert_eq!(run(dead_target_args(dir.to_str().unwrap(), "1s")).await, Exit::SloFailed);

    let manifest = read_manifest(&dir);
    assert_eq!(manifest["passed"], false);
    let r = manifest["slo_results"]
        .as_array()
        .and_then(|rs| rs.iter().find(|r| r["name"] == "successful_requests"))
        .expect("successful_requests result")
        .clone();
    assert_eq!(r["passed"], false);
    let observed = r["observed"].as_str().unwrap();
    assert!(observed.starts_with("0 of ") && observed != "0 of 0", "{observed}");
}

#[tokio::test]
async fn e2e_all_checks_failed_without_slos_exits_one() {
    // Every response is a 200 carrying GraphQL `errors`: answered, never succeeded.
    let mock = spawn_error_mock().await;
    let dir = tempdir("allfail");
    let cfg = write_cfg(&dir, &mock.url, "");
    assert_eq!(run(args(&cfg, &["--out", dir.to_str().unwrap()])).await, Exit::SloFailed);
    assert!(mock.hits.load(Ordering::SeqCst) > 10);
    assert!(has_slo(&read_manifest(&dir), "successful_requests"));
}

/// Answers every request with `404 Not Found`; for bodiless GETs only.
async fn spawn_404_mock() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let h = h.clone();
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                while !data.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => data.extend_from_slice(&buf[..n]),
                    }
                }
                h.fetch_add(1, Ordering::SeqCst);
                let _ = sock
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            });
        }
    });
    (origin, hits)
}

#[tokio::test]
async fn e2e_allowed_non_2xx_status_passes_without_slos() {
    // `checks.status` defines success; a 404 it allows is not a failed run.
    let (origin, hits) = spawn_404_mock().await;
    let dir = tempdir("allow404");
    let cfg = dir.join("cfg.yaml");
    std::fs::write(
        &cfg,
        format!(
            "target:\n  url: {origin}\n  protocol: rest\nload:\n  profile: constant\n  rate: 20\n  duration: 1s\n\
             checks:\n  status: [404]\nscenarios:\n  - {{name: gone, weight: 1, request: {{method: GET, path: /gone}}}}\n"
        ),
    )
    .unwrap();
    let extra = ["--out", dir.to_str().unwrap(), "--live-stats", "0s"];
    assert_eq!(run(args(&cfg, &extra)).await, Exit::Ok);
    assert!(hits.load(Ordering::SeqCst) > 10);
}

#[tokio::test]
async fn e2e_allowed_non_2xx_status_counts_as_success() {
    // The checker's verdict is the only definition of success: a 404 listed in
    // `checks.status` is a success in the SLO verdict and in every report.
    let (origin, hits) = spawn_404_mock().await;
    let dir = tempdir("allow404-slo");
    let cfg = dir.join("cfg.yaml");
    std::fs::write(
        &cfg,
        format!(
            "target:\n  url: {origin}\n  protocol: rest\nload:\n  profile: constant\n  rate: 20\n  duration: 1s\n\
             checks:\n  status: [404]\nscenarios:\n  - {{name: gone, weight: 1, request: {{method: GET, path: /gone}}}}\n\
             slos:\n  success_rate: 0.99\n  throughput_min: 5\n"
        ),
    )
    .unwrap();
    let extra = ["--out", dir.to_str().unwrap(), "--live-stats", "0s"];
    assert_eq!(run(args(&cfg, &extra)).await, Exit::Ok);
    assert!(hits.load(Ordering::SeqCst) > 10);

    let report = newest_report(&dir);
    let metrics: serde_json::Value =
        serde_json::from_slice(&std::fs::read(report.join("metrics.json")).unwrap()).unwrap();
    assert_eq!(metrics["success"], 1.0, "{metrics}");
    assert!(metrics["throughput"].as_f64().unwrap() > 5.0, "{metrics}");
    // The status codes stay what the server said.
    assert_eq!(metrics["status_codes"]["404"], metrics["requests"], "{metrics}");
}

#[tokio::test]
async fn e2e_interrupted_dead_target_exits_130() {
    let dir = tempdir("dead-int");
    let exit = run_with_interrupt(
        dead_target_args(dir.to_str().unwrap(), "30s"),
        tokio::time::sleep(std::time::Duration::from_millis(500)),
    )
    .await;
    assert_eq!(exit, Exit::Interrupted);
    assert!(!has_slo(&read_manifest(&dir), "successful_requests"));
}

#[tokio::test]
async fn e2e_zero_config_sends_method_and_body() {
    let mock = spawn_mock().await;
    let dir = tempdir("zero-post");
    let url = format!("{}/items", origin(&mock));
    let out = dir.to_str().unwrap();
    let args = Args::parse_from([
        "redliner", "--url", &url, "--method", "POST", "--body", r#"{"a":1}"#, "--rate", "50",
        "--duration", "1s", "--live-stats", "0s", "--out", out,
    ]);
    assert_eq!(run(args).await, Exit::Ok);

    let lines = mock.lines.lock().unwrap().clone();
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|l| l == "POST /items HTTP/1.1"), "{lines:?}");
    let bodies = mock.bodies.lock().unwrap();
    assert!(bodies.iter().all(|b| b == r#"true|{"a":1}"#), "{bodies:?}");
}

#[tokio::test]
async fn e2e_zero_config_validate_fires_nothing() {
    let mock = spawn_mock().await;
    let url = format!("{}/items", origin(&mock));
    let args = Args::parse_from(["redliner", "--url", &url, "--validate"]);
    assert_eq!(run(args).await, Exit::Ok);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 0);
}


/// Runs the binary in `cwd` with only `args`, returning (exit code, stderr).
async fn run_binary_in(cwd: &std::path::Path, args: &[&str]) -> (Option<i32>, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_redliner"));
    cmd.current_dir(cwd).args(args);
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_zero_config_validates_flags_like_a_config() {
    let dir = tempdir("zero-invalid");
    let (code, stderr) =
        run_binary_in(&dir, &["--url", "http://127.0.0.1:9/x", "--rate=-1", "--validate"]).await;
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(stderr.contains("(command line): invalid config"), "{stderr}");
    assert!(stderr.contains("load.rate must be > 0"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_no_config_and_no_url_explains_both() {
    let dir = tempdir("no-config");
    let (code, stderr) = run_binary_in(&dir, &[]).await;
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(stderr.contains("--config <file>") && stderr.contains("--url <url>"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_redliner_yaml_in_cwd_beats_zero_config() {
    // `--url` with a redliner.yaml present keeps its old meaning: override
    // target.url in that file (a GraphQL scenario here), not zero-config.
    let mock = spawn_mock().await;
    let dir = tempdir("cwd-yaml");
    let cfg = write_cfg(&dir, "http://unused.invalid", "");
    std::fs::rename(cfg, dir.join("redliner.yaml")).unwrap();
    let (code, stderr) = run_binary_in(
        &dir,
        &["--url", &mock.url, "--live-stats", "0s", "--out", dir.to_str().unwrap()],
    )
    .await;
    assert_eq!(code, Some(0), "stderr: {stderr}");
    let lines = mock.lines.lock().unwrap().clone();
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|l| l.starts_with("POST /graphql")), "{lines:?}");
}

#[tokio::test]
async fn e2e_slo_flags_gate_a_zero_config_run() {
    let mock = spawn_mock().await;
    let dir = tempdir("zero-slo-fail");
    let url = format!("{}/items", origin(&mock));
    let out = dir.to_str().unwrap();
    let args = Args::parse_from([
        "redliner", "--url", &url, "--rate", "50", "--duration", "1s", "--live-stats", "0s",
        "--slo-p99", "1us", "--out", out,
    ]);
    assert_eq!(run(args).await, Exit::SloFailed);
    let manifest = read_manifest(&dir);
    assert_eq!(manifest["passed"], false);
    assert_eq!(manifest["slo_results"][0]["name"], "latency_p99");
}

#[tokio::test]
async fn e2e_slo_flags_pass_when_met() {
    let mock = spawn_mock().await;
    let dir = tempdir("zero-slo-pass");
    let url = format!("{}/items", origin(&mock));
    let out = dir.to_str().unwrap();
    let args = Args::parse_from([
        "redliner", "--url", &url, "--rate", "50", "--duration", "1s", "--live-stats", "0s",
        "--slo-success-rate", "0.99", "--slo-p99", "10s", "--out", out,
    ]);
    assert_eq!(run(args).await, Exit::Ok);
    let manifest = read_manifest(&dir);
    assert_eq!(manifest["passed"], true);
    assert_eq!(manifest["slo_results"].as_array().map(Vec::len), Some(2));
}

/// A REST config whose POST body draws from uuid and sequence pools.
fn write_generator_cfg(dir: &std::path::Path, base: &str) -> std::path::PathBuf {
    let p = dir.join("cfg.yaml");
    std::fs::write(
        &p,
        format!(
            "target:\n  url: {base}\n  protocol: rest\nload:\n  profile: constant\n  rate: 50\n  duration: 1s\n  seed: 42\n\
             data:\n  pools:\n    key: {{type: uuid}}\n    n: {{type: sequence, start: 100}}\n\
             scenarios:\n  - name: create\n    weight: 1\n    request:\n      method: POST\n      path: /orders\n      \
             body: '{{\"key\":\"{{{{key}}}}\",\"n\":{{{{n}}}}}}'\n"
        ),
    )
    .unwrap();
    p
}

/// The decoded POST bodies the mock saw.
fn json_bodies(mock: &Mock) -> Vec<serde_json::Value> {
    mock.bodies
        .lock()
        .unwrap()
        .iter()
        .map(|b| serde_json::from_str(b.strip_prefix("true|").expect("a POST")).expect("JSON body"))
        .collect()
}

#[tokio::test]
async fn e2e_generator_pools_give_unique_values() {
    let mock = spawn_mock().await;
    let dir = tempdir("gen");
    let cfg = write_generator_cfg(&dir, &origin(&mock));
    let out = dir.to_str().unwrap();
    assert_eq!(run(args(&cfg, &["--out", out, "--live-stats", "0s"])).await, Exit::Ok);

    let bodies = json_bodies(&mock);
    assert!(bodies.len() > 10, "saw {} requests", bodies.len());
    let keys: std::collections::HashSet<&str> =
        bodies.iter().map(|b| b["key"].as_str().expect("key is a string")).collect();
    assert_eq!(keys.len(), bodies.len(), "every key is unique");
    let mut ns: Vec<i64> = bodies.iter().map(|b| b["n"].as_i64().expect("n is a number")).collect();
    ns.sort_unstable();
    let want: Vec<i64> = (100..100 + bodies.len() as i64).collect();
    assert_eq!(ns, want, "one sequence value per request, from start");
}

#[tokio::test]
async fn e2e_uuid_pool_repeats_with_the_same_seed() {
    let keys = |mock: &Mock| -> std::collections::BTreeSet<String> {
        json_bodies(mock)
            .iter()
            .map(|b| b["key"].as_str().unwrap().to_string())
            .collect()
    };
    let (a, b) = (spawn_mock().await, spawn_mock().await);
    let (da, db) = (tempdir("gen-seed-a"), tempdir("gen-seed-b"));
    for (mock, dir) in [(&a, &da), (&b, &db)] {
        let cfg = write_generator_cfg(dir, &origin(mock));
        let out = dir.to_str().unwrap();
        assert_eq!(run(args(&cfg, &["--out", out, "--live-stats", "0s"])).await, Exit::Ok);
    }
    let (ka, kb) = (keys(&a), keys(&b));
    let common = ka.intersection(&kb).count();
    // Same seed, same draw order: the runs share every key both fired (the
    // last hit or two can fall either side of the 1s cut-off).
    assert!(common + 2 >= ka.len().min(kb.len()) && common > 10, "{common} of {}", ka.len());
}

/// Runs the binary with `args`; returns the exit code and stdout.
async fn run_compare(args: &[&str]) -> (Option<i32>, String, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_redliner"));
    cmd.arg("compare").args(args);
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_compare_gates_a_regression_between_real_runs() {
    let mock = spawn_mock().await;
    let dir = tempdir("compare");
    let cfg = write_cfg(&dir, &mock.url, "");
    assert_eq!(run(args(&cfg, &["--out", dir.to_str().unwrap()])).await, Exit::Ok);
    let base = newest_report(&dir);

    // A run against itself: no change, so even a 0% threshold passes.
    let b = base.to_str().unwrap();
    let (code, stdout, stderr) = run_compare(&[b, b, "--max-regression", "0%"]).await;
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("p95") && stdout.contains("ping"), "{stdout}");
    assert!(stdout.trim_end().ends_with("no regressions over 0%"), "{stdout}");

    // The same run with p99 at 10s.
    let cand = dir.join("candidate");
    std::fs::create_dir_all(&cand).unwrap();
    let mut m: serde_json::Value =
        serde_json::from_slice(&std::fs::read(base.join("metrics.json")).unwrap()).unwrap();
    m["latencies"]["p99"] = "10s".into();
    std::fs::write(cand.join("metrics.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    let (code, stdout, stderr) =
        run_compare(&[b, cand.to_str().unwrap(), "--max-regression", "10%"]).await;
    assert_eq!(code, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("REGRESSED"), "{stdout}");
    assert!(stdout.contains("only in baseline: ping"), "{stdout}");

    // --junit records each gated metric as a test case, failures included.
    let junit = dir.join("compare.xml");
    let (code, _, stderr) = run_compare(&[
        b,
        cand.to_str().unwrap(),
        "--max-regression",
        "10%",
        "--junit",
        junit.to_str().unwrap(),
    ])
    .await;
    assert_eq!(code, Some(1), "stderr: {stderr}");
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.contains("<testsuite name=\"redliner.compare\" tests=\"4\" failures=\"1\""), "{xml}");
    assert!(xml.contains("<testcase name=\"overall p99\""), "{xml}");
    assert!(xml.contains("over the 10% limit"), "{xml}");

    // An unwritable --junit path is an error, not a silent pass.
    let (code, _, stderr) =
        run_compare(&[b, b, "--junit", dir.join("no/such/dir/x.xml").to_str().unwrap()]).await;
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(stderr.contains("x.xml"), "{stderr}");

    // Without a threshold it reports and exits 0.
    let (code, _, _) = run_compare(&[b, cand.to_str().unwrap()]).await;
    assert_eq!(code, Some(0));

    // A missing run is an error.
    let (code, _, stderr) = run_compare(&[b, dir.join("nope").to_str().unwrap()]).await;
    assert_eq!(code, Some(2));
    assert!(stderr.contains("nope"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_compare_includes_each_steps_phase() {
    let mock = spawn_mock().await;
    let dir = tempdir("compare-phases");
    let cfg = write_cfg(&dir, &mock.url, "");
    let yaml = std::fs::read_to_string(&cfg).unwrap().replace(
        "profile: constant\n  rate: 50\n  duration: 1s\n",
        "profile: steps\n  steps:\n    - { rate: 20, duration: 500ms }\n    - { rate: 40, duration: 500ms }\n",
    );
    assert!(yaml.contains("profile: steps"), "{yaml}");
    std::fs::write(&cfg, yaml).unwrap();
    assert_eq!(run(args(&cfg, &["--out", dir.to_str().unwrap()])).await, Exit::Ok);
    let b = newest_report(&dir);
    let b = b.to_str().unwrap();

    let (code, stdout, stderr) = run_compare(&[b, b, "--max-regression", "0%"]).await;
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("phase step-1(20 rps)"), "{stdout}");
    assert!(stdout.contains("phase step-2(40 rps)"), "{stdout}");
}

/// The JSON every request to the TLS mock gets: a harvestable product list.
const TLS_BODY: &str = "{\"data\":{\"products\":[{\"id\":7}]}}";

/// An HTTPS server with the self-signed cert in tests/fixtures/tls, answering
/// every request `200` with [`TLS_BODY`]. Returns its origin and the number
/// of HTTP requests that got through the handshake.
async fn spawn_tls_mock() -> (String, Arc<AtomicUsize>) {
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio_rustls::rustls::{crypto::ring, ServerConfig};

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tls");
    let certs = CertificateDer::pem_file_iter(dir.join("self-signed.crt"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_file(dir.join("self-signed.key")).unwrap();
    let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { break };
            let (acceptor, h) = (acceptor.clone(), h.clone());
            tokio::spawn(async move {
                // A client that rejects the cert aborts the handshake here.
                let Ok(mut tls) = acceptor.accept(sock).await else { return };
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                while !data.windows(4).any(|w| w == b"\r\n\r\n") {
                    match tls.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => data.extend_from_slice(&buf[..n]),
                    }
                }
                h.fetch_add(1, Ordering::SeqCst);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{TLS_BODY}",
                    TLS_BODY.len()
                );
                let _ = tls.write_all(resp.as_bytes()).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    (format!("https://127.0.0.1:{}", addr.port()), hits)
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_insecure_tls_reaches_a_self_signed_server() {
    let (origin, hits) = spawn_tls_mock().await;
    let dir = tempdir("tls");
    let url = format!("{origin}/health");
    let out = dir.to_str().unwrap();
    let base = ["--url", url.as_str(), "--rate", "20", "--duration", "500ms", "--slo-success-rate", "0.99", "--out", out];

    // Verification on (the default): the handshake fails, nothing gets through.
    let (code, stderr) = run_binary_in(&dir, &base).await;
    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert!(!stderr.contains("verification is off"), "{stderr}");

    // --insecure: requests get through, and the run says so loudly.
    let mut insecure = base.to_vec();
    insecure.push("--insecure");
    let (code, stderr) = run_binary_in(&dir, &insecure).await;
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(hits.load(Ordering::SeqCst) > 0);
    assert!(stderr.contains("warning: TLS certificate verification is off"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_insecure_tls_covers_the_harvest_probe() {
    let (origin, hits) = spawn_tls_mock().await;
    let dir = tempdir("tls-harvest");
    let p = dir.join("cfg.yaml");
    let write = |insecure: bool| {
        std::fs::write(
            &p,
            format!(
                "target:\n  url: {origin}\n  protocol: rest\n  insecure_tls: {insecure}\n\
                 load:\n  profile: constant\n  rate: 20\n  duration: 500ms\n\
                 data:\n  harvest:\n    - request:\n        method: GET\n        path: /seed\n      extract:\n        ids: data.products[].id\n\
                 scenarios:\n  - name: product\n    weight: 1\n    request:\n      method: GET\n      path: \"/products/{{{{ids}}}}\"\n"
            ),
        )
        .unwrap();
    };

    // The harvest probe has its own client; with verification on it fails first.
    write(false);
    assert_eq!(run(args(&p, &["--out", dir.to_str().unwrap()])).await, Exit::Error);
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    write(true);
    assert_eq!(run(args(&p, &["--out", dir.to_str().unwrap()])).await, Exit::Ok);
    assert!(hits.load(Ordering::SeqCst) > 1, "harvest probe plus attack traffic");
}

#[tokio::test]
async fn e2e_rest_header_placeholders_send_a_fresh_value_per_request() {
    let mock = spawn_mock().await;
    let dir = tempdir("header-templates");
    let base = mock.url.trim_end_matches("/graphql");
    let p = dir.join("cfg.yaml");
    std::fs::write(
        &p,
        format!(
            "target:\n  url: {base}\n  protocol: rest\nload:\n  profile: constant\n  rate: 50\n  duration: 500ms\n\
             data:\n  pools:\n    keys: {{ type: uuid }}\n\
             scenarios:\n  - name: create\n    weight: 1\n    request:\n      method: POST\n      path: /orders\n      body: \"{{}}\"\n\
             \x20     headers:\n        Idempotency-Key: \"{{{{keys}}}}\"\n"
        ),
    )
    .unwrap();
    assert_eq!(run(args(&p, &["--out", dir.to_str().unwrap()])).await, Exit::Ok);
    let keys: Vec<String> = mock
        .heads
        .lock()
        .unwrap()
        .iter()
        .filter_map(|h| h.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("idempotency-key: ").map(str::to_string)))
        .collect();
    assert!(keys.len() > 5, "keys: {keys:?}");
    assert!(keys.iter().all(|k| k.len() == 36 && !k.contains('{')), "keys: {keys:?}");
    let unique: std::collections::BTreeSet<_> = keys.iter().collect();
    assert_eq!(unique.len(), keys.len(), "every request gets its own key");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_summary_shows_p999_and_status_classes() {
    let mock = spawn_mock().await;
    let dir = tempdir("summary");
    let cfg = write_cfg(&dir, &mock.url, "slos:\n  p999: 10s\n");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_redliner"));
    cmd.arg("--config").arg(&cfg).args(["--out", dir.to_str().unwrap()]);
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("latency p99.9 : "), "{stdout}");
    let status = stdout.lines().find(|l| l.starts_with("status        : ")).expect("status line");
    assert!(status.starts_with("status        : 2xx "), "{status}");
    assert!(stdout.contains("latency_p999"), "the p99.9 SLO is evaluated: {stdout}");
}

