use http::{HeaderMap, HeaderName, HeaderValue, Method};
use rand_chacha::ChaCha8Rng;
use volley::Target;

use super::headers::base_headers;
use super::{EncodeError, Encoder};
use crate::config::{Scenario, TargetConfig};
use crate::data::{render_string, Pools};

struct RestRoute {
    name: String,
    method: String,
    /// Headers with fixed values.
    headers: HeaderMap,
    /// Headers whose values hold `{{pool}}` placeholders, rendered per request.
    templated: Vec<HeaderTemplate>,
    pattern: regex::Regex,
}

struct HeaderTemplate {
    name: HeaderName,
    /// The key as written in the config, for error messages.
    key: String,
    value: String,
}

pub struct RestEncoder {
    base_url: String,
    routes: Vec<RestRoute>,
}

impl RestEncoder {
    pub fn new(cfg: &TargetConfig, scenarios: &[Scenario]) -> Result<Self, EncodeError> {
        let base_url = cfg.url.trim_end_matches('/').to_string();
        let base = base_headers(cfg)?;
        let mut routes = Vec::with_capacity(scenarios.len());
        for sc in scenarios {
            let r = &sc.request;
            // Per-route clone so scenario headers never leak into the base or other routes.
            let mut headers = base.clone();
            let mut templated = Vec::new();
            for (k, v) in &r.headers {
                let n = HeaderName::from_bytes(k.as_bytes())
                    .map_err(|e| EncodeError::InvalidHeader(format!("{k}: {e}")))?;
                // Checked even when templated, so a bad character fails at startup.
                let hv = HeaderValue::from_str(v)
                    .map_err(|e| EncodeError::InvalidHeader(format!("{k}: {e}")))?;
                if v.contains("{{") {
                    templated.push(HeaderTemplate { name: n, key: k.clone(), value: v.clone() });
                } else {
                    headers.insert(n, hv);
                }
            }
            if !r.body.is_empty() && !headers.contains_key("content-type") {
                headers.insert("content-type", HeaderValue::from_static("application/json"));
            }
            routes.push(RestRoute {
                name: sc.name.clone(),
                method: r.method.to_ascii_uppercase(),
                headers,
                templated,
                pattern: path_pattern(&base_url, &r.path),
            });
        }
        Ok(Self { base_url, routes })
    }
}

impl Encoder for RestEncoder {
    fn encode(
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
        t.method = Method::from_bytes(route.method.as_bytes())
            .map_err(|e| EncodeError::InvalidHeader(format!("method {}: {e}", route.method)))?;
        t.url = format!(
            "{}{}",
            self.base_url,
            render_string(&sc.request.path, pools, rng)?
        );
        t.header = route.headers.clone();
        if !sc.request.body.is_empty() {
            t.body = render_string(&sc.request.body, pools, rng)?.into_bytes();
        }
        // After the path and body, so adding a templated header to a seeded
        // config leaves the paths and bodies it draws unchanged.
        for h in &route.templated {
            let v = render_string(&h.value, pools, rng)?;
            let v = HeaderValue::from_str(&v)
                .map_err(|e| EncodeError::InvalidHeader(format!("{}: {e}", h.key)))?;
            t.header.insert(h.name.clone(), v);
        }
        Ok(())
    }

    fn operation(&self, method: &str, url: &str) -> Option<String> {
        self.routes
            .iter()
            .find(|r| r.method == method && r.pattern.is_match(url))
            .map(|r| r.name.clone())
    }
}

