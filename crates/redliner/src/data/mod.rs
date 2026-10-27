//! Data pools, `{{placeholder}}` templating, and JSON-path extraction
//! (port of the Go `data.go`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::Rng;
use rand_chacha::ChaCha8Rng;
use serde_json::Value;

use crate::config::DataConfig;
use crate::encode::EncodeError;

pub mod harvest;

/// Build-time failures while resolving pools or extracting values.
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("line {0:?} is not an int")]
    NotInt(String),
    #[error("no values in {0}")]
    EmptyFile(String),
    #[error("path {path:?}: {msg}")]
    Path { path: String, msg: String },
    #[error("{0}")]
    Encode(#[from] EncodeError),
    #[error("{0} — is the server up?")]
    Http(String),
    #[error("status {code} from {url}")]
    Status { code: u16, url: String },
    #[error("decode harvest response: {0}")]
    Decode(String),
    #[error("harvest query returned GraphQL errors: {0}")]
    GraphqlErrors(String),
    #[error("pool {pool:?}: path {path:?} matched nothing — schema mismatch?")]
    EmptyMatch { pool: String, path: String },
    #[error("pool {name:?}: {source}")]
    Pool {
        name: String,
        #[source]
        source: Box<DataError>,
    },
}

/// A named source of values that scenarios draw from at attack time
/// (`{{pool_name}}` placeholders). `List` is also the variant harvest appends to.
#[derive(Debug, Clone)]
pub enum Pool {
    List(Vec<Value>),
    Range { min: i64, max: i64 },
    /// A random (v4) UUID per draw, from the seeded RNG.
    Uuid,
    /// The next integer per draw. Clones share the counter.
    Sequence(Arc<AtomicI64>),
    /// The wall-clock time of the draw.
    Timestamp(TimeFormat),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFormat {
    /// `2026-01-02T03:04:05.678Z`
    Rfc3339,
    Unix,
    UnixMs,
}

impl Pool {
    /// Draws one value. Hot path: once per placeholder per request.
    pub fn sample(&self, rng: &mut ChaCha8Rng) -> Value {
        match self {
            Pool::List(values) if values.is_empty() => Value::Null,
            Pool::List(values) => values[rng.random_range(0..values.len())].clone(),
            Pool::Range { min, max } => Value::from(rng.random_range(*min..=*max)),
            Pool::Uuid => Value::String(uuid_v4(rng)),
            Pool::Sequence(next) => Value::from(next.fetch_add(1, Ordering::Relaxed)),
            Pool::Timestamp(format) => timestamp(*format, SystemTime::now()),
        }
    }
}

/// RFC 9562 version-4 layout: random bits with the version and variant set.
fn uuid_v4(rng: &mut ChaCha8Rng) -> String {
    let mut b: [u8; 16] = rng.random();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

fn timestamp(format: TimeFormat, now: SystemTime) -> Value {
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    match format {
        TimeFormat::Unix => Value::from(since.as_secs()),
        TimeFormat::UnixMs => Value::from(u64::try_from(since.as_millis()).unwrap_or(u64::MAX)),
        TimeFormat::Rfc3339 => Value::String(
            chrono::DateTime::<chrono::Utc>::from(now)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
    }
}

/// The resolved registry, built once at startup and extended by harvest.
pub type Pools = HashMap<String, Pool>;

pub fn build_pools(cfg: &DataConfig) -> Result<Pools, DataError> {
    let mut pools = Pools::new();
    for (name, pc) in &cfg.pools {
        match pc.pool_type.as_str() {
            "list" => {
                let vals = pc.values.iter().map(normalize_scalar).collect();
                pools.insert(name.clone(), Pool::List(vals));
            }
            "range" => {
                pools.insert(
                    name.clone(),
                    Pool::Range {
                        min: pc.min,
                        max: pc.max,
                    },
                );
            }
            "file" => {
                let vals = read_pool_file(&pc.path, &pc.as_type).map_err(|e| DataError::Pool {
                    name: name.clone(),
                    source: Box::new(e),
                })?;
                pools.insert(name.clone(), Pool::List(vals));
            }
            "uuid" => {
                pools.insert(name.clone(), Pool::Uuid);
            }
            "sequence" => {
                let start = AtomicI64::new(pc.start.unwrap_or(1));
                pools.insert(name.clone(), Pool::Sequence(Arc::new(start)));
            }
            "timestamp" => {
                let format = match pc.as_type.as_str() {
                    "unix" => TimeFormat::Unix,
                    "unix_ms" => TimeFormat::UnixMs,
                    _ => TimeFormat::Rfc3339,
                };
                pools.insert(name.clone(), Pool::Timestamp(format));
            }
            _ => {}
        }
    }
    Ok(pools)
}

fn read_pool_file(path: &str, as_type: &str) -> Result<Vec<Value>, DataError> {
    let text = std::fs::read_to_string(path)?;
    let mut vals = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if as_type == "int" {
            let n: i64 = line
                .parse()
                .map_err(|_| DataError::NotInt(line.to_string()))?;
            vals.push(Value::from(n));
        } else {
            vals.push(Value::String(line.to_string()));
        }
    }
    if vals.is_empty() {
        return Err(DataError::EmptyFile(path.to_string()));
    }
    Ok(vals)
}

/// Collapses integral floats to integers so int-ish values stay ints
/// (GraphQL `Int!` variables must not be sent as `5.0`).
pub fn normalize_scalar(v: &Value) -> Value {
    if let Value::Number(n) = v {
        if n.is_f64() {
            if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f.abs() < 9.0e18 {
                    return Value::from(f as i64);
                }
            }
        }
    }
    v.clone()
}

/// Walks decoded JSON along a dot path where a segment suffixed `[]` fans out
/// over array elements (`data.menus[].items[].id`), returning every leaf.
pub fn extract_path(doc: &Value, path: &str) -> Result<Vec<Value>, DataError> {
    let err = |msg: String| DataError::Path {
        path: path.to_string(),
        msg,
    };
    let mut cur: Vec<Value> = vec![doc.clone()];
    for seg in path.split('.') {
        let fan_out = seg.ends_with("[]");
        let key = seg.strip_suffix("[]").unwrap_or(seg);
        let mut next = Vec::new();
        for node in &cur {
            let Value::Object(m) = node else {
                return Err(err(format!(
                    "segment {seg:?}: expected object, got {}",
                    kind(node)
                )));
            };
            let val = match m.get(key) {
                None | Some(Value::Null) => continue, // tolerate sparse data
                Some(v) => v,
            };
            if fan_out {
                let Value::Array(arr) = val else {
                    return Err(err(format!(
                        "segment {seg:?}: expected array, got {}",
                        kind(val)
                    )));
                };
                next.extend(arr.iter().cloned());
            } else {
                next.push(val.clone());
            }
        }
        cur = next;
    }
    Ok(cur.iter().map(normalize_scalar).collect())
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Substitutes every `{{pool}}` placeholder in `s` with a fresh sample;
/// embedded values are stringified. Sampled values are inserted verbatim and
/// never rescanned, so a value containing `{{` is data, not a placeholder.
pub fn render_string(s: &str, pools: &Pools, rng: &mut ChaCha8Rng) -> Result<String, EncodeError> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start..].find("}}") else {
            return Err(EncodeError::BadTemplate(format!(
                "unterminated {{{{ in {s:?}"
            )));
        };
        let name = rest[start + 2..start + end].trim();
        let pool = pools
            .get(name)
            .ok_or_else(|| EncodeError::UnknownPool(name.to_string()))?;
        out.push_str(&rest[..start]);
        out.push_str(&stringify(&pool.sample(rng)));
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Substitutes placeholders inside an arbitrary JSON value (GraphQL
/// variables). A string that is exactly one placeholder keeps the sampled
/// value's native type; anything else falls back to string interpolation.
pub fn render_value(v: &Value, pools: &Pools, rng: &mut ChaCha8Rng) -> Result<Value, EncodeError> {
    match v {
        Value::String(t) => {
            let trimmed = t.trim();
            if trimmed.starts_with("{{") && trimmed.ends_with("}}") {
                if let Some(inner) = trimmed.get(2..trimmed.len() - 2) {
                    let inner = inner.trim();
                    if !inner.contains("{{") && !inner.contains("}}") {
                        let pool = pools
                            .get(inner)
                            .ok_or_else(|| EncodeError::UnknownPool(inner.to_string()))?;
                        return Ok(pool.sample(rng));
                    }
                }
            }
            render_string(t, pools, rng).map(Value::String)
        }
        Value::Object(m) => m
            .iter()
            .map(|(k, val)| Ok((k.clone(), render_value(val, pools, rng)?)))
            .collect::<Result<serde_json::Map<_, _>, EncodeError>>()
            .map(Value::Object),
        Value::Array(a) => a
            .iter()
            .map(|val| render_value(val, pools, rng))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(normalize_scalar(other)),
    }
}

fn stringify(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DataConfig, PoolConfig};
    use crate::encode::EncodeError;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use serde_json::json;

    fn rng() -> ChaCha8Rng {
        ChaCha8Rng::seed_from_u64(1)
    }

    fn ids() -> Pools {
        let mut p = Pools::new();
        p.insert("ids".into(), Pool::List(vec![json!(5)]));
        p
    }

    #[test]
    fn render_value_keeps_native_type() {
        let p = ids();
        let out = render_value(&json!({"id":"{{ids}}"}), &p, &mut rng()).unwrap();
        assert_eq!(out, json!({"id":5}));
        let out = render_value(&json!({"p":"/u/{{ids}}/x"}), &p, &mut rng()).unwrap();
        assert_eq!(out, json!({"p":"/u/5/x"}));
    }

    #[test]
    fn render_string_unknown_pool() {
        let e = render_string("{{nope}}", &Pools::new(), &mut rng()).unwrap_err();
        assert!(matches!(e, EncodeError::UnknownPool(ref n) if n == "nope"));
    }

    #[test]
    fn render_string_unterminated() {
        let e = render_string("a {{ids", &ids(), &mut rng()).unwrap_err();
        assert!(matches!(e, EncodeError::BadTemplate(_)));
    }

    fn named(value: &str) -> Pools {
        let mut p = Pools::new();
        p.insert("name".into(), Pool::List(vec![json!(value)]));
        p
    }

    #[test]
    fn render_string_does_not_reparse_sampled_placeholder() {
        let out = render_string("/u/{{name}}", &named("{{other}}"), &mut rng()).unwrap();
        assert_eq!(out, "/u/{{other}}");
    }

    #[test]
    fn render_string_does_not_reparse_sampled_braces() {
        let out = render_string("/u/{{name}}/x", &named("a{{b"), &mut rng()).unwrap();
        assert_eq!(out, "/u/a{{b/x");
    }

    #[test]
    fn render_string_fills_every_placeholder() {
        let out = render_string("{{ids}}-{{ ids }}/{{ids}}", &ids(), &mut rng()).unwrap();
        assert_eq!(out, "5-5/5");
    }

    #[test]
    fn range_pool_bounds() {
        let p = Pool::Range { min: 1, max: 3 };
        let mut r = rng();
        for _ in 0..200 {
            let v = p.sample(&mut r);
            let n = v.as_i64().expect("integer value");
            assert!((1..=3).contains(&n));
        }
    }

    fn pool(cfg: PoolConfig) -> Pool {
        let mut data = DataConfig::default();
        data.pools.insert("p".into(), cfg);
        build_pools(&data).unwrap().remove("p").expect("pool built")
    }

    fn typed(pool_type: &str) -> PoolConfig {
        PoolConfig {
            pool_type: pool_type.into(),
            ..Default::default()
        }
    }

    #[test]
    fn uuid_pool_yields_distinct_v4_uuids() {
        let p = pool(typed("uuid"));
        let mut r = rng();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let v = p.sample(&mut r);
            let s = v.as_str().expect("uuid is a string");
            assert_eq!(s.len(), 36, "{s}");
            for (i, c) in s.char_indices() {
                match i {
                    8 | 13 | 18 | 23 => assert_eq!(c, '-', "{s}"),
                    14 => assert_eq!(c, '4', "version nibble: {s}"),
                    19 => assert!("89ab".contains(c), "variant nibble: {s}"),
                    _ => assert!(c.is_ascii_hexdigit() && !c.is_ascii_uppercase(), "{s}"),
                }
            }
            assert!(seen.insert(s.to_string()), "duplicate {s}");
        }
    }

