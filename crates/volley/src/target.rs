//! Request target and per-request result records.

use std::time::{Duration, SystemTime};

/// A single HTTP request to issue (Go `vegeta.Target`).
#[derive(Debug, Clone, Default)]
pub struct Target {
    pub method: http::Method,
    pub url: String,
    pub body: Vec<u8>,
    pub header: http::HeaderMap,
}

/// The outcome of one request (Go `vegeta.Result`).
#[derive(Debug, Clone)]
pub struct AttackResult {
    pub attack: String,
    pub seq: u64,
    pub code: u16,
    pub timestamp: SystemTime,
    pub latency: Duration,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub error: Option<String>,
    pub method: String,
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_default_is_get() {
        let t = Target::default();
        assert_eq!(t.method, http::Method::GET);
        assert!(t.body.is_empty());
    }
}
