//! Reading Swagger 2.0 and OpenAPI 3.x documents as plain JSON values.
//!
//! A typed OpenAPI crate would cover only one version; walking `Value`
//! handles 2.0, 3.0 and 3.1 with the few fields the generator reads.

use serde_json::{json, Value};

use crate::ImportError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    Swagger2,
    OpenApi3,
}

/// Parses `text` as JSON, else as YAML.
pub fn load_doc(text: &str) -> Result<Value, ImportError> {
    if let Ok(v) = serde_json::from_str(text) {
        return Ok(v);
    }
    serde_yaml_ng::from_str(text)
        .map_err(|e| ImportError::Decode { what: "spec".into(), message: e.to_string() })
}

pub fn version(doc: &Value) -> Result<Version, ImportError> {
    // Unquoted in YAML, `swagger: 2.0` is a float, not a string.
    let text = |key: &str| match doc.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    if text("swagger").as_deref() == Some("2.0") {
        return Ok(Version::Swagger2);
    }
    if text("openapi").is_some_and(|v| v.starts_with("3.")) {
        return Ok(Version::OpenApi3);
    }
    Err(ImportError::Shape("not a Swagger 2.0 or OpenAPI 3.x document".into()))
}

/// Follows local `#/...` references (at most 16 hops). An external or
/// dangling reference, or a loop, is `None`; a non-reference is itself.
pub fn resolve<'a>(doc: &'a Value, v: &'a Value) -> Option<&'a Value> {
    let mut cur = v;
    for _ in 0..=16 {
        match cur.get("$ref").and_then(Value::as_str) {
            None => return Some(cur),
            Some(r) => cur = doc.pointer(r.strip_prefix('#')?)?,
        }
    }
    None
}

/// The API's base URL, without a trailing slash: the override if given,
/// else the spec's server (3.x) or host (2.0), a relative one joined onto
/// the URL the spec was fetched from.
pub fn base_url(
    doc: &Value,
    ver: Version,
    spec_url: Option<&reqwest::Url>,
    base_override: Option<&str>,
) -> Result<String, ImportError> {
    if let Some(b) = base_override {
        return Ok(b.trim_end_matches('/').to_string());
    }
    let raw = match ver {
        Version::OpenApi3 => match doc.get("servers").and_then(|s| s.get(0)) {
            Some(server) => {
                let mut url = server.get("url").and_then(Value::as_str).unwrap_or("/").to_string();
                for (name, var) in server.get("variables").and_then(Value::as_object).into_iter().flatten() {
                    if let Some(default) = var.get("default").and_then(Value::as_str) {
                        url = url.replace(&format!("{{{name}}}"), default);
                    }
                }
                url
            }
            None => "/".to_string(),
        },
        Version::Swagger2 => {
            let base_path = doc.get("basePath").and_then(Value::as_str).unwrap_or("");
            match doc.get("host").and_then(Value::as_str) {
                Some(host) => {
                    let scheme = doc
                        .get("schemes")
                        .and_then(|s| s.get(0))
                        .and_then(Value::as_str)
                        .unwrap_or("https");
                    format!("{scheme}://{host}{base_path}")
                }
                // Swagger 2.0: no host means the host that serves the spec.
                None if base_path.is_empty() => "/".to_string(),
                None => base_path.to_string(),
            }
        }
    };
    let absolute = match reqwest::Url::parse(&raw) {
        Ok(_) => raw,
        Err(_) => spec_url
            .and_then(|base| base.join(&raw).ok())
            .map(|u| u.to_string())
            .ok_or(ImportError::NoBaseUrl)?,
    };
    Ok(absolute.trim_end_matches('/').to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Path,
    Query,
    Header,
    Cookie,
}

/// One request parameter, with the values its pool should start with.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub location: Location,
    pub required: bool,
    pub values: Vec<Value>,
    /// The expected type when `values` is a placeholder.
    pub todo: Option<String>,
}