    #[test]
    fn uuid_pool_is_reproducible_from_the_seed() {
        let p = pool(typed("uuid"));
        let draw = |seed| {
            let mut r = ChaCha8Rng::seed_from_u64(seed);
            (0..5).map(|_| p.sample(&mut r)).collect::<Vec<_>>()
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));
    }

    #[test]
    fn sequence_pool_counts_up_from_start() {
        let p = pool(PoolConfig {
            start: Some(10),
            ..typed("sequence")
        });
        let mut r = rng();
        let got: Vec<Value> = (0..3).map(|_| p.sample(&mut r)).collect();
        assert_eq!(got, vec![json!(10), json!(11), json!(12)]);
        assert!(got[0].is_i64(), "native integer");
    }

    #[test]
    fn sequence_pool_starts_at_one_by_default() {
        let p = pool(typed("sequence"));
        assert_eq!(p.sample(&mut rng()), json!(1));
        assert_eq!(p.sample(&mut rng()), json!(2));
    }

    #[test]
    fn every_placeholder_draws_the_next_sequence_value() {
        let mut pools = Pools::new();
        pools.insert("n".into(), pool(typed("sequence")));
        let out = render_string("{{n}}-{{n}}", &pools, &mut rng()).unwrap();
        assert_eq!(out, "1-2");
        let out = render_value(&json!({"n": "{{n}}"}), &pools, &mut rng()).unwrap();
        assert_eq!(out, json!({"n": 3}));
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    #[test]
    fn timestamp_pool_unix_and_unix_ms() {
        let secs = pool(PoolConfig {
            as_type: "unix".into(),
            ..typed("timestamp")
        });
        let ms = pool(PoolConfig {
            as_type: "unix_ms".into(),
            ..typed("timestamp")
        });
        let before = now_ms();
        let s = secs.sample(&mut rng()).as_i64().expect("unix is an integer");
        let m = ms.sample(&mut rng()).as_i64().expect("unix_ms is an integer");
        let after = now_ms();
        assert!((before / 1000..=after / 1000).contains(&s), "{s}");
        assert!((before..=after).contains(&m), "{m}");
    }

    #[test]
    fn timestamp_pool_defaults_to_rfc3339_millis() {
        let p = pool(typed("timestamp"));
        let before = now_ms();
        let v = p.sample(&mut rng());
        let after = now_ms();
        let s = v.as_str().expect("rfc3339 is a string");
        assert_eq!(s.len(), "2026-01-02T03:04:05.678Z".len(), "{s}");
        assert!(s.ends_with('Z'), "{s}");
        let t = crate::config::parse_rfc3339(s).unwrap().timestamp_millis();
        assert!((before..=after).contains(&t), "{s}");
    }

    #[test]
    fn extract_path_fans_out() {
        let doc = json!({"data":{"items":[{"id":1},{"id":2}]}});
        assert_eq!(
            extract_path(&doc, "data.items[].id").unwrap(),
            vec![json!(1), json!(2)]
        );
    }

    #[test]
    fn extract_path_normalizes_integral_float() {
        let got = extract_path(&json!({"id":5.0}), "id").unwrap();
        assert_eq!(got, vec![json!(5)]);
        assert!(got[0].is_i64());
    }

    #[test]
    fn build_pools_file_as_int() {
        let path = std::env::temp_dir().join(format!("rl-pool-{}.txt", std::process::id()));
        std::fs::write(&path, "1\n# c\n\n2\n").unwrap();
        let mut cfg = DataConfig::default();
        cfg.pools.insert(
            "f".into(),
            PoolConfig {
                pool_type: "file".into(),
                path: path.to_string_lossy().into_owned(),
                as_type: "int".into(),
                ..Default::default()
            },
        );
        let pools = build_pools(&cfg).unwrap();
        std::fs::remove_file(&path).ok();
        match &pools["f"] {
            Pool::List(v) => assert_eq!(v, &vec![json!(1), json!(2)]),
            _ => panic!("expected list pool"),
        }
    }
}
