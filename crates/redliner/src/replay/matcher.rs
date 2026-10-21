//! Classifies each logged event to the scenario whose `match` rule it
//! satisfies.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use regex::Regex;

use super::parse::{RawEvent, Trace};
use super::ReplayError;
use crate::config::{MatchConfig, Protocol, Scenario};

/// How many unmatched keys `MatchResult::unmatched_top` keeps.
const UNMATCHED_TOP: usize = 10;

/// One event assigned to a scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct Matched {
    pub at: DateTime<Utc>,
    /// Index into the scenario list passed to `match_events`.
    pub scenario: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MatchResult {
    /// In trace order.
    pub matched: Vec<Matched>,
    /// Keyed by scenario name; only scenarios with at least one match.
    pub per_scenario: BTreeMap<String, u64>,
    pub unmatched: u64,
    /// The most frequent unmatched `"{method} {path}[ op={operation}]"`
    /// keys: count descending, then key ascending.
    pub unmatched_top: Vec<(String, u64)>,
}

/// A scenario's rule, ready to test against events.
enum Rule {
    Operation(String),
    Rest { method: Option<String>, path: Regex },
}

impl Rule {
    fn compile(m: &MatchConfig, protocol: Protocol) -> Option<Rule> {
        match protocol {
            Protocol::Graphql => {
                (!m.operation.is_empty()).then(|| Rule::Operation(m.operation.clone()))
            }
            Protocol::Rest => {
                if m.path.is_empty() {
                    return None;
                }
                Some(Rule::Rest {
                    method: (!m.method.is_empty()).then(|| m.method.clone()),
                    path: glob_regex(&m.path),
                })
            }
        }
    }

    fn matches(&self, ev: &RawEvent) -> bool {
        match self {
            Rule::Operation(op) => ev.operation.as_deref() == Some(op.as_str()),
            Rule::Rest { method, path } => {
                method
                    .as_ref()
                    .is_none_or(|m| m.eq_ignore_ascii_case(&ev.method))
                    && path.is_match(&ev.path)
            }
        }
    }
}

/// `**` matches anything, `*` matches within one path segment, everything
/// else is literal. Anchored at both ends.
fn glob_regex(glob: &str) -> Regex {
    let mut pattern = String::from("^");
    let mut literal = String::new();
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '*' {
            literal.push(c);
            continue;
        }
        pattern.push_str(&regex::escape(&literal));
        literal.clear();
        if chars.peek() == Some(&'*') {
            chars.next();
            pattern.push_str(".*");
        } else {
            pattern.push_str("[^/]*");
        }
    }
    pattern.push_str(&regex::escape(&literal));
    pattern.push('$');
    Regex::new(&pattern).expect("escaped glob is a valid regex")
}

fn event_key(ev: &RawEvent) -> String {
    match &ev.operation {
        Some(op) => format!("{} {} op={op}", ev.method, ev.path),
        None => format!("{} {}", ev.method, ev.path),
    }
}

