use http::{HeaderMap, HeaderValue, Method};
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use volley::Target;

use super::headers::base_headers;
use super::{EncodeError, Encoder};
use crate::config::{Scenario, TargetConfig};

/// Go `url.QueryEscape` keeps only alphanumerics and `-_.~`.
const QUERY_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

pub struct GraphqlEncoder {
    endpoint: String,
    headers: HeaderMap,
}

impl GraphqlEncoder {
    pub fn new(cfg: &TargetConfig) -> Result<Self, EncodeError> {
        let mut headers = base_headers(cfg)?;
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        Ok(Self {
            endpoint: cfg.url.clone(),
            headers,
        })
    }

    pub fn encode(&self, t: &mut Target, sc: &Scenario) -> Result<(), EncodeError> {
        // SEAM: {{placeholder}} pool templating replaces the static variables pass in the data-pools plan.
        let body = serde_json::to_vec(&serde_json::json!({
            "operationName": sc.name,
            "query": sc.request.query,
            "variables": sc.request.variables,
        }))
        .map_err(|e| EncodeError::Body(e.to_string()))?;
        t.method = Method::POST;
        // The op param is a correlation tag so per-operation metrics can be split out.
        t.url = format!("{}?op={}", self.endpoint, query_escape(&sc.name));
        t.body = body;
        t.header = self.headers.clone();
        Ok(())
    }

    /// Recovers the operation name from a URL produced by `encode`.
    pub fn operation(&self, url: &str) -> Option<String> {
        let i = url.find("?op=")?;
        let raw = url[i + 4..].replace('+', " ");
        Some(percent_decode_str(&raw).decode_utf8_lossy().into_owned())
    }
}

impl Encoder for GraphqlEncoder {
    fn encode(&self, t: &mut Target, sc: &Scenario) -> Result<(), EncodeError> {
        GraphqlEncoder::encode(self, t, sc)
    }

    fn operation(&self, _method: &str, url: &str) -> Option<String> {
        GraphqlEncoder::operation(self, url)
    }
}

fn query_escape(s: &str) -> String {
    utf8_percent_encode(s, QUERY_SET)
        .to_string()
        .replace("%20", "+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RequestConfig;

    fn scenario(name: &str) -> Scenario {
        Scenario {
            name: name.into(),
            weight: 1.0,
            request: RequestConfig {
                query: "query GetProduct { p { id } }".into(),
                variables: [("id".to_string(), serde_json::json!(5))].into(),
                ..Default::default()
            },
        }
    }

    fn enc() -> GraphqlEncoder {
        GraphqlEncoder::new(&TargetConfig {
            url: "http://h/graphql".into(),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn graphql_encode_builds_post() {
        let e = enc();
        let mut t = Target::default();
        e.encode(&mut t, &scenario("GetProduct")).unwrap();
        assert_eq!(t.method, Method::POST);
        assert!(t.url.contains("?op=GetProduct"));
        assert_eq!(t.header["content-type"], "application/json");
        let v: serde_json::Value = serde_json::from_slice(&t.body).unwrap();
        assert_eq!(v["operationName"], "GetProduct");
        assert_eq!(v["query"], "query GetProduct { p { id } }");
        assert_eq!(v["variables"]["id"], 5);
        assert_eq!(Encoder::operation(&e, "POST", &t.url).as_deref(), Some("GetProduct"));
    }

    #[test]
    fn operation_round_trips_escaped_names() {
        let e = enc();
        let mut t = Target::default();
        e.encode(&mut t, &scenario("Get Product&x=1")).unwrap();
        assert_eq!(Encoder::operation(&e, "POST", &t.url).as_deref(), Some("Get Product&x=1"));
        assert_eq!(Encoder::operation(&e, "POST", "http://h/graphql"), None);
    }
}
