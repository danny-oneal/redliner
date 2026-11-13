//! Harvest: a pre-attack bootstrap probe that fills pools from the live
//! server (port of the Go `bootstrap.go`).

use std::collections::BTreeMap;

use http::{HeaderMap, HeaderValue};
use serde_json::{json, Value};

use super::{extract_path, DataError, Pool, Pools};
use crate::checks::first_graphql_error;
use crate::config::{Config, Protocol, RequestConfig, TargetConfig};
use crate::encode::headers::{base_headers, set};

/// One-shot HTTP client that reuses the target's headers/auth so harvest
/// requests look like real client traffic.
pub struct BootstrapClient {
    target: TargetConfig,
    client: reqwest::Client,
    headers: HeaderMap,
}

impl BootstrapClient {
    pub fn new(cfg: &TargetConfig) -> Result<Self, DataError> {
        let headers = base_headers(cfg)?;
        // Connection options as the attacker's client sets them, so a target
        // that speaks only HTTP/2 (or must not reuse connections) is probed
        // the way it is attacked.
        let mut builder = reqwest::Client::builder()
            .timeout(cfg.timeout.as_std())
            .danger_accept_invalid_certs(cfg.insecure_tls);
        if !cfg.keep_alive.unwrap_or(true) {
            builder = builder.pool_max_idle_per_host(0);
        }
        if cfg.http2 {
            builder = builder.http2_prior_knowledge();
        }
        let client = builder
            .build()
            .map_err(|e| DataError::Http(e.to_string()))?;
        Ok(Self {
            target: cfg.clone(),
            client,
            headers,
        })
    }

    pub async fn fetch(&self, proto: Protocol, rc: &RequestConfig) -> Result<Value, DataError> {
        // Layered as the encoders do: the target's headers, the GraphQL
        // content type, then the harvest request's own headers, sent as
        // written (the pools are what the harvest is filling).
        let mut headers = self.headers.clone();
        if proto == Protocol::Graphql {
            headers.insert("content-type", HeaderValue::from_static("application/json"));
        }
        for (k, v) in &rc.headers {
            set(&mut headers, k, v)?;
        }
        let req = match proto {
            Protocol::Graphql => {
                let body = json!({"query": rc.query, "variables": rc.variables});
                self.client
                    .post(&self.target.url)
                    .headers(headers)
                    .body(body.to_string())
            }
            Protocol::Rest => {
                let method = if rc.method.is_empty() {
                    "GET".to_string()
                } else {
                    rc.method.to_uppercase()
                };
                let method = reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|e| DataError::Http(e.to_string()))?;
                let url = format!("{}{}", self.target.url.trim_end_matches('/'), rc.path);
                let mut req = self.client.request(method, url);
                if !rc.body.is_empty() {
                    if !headers.contains_key("content-type") {
                        headers.insert("content-type", HeaderValue::from_static("application/json"));
                    }
                    req = req.body(rc.body.clone());
                }
                req.headers(headers)
            }
        };

        let resp = req
            .send()
            .await
            .map_err(|e| DataError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(DataError::Status {
                code: resp.status().as_u16(),
                url: resp.url().to_string(),
            });
        }
        let raw = resp
            .bytes()
            .await
            .map_err(|e| DataError::Http(e.to_string()))?;
        let doc: Value =
            serde_json::from_slice(&raw).map_err(|e| DataError::Decode(e.to_string()))?;
        if proto == Protocol::Graphql {
            if let Some(msg) = first_graphql_error(&doc) {
                return Err(DataError::GraphqlErrors(msg));
            }
        }
        Ok(doc)
    }
}

/// Extracts each `pool -> path` from `doc` and appends into the pools.
fn apply_extract(
    doc: &Value,
    extract: &BTreeMap<String, String>,
    pools: &mut Pools,
) -> Result<(), DataError> {
    for (pool, path) in extract {
        let vals = extract_path(doc, path)?;
        if vals.is_empty() {
            return Err(DataError::EmptyMatch {
                pool: pool.clone(),
                path: path.clone(),
            });
        }
        match pools.get_mut(pool) {
            Some(Pool::List(existing)) => existing.extend(vals),
            _ => {
                pools.insert(pool.clone(), Pool::List(vals));
            }
        }
    }
    Ok(())
}

