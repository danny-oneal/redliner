//! Reads a production request log (JSON lines or HAR) into a time-ordered
//! list of events. Parsing only: matching and scheduling live elsewhere.

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::ReplayError;
use crate::config::{parse_rfc3339, ReplayConfig, ReplayFields, WindowConfig};
use crate::data::extract_path;

/// How many malformed-line descriptions a `Trace` keeps.
const MAX_MALFORMED_EXAMPLES: usize = 3;

/// One logged request. `line` is the 1-based JSONL line number, or the
/// 1-based entry index for HAR.
#[derive(Debug, Clone, PartialEq)]
pub struct RawEvent {
    pub at: DateTime<Utc>,
    pub line: usize,
    /// Uppercased.
    pub method: String,
    /// No scheme, host, query or fragment; starts with `/`.
    pub path: String,
    pub operation: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Trace {
    /// Stable-sorted by `at`.
    pub events: Vec<RawEvent>,
    pub malformed: u64,
    /// At most three, formatted `line {n}: {reason}`.
    pub malformed_examples: Vec<String>,
    /// Events removed by the replay window (0 without one).
    pub filtered: u64,
}

impl Trace {
    fn reject(&mut self, line: usize, reason: &str) {
        self.malformed += 1;
        if self.malformed_examples.len() < MAX_MALFORMED_EXAMPLES {
            self.malformed_examples
                .push(format!("line {line}: {reason}"));
        }
    }

    fn finish(mut self) -> Self {
        self.events.sort_by_key(|e| e.at); // stable: ties keep log order
        self
    }
}

/// Read `cfg.source` in `cfg.format` and apply `cfg.window`.
pub fn read_trace(cfg: &ReplayConfig) -> Result<Trace, ReplayError> {
    let io_err = |source| ReplayError::Io {
        path: cfg.source.clone().into(),
        source,
    };
    // JSONL is read as bytes so one invalid-UTF-8 line is a malformed line,
    // not a fatal whole-file error. HAR is a single JSON document.
    let mut trace = if cfg.format == "har" {
        let text = std::fs::read_to_string(&cfg.source).map_err(io_err)?;
        parse_har(&text)?
    } else {
        let bytes = std::fs::read(&cfg.source).map_err(io_err)?;
        parse_jsonl_bytes(&bytes, &cfg.fields)
    };
    if let Some(w) = &cfg.window {
        apply_window(&mut trace, w)?;
    }
    Ok(trace)
}

/// One JSON object per line; blank lines are skipped and not counted. A line
/// that is not valid UTF-8 is counted malformed rather than aborting the file.
pub fn parse_jsonl_bytes(bytes: &[u8], fields: &ReplayFields) -> Trace {
    let mut trace = Trace::default();
    for (i, raw) in bytes.split(|&b| b == b'\n').enumerate() {
        let line = i + 1;
        // Tolerate CRLF by dropping a single trailing '\r'.
        let raw = match raw.split_last() {
            Some((b'\r', head)) => head,
            _ => raw,
        };
        match std::str::from_utf8(raw) {
            Ok(s) if s.trim().is_empty() => continue,
            Ok(s) => match jsonl_event(s, line, fields) {
                Ok(ev) => trace.events.push(ev),
                Err(reason) => trace.reject(line, &reason),
            },
            Err(_) => trace.reject(line, "invalid UTF-8"),
        }
    }
    trace.finish()
}

/// One JSON object per line; blank lines are skipped and not counted.
pub fn parse_jsonl(text: &str, fields: &ReplayFields) -> Trace {
    parse_jsonl_bytes(text.as_bytes(), fields)
}

fn jsonl_event(raw: &str, line: usize, fields: &ReplayFields) -> Result<RawEvent, String> {
    let doc: Value = serde_json::from_str(raw).map_err(|e| format!("invalid JSON: {e}"))?;
    if !doc.is_object() {
        return Err("not a JSON object".into());
    }
    let ts = first_value(&doc, &fields.ts)?.ok_or_else(|| missing(&fields.ts))?;
    let method = required_str(&doc, &fields.method)?;
    let path = required_str(&doc, &fields.path)?;
    // A malformed operation path is a malformed line; a missing or
    // non-string value is simply "no operation".
    let operation =
        first_value(&doc, &fields.operation)?.and_then(|v| v.as_str().map(str::to_owned));
    Ok(RawEvent {
        at: parse_ts(&ts)?,
        line,
        method: method.to_ascii_uppercase(),
        path: normalize_path(&path)?,
        operation,
    })
}

/// HAR 1.2: `log.entries[]`. A document without that array is an error; a
/// bad entry is only malformed.
pub fn parse_har(text: &str) -> Result<Trace, ReplayError> {
    let doc: Value = serde_json::from_str(text).map_err(|e| ReplayError::Har(e.to_string()))?;
    let entries = doc
        .pointer("/log/entries")
        .and_then(Value::as_array)
        .ok_or_else(|| ReplayError::Har("missing log.entries array".into()))?;
    let mut trace = Trace::default();
    for (i, entry) in entries.iter().enumerate() {
        let index = i + 1;
        match har_event(entry, index) {
            Ok(ev) => trace.events.push(ev),
            Err(reason) => trace.reject(index, &reason),
        }
    }
    Ok(trace.finish())
}

fn har_event(entry: &Value, index: usize) -> Result<RawEvent, String> {
    let started = entry
        .get("startedDateTime")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("startedDateTime"))?;
    let method = entry
        .pointer("/request/method")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("request.method"))?;
    let url = entry
        .pointer("/request/url")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("request.url"))?;
    // GraphQL clients name the operation in the POST body.
    let operation = entry
        .pointer("/request/postData/text")
        .and_then(Value::as_str)
        .and_then(|t| serde_json::from_str::<Value>(t).ok())
        .and_then(|body| body.get("operationName")?.as_str().map(str::to_owned));
    Ok(RawEvent {
        at: parse_rfc3339(started)?,
        line: index,
        method: method.to_ascii_uppercase(),
        path: normalize_path(url)?,
        operation,
    })
}

