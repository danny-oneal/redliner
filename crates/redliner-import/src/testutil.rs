//! Test helpers: the contract every draft must meet.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// An in-process HTTP server answering every request with one response.
pub struct Mock {
    /// `http://127.0.0.1:PORT`, no trailing slash.
    pub url: String,
    /// Each raw request received: request line, headers and body.
    pub requests: Arc<Mutex<Vec<String>>>,
}

/// Serves `status` with `body` to every request on any path, recording each
/// request in full (headers, then a `Content-Length` body).
pub async fn spawn_mock(status: u16, content_type: &'static str, body: String) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let (seen, body) = (seen.clone(), body.clone());
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&data);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                seen.lock().unwrap().push(String::from_utf8_lossy(&data).into_owned());
                let resp = format!(
                    "HTTP/1.1 {status} Mock\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Mock { url, requests }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// An introspection response for the schema in the plan's Task 2.
pub const INTROSPECTION_JSON: &str = include_str!("../tests/fixtures/introspection.json");
/// A server that refuses introspection.
pub const INTROSPECTION_DISABLED_JSON: &str =
    include_str!("../tests/fixtures/introspection-disabled.json");

/// The parsed introspection fixture.
pub fn fixture_schema() -> crate::graphql::schema::Schema {
    crate::graphql::schema::parse(&serde_json::from_str(INTROSPECTION_JSON).unwrap()).unwrap()
}

/// Loads `yaml` with redliner's own config loader, builds its pools and
/// renders every scenario's templates; panics with the YAML on any failure.
pub fn assert_contract(yaml: &str) -> redliner::config::Config {
    let path = std::env::temp_dir().join(format!(
        "redliner-import-{}-{}.yaml",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&path, yaml).unwrap();
    let cfg = match redliner::config::load_config_file(&path) {
        Ok(c) => c,
        Err(e) => panic!("draft does not load: {e}\n---\n{yaml}"),
    };
    let _ = std::fs::remove_file(&path);
    let pools = match redliner::data::build_pools(&cfg.data) {
        Ok(p) => p,
        Err(e) => panic!("pools do not build: {e}\n---\n{yaml}"),
    };
    let mut rng = ChaCha8Rng::seed_from_u64(1);
    for s in &cfg.scenarios {
        let r = &s.request;
        let vars = Value::Object(r.variables.clone().into_iter().collect());
        if let Err(e) = redliner::data::render_value(&vars, &pools, &mut rng) {
            panic!("scenario {:?}: variables do not render: {e}\n---\n{yaml}", s.name);
        }
        let mut strings = vec![("path", r.path.as_str())];
        if !r.body.is_empty() {
            strings.push(("body", r.body.as_str()));
        }
        for v in r.headers.values() {
            strings.push(("header", v.as_str()));
        }
        for (what, text) in strings {
            if let Err(e) = redliner::data::render_string(text, &pools, &mut rng) {
                panic!("scenario {:?}: {what} does not render: {e}\n---\n{yaml}", s.name);
            }
        }
    }
    cfg
}
