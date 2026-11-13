use http::{HeaderMap, HeaderValue, Method};
use percent_encoding::utf8_percent_encode;
use rand_chacha::ChaCha8Rng;
use volley::Target;

use super::headers::{base_headers, render_templates, scenario_headers, HeaderTemplate};
use super::{EncodeError, Encoder, URL_COMPONENT};
use crate::config::{Scenario, TargetConfig};
use crate::data::{render_value, Pools};

struct GraphqlRoute {
    name: String,
    /// The endpoint plus an `op` query parameter naming the scenario.
    url: String,
    /// The scenario name, sent as `operationName` only when the query defines
    /// an operation by that name: a spec-compliant server rejects an
    /// `operationName` the document does not define.
    operation_name: Option<String>,
    /// Headers with fixed values.
    headers: HeaderMap,
    /// Headers whose values hold `{{pool}}` placeholders, rendered per request.
    templated: Vec<HeaderTemplate>,
}

pub struct GraphqlEncoder {
    routes: Vec<GraphqlRoute>,
}

impl GraphqlEncoder {
    pub fn new(cfg: &TargetConfig, scenarios: &[Scenario]) -> Result<Self, EncodeError> {
        let mut base = base_headers(cfg)?;
        base.insert("content-type", HeaderValue::from_static("application/json"));
        // The op param is a correlation tag naming the scenario in server logs;
        // joined with `&` when the endpoint already has a query string.
        let sep = if cfg.url.contains('?') { '&' } else { '?' };
        let mut routes = Vec::with_capacity(scenarios.len());
        for sc in scenarios {
            let (headers, templated) = scenario_headers(&base, &sc.request.headers)?;
            routes.push(GraphqlRoute {
                name: sc.name.clone(),
                url: format!("{}{sep}op={}", cfg.url, query_escape(&sc.name)),
                operation_name: defines_operation(&sc.request.query, &sc.name)
                    .then(|| sc.name.clone()),
                headers,
                templated,
            });
        }
        Ok(Self { routes })
    }

    pub fn encode(
        &self,
        t: &mut Target,
        sc: &Scenario,
        pools: &Pools,
        rng: &mut ChaCha8Rng,
    ) -> Result<(), EncodeError> {
        let route = self
            .routes
            .iter()
            .find(|r| r.name == sc.name)
            .ok_or_else(|| EncodeError::InvalidHeader(format!("unknown scenario {}", sc.name)))?;
        let variables = render_value(
            &serde_json::Value::Object(sc.request.variables.clone().into_iter().collect()),
            pools,
            rng,
        )?;
        let mut body = serde_json::json!({
            "query": sc.request.query,
            "variables": variables,
        });
        if let Some(op) = &route.operation_name {
            body["operationName"] = op.as_str().into();
        }
        let body = serde_json::to_vec(&body).map_err(|e| EncodeError::Body(e.to_string()))?;
        t.method = Method::POST;
        t.url.clone_from(&route.url);
        t.body = body;
        t.header = route.headers.clone();
        // After the variables, as for REST, so adding a templated header to a
        // seeded config leaves the variables it draws unchanged.
        render_templates(&mut t.header, &route.templated, pools, rng)
    }
}

impl Encoder for GraphqlEncoder {
    fn encode(
        &self,
        t: &mut Target,
        sc: &Scenario,
        pools: &Pools,
        rng: &mut ChaCha8Rng,
    ) -> Result<(), EncodeError> {
        GraphqlEncoder::encode(self, t, sc, pools, rng)
    }
}

