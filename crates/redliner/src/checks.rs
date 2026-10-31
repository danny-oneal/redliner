//! App-layer checks (Go `checks.go`).
//!
//! Reclassifies results that passed at the HTTP layer but failed at the
//! application layer. The canonical case: GraphQL servers return 200 for
//! resolver errors, so a load test that only looks at status codes reports
//! 100% success while every response is an error payload. A failed check
//! zeroes the code and records a descriptive error, so the failure shows up
//! in the success rate, the SLO gate, and the error report.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use volley::AttackResult;

use crate::config::ChecksConfig;

/// Reclassifies application-layer failures on otherwise-successful results.
pub struct Checker {
    /// Acceptable HTTP status codes; `None` accepts the default 2xx/3xx.
    statuses: Option<HashSet<u16>>,
    graphql_errors: bool,
    /// Results that passed every check (see [`Checker::accepted`]).
    accepted: AtomicU64,
}

impl Checker {
    pub fn new(cfg: &ChecksConfig) -> Self {
        let statuses = if cfg.statuses.is_empty() {
            None
        } else {
            Some(cfg.statuses.iter().map(|&s| s as u16).collect())
        };
        Self {
            statuses,
            graphql_errors: cfg.graphql_errors.unwrap_or(false),
            accepted: AtomicU64::new(0),
        }
    }

    /// How many applied results [`Checker::accepts`] counted.
    pub fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    /// Whether `r` succeeded: no error, and a status in `checks.status`
    /// (2xx/3xx when that is empty). The one definition of success; the
    /// metrics, the SLO gate and the reports all take it from here. Call it
    /// after [`Checker::apply`] has reclassified `r`.
    pub fn accepts(&self, r: &AttackResult) -> bool {
        let ok_status = match &self.statuses {
            Some(statuses) => statuses.contains(&r.code),
            None => (200..400).contains(&r.code),
        };
        ok_status && r.error.as_deref().is_none_or(str::is_empty)
    }

    /// Mutates `r` in place; returns `true` if the result was reclassified.
    /// Tallies the result towards [`Checker::accepted`].
    pub fn apply(&self, r: &mut AttackResult) -> bool {
        let reclassified = self.reclassify(r);
        if self.accepts(r) {
            self.accepted.fetch_add(1, Ordering::Relaxed);
        }
        reclassified
    }

    fn reclassify(&self, r: &mut AttackResult) -> bool {
        if r.error.as_deref().is_some_and(|e| !e.is_empty()) {
            return false; // transport already failed; keep the original error
        }
        if let Some(statuses) = &self.statuses {
            if !statuses.contains(&r.code) {
                r.error = Some(format!("check: status {} not in allowed set", r.code));
                r.code = 0;
                return true;
            }
        }
        if self.graphql_errors && (200..400).contains(&r.code) && !r.body.is_empty() {
            if let Ok(doc) = serde_json::from_slice::<Value>(&r.body) {
                if let Some(msg) = first_graphql_error(&doc) {
                    r.error = Some(format!("check: graphql errors (http {}): {msg}", r.code));
                    r.code = 0;
                    return true;
                }
            }
        }
        false
    }
}

