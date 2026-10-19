//! Harvest: a pre-attack bootstrap probe that fills pools from the live
//! server (port of the Go `bootstrap.go`).

use std::collections::BTreeMap;

use http::HeaderMap;
use serde_json::{json, Value};

use super::{extract_path, DataError, Pool, Pools};
use crate::config::{Config, Protocol, RequestConfig, TargetConfig};
use crate::encode::headers::base_headers;

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
        let client = reqwest::Client::builder()
            .timeout(cfg.timeout.as_std())
            .build()
            .map_err(|e| DataError::Http(e.to_string()))?;
        Ok(Self {
            target: cfg.clone(),
            client,
            headers,
        })
    }

    pub async fn fetch(&self, proto: Protocol, rc: &RequestConfig) -> Result<Value, DataError> {
        let req = match proto {
            Protocol::Graphql => {
                let body = json!({"query": rc.query, "variables": rc.variables});
                self.client
                    .post(&self.target.url)
                    .headers(self.headers.clone())
                    .header("content-type", "application/json")
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
                let mut req = self
                    .client
                    .request(method, url)
                    .headers(self.headers.clone());
                if !rc.body.is_empty() {
                    req = req
                        .header("content-type", "application/json")
                        .body(rc.body.clone());
                }
                req
            }
        };

        let resp = req
            .send()
            .await
            .map_err(|e| DataError::Http(e.to_string()))?;
        if resp.status() != reqwest::StatusCode::OK {
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

/// Message of the first entry of a non-empty top-level `errors` array.
/// No shared graphql-errors helper is reachable from the data module, so
/// this mirrors Go's `errsPresent`/`firstGraphQLError` minimally.
fn first_graphql_error(doc: &Value) -> Option<String> {
    let first = doc.get("errors")?.as_array()?.first()?;
    Some(
        first
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| first.to_string(), str::to_string),
    )
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

    #[test]
    fn graphql_errors_detected() {
        assert_eq!(
            first_graphql_error(&json!({"errors":[{"message":"boom"}]})).as_deref(),
            Some("boom")
        );
        assert!(first_graphql_error(&json!({"errors":[]})).is_none());
        assert!(first_graphql_error(&json!({"data":{}})).is_none());
    }
}