/// The operation's parameters in declared order, then the path item's that
/// the operation doesn't override. `Err` is why the operation is skipped.
pub fn params(doc: &Value, ver: Version, path_item: &Value, op: &Value) -> Result<Vec<Param>, String> {
    let key = |p: &Value| (p.get("name").cloned(), p.get("in").cloned());
    let own = declared(doc, op)?;
    let mut merged = own.clone();
    for p in declared(doc, path_item)? {
        if !own.iter().any(|o| key(o) == key(p)) {
            merged.push(p);
        }
    }

    let mut out = Vec::new();
    for p in merged {
        let location = match p.get("in").and_then(Value::as_str) {
            Some("path") => Location::Path,
            Some("query") => Location::Query,
            Some("header") => Location::Header,
            Some("cookie") => Location::Cookie,
            Some("body") => continue,
            Some("formData") => return Err("uses formData parameters".into()),
            other => return Err(format!("has a parameter in an unknown location {other:?}")),
        };
        let name = p.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        let required = location == Location::Path
            || p.get("required").and_then(Value::as_bool).unwrap_or(false);
        let (values, todo) = param_values(doc, ver, p);
        out.push(Param { name, location, required, values, todo });
    }
    Ok(out)
}

/// `v`'s `parameters`, each resolved; a reference that can't be is the
/// reason to skip the operation.
fn declared<'a>(doc: &'a Value, v: &'a Value) -> Result<Vec<&'a Value>, String> {
    let mut out = Vec::new();
    for p in v.get("parameters").and_then(Value::as_array).into_iter().flatten() {
        match resolve(doc, p) {
            Some(p) => out.push(p),
            None => {
                let r = p.get("$ref").and_then(Value::as_str).unwrap_or_default();
                let kind = if r.starts_with('#') { "an unresolvable" } else { "an external" };
                return Err(format!("uses {kind} $ref ({r})"));
            }
        }
    }
    Ok(out)
}

/// The values a parameter's pool starts with, first source found:
/// `example`, every `examples.*.value`, `schema.example`, every
/// `schema.examples[]`, `schema.default`, every `schema.enum`; for 2.0 also
/// `x-example`, `default`, `enum` on the parameter. Else a typed placeholder
/// and the type it stands for.
pub(crate) fn param_values(doc: &Value, ver: Version, p: &Value) -> (Vec<Value>, Option<String>) {
    let schema = p.get("schema").and_then(|s| resolve(doc, s));
    let one = |v: Option<&Value>| v.filter(|v| !v.is_null()).map(|v| vec![v.clone()]);
    let many = |v: Option<&Value>| v.and_then(Value::as_array).filter(|a| !a.is_empty()).cloned();
    let schema_field = |k: &str| schema.and_then(|s| s.get(k));
    let found = one(p.get("example"))
        .or_else(|| {
            let examples = p.get("examples").and_then(Value::as_object)?;
            let values: Vec<Value> = examples
                .values()
                .filter_map(|e| resolve(doc, e)?.get("value").cloned())
                .collect();
            (!values.is_empty()).then_some(values)
        })
        .or_else(|| one(schema_field("example")))
        .or_else(|| many(schema_field("examples")))
        .or_else(|| one(schema_field("default")))
        .or_else(|| many(schema_field("enum")))
        .or_else(|| match ver {
            Version::Swagger2 => one(p.get("x-example"))
                .or_else(|| one(p.get("default")))
                .or_else(|| many(p.get("enum"))),
            Version::OpenApi3 => None,
        });
    if let Some(values) = found {
        return (values, None);
    }
    let ty = schema_field("type").or_else(|| p.get("type")).and_then(Value::as_str).unwrap_or("string");
    let value = match ty {
        "integer" => json!(1),
        "number" => json!(1.0),
        "boolean" => json!(true),
        _ => json!("TODO"),
    };
    (vec![value], Some(ty.to_string()))
}