/// Keep events in the half-open `[start, end)` window; count the rest.
fn apply_window(trace: &mut Trace, w: &WindowConfig) -> Result<(), ReplayError> {
    let bound = |s: &str| parse_rfc3339(s).map_err(ReplayError::Window);
    let (start, end) = (bound(&w.start)?, bound(&w.end)?);
    let before = trace.events.len();
    trace.events.retain(|e| start <= e.at && e.at < end);
    trace.filtered = (before - trace.events.len()) as u64;
    Ok(())
}

fn missing(field: &str) -> String {
    format!("missing field {field:?}")
}

/// The first value at a dot path; a path walk error means a malformed line.
fn first_value(doc: &Value, path: &str) -> Result<Option<Value>, String> {
    let vals = extract_path(doc, path).map_err(|e| e.to_string())?;
    Ok(vals.into_iter().next())
}

fn required_str(doc: &Value, path: &str) -> Result<String, String> {
    match first_value(doc, path)? {
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(format!("field {path:?} is not a string")),
        None => Err(missing(path)),
    }
}

/// A string is RFC3339; a number is an epoch whose unit is inferred from its
/// magnitude: `>= 1e17` → nanoseconds, `>= 1e14` → microseconds, `>= 1e11` →
/// milliseconds, else seconds (fractions allowed).
fn parse_ts(v: &Value) -> Result<DateTime<Utc>, String> {
    match v {
        Value::String(s) => parse_rfc3339(s),
        Value::Number(n) => {
            let f = n
                .as_f64()
                .filter(|f| f.is_finite())
                .ok_or_else(|| format!("{n} is not a valid epoch timestamp"))?;
            // Nanoseconds-per-unit, chosen by magnitude so ms/µs/ns logs are
            // not mistaken for seconds (which stretches a run for decades).
            let per_unit: i128 = match f.abs() {
                a if a >= 1e17 => 1,
                a if a >= 1e14 => 1_000,
                a if a >= 1e11 => 1_000_000,
                _ => 1_000_000_000,
            };
            // Integer timestamps convert exactly (f64 loses precision past 2^53,
            // which matters at nanosecond scale); fractional ones use the float.
            let total_nanos: i128 = match n.as_i64() {
                Some(i) => i as i128 * per_unit,
                None => (f * per_unit as f64).round() as i128,
            };
            let secs = i64::try_from(total_nanos.div_euclid(1_000_000_000))
                .map_err(|_| format!("{n} is out of range for an epoch timestamp"))?;
            let nanos = total_nanos.rem_euclid(1_000_000_000) as u32;
            DateTime::from_timestamp(secs, nanos)
                .ok_or_else(|| format!("{n} is out of range for an epoch timestamp"))
        }
        _ => Err("timestamp is neither a string nor a number".into()),
    }
}