/// Runs every configured harvest request and merges the extracted values
/// into `pools`. Pool names shared across harvests append.
pub async fn run_harvest(cfg: &Config, pools: &mut Pools) -> Result<(), DataError> {
    if cfg.data.harvest.is_empty() {
        return Ok(());
    }
    let bc = BootstrapClient::new(&cfg.target)?;
    for h in &cfg.data.harvest {
        let doc = bc.fetch(cfg.target.protocol, &h.request).await?;
        apply_extract(&doc, &h.extract, pools)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// An HTTP/1.1 server answering every bodiless request with `status` and
    /// `{}`, keeping connections open. Returns its base URL and a count of
    /// the connections it accepted.
    async fn serve(status: &'static str) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let conns = Arc::new(AtomicUsize::new(0));
        let c = conns.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                c.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let (mut data, mut buf) = (Vec::new(), [0u8; 1024]);
                    while let Ok(n @ 1..) = sock.read(&mut buf).await {
                        data.extend_from_slice(&buf[..n]);
                        while let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                            data.drain(..i + 4);
                            let resp = format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\n\r\n{{}}");
                            let _ = sock.write_all(resp.as_bytes()).await;
                        }
                    }
                });
            }
        });
        (url, conns)
    }

    fn target(url: &str) -> TargetConfig {
        TargetConfig {
            url: url.into(),
            timeout: crate::config::ConfigDuration(Duration::from_secs(5)),
            ..Default::default()
        }
    }

    fn get(path: &str) -> RequestConfig {
        RequestConfig { path: path.into(), ..Default::default() }
    }

    #[tokio::test]
    async fn harvest_accepts_any_2xx() {
        let (url, _) = serve("201 Created").await;
        let bc = BootstrapClient::new(&target(&url)).unwrap();
        assert_eq!(bc.fetch(Protocol::Rest, &get("/items")).await.unwrap(), json!({}));
        let (url, _) = serve("302 Found").await;
        let bc = BootstrapClient::new(&target(&url)).unwrap();
        let e = bc.fetch(Protocol::Rest, &get("/items")).await.unwrap_err();
        assert!(matches!(e, DataError::Status { code: 302, .. }), "{e:?}");
    }

    #[tokio::test]
    async fn harvest_honours_keep_alive_off() {
        for (keep_alive, want) in [(None, 1), (Some(false), 2)] {
            let (url, conns) = serve("200 OK").await;
            let bc = BootstrapClient::new(&TargetConfig { keep_alive, ..target(&url) }).unwrap();
            for _ in 0..2 {
                bc.fetch(Protocol::Rest, &get("/items")).await.unwrap();
            }
            assert_eq!(conns.load(Ordering::SeqCst), want, "keep_alive {keep_alive:?}");
        }
    }

    #[tokio::test]
    async fn harvest_speaks_http2_when_the_target_does() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bc = BootstrapClient::new(&TargetConfig { http2: true, ..target(&url) }).unwrap();
        let probe = tokio::spawn(async move { bc.fetch(Protocol::Rest, &get("/items")).await });
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut preface = [0u8; 24];
        sock.read_exact(&mut preface).await.unwrap();
        assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
        probe.abort();
    }

    fn extract(pool: &str, path: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(pool.to_string(), path.to_string())])
    }

    #[test]
    fn run_harvest_appends_to_existing_pool() {
        let mut pools = Pools::new();
        pools.insert("ids".into(), Pool::List(vec![json!(1)]));
        let doc = json!({"data":{"items":[{"id":2},{"id":3}]}});
        apply_extract(&doc, &extract("ids", "data.items[].id"), &mut pools).unwrap();
        match &pools["ids"] {
            Pool::List(v) => assert_eq!(v, &vec![json!(1), json!(2), json!(3)]),
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn run_harvest_empty_match_errors() {
        let mut pools = Pools::new();
        let doc = json!({"data":{"items":[]}});
        let err = apply_extract(&doc, &extract("ids", "data.items[].id"), &mut pools).unwrap_err();
        assert!(matches!(err, DataError::EmptyMatch { .. }), "{err:?}");
        assert!(pools.is_empty());
    }
}
