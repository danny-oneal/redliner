//! Data pools, `{{placeholder}}` templating, and JSON-path extraction
//! (port of the Go `data.go`).

use std::collections::HashMap;

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
}

impl Pool {
    /// Draws one value. Hot path: once per placeholder per request.
    pub fn sample(&self, rng: &mut ChaCha8Rng) -> Value {
        match self {
            Pool::List(values) if values.is_empty() => Value::Null,
            Pool::List(values) => values[rng.random_range(0..values.len())].clone(),
            Pool::Range { min, max } => Value::from(rng.random_range(*min..=*max)),
        }
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
/// embedded values are stringified.
pub fn render_string(s: &str, pools: &Pools, rng: &mut ChaCha8Rng) -> Result<String, EncodeError> {
    let mut s = s.to_string();
    loop {
        let Some(start) = s.find("{{") else {
            return Ok(s);
        };
        let Some(end) = s[start..].find("}}") else {
            return Err(EncodeError::BadTemplate(format!(
                "unterminated {{{{ in {s:?}"
            )));
        };
        let name = s[start + 2..start + end].trim();
        let pool = pools
            .get(name)
            .ok_or_else(|| EncodeError::UnknownPool(name.to_string()))?;
        let rendered = stringify(&pool.sample(rng));
        s = format!("{}{}{}", &s[..start], rendered, &s[start + end + 2..]);
    }
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