/// Assign every event in `trace` to the one scenario whose rule it
/// satisfies. An event matching two or more scenarios is an error.
pub fn match_events(
    trace: &Trace,
    scenarios: &[Scenario],
    protocol: Protocol,
) -> Result<MatchResult, ReplayError> {
    let rules: Vec<Option<Rule>> = scenarios
        .iter()
        .map(|s| s.matcher.as_ref().and_then(|m| Rule::compile(m, protocol)))
        .collect();

    let mut result = MatchResult::default();
    let mut unmatched: BTreeMap<String, u64> = BTreeMap::new();
    for ev in &trace.events {
        let mut hits = rules
            .iter()
            .enumerate()
            .filter(|(_, r)| r.as_ref().is_some_and(|r| r.matches(ev)))
            .map(|(i, _)| i);
        let first = hits.next();
        if let (Some(a), Some(b)) = (first, hits.next()) {
            return Err(ReplayError::Ambiguous {
                line: ev.line,
                event: event_key(ev),
                first: scenarios[a].name.clone(),
                second: scenarios[b].name.clone(),
            });
        }
        match first {
            Some(i) => {
                result.matched.push(Matched {
                    at: ev.at,
                    scenario: i,
                });
                *result
                    .per_scenario
                    .entry(scenarios[i].name.clone())
                    .or_insert(0) += 1;
            }
            None => {
                result.unmatched += 1;
                *unmatched.entry(event_key(ev)).or_insert(0) += 1;
            }
        }
    }

    let mut top: Vec<(String, u64)> = unmatched.into_iter().collect();
    // BTreeMap iterates keys ascending and the sort is stable.
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    top.truncate(UNMATCHED_TOP);
    result.unmatched_top = top;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RequestConfig;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn ev(line: usize, method: &str, path: &str, op: Option<&str>) -> RawEvent {
        RawEvent {
            at: at(line as i64),
            line,
            method: method.to_string(),
            path: path.to_string(),
            operation: op.map(str::to_string),
        }
    }

    fn trace(events: Vec<RawEvent>) -> Trace {
        Trace {
            events,
            ..Trace::default()
        }
    }

    fn gql(name: &str, operation: &str) -> Scenario {
        Scenario {
            name: name.to_string(),
            weight: 1.0,
            request: RequestConfig::default(),
            matcher: Some(MatchConfig {
                operation: operation.to_string(),
                ..MatchConfig::default()
            }),
        }
    }

    fn rest(name: &str, method: &str, path: &str) -> Scenario {
        Scenario {
            name: name.to_string(),
            weight: 1.0,
            request: RequestConfig::default(),
            matcher: Some(MatchConfig {
                method: method.to_string(),
                path: path.to_string(),
                ..MatchConfig::default()
            }),
        }
    }

    #[test]
    fn graphql_matches_by_operation() {
        let scenarios = [gql("a", "GetSku"), gql("b", "ListSkus")];
        let t = trace(vec![
            ev(1, "POST", "/graphql", Some("ListSkus")),
            ev(2, "POST", "/graphql", Some("GetSku")),
            ev(3, "POST", "/graphql", Some("getsku")),
            ev(4, "POST", "/graphql", None),
        ]);
        let r = match_events(&t, &scenarios, Protocol::Graphql).unwrap();
        let hits: Vec<(DateTime<Utc>, usize)> =
            r.matched.iter().map(|m| (m.at, m.scenario)).collect();
        assert_eq!(hits, vec![(at(1), 1), (at(2), 0)]);
        assert_eq!(r.per_scenario["a"], 1);
        assert_eq!(r.per_scenario["b"], 1);
        assert_eq!(r.unmatched, 2);
    }

    #[test]
    fn rest_glob_single_segment() {
        let scenarios = [rest("s", "", "/skus/*")];
        let t = trace(vec![
            ev(1, "GET", "/skus/1", None),
            ev(2, "GET", "/skus/1/reviews", None),
        ]);
        let r = match_events(&t, &scenarios, Protocol::Rest).unwrap();
        assert_eq!(r.matched.len(), 1);
        assert_eq!(r.matched[0].at, at(1));
        assert_eq!(r.unmatched, 1);
    }

    #[test]
    fn rest_double_star() {
        let scenarios = [rest("s", "", "/skus/**")];
        let t = trace(vec![
            ev(1, "GET", "/skus/1", None),
            ev(2, "GET", "/skus/1/reviews", None),
            ev(3, "GET", "/other", None),
        ]);
        let r = match_events(&t, &scenarios, Protocol::Rest).unwrap();
        assert_eq!(r.matched.len(), 2);
        assert_eq!(r.unmatched, 1);
    }

    #[test]
    fn rest_glob_escapes_regex_metacharacters() {
        let scenarios = [rest("s", "", "/a.b/(x)+")];
        let t = trace(vec![
            ev(1, "GET", "/a.b/(x)+", None),
            ev(2, "GET", "/aXb/(x)+", None),
        ]);
        let r = match_events(&t, &scenarios, Protocol::Rest).unwrap();
        assert_eq!(r.matched.len(), 1);
        assert_eq!(r.matched[0].at, at(1));
    }

    #[test]
    fn rest_method_optional_and_case_insensitive() {
        let scenarios = [rest("get", "get", "/a"), rest("any", "", "/b")];
        let t = trace(vec![
            ev(1, "GET", "/a", None),
            ev(2, "POST", "/a", None),
            ev(3, "POST", "/b", None),
            ev(4, "GET", "/b", None),
        ]);
        let r = match_events(&t, &scenarios, Protocol::Rest).unwrap();
        let hits: Vec<usize> = r.matched.iter().map(|m| m.scenario).collect();
        assert_eq!(hits, vec![0, 1, 1]);
        assert_eq!(r.unmatched, 1);
    }

    #[test]
    fn overlapping_rules_error_names_both() {
        let scenarios = [rest("first", "", "/skus/*"), rest("second", "", "/skus/**")];
        let t = trace(vec![ev(1, "GET", "/skus/1", None)]);
        let err = match_events(&t, &scenarios, Protocol::Rest).unwrap_err();
        assert!(matches!(err, ReplayError::Ambiguous { .. }));
        let msg = err.to_string();
        assert!(msg.contains("first"), "{msg}");
        assert!(msg.contains("second"), "{msg}");
        assert!(msg.contains("line 1"), "{msg}");
        assert!(msg.contains("GET /skus/1"), "{msg}");
    }

    #[test]
    fn unmatched_tally_and_top_list_ordered() {
        let scenarios = [rest("s", "", "/hit")];
        let mut events = vec![ev(1, "GET", "/hit", None)];
        let mut line = 2;
        for (method, path, op, n) in [
            ("GET", "/b", None, 3),
            ("GET", "/a", None, 3),
            ("POST", "/g", Some("Op"), 5),
        ] {
            for _ in 0..n {
                events.push(ev(line, method, path, op));
                line += 1;
            }
        }
        // Twelve distinct singletons push the list past ten entries.
        for i in 0..12 {
            events.push(ev(line, "GET", &format!("/z{i:02}"), None));
            line += 1;
        }
        let r = match_events(&trace(events), &scenarios, Protocol::Rest).unwrap();
        assert_eq!(r.unmatched, 3 + 3 + 5 + 12);
        assert_eq!(r.unmatched_top.len(), 10);
        assert_eq!(r.unmatched_top[0], ("POST /g op=Op".to_string(), 5));
        assert_eq!(r.unmatched_top[1], ("GET /a".to_string(), 3));
        assert_eq!(r.unmatched_top[2], ("GET /b".to_string(), 3));
        assert_eq!(r.unmatched_top[3], ("GET /z00".to_string(), 1));
        assert_eq!(r.unmatched_top[9], ("GET /z06".to_string(), 1));
    }

    #[test]
    fn scenario_without_match_never_matches() {
        let mut s = rest("s", "", "/a");
        s.matcher = None;
        let t = trace(vec![ev(1, "GET", "/a", None)]);
        let r = match_events(&t, &[s], Protocol::Rest).unwrap();
        assert!(r.matched.is_empty());
        assert!(r.per_scenario.is_empty());
        assert_eq!(r.unmatched, 1);
    }
}