/// Whether the GraphQL document `doc` defines an operation called `name`:
/// `query name`, `mutation name` or `subscription name` outside any
/// selection set, comment or string.
fn defines_operation(doc: &str, name: &str) -> bool {
    let b = doc.as_bytes();
    let (mut i, mut depth, mut after_keyword) = (0, 0usize, false);
    while i < b.len() {
        let c = b[i];
        if c == b'#' {
            while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                i += 1;
            }
            continue;
        }
        if c == b'"' {
            i = skip_string(b, i);
            after_keyword = false;
            continue;
        }
        if c == b'_' || c.is_ascii_alphabetic() {
            let start = i;
            while i < b.len() && (b[i] == b'_' || b[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            let word = &doc[start..i];
            if depth == 0 && after_keyword && word == name {
                return true;
            }
            after_keyword =
                depth == 0 && matches!(word, "query" | "mutation" | "subscription");
            continue;
        }
        match c {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        // Whitespace and commas are insignificant; any other token ends `query <name>`.
        if !(c.is_ascii_whitespace() || c == b',') {
            after_keyword = false;
        }
        i += 1;
    }
    false
}

/// The index just past the string literal (`"..."` or `"""..."""`) at `i`.
fn skip_string(b: &[u8], i: usize) -> usize {
    if b[i..].starts_with(b"\"\"\"") {
        let mut j = i + 3;
        while j < b.len() {
            if b[j..].starts_with(b"\\\"\"\"") {
                j += 4;
            } else if b[j..].starts_with(b"\"\"\"") {
                return j + 3;
            } else {
                j += 1;
            }
        }
        return b.len();
    }
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' | b'\n' => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

fn query_escape(s: &str) -> String {
    utf8_percent_encode(s, URL_COMPONENT)
        .to_string()
        .replace("%20", "+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RequestConfig;
    use crate::data::{Pool, Pools};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn rng() -> ChaCha8Rng {
        ChaCha8Rng::seed_from_u64(1)
    }

    fn scenario(name: &str) -> Scenario {
        Scenario {
            name: name.into(),
            weight: 1.0,
            request: RequestConfig {
                query: "query GetProduct { p { id } }".into(),
                variables: [("id".to_string(), serde_json::json!(5))].into(),
                ..Default::default()
            },
            matcher: None,
        }
    }

    fn enc() -> GraphqlEncoder {
        enc_for(&[scenario("GetProduct"), scenario("Get Product&x=1")])
    }

    fn enc_for(scenarios: &[Scenario]) -> GraphqlEncoder {
        GraphqlEncoder::new(
            &TargetConfig {
                url: "http://h/graphql".into(),
                ..Default::default()
            },
            scenarios,
        )
        .unwrap()
    }

    /// The `operationName` sent for a scenario called `name` with `query`.
    fn sent_operation_name(name: &str, query: &str) -> Option<String> {
        let mut sc = scenario(name);
        sc.request.query = query.into();
        let mut t = Target::default();
        enc_for(std::slice::from_ref(&sc)).encode(&mut t, &sc, &Pools::new(), &mut rng()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&t.body).unwrap();
        v.get("operationName").map(|n| n.as_str().unwrap().to_string())
    }

    #[test]
    fn operation_name_is_sent_only_when_the_query_defines_it() {
        let named = |q: &str| sent_operation_name("GetProduct", q);
        assert_eq!(named("query GetProduct($id: Int!) { p(id: $id) { id } }").as_deref(), Some("GetProduct"));
        assert_eq!(named("mutation GetProduct { p }").as_deref(), Some("GetProduct"));
        assert_eq!(
            named("query Other { p }\nsubscription GetProduct { p }").as_deref(),
            Some("GetProduct"),
            "one of several operations"
        );
        // Anonymous operations: the README's own example shape.
        assert_eq!(named("query($id:Int!){ product(id:$id){ name } }"), None);
        assert_eq!(named("{ product(id: 1) { name } }"), None);
        assert_eq!(named("query { p }"), None);
        // The name elsewhere is not an operation definition.
        assert_eq!(named("query GetProducts { p }"), None, "a prefix");
        assert_eq!(named("query Other { query GetProduct }"), None, "inside a selection set");
        assert_eq!(named("# query GetProduct\n{ p }"), None, "in a comment");
        assert_eq!(named(r#"{ p(q: "} query GetProduct {") }"#), None, "in a string");
        assert_eq!(named(r#"{ p(q: """ \""" } query GetProduct """) }"#), None, "in a block string");
        assert_eq!(named("fragment GetProduct on P { id } { p }"), None, "a fragment");
        // A scenario name that is not a GraphQL name never matches.
        assert_eq!(sent_operation_name("product lookup", "{ product(id: 1) { name } }"), None);
    }

    #[test]
    fn graphql_scenario_headers_layer_over_the_target_headers() {
        let target = TargetConfig {
            url: "http://h/graphql".into(),
            headers: [("X-Env", "base"), ("X-Keep", "k")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into(),
            ..Default::default()
        };
        let mut sc = scenario("GetProduct");
        sc.request.headers = [("X-Env", "scenario"), ("X-Id", "id-{{id}}")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into();
        let other = scenario("Other");
        let e = GraphqlEncoder::new(&target, &[sc.clone(), other.clone()]).unwrap();
        let mut pools = Pools::new();
        pools.insert("id".into(), Pool::List(vec![serde_json::json!(5)]));
        let mut t = Target::default();
        e.encode(&mut t, &sc, &pools, &mut rng()).unwrap();
        assert_eq!(t.header["x-env"], "scenario");
        assert_eq!(t.header.get_all("x-env").iter().count(), 1);
        assert_eq!(t.header["x-keep"], "k");
        assert_eq!(t.header["x-id"], "id-5");
        assert_eq!(t.header["content-type"], "application/json");
        // Another scenario sees only the target's headers.
        let mut t = Target::default();
        e.encode(&mut t, &other, &pools, &mut rng()).unwrap();
        assert_eq!(t.header["x-env"], "base");
        assert!(t.header.get("x-id").is_none());
    }

    #[test]
    fn graphql_encode_builds_post() {
        let e = enc();
        let mut t = Target::default();
        e.encode(&mut t, &scenario("GetProduct"), &Pools::new(), &mut rng()).unwrap();
        assert_eq!(t.method, Method::POST);
        assert!(t.url.contains("?op=GetProduct"));
        assert_eq!(t.header["content-type"], "application/json");
        let v: serde_json::Value = serde_json::from_slice(&t.body).unwrap();
        assert_eq!(v["operationName"], "GetProduct");
        assert_eq!(v["query"], "query GetProduct { p { id } }");
        assert_eq!(v["variables"]["id"], 5);
    }

    #[test]
    fn op_tag_escapes_the_scenario_name() {
        let e = enc();
        let mut t = Target::default();
        e.encode(&mut t, &scenario("Get Product&x=1"), &Pools::new(), &mut rng()).unwrap();
        assert_eq!(t.url, "http://h/graphql?op=Get+Product%26x%3D1");
    }

    #[test]
    fn op_tag_joins_an_endpoint_query_string() {
        let sc = scenario("GetProduct");
        let target = TargetConfig {
            url: "http://h/graphql?tenant=t1".into(),
            ..Default::default()
        };
        let e = GraphqlEncoder::new(&target, std::slice::from_ref(&sc)).unwrap();
        let mut t = Target::default();
        e.encode(&mut t, &sc, &Pools::new(), &mut rng()).unwrap();
        assert_eq!(t.url, "http://h/graphql?tenant=t1&op=GetProduct");
    }

    #[test]
    fn graphql_encode_renders_variables() {
        let e = enc();
        let mut sc = scenario("GetProduct");
        sc.request.variables = [("id".to_string(), serde_json::json!("{{id}}"))].into();
        let mut pools = Pools::new();
        pools.insert("id".into(), Pool::List(vec![serde_json::json!(5)]));
        let mut t = Target::default();
        e.encode(&mut t, &sc, &pools, &mut rng()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&t.body).unwrap();
        assert_eq!(v["variables"]["id"], 5);
        assert!(v["variables"]["id"].is_number());
    }
}