/// Message of the first entry of a non-empty top-level `errors` array, or
/// `None` when there is no such array (Go `errsPresent` + `firstGraphQLError`).
fn first_graphql_error(doc: &Value) -> Option<String> {
    let first = doc.get("errors")?.as_array()?.first()?;
    Some(
        first
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| "unknown error shape".to_string(), str::to_string),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn checker(statuses: Option<&[u16]>, graphql_errors: bool) -> Checker {
        Checker {
            statuses: statuses.map(|s| s.iter().copied().collect()),
            graphql_errors,
            accepted: AtomicU64::new(0),
        }
    }

    #[test]
    fn accepts_is_the_success_verdict_and_matches_the_tally() {
        let verdicts = |c: &Checker, rs: Vec<AttackResult>| -> Vec<bool> {
            rs.into_iter()
                .map(|mut r| {
                    c.apply(&mut r);
                    c.accepts(&r)
                })
                .collect()
        };
        // checks.status [404]: the allowed 404 succeeds; a 200 is outside the set.
        let allow_404 = checker(Some(&[404]), false);
        let v = verdicts(&allow_404, vec![res(404, b"", None), res(200, b"", None)]);
        assert_eq!(v, [true, false]);
        assert_eq!(allow_404.accepted(), 1);

        // Default: 2xx/3xx without an error or a GraphQL `errors` payload.
        let default = checker(None, true);
        let v = verdicts(
            &default,
            vec![
                res(200, b"{}", None),
                res(302, b"", None),
                res(500, b"", None),
                res(0, b"", Some("connection refused")),
                res(200, br#"{"errors":[{"message":"boom"}]}"#, None),
            ],
        );
        assert_eq!(v, [true, true, false, false, false]);
        assert_eq!(default.accepted(), 2);

        // A body that failed mid-read keeps its real status but carries an
        // error: never a success, whatever the status rule.
        let truncated = res(200, b"", Some("error reading a body from connection"));
        assert!(!checker(None, false).accepts(&truncated));
        assert!(!checker(Some(&[200]), false).accepts(&truncated));
    }

    fn res(code: u16, body: &[u8], err: Option<&str>) -> AttackResult {
        AttackResult {
            attack: "t".into(),
            seq: 0,
            code,
            timestamp: SystemTime::now(),
            latency: Duration::ZERO,
            bytes_in: body.len() as u64,
            bytes_out: 0,
            error: err.map(String::from),
            method: "POST".into(),
            url: "http://x".into(),
            body: body.to_vec(),
        }
    }

    #[test]
    fn accepted_counts_the_checkers_verdict() {
        // An allowed non-2xx status is a success; so is 2xx/3xx by default.
        let allow_404 = checker(Some(&[404]), false);
        allow_404.apply(&mut res(404, b"", None));
        allow_404.apply(&mut res(200, b"", None));
        assert_eq!(allow_404.accepted(), 1);

        let default = checker(None, true);
        default.apply(&mut res(200, b"{}", None));
        default.apply(&mut res(500, b"", None));
        default.apply(&mut res(0, b"", Some("connection refused")));
        default.apply(&mut res(200, br#"{"errors":[{"message":"boom"}]}"#, None));
        assert_eq!(default.accepted(), 1);
    }

    #[test]
    fn reclassifies_graphql_errors() {
        let c = checker(None, true);
        let mut r = res(200, br#"{"errors":[{"message":"boom"}]}"#, None);
        assert!(c.apply(&mut r));
        assert_eq!(r.code, 0);
        let e = r.error.unwrap();
        assert!(e.contains("graphql errors") && e.contains("boom"), "{e}");
    }

    #[test]
    fn graphql_ok_body_passes() {
        let c = checker(None, true);
        let mut r = res(200, br#"{"data":{"x":1}}"#, None);
        assert!(!c.apply(&mut r));
        assert_eq!(r.code, 200);
        assert!(r.error.is_none());
    }

    #[test]
    fn graphql_errors_off_ignores_payload() {
        let c = checker(None, false);
        let mut r = res(200, br#"{"errors":[{"message":"boom"}]}"#, None);
        assert!(!c.apply(&mut r));
        assert_eq!(r.code, 200);
    }

    #[test]
    fn status_set_both_ways() {
        let c = checker(Some(&[200]), false);
        let mut bad = res(500, b"", None);
        assert!(c.apply(&mut bad));
        assert_eq!(bad.code, 0);
        assert!(bad.error.unwrap().contains("status 500 not in allowed set"));

        let mut ok = res(200, b"", None);
        assert!(!c.apply(&mut ok));
        assert_eq!(ok.code, 200);
    }

    #[test]
    fn transport_error_preserved() {
        let c = checker(Some(&[200]), true);
        let mut r = res(0, b"", Some("connection refused"));
        assert!(!c.apply(&mut r));
        assert_eq!(r.code, 0);
        assert_eq!(r.error.as_deref(), Some("connection refused"));
    }

    #[test]
    fn empty_body_not_reclassified() {
        let c = checker(None, true);
        let mut r = res(200, b"", None);
        assert!(!c.apply(&mut r));
        assert_eq!(r.code, 200);
    }
}