/// Strip `http(s)://host`, then cut at the first `?` or `#`.
fn normalize_path(raw: &str) -> Result<String, String> {
    let rest = ["https://", "http://"]
        .iter()
        .find_map(|scheme| raw.strip_prefix(scheme))
        .map_or(raw, |after| {
            after.find(['/', '?', '#']).map_or("", |i| &after[i..])
        });
    let path = rest.find(['?', '#']).map_or(rest, |i| &rest[..i]);
    if path.starts_with('/') {
        Ok(path.to_owned())
    } else {
        // Strip the query and fragment from the reported value: they can carry
        // secrets and would otherwise land in malformed_examples and the manifest.
        let shown = raw.find(['?', '#']).map_or(raw, |i| &raw[..i]);
        Err(format!("path {shown:?} does not start with '/'"))
    }
}

#[cfg(test)]
mod tests {
    use super::super::ReplayError;
    use super::*;
    use crate::config::{ReplayConfig, ReplayFields, WindowConfig};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/replay")
            .join(name)
    }

    fn default_fields() -> ReplayFields {
        ReplayFields {
            ts: "ts".into(),
            method: "method".into(),
            path: "path".into(),
            operation: "operation".into(),
        }
    }

    fn cfg(name: &str, format: &str) -> ReplayConfig {
        ReplayConfig {
            source: fixture(name).to_string_lossy().into_owned(),
            format: format.into(),
            fields: default_fields(),
            ..ReplayConfig::default()
        }
    }

    fn offsets(t: &Trace) -> Vec<Duration> {
        let first = t.events[0].at;
        t.events
            .iter()
            .map(|e| (e.at - first).to_std().unwrap())
            .collect()
    }

    #[test]
    fn jsonl_reads_events_in_time_order() {
        let t = read_trace(&cfg("basic.jsonl", "json")).unwrap();
        let ops: Vec<_> = t
            .events
            .iter()
            .map(|e| e.operation.as_deref().unwrap())
            .collect();
        assert_eq!(ops, ["a", "b", "c", "d"]);
        assert_eq!(
            offsets(&t),
            [
                Duration::ZERO,
                Duration::from_millis(250),
                Duration::from_secs(1),
                Duration::from_millis(1500)
            ]
        );
        assert_eq!(t.malformed, 0);
        assert_eq!(t.filtered, 0);
        // Uppercased method, normalized path, physical line numbers.
        assert_eq!(t.events[2].method, "GET");
        assert_eq!(t.events[2].path, "/skus/2");
        assert_eq!(t.events[3].path, "/orders");
        assert_eq!(t.events[3].line, 4);
    }

    #[test]
    fn jsonl_epoch_seconds_timestamps() {
        let text = "{\"ts\":1760968800.5,\"method\":\"GET\",\"path\":\"/a\"}\n\
                    {\"ts\":1760968801,\"method\":\"GET\",\"path\":\"/b\"}\n";
        let t = parse_jsonl(text, &default_fields());
        assert_eq!(t.malformed, 0);
        assert_eq!(offsets(&t), [Duration::ZERO, Duration::from_millis(500)]);
    }

    #[test]
    fn jsonl_epoch_unit_inferred_by_magnitude() {
        // The same instant as epoch seconds, millis, micros, and nanos.
        let text = "{\"ts\":1760968800,\"method\":\"GET\",\"path\":\"/s\"}\n\
                    {\"ts\":1760968800000,\"method\":\"GET\",\"path\":\"/ms\"}\n\
                    {\"ts\":1760968800000000,\"method\":\"GET\",\"path\":\"/us\"}\n\
                    {\"ts\":1760968800000000000,\"method\":\"GET\",\"path\":\"/ns\"}\n";
        let t = parse_jsonl(text, &default_fields());
        assert_eq!(t.malformed, 0);
        let ats: Vec<_> = t.events.iter().map(|e| e.at).collect();
        assert!(ats.windows(2).all(|w| w[0] == w[1]), "ats differ: {ats:?}");
        assert_eq!(ats[0].timestamp(), 1760968800);
        assert_eq!(ats[0].timestamp_subsec_nanos(), 0);
    }

    #[test]
    fn jsonl_epoch_millis_sub_second() {
        // Millis are read as millis (250ms apart), not as seconds.
        let text = "{\"ts\":1760968800000,\"method\":\"GET\",\"path\":\"/a\"}\n\
                    {\"ts\":1760968800250,\"method\":\"GET\",\"path\":\"/b\"}\n";
        let t = parse_jsonl(text, &default_fields());
        assert_eq!(t.malformed, 0);
        assert_eq!(offsets(&t), [Duration::ZERO, Duration::from_millis(250)]);
    }

    #[test]
    fn jsonl_epoch_nanos_preserve_sub_second() {
        let text = "{\"ts\":1760968800000000000,\"method\":\"GET\",\"path\":\"/a\"}\n\
                    {\"ts\":1760968800123456789,\"method\":\"GET\",\"path\":\"/b\"}\n";
        let t = parse_jsonl(text, &default_fields());
        assert_eq!(t.events[1].at.timestamp_subsec_nanos(), 123456789);
    }

    #[test]
    fn jsonl_equal_timestamps_keep_file_order() {
        // Determinism: stable sort leaves events with the same ts in file order.
        // 60 tied events interleaved with out-of-order earlier ones: enough
        // that an unstable sort (insertion sort only below ~20) would scramble
        // the ties.
        let mut text = String::new();
        for i in 0..60 {
            text.push_str(&format!(
                "{{\"ts\":5,\"method\":\"GET\",\"path\":\"/t\",\"operation\":\"tie{i:02}\"}}\n"
            ));
            if i % 3 == 0 {
                text.push_str(&format!(
                    "{{\"ts\":{},\"method\":\"GET\",\"path\":\"/e\",\"operation\":\"early\"}}\n",
                    i % 4
                ));
            }
        }
        let t = parse_jsonl(&text, &default_fields());
        let ties: Vec<_> = t
            .events
            .iter()
            .filter_map(|e| e.operation.as_deref().filter(|o| o.starts_with("tie")))
            .collect();
        let want: Vec<String> = (0..60).map(|i| format!("tie{i:02}")).collect();
        assert_eq!(ties, want);
    }

    #[test]
    fn jsonl_invalid_utf8_line_is_malformed_not_fatal() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"{\"ts\":1,\"method\":\"GET\",\"path\":\"/a\"}\n");
        bytes.extend_from_slice(&[0xff, 0xfe]); // invalid UTF-8 on line 2
        bytes.push(b'\n');
        bytes.extend_from_slice(b"{\"ts\":2,\"method\":\"GET\",\"path\":\"/b\"}\n");
        let t = parse_jsonl_bytes(&bytes, &default_fields());
        assert_eq!(t.events.len(), 2);
        assert_eq!(t.malformed, 1);
        assert!(t.malformed_examples[0].starts_with("line 2:"), "{:?}", t.malformed_examples);
        assert!(
            t.malformed_examples[0].contains("invalid UTF-8"),
            "{:?}",
            t.malformed_examples
        );
    }

    #[test]
    fn normalize_path_error_strips_query_and_fragment() {
        // A secret in the query string must not leak into the malformed reason.
        let err = normalize_path("notapath?token=s3cret#frag").unwrap_err();
        assert!(!err.contains("s3cret"), "secret leaked: {err}");
        assert!(!err.contains("frag"), "fragment leaked: {err}");
        assert!(err.contains("notapath"), "{err}");
    }

    #[test]
    fn jsonl_skips_malformed_lines_and_counts_them() {
        let valid = "{\"ts\":\"2025-10-20T14:00:00Z\",\"method\":\"GET\",\"path\":\"/a\"}";
        let text = format!("not json\n\n{{\"method\":\"GET\",\"path\":\"/a\"}}\n{valid}");
        let t = parse_jsonl(&text, &default_fields());
        assert_eq!(t.malformed, 2);
        assert_eq!(t.events.len(), 1);
        assert!(t.malformed_examples[0].starts_with("line 1:"));
        assert!(t.malformed_examples[1].starts_with("line 3:"));
    }

    #[test]
    fn jsonl_caps_malformed_examples_at_three() {
        let t = parse_jsonl("x\nx\nx\nx\nx\n", &default_fields());
        assert_eq!(t.malformed, 5);
        assert_eq!(t.malformed_examples.len(), 3);
    }

    #[test]
    fn jsonl_rejects_non_object_and_bad_path_and_bad_ts() {
        let text = "[1]\n\
                    {\"ts\":\"2025-10-20T14:00:00Z\",\"method\":\"GET\",\"path\":\"nope\"}\n\
                    {\"ts\":\"yesterday\",\"method\":\"GET\",\"path\":\"/a\"}\n\
                    {\"ts\":\"2025-10-20T14:00:00Z\",\"method\":1,\"path\":\"/a\"}\n";
        let t = parse_jsonl(text, &default_fields());
        assert_eq!(t.malformed, 4);
        assert!(t.events.is_empty());
    }

    #[test]
    fn jsonl_non_string_operation_is_none() {
        let text = "{\"ts\":1,\"method\":\"GET\",\"path\":\"/a\",\"operation\":7}\n\
                    {\"ts\":2,\"method\":\"GET\",\"path\":\"/a\",\"operation\":null}\n\
                    {\"ts\":3,\"method\":\"GET\",\"path\":\"/a\"}\n";
        let t = parse_jsonl(text, &default_fields());
        assert_eq!(t.malformed, 0);
        assert!(t.events.iter().all(|e| e.operation.is_none()));
    }

    #[test]
    fn jsonl_custom_field_paths() {
        let fields = ReplayFields {
            ts: "time".into(),
            method: "req.method".into(),
            path: "req.url".into(),
            operation: "gql.op".into(),
        };
        let text = "{\"time\":\"2025-10-20T14:00:00Z\",\"req\":{\"method\":\"post\",\"url\":\"/graphql\"},\"gql\":{\"op\":\"getCountry\"}}";
        let t = parse_jsonl(text, &fields);
        assert_eq!(t.malformed, 0);
        let e = &t.events[0];
        assert_eq!(e.method, "POST");
        assert_eq!(e.path, "/graphql");
        assert_eq!(e.operation.as_deref(), Some("getCountry"));
    }

    #[test]
    fn path_is_normalized() {
        assert_eq!(
            normalize_path("https://api.x/skus/1?x=y#f").unwrap(),
            "/skus/1"
        );
        assert_eq!(normalize_path("http://api.x:8080/a").unwrap(), "/a");
        assert_eq!(normalize_path("/a/b?c").unwrap(), "/a/b");
        assert_eq!(normalize_path("/a#frag").unwrap(), "/a");
        assert!(normalize_path("skus/1").is_err());
        assert!(normalize_path("https://api.x").is_err());
        assert!(normalize_path("https://api.x?y=1").is_err());
    }

    #[test]
    fn window_filters_half_open() {
        let text = "{\"ts\":\"2025-10-20T14:00:00Z\",\"method\":\"GET\",\"path\":\"/a\"}\n\
                    {\"ts\":\"2025-10-20T14:00:05Z\",\"method\":\"GET\",\"path\":\"/b\"}\n\
                    {\"ts\":\"2025-10-20T14:00:10Z\",\"method\":\"GET\",\"path\":\"/c\"}\n";
        let mut t = parse_jsonl(text, &default_fields());
        let w = WindowConfig {
            start: "2025-10-20T14:00:00Z".into(),
            end: "2025-10-20T14:00:10Z".into(),
        };
        apply_window(&mut t, &w).unwrap();
        let paths: Vec<_> = t.events.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/a", "/b"]);
        assert_eq!(t.filtered, 1);
    }

    #[test]
    fn read_trace_applies_window_from_config() {
        let mut c = cfg("basic.jsonl", "json");
        c.window = Some(WindowConfig {
            start: "2025-10-20T14:00:00.250Z".into(),
            end: "2025-10-20T14:00:01.500Z".into(),
        });
        let t = read_trace(&c).unwrap();
        assert_eq!(t.events.len(), 2); // b (start, inclusive) and c; d is at end
        assert_eq!(t.filtered, 2);
    }

    #[test]
    fn read_trace_rejects_bad_window() {
        let mut c = cfg("basic.jsonl", "json");
        c.window = Some(WindowConfig {
            start: "soon".into(),
            end: "2025-10-20T14:00:01Z".into(),
        });
        assert!(matches!(read_trace(&c), Err(ReplayError::Window(_))));
    }

    #[test]
    fn read_trace_missing_file_is_io_error() {
        let c = cfg("does-not-exist.jsonl", "json");
        let e = read_trace(&c).unwrap_err();
        assert!(matches!(e, ReplayError::Io { .. }));
        assert!(e.to_string().starts_with("replay: read "));
        // The io source is carried by #[source] (shown once by the anyhow
        // chain), not duplicated into the Display message.
        assert!(!e.to_string().contains("os error"), "source leaked: {e}");
    }

    #[test]
    fn har_reads_entries_and_graphql_operation() {
        let t = read_trace(&cfg("basic.har", "har")).unwrap();
        assert_eq!(t.events.len(), 2);
        assert_eq!(t.malformed, 0);
        let (get, post) = (&t.events[0], &t.events[1]);
        assert_eq!((get.method.as_str(), get.path.as_str()), ("GET", "/skus/1"));
        assert_eq!(get.operation, None);
        assert_eq!(get.line, 1);
        assert_eq!(post.method, "POST");
        assert_eq!(post.path, "/graphql");
        assert_eq!(post.operation.as_deref(), Some("getCountry"));
        assert_eq!(post.line, 2);
        assert_eq!(offsets(&t)[1], Duration::from_millis(500));
    }

    #[test]
    fn har_bad_entry_is_malformed() {
        let text = r#"{"log":{"entries":[
            {"startedDateTime":"nope","request":{"method":"GET","url":"/a"}},
            {"startedDateTime":"2025-10-20T14:00:00Z","request":{"method":"GET","url":"/a"}}
        ]}}"#;
        let t = parse_har(text).unwrap();
        assert_eq!(t.malformed, 1);
        assert_eq!(t.events.len(), 1);
        assert!(t.malformed_examples[0].starts_with("line 1:"));
    }

    #[test]
    fn har_invalid_document_errors() {
        assert!(matches!(parse_har("{}"), Err(ReplayError::Har(_))));
        assert!(matches!(parse_har("not json"), Err(ReplayError::Har(_))));
        assert!(matches!(
            parse_har(r#"{"log":{"entries":{}}}"#),
            Err(ReplayError::Har(_))
        ));
    }
}