/// A JSON request body example: 3.x `requestBody` JSON content's `example`,
/// its first `examples.*.value`, or its schema's `example`; 2.0 the body
/// parameter's schema `example`.
pub fn body_example(doc: &Value, ver: Version, op: &Value) -> Option<Value> {
    match ver {
        Version::OpenApi3 => {
            let body = resolve(doc, op.get("requestBody")?)?;
            let media = body
                .get("content")?
                .as_object()?
                .iter()
                .find(|(k, _)| k.starts_with("application/json") || k.contains("+json"))
                .map(|(_, v)| v)?;
            media
                .get("example")
                .cloned()
                .or_else(|| {
                    media
                        .get("examples")?
                        .as_object()?
                        .values()
                        .find_map(|e| resolve(doc, e)?.get("value").cloned())
                })
                .or_else(|| resolve(doc, media.get("schema")?)?.get("example").cloned())
        }
        Version::Swagger2 => {
            let body = op
                .get("parameters")?
                .as_array()?
                .iter()
                .filter_map(|p| resolve(doc, p))
                .find(|p| p.get("in").and_then(Value::as_str) == Some("body"))?;
            resolve(doc, body.get("schema")?)?.get("example").cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{petstore2, petstore3};
    use crate::ImportError;
    use serde_json::json;

    #[test]
    fn load_doc_reads_json_and_yaml() {
        assert_eq!(load_doc(r#"{"a":1}"#).unwrap(), json!({"a": 1}));
        assert_eq!(load_doc("a: 1\nb: [x]\n").unwrap(), json!({"a": 1, "b": ["x"]}));
        assert!(matches!(load_doc("{"), Err(ImportError::Decode { .. })));
    }

    #[test]
    fn yaml_numeric_keys_and_unquoted_versions() {
        // Real YAML specs write response codes unquoted, and `swagger: 2.0` is a float.
        let doc = load_doc("swagger: 2.0\nresponses:\n  200: {description: ok}\n").unwrap();
        assert_eq!(doc["responses"]["200"]["description"], "ok");
        assert_eq!(version(&doc).unwrap(), Version::Swagger2);
        assert_eq!(version(&json!({"openapi": 3.0})).unwrap(), Version::OpenApi3);
    }

    #[test]
    fn version_detection() {
        assert_eq!(version(&petstore2()).unwrap(), Version::Swagger2);
        assert_eq!(version(&petstore3()).unwrap(), Version::OpenApi3);
        assert_eq!(version(&json!({"openapi": "3.1.0"})).unwrap(), Version::OpenApi3);
        assert!(matches!(version(&json!({"info": {}})), Err(ImportError::Shape(_))));
    }

    #[test]
    fn resolve_follows_local_refs_and_stops() {
        let doc = json!({
            "components": {"parameters": {
                "A": {"$ref": "#/components/parameters/B"},
                "B": {"name": "b"},
                "L": {"$ref": "#/components/parameters/L"},
            }},
            "paths": {"/pets": {"get": {}}},
        });
        let r = |s: &str| json!({ "$ref": s });
        assert_eq!(resolve(&doc, &r("#/components/parameters/A")), Some(&json!({"name": "b"})));
        assert_eq!(resolve(&doc, &r("#/components/parameters/L")), None);
        assert_eq!(resolve(&doc, &r("x.yaml#/a")), None);
        assert_eq!(resolve(&doc, &r("#/paths/~1pets")), Some(&json!({"get": {}})));
        let plain = json!({"name": "p"});
        assert_eq!(resolve(&doc, &plain), Some(&plain));
    }

    #[test]
    fn base_url_from_server_variables() {
        let doc = petstore3();
        assert_eq!(
            base_url(&doc, Version::OpenApi3, None, None).unwrap(),
            "https://api.pets.example.com/v1"
        );
    }

    #[test]
    fn base_url_from_swagger2_host() {
        let doc = petstore2();
        assert_eq!(base_url(&doc, Version::Swagger2, None, None).unwrap(), "https://pets.example.com/api");
    }

    #[test]
    fn relative_server_needs_spec_url_or_override() {
        let doc = json!({"openapi": "3.0.0", "servers": [{"url": "/v1"}]});
        let v3 = Version::OpenApi3;
        assert!(matches!(base_url(&doc, v3, None, None), Err(ImportError::NoBaseUrl)));
        let spec = reqwest::Url::parse("http://h:8080/openapi.json").unwrap();
        assert_eq!(base_url(&doc, v3, Some(&spec), None).unwrap(), "http://h:8080/v1");
        assert_eq!(base_url(&doc, v3, None, Some("http://x/api/")).unwrap(), "http://x/api");
        let bare = json!({"openapi": "3.0.0"});
        let spec = reqwest::Url::parse("http://h/openapi.json").unwrap();
        assert_eq!(base_url(&bare, v3, Some(&spec), None).unwrap(), "http://h");
    }

    #[test]
    fn params_merge_path_item_and_operation() {
        let doc = petstore3();
        let item = &doc["paths"]["/pets/{petId}"];
        let ps = params(&doc, Version::OpenApi3, item, &item["get"]).unwrap();
        let got: Vec<(&str, Location, bool)> =
            ps.iter().map(|p| (p.name.as_str(), p.location, p.required)).collect();
        assert_eq!(
            got,
            [
                ("X-Request-Id", Location::Header, true),
                ("Authorization", Location::Header, true),
                ("petId", Location::Path, true),
            ]
        );
        assert_eq!(ps[2].values, [json!(42)]);
        assert_eq!(ps[2].todo, None);
        assert_eq!(ps[0].values, [json!("TODO")]);
        assert_eq!(ps[0].todo.as_deref(), Some("string"));
    }

    #[test]
    fn param_value_precedence() {
        use Version::{OpenApi3 as V3, Swagger2 as V2};
        let empty = json!({});
        let ids = json!({"components": {"schemas": {"Id": {"type": "integer", "example": 3}}}});
        let rows: Vec<(Version, &serde_json::Value, serde_json::Value, serde_json::Value, Option<&str>)> = vec![
            (V3, &empty, json!({"example": 5, "schema": {"type": "integer", "default": 9}}), json!([5]), None),
            (V3, &empty, json!({"examples": {"a": {"value": "x"}, "b": {"value": "y"}}}), json!(["x", "y"]), None),
            (V3, &empty, json!({"schema": {"example": "s"}}), json!(["s"]), None),
            (V3, &empty, json!({"schema": {"examples": ["p", "q"]}}), json!(["p", "q"]), None),
            (V3, &empty, json!({"schema": {"type": "integer", "default": 20}}), json!([20]), None),
            (V3, &empty, json!({"schema": {"enum": ["small", "large"]}}), json!(["small", "large"]), None),
            (V3, &ids, json!({"schema": {"$ref": "#/components/schemas/Id"}}), json!([3]), None),
            (V2, &empty, json!({"type": "integer", "x-example": 7}), json!([7]), None),
            (V2, &empty, json!({"type": "string", "enum": ["available", "sold"]}), json!(["available", "sold"]), None),
            (V3, &empty, json!({"schema": {"type": "integer"}}), json!([1]), Some("integer")),
            (V3, &empty, json!({"schema": {"type": "number"}}), json!([1.0]), Some("number")),
            (V3, &empty, json!({"schema": {"type": "boolean"}}), json!([true]), Some("boolean")),
            (V3, &empty, json!({"schema": {"type": "string", "format": "uuid"}}), json!(["TODO"]), Some("string")),
            (V3, &empty, json!({}), json!(["TODO"]), Some("string")),
        ];
        for (ver, doc, p, want, todo) in rows {
            let (values, got_todo) = param_values(doc, ver, &p);
            assert_eq!(serde_json::Value::Array(values), want, "{p}");
            assert_eq!(got_todo.as_deref(), todo, "{p}");
        }
    }

    #[test]
    fn external_ref_skips_operation() {
        let doc = petstore3();
        let item = &doc["paths"]["/legacy"];
        assert_eq!(
            params(&doc, Version::OpenApi3, item, &item["get"]).unwrap_err(),
            "uses an external $ref (other.yaml#/components/parameters/X)"
        );
    }

    #[test]
    fn body_example_sources() {
        let v3 = Version::OpenApi3;
        let doc3 = petstore3();
        assert_eq!(
            body_example(&doc3, v3, &doc3["paths"]["/pets"]["post"]),
            Some(json!({"name": "Rex", "kind": "dog"}))
        );
        let examples = json!({"requestBody": {"content": {"application/json": {
            "examples": {"one": {"value": {"a": 1}}}}}}});
        assert_eq!(body_example(&json!({}), v3, &examples), Some(json!({"a": 1})));
        let schema = json!({"requestBody": {"content": {"application/json": {
            "schema": {"example": {"b": 2}}}}}});
        assert_eq!(body_example(&json!({}), v3, &schema), Some(json!({"b": 2})));
        let doc2 = petstore2();
        assert_eq!(
            body_example(&doc2, Version::Swagger2, &doc2["paths"]["/pets"]["post"]),
            Some(json!({"name": "Rex"}))
        );
        assert_eq!(body_example(&doc3, v3, &doc3["paths"]["/pets/{petId}"]["get"]), None);
    }
}