/// Anchored regex for `base+path` where each `{{...}}` placeholder matches one URL segment value.
fn path_pattern(base: &str, path: &str) -> regex::Regex {
    let mut pat = String::from("^");
    let mut rest = format!("{base}{path}");
    while let Some(i) = rest.find("{{") {
        pat.push_str(&regex::escape(&rest[..i]));
        pat.push_str("[^/?&]+");
        match rest[i..].find("}}") {
            Some(j) => rest = rest[i + j + 2..].to_string(),
            None => {
                rest.clear();
                break;
            }
        }
    }
    pat.push_str(&regex::escape(&rest));
    pat.push('$');
    regex::Regex::new(&pat).expect("path pattern is a valid regex")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RequestConfig;
    use http::Method;
    use crate::data::{Pool, Pools};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::BTreeMap;

    fn rng() -> ChaCha8Rng {
        ChaCha8Rng::seed_from_u64(1)
    }

    fn sc(name: &str, method: &str, path: &str, body: &str, hdrs: &[(&str, &str)]) -> Scenario {
        Scenario {
            name: name.into(),
            weight: 1.0,
            request: RequestConfig {
                method: method.into(),
                path: path.into(),
                body: body.into(),
                headers: hdrs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<BTreeMap<_, _>>(),
                ..Default::default()
            },
            matcher: None,
        }
    }

    fn target(url: &str) -> TargetConfig {
        TargetConfig {
            url: url.into(),
            ..Default::default()
        }
    }

    #[test]
    fn rest_encode_builds_request() {
        let s = sc("GetProduct", "get", "/products/5", "", &[("X-Trace", "t1")]);
        let enc = RestEncoder::new(&target("http://h/"), std::slice::from_ref(&s)).unwrap();
        let mut t = Target::default();
        enc.encode(&mut t, &s, &Pools::new(), &mut rng()).unwrap();
        assert_eq!(t.method, Method::GET);
        assert_eq!(t.url, "http://h/products/5");
        assert_eq!(t.header["x-trace"], "t1");
        assert_eq!(t.header["accept"], "application/json");
        assert!(t.body.is_empty());
    }

    #[test]
    fn rest_body_sets_json_content_type() {
        let a = sc("A", "POST", "/a", "{\"a\":1}", &[]);
        let b = sc("B", "POST", "/b", "x", &[("Content-Type", "text/plain")]);
        let enc = RestEncoder::new(&target("http://h"), &[a.clone(), b.clone()]).unwrap();
        let mut t = Target::default();
        enc.encode(&mut t, &a, &Pools::new(), &mut rng()).unwrap();
        assert_eq!(t.header["content-type"], "application/json");
        assert_eq!(t.body, b"{\"a\":1}");
        let mut t2 = Target::default();
        enc.encode(&mut t2, &b, &Pools::new(), &mut rng()).unwrap();
        assert_eq!(t2.header["content-type"], "text/plain");
    }

    #[test]
    fn rest_headers_do_not_leak_between_routes() {
        let a = sc("A", "GET", "/a", "", &[("X-A", "1")]);
        let b = sc("B", "GET", "/b", "", &[]);
        let enc = RestEncoder::new(&target("http://h"), &[a.clone(), b.clone()]).unwrap();
        let mut t = Target::default();
        enc.encode(&mut t, &b, &Pools::new(), &mut rng()).unwrap();
        assert!(t.header.get("x-a").is_none());
        let mut ta = Target::default();
        enc.encode(&mut ta, &a, &Pools::new(), &mut rng()).unwrap();
        assert_eq!(ta.header["x-a"], "1");
    }

    #[test]
    fn rest_scenario_header_overrides_base_header() {
        // base_headers sets accept: application/json; the scenario must win.
        let a = sc("A", "GET", "/a", "", &[("Accept", "text/html")]);
        let b = sc("B", "GET", "/b", "", &[]);
        let enc = RestEncoder::new(&target("http://h"), &[a.clone(), b.clone()]).unwrap();
        let mut ta = Target::default();
        enc.encode(&mut ta, &a, &Pools::new(), &mut rng()).unwrap();
        assert_eq!(ta.header["accept"], "text/html");
        assert_eq!(ta.header.get_all("accept").iter().count(), 1);
        // The override did not mutate the shared base seen by other routes.
        let mut tb = Target::default();
        enc.encode(&mut tb, &b, &Pools::new(), &mut rng()).unwrap();
        assert_eq!(tb.header["accept"], "application/json");
    }

    #[test]
    fn rest_operation_matches_by_pattern() {
        let s = sc("GetProduct", "GET", "/products/{{id}}", "", &[]);
        let enc = RestEncoder::new(&target("http://h"), &[s]).unwrap();
        assert_eq!(
            enc.operation("GET", "http://h/products/42"),
            Some("GetProduct".to_string())
        );
        assert_eq!(enc.operation("POST", "http://h/products/42"), None);
        assert_eq!(enc.operation("GET", "http://h/orders/1"), None);
    }

    #[test]
    fn rest_encode_renders_path() {
        let s = sc("GetProduct", "GET", "/products/{{id}}", "", &[]);
        let enc = RestEncoder::new(&target("http://h"), std::slice::from_ref(&s)).unwrap();
        let mut pools = Pools::new();
        pools.insert("id".into(), Pool::List(vec![serde_json::json!(5)]));
        let mut t = Target::default();
        enc.encode(&mut t, &s, &pools, &mut rng()).unwrap();
        assert_eq!(t.url, "http://h/products/5");
    }

    #[test]
    fn rest_header_placeholders_render_per_request() {
        let s = sc("Create", "POST", "/orders", "{}", &[("Idempotency-Key", "{{keys}}"), ("X-Trace", "run-{{n}}"), ("X-Static", "s")]);
        let enc = RestEncoder::new(&target("http://h"), std::slice::from_ref(&s)).unwrap();
        let mut pools = Pools::new();
        pools.insert("keys".into(), Pool::Uuid);
        pools.insert("n".into(), Pool::Sequence(Default::default()));
        let mut r = rng();
        let (mut a, mut b) = (Target::default(), Target::default());
        enc.encode(&mut a, &s, &pools, &mut r).unwrap();
        enc.encode(&mut b, &s, &pools, &mut r).unwrap();
        let key = |t: &Target| t.header["idempotency-key"].to_str().unwrap().to_string();
        assert_eq!(key(&a).len(), 36, "{}", key(&a));
        assert_ne!(key(&a), key(&b), "a fresh key per request");
        assert_eq!(a.header["x-trace"], "run-0");
        assert_eq!(b.header["x-trace"], "run-1");
        assert_eq!(a.header["x-static"], "s");
        assert!(a.header.get("x-trace").is_some() && a.header.get_all("x-trace").iter().count() == 1);
    }

    #[test]
    fn rest_header_templates_leave_path_and_body_draws_unchanged() {
        // Headers render after the path and body, so adding a templated
        // header to a seeded config doesn't change the paths and bodies it sent.
        let plain = sc("A", "POST", "/items/{{id}}", "{\"id\":{{id}}}", &[]);
        let templated = sc("A", "POST", "/items/{{id}}", "{\"id\":{{id}}}", &[("X-Id", "{{id}}")]);
        let mut pools = Pools::new();
        pools.insert("id".into(), Pool::Range { min: 1, max: 1_000_000 });
        let sent = |s: &Scenario| {
            let enc = RestEncoder::new(&target("http://h"), std::slice::from_ref(s)).unwrap();
            let mut t = Target::default();
            enc.encode(&mut t, s, &pools, &mut rng()).unwrap();
            (t.url, t.body)
        };
        assert_eq!(sent(&plain), sent(&templated));
    }

    #[test]
    fn rest_header_rendered_value_must_be_valid() {
        let s = sc("A", "GET", "/a", "", &[("X-V", "{{v}}")]);
        let enc = RestEncoder::new(&target("http://h"), std::slice::from_ref(&s)).unwrap();
        let mut pools = Pools::new();
        pools.insert("v".into(), Pool::List(vec![serde_json::json!("a\nb")]));
        let err = enc.encode(&mut Target::default(), &s, &pools, &mut rng()).unwrap_err();
        assert!(matches!(err, EncodeError::InvalidHeader(ref m) if m.starts_with("X-V: ")), "{err}");

        let err = enc.encode(&mut Target::default(), &s, &Pools::new(), &mut rng()).unwrap_err();
        assert!(matches!(err, EncodeError::UnknownPool(ref p) if p == "v"), "{err}");
    }
}

