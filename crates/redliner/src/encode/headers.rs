use std::collections::BTreeMap;

use base64::Engine;
use http::{HeaderMap, HeaderName, HeaderValue};
use rand_chacha::ChaCha8Rng;

use super::EncodeError;
use crate::config::TargetConfig;
use crate::data::{render_string, Pools};

/// A scenario header whose value holds `{{pool}}` placeholders, rendered per request.
pub struct HeaderTemplate {
    name: HeaderName,
    /// The key as written in the config, for error messages.
    key: String,
    value: String,
}

/// Layers a scenario's `headers` over `base`, for that scenario only. Values
/// with `{{pool}}` placeholders come back as templates for [`render_templates`].
pub fn scenario_headers(
    base: &HeaderMap,
    headers: &BTreeMap<String, String>,
) -> Result<(HeaderMap, Vec<HeaderTemplate>), EncodeError> {
    // A clone so scenario headers never leak into the base or other scenarios.
    let mut h = base.clone();
    let mut templated = Vec::new();
    for (k, v) in headers {
        let n = HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| EncodeError::InvalidHeader(format!("{k}: {e}")))?;
        // Checked even when templated, so a bad character fails at startup.
        let hv = HeaderValue::from_str(v)
            .map_err(|e| EncodeError::InvalidHeader(format!("{k}: {e}")))?;
        if v.contains("{{") {
            templated.push(HeaderTemplate { name: n, key: k.clone(), value: v.clone() });
        } else {
            h.insert(n, hv);
        }
    }
    Ok((h, templated))
}

/// Renders each template into `h`, replacing any header of the same name.
pub fn render_templates(
    h: &mut HeaderMap,
    templated: &[HeaderTemplate],
    pools: &Pools,
    rng: &mut ChaCha8Rng,
) -> Result<(), EncodeError> {
    for t in templated {
        let v = render_string(&t.value, pools, rng)?;
        let v = HeaderValue::from_str(&v)
            .map_err(|e| EncodeError::InvalidHeader(format!("{}: {e}", t.key)))?;
        h.insert(t.name.clone(), v);
    }
    Ok(())
}

/// Mirrors Go `baseHeaders`: Accept, user headers, then auth from the env.
pub fn base_headers(cfg: &TargetConfig) -> Result<HeaderMap, EncodeError> {
    let mut h = HeaderMap::new();
    h.insert("accept", HeaderValue::from_static("application/json"));
    for (k, v) in &cfg.headers {
        set(&mut h, k, v)?;
    }
    let a = &cfg.auth;
    match a.auth_type.as_str() {
        "bearer" => {
            let tok = require_env(&a.token_env)?;
            set(&mut h, "authorization", &format!("Bearer {tok}"))?;
        }
        "header" => {
            let tok = require_env(&a.token_env)?;
            set(&mut h, &a.header, &tok)?;
        }
        "basic" => {
            let user = require_env(&a.username_env)?;
            let pass = require_env(&a.password_env)?;
            let cred = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
            set(&mut h, "authorization", &format!("Basic {cred}"))?;
        }
        // "" | "none" (and anything else, as in Go) adds no auth header.
        _ => {}
    }
    Ok(h)
}

/// Inserts `name: value`, replacing any header of the same name.
pub fn set(h: &mut HeaderMap, name: &str, value: &str) -> Result<(), EncodeError> {
    let n = HeaderName::from_bytes(name.as_bytes())
        .map_err(|e| EncodeError::InvalidHeader(format!("{name}: {e}")))?;
    let v = HeaderValue::from_str(value)
        .map_err(|e| EncodeError::InvalidHeader(format!("{name}: {e}")))?;
    h.insert(n, v);
    Ok(())
}

/// Like Go `requireEnv`: unset or empty both count as missing.
fn require_env(name: &str) -> Result<String, EncodeError> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(EncodeError::MissingEnv(name.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthConfig;

    // Each test uses its own env var names: set_var is process-global and
    // tests run in parallel.
    fn cfg(auth: AuthConfig) -> TargetConfig {
        TargetConfig {
            auth,
            ..Default::default()
        }
    }

    #[test]
    fn base_headers_bearer_from_env() {
        std::env::set_var("RL_TEST_BEARER_TOKEN", "abc");
        let c = cfg(AuthConfig {
            auth_type: "bearer".into(),
            token_env: "RL_TEST_BEARER_TOKEN".into(),
            ..Default::default()
        });
        let h = base_headers(&c).unwrap();
        assert_eq!(h["authorization"], "Bearer abc");
        assert_eq!(h["accept"], "application/json");
    }

    #[test]
    fn base_headers_missing_env_errors() {
        let c = cfg(AuthConfig {
            auth_type: "bearer".into(),
            token_env: "RL_TEST_DEFINITELY_UNSET".into(),
            ..Default::default()
        });
        match base_headers(&c) {
            Err(EncodeError::MissingEnv(n)) => assert_eq!(n, "RL_TEST_DEFINITELY_UNSET"),
            other => panic!("expected MissingEnv, got {other:?}"),
        }
    }

    #[test]
    fn base_headers_basic_base64() {
        std::env::set_var("RL_TEST_BASIC_USER", "user");
        std::env::set_var("RL_TEST_BASIC_PASS", "pass");
        let c = cfg(AuthConfig {
            auth_type: "basic".into(),
            username_env: "RL_TEST_BASIC_USER".into(),
            password_env: "RL_TEST_BASIC_PASS".into(),
            ..Default::default()
        });
        let h = base_headers(&c).unwrap();
        assert_eq!(h["authorization"], "Basic dXNlcjpwYXNz");
    }

    #[test]
    fn base_headers_custom_header_auth() {
        std::env::set_var("RL_TEST_HDR_TOKEN", "k123");
        let c = cfg(AuthConfig {
            auth_type: "header".into(),
            token_env: "RL_TEST_HDR_TOKEN".into(),
            header: "X-Api-Key".into(),
            ..Default::default()
        });
        assert_eq!(base_headers(&c).unwrap()["x-api-key"], "k123");
    }
}
