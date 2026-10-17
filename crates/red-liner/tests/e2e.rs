use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use clap::Parser;
use red_liner::{run, Args, Exit};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Mock {
    url: String,
    hits: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<String>>>,
}

async fn spawn_mock() -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let (h, b) = (hits.clone(), bodies.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let (h, b) = (h.clone(), b.clone());
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
                            let is_post = text.starts_with("POST");
                            b.lock().unwrap().push(format!("{is_post}|{body}"));
                            let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"data\":{}}";
                            let _ = sock.write_all(resp.as_bytes()).await;
                            return;
                        }
                    }
                }
            });
        }
    });
    Mock { url: format!("http://{addr}/graphql"), hits, bodies }
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("red-liner-e2e-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_cfg(dir: &std::path::Path, url: &str, slos: &str) -> std::path::PathBuf {
    let p = dir.join("cfg.yaml");
    std::fs::write(
        &p,
        format!(
            "target:\n  url: {url}\nload:\n  profile: constant\n  rate: 50\n  duration: 1s\n\
             scenarios:\n  - name: ping\n    weight: 1\n    request:\n      query: \"{{ ping }}\"\n{slos}"
        ),
    )
    .unwrap();
    p
}

fn args(cfg: &std::path::Path, extra: &[&str]) -> Args {
    let mut v = vec!["red-liner", "--config", cfg.to_str().unwrap()];
    v.extend_from_slice(extra);
    Args::parse_from(v)
}

#[tokio::test]
async fn e2e_graphql_runs_and_exits_zero() {
    let mock = spawn_mock().await;
    let dir = tempdir("ok");
    let cfg = write_cfg(&dir, &mock.url, "");
    assert_eq!(run(args(&cfg, &[])).await, Exit::Ok);
    let n = mock.hits.load(Ordering::SeqCst);
    assert!(n > 10, "expected requests, saw {n}");
    let bodies = mock.bodies.lock().unwrap();
    assert!(bodies.iter().all(|b| b.starts_with("true|") && b.contains("\"query\"")));
}

#[tokio::test]
async fn e2e_slo_violation_exits_one() {
    let mock = spawn_mock().await;
    let dir = tempdir("slo");
    let cfg = write_cfg(&dir, &mock.url, "slos:\n  throughput_min: 1000000\n");
    assert_eq!(run(args(&cfg, &[])).await, Exit::SloFailed);
}

#[tokio::test]
async fn validate_flag_exits_zero_without_firing() {
    let mock = spawn_mock().await;
    let dir = tempdir("val");
    let cfg = write_cfg(&dir, &mock.url, "");
    assert_eq!(run(args(&cfg, &["--validate"])).await, Exit::Ok);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(mock.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_config_exits_error() {
    let a = Args::parse_from(["red-liner", "--config", "/nonexistent/x.yaml"]);
    assert_eq!(run(a).await, Exit::Error);
}
