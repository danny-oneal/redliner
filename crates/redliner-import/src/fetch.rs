//! One-shot HTTP fetching of schemas and specs.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

use crate::ImportError;

/// How to reach the source: extra headers, a bearer token's env var, and
/// whether to skip TLS certificate checks.
#[derive(Debug, Clone, Default)]
pub struct FetchOpts {
    pub headers: Vec<(String, String)>,
    pub bearer_env: Option<String>,
    pub insecure: bool,
}

pub struct Fetcher {
    client: reqwest::Client,
    headers: HeaderMap,
}

impl Fetcher {
    /// Fails before any request when `bearer_env` names an unset variable.
    pub fn new(opts: &FetchOpts) -> Result<Fetcher, ImportError> {
        let mut headers = HeaderMap::new();
        for (k, v) in &opts.headers {
            // Name only in the error: the value may be a credential.
            let name =
                HeaderName::from_bytes(k.as_bytes()).map_err(|_| ImportError::Header(k.clone()))?;
            let mut value = HeaderValue::from_str(v).map_err(|_| ImportError::Header(k.clone()))?;
            value.set_sensitive(true);
            headers.append(name, value);
        }
        if let Some(var) = &opts.bearer_env {
            let token = std::env::var(var)
                .ok()
                .filter(|t| !t.is_empty())
                .ok_or_else(|| ImportError::MissingEnv(var.clone()))?;
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| ImportError::Header("Authorization".into()))?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .danger_accept_invalid_certs(opts.insecure)
            .build()
            .map_err(|source| ImportError::Http { url: "http client".into(), source })?;
        Ok(Fetcher { client, headers })
    }

    /// POSTs `body` as JSON and returns the JSON reply. A non-2xx reply is
    /// still returned when it carries GraphQL `errors` (servers refuse
    /// introspection that way); any other non-2xx is its status.
    pub async fn post_json(&self, url: &str, body: &Value) -> Result<Value, ImportError> {
        let req = self
            .client
            .post(url)
            .headers(self.headers.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string());
        let (code, bytes) = self.send(url, req).await?;
        let ok = (200..300).contains(&code);
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) if ok || v.get("errors").is_some() => Ok(v),
            Ok(_) => Err(ImportError::Status { url: url.to_string(), code }),
            Err(e) if ok => Err(ImportError::Decode {
                what: url.to_string(),
                message: format!("response is not JSON: {e}"),
            }),
            Err(_) => Err(ImportError::Status { url: url.to_string(), code }),
        }
    }

    /// GETs `url` as text; a non-2xx reply is its status.
    pub async fn get_text(&self, url: &str) -> Result<String, ImportError> {
        let req = self.client.get(url).headers(self.headers.clone());
        let (code, bytes) = self.send(url, req).await?;
        if !(200..300).contains(&code) {
            return Err(ImportError::Status { url: url.to_string(), code });
        }
        String::from_utf8(bytes).map_err(|e| ImportError::Decode {
            what: url.to_string(),
            message: format!("response is not UTF-8: {e}"),
        })
    }

    async fn send(
        &self,
        url: &str,
        req: reqwest::RequestBuilder,
    ) -> Result<(u16, Vec<u8>), ImportError> {
        let resp = req
            .send()
            .await
            .map_err(|source| ImportError::Http { url: url.to_string(), source })?;
        let code = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|source| ImportError::Http { url: url.to_string(), source })?;
        Ok((code, bytes.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_bearer_env_fails_before_any_request() {
        std::env::remove_var("REDLINER_IMPORT_T3_UNSET");
        let opts = FetchOpts {
            headers: vec![],
            bearer_env: Some("REDLINER_IMPORT_T3_UNSET".into()),
            insecure: false,
        };
        assert!(matches!(
            Fetcher::new(&opts),
            Err(ImportError::MissingEnv(v)) if v == "REDLINER_IMPORT_T3_UNSET"
        ));
    }
}
