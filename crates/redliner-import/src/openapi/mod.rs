//! The OpenAPI front end: Swagger 2.0 / OpenAPI 3.x to a [`Draft`](crate::draft::Draft).

pub mod doc;

use serde_json::Value;

use crate::draft::{Draft, PoolDraft, Protocol, RequestDraft, ScenarioDraft};
use crate::ImportError;
use doc::{base_url, body_example, params, resolve, version, Location};

/// Where the document came from and how much of it to make live.
#[derive(Debug, Clone)]
pub struct OpenapiOpts {
    /// For the header comment, e.g. `OpenAPI document petstore.yaml`.
    pub source: String,
    /// The URL the spec was fetched from; relative servers resolve against it.
    pub spec_url: Option<reqwest::Url>,
    /// Overrides the spec's server.
    pub base_url: Option<String>,
    /// Write POST/PUT/PATCH/DELETE live instead of commented out.
    pub include_unsafe: bool,
}

/// Safe methods first; OPTIONS and TRACE are noted and skipped.
const METHODS: [&str; 6] = ["get", "head", "post", "put", "patch", "delete"];

/// One scenario per operation, in path order.
pub fn draft(doc: &Value, opts: &OpenapiOpts) -> Result<Draft, ImportError> {
    let ver = version(doc)?;
    let url = base_url(doc, ver, opts.spec_url.as_ref(), opts.base_url.as_deref())?;
    let mut d = Draft::new(opts.source.clone(), url, Protocol::Rest);
    let declared = |v: Option<&Value>| {
        v.is_some_and(|v| {
            v.as_array().is_some_and(|a| !a.is_empty()) || v.as_object().is_some_and(|o| !o.is_empty())
        })
    };
    if declared(doc.get("security"))
        || declared(doc.pointer("/components/securitySchemes"))
        || declared(doc.get("securityDefinitions"))
    {
        d.notes.push("this API declares security schemes; set target.auth or pass --bearer-env".into());
    }

    let mut commented = 0;
    for (path, item) in doc.get("paths").and_then(Value::as_object).into_iter().flatten() {
        let Some(item) = resolve(doc, item) else {
            d.notes.push(format!("{path}: skipped — its path item uses an unresolvable $ref"));
            continue;
        };
        for method in METHODS {
            let Some(op) = item.get(method) else { continue };
            let upper = method.to_ascii_uppercase();
            let ps = match params(doc, ver, item, op) {
                Ok(ps) => ps,
                Err(reason) => {
                    d.notes.push(format!("{upper} {path}: skipped — {reason}"));
                    continue;
                }
            };
            let base = op
                .get("operationId")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map_or_else(|| format!("{upper} {path}"), String::from);
            let name = d.scenario_name(&base);

            let (mut path_pools, mut query, mut headers) = (Vec::new(), Vec::new(), Vec::new());
            for p in ps {
                let wanted = p.location == Location::Path || p.required;
                if !wanted {
                    continue;
                }
                if p.location == Location::Cookie {
                    d.notes.push(format!("{name}: cookie parameter {} skipped", p.name));
                    continue;
                }
                if p.location == Location::Header && p.name.eq_ignore_ascii_case("authorization") {
                    d.notes.push(format!(
                        "{name}: Authorization header parameter skipped; set target.auth or pass --bearer-env"
                    ));
                    continue;
                }
                let pool = d.add_pool(
                    &format!("{name}_{}", p.name),
                    PoolDraft { values: p.values, todo: p.todo },
                );
                let placeholder = format!("{{{{{pool}}}}}");
                match p.location {
                    Location::Path => path_pools.push((p.name, placeholder)),
                    Location::Query => query.push(format!("{}={placeholder}", p.name)),
                    Location::Header => headers.push((p.name, placeholder)),
                    Location::Cookie => {}
                }
            }
            let mut url_path = fill_path(path, &path_pools);
            if !query.is_empty() {
                url_path.push('?');
                url_path.push_str(&query.join("&"));
            }

            let body = matches!(method, "post" | "put" | "patch").then(|| match body_example(doc, ver, op) {
                // redliner reads `{{` in a body as a placeholder (compact JSON
                // has it only inside strings); a pool value is sent verbatim.
                Some(v) if v.to_string().contains("{{") => {
                    let pool = d.add_pool(
                        &format!("{name}_body"),
                        PoolDraft { values: vec![Value::String(v.to_string())], todo: None },
                    );
                    format!("{{{{{pool}}}}}")
                }
                Some(v) => v.to_string(),
                None => {
                    d.notes.push(format!("{name}: no JSON request body example; body is {{}} (TODO)"));
                    "{}".to_string()
                }
            });
            let changes_state = method != "get" && method != "head";
            let reason = (changes_state && !opts.include_unsafe).then(|| format!("{upper} changes state"));
            if reason.is_some() {
                commented += 1;
            }
            d.scenarios.push(ScenarioDraft {
                name,
                request: RequestDraft::Rest { method: upper, path: url_path, headers, body },
                commented: reason,
            });
        }
        for method in ["options", "trace"] {
            if item.get(method).is_some() {
                d.notes.push(format!("{path}: {} skipped", method.to_ascii_uppercase()));
            }
        }
    }

    if d.scenarios.iter().all(|s| s.commented.is_some()) {
        return Err(ImportError::NoOperations(match commented {
            0 => "the spec has no usable operations".into(),
            1 => "its only operation changes state; pass --include-unsafe to scaffold it".into(),
            n => format!("all {n} operations change state; pass --include-unsafe to scaffold them"),
        }));
    }
    Ok(d)
}

/// `template` with each `{param}` that has a pool replaced by its
/// placeholder, in one pass so a replacement is never re-scanned.
fn fill_path(template: &str, pools: &[(String, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        let Some(j) = rest[i..].find('}') else { break };
        let name = &rest[i + 1..i + j];
        out.push_str(&rest[..i]);
        match pools.iter().find(|(n, _)| n == name) {
            Some((_, placeholder)) => out.push_str(placeholder),
            None => out.push_str(&rest[i..=i + j]),
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft::{render, Draft, RequestDraft, ScenarioDraft};
    use crate::testutil::{assert_contract, petstore2, petstore3};
    use crate::ImportError;
    use serde_json::json;

    fn opts(include_unsafe: bool) -> OpenapiOpts {
        OpenapiOpts { source: "a test".into(), spec_url: None, base_url: None, include_unsafe }
    }

    fn scenario<'a>(d: &'a Draft, name: &str) -> &'a ScenarioDraft {
        d.scenarios.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no scenario {name}"))
    }

    /// (path, headers, body) of a REST scenario.
    fn rest<'a>(d: &'a Draft, name: &str) -> (&'a str, &'a [(String, String)], Option<&'a str>) {
        match &scenario(d, name).request {
            RequestDraft::Rest { path, headers, body, .. } => (path, headers, body.as_deref()),
            other => panic!("not rest: {other:?}"),
        }
    }

    fn pool(d: &Draft, name: &str) -> (serde_json::Value, Option<String>) {
        let p = &d.pools[name];
        (serde_json::Value::Array(p.values.clone()), p.todo.clone())
    }

    #[test]
    fn petstore3_scenarios_in_order() {
        let d = draft(&petstore3(), &opts(false)).unwrap();
        let names: Vec<&str> = d.scenarios.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["listPets", "createPet", "getPetById", "deletePet", "GET /pets/{petId}/photos"]);
        let commented: Vec<Option<&str>> = d.scenarios.iter().map(|s| s.commented.as_deref()).collect();
        assert_eq!(
            commented,
            [None, Some("POST changes state"), None, Some("DELETE changes state"), None]
        );
        assert_eq!(d.target_url, "https://api.pets.example.com/v1");
    }

    #[test]
    fn petstore3_paths_and_pools() {
        let d = draft(&petstore3(), &opts(false)).unwrap();
        assert_eq!(rest(&d, "listPets").0, "/pets?limit={{listPets_limit}}");
        assert_eq!(rest(&d, "getPetById").0, "/pets/{{getPetById_petId}}");
        assert_eq!(
            rest(&d, "GET /pets/{petId}/photos").0,
            "/pets/{{GET_pets_petId_photos_petId}}/photos?size={{GET_pets_petId_photos_size}}"
        );
        assert_eq!(
            rest(&d, "getPetById").1,
            [("X-Request-Id".to_string(), "{{getPetById_X_Request_Id}}".to_string())]
        );
        assert_eq!(pool(&d, "listPets_limit"), (json!([20]), None));
        assert_eq!(pool(&d, "getPetById_petId"), (json!([42]), None));
        assert_eq!(pool(&d, "getPetById_X_Request_Id"), (json!(["TODO"]), Some("string".into())));
        assert_eq!(pool(&d, "GET_pets_petId_photos_petId"), (json!([1]), Some("integer".into())));
        assert_eq!(pool(&d, "GET_pets_petId_photos_size"), (json!(["small", "large"]), None));
        assert_eq!(pool(&d, "deletePet_petId"), (json!([42]), None));
        assert!(!d.pools.keys().any(|k| k.starts_with("createPet")), "{:?}", d.pools.keys());
    }

    #[test]
    fn petstore3_notes() {
        let d = draft(&petstore3(), &opts(false)).unwrap();
        assert_eq!(
            d.notes,
            [
                "this API declares security schemes; set target.auth or pass --bearer-env",
                "GET /legacy: skipped — uses an external $ref (other.yaml#/components/parameters/X)",
                "/legacy: OPTIONS skipped",
                "getPetById: Authorization header parameter skipped; set target.auth or pass --bearer-env",
            ]
        );
    }

    #[test]
    fn bodies_only_for_post_put_patch() {
        let d = draft(&petstore3(), &opts(false)).unwrap();
        assert_eq!(rest(&d, "createPet").2, Some("{\"kind\":\"dog\",\"name\":\"Rex\"}"));
        for name in ["deletePet", "listPets", "getPetById", "GET /pets/{petId}/photos"] {
            assert_eq!(rest(&d, name).2, None, "{name}");
        }
    }

    #[test]
    fn include_unsafe_makes_them_live() {
        let d = draft(&petstore3(), &opts(true)).unwrap();
        assert!(d.scenarios.iter().all(|s| s.commented.is_none()));
    }

    #[test]
    fn duplicate_operation_ids_are_suffixed() {
        let doc = json!({"openapi": "3.0.0", "servers": [{"url": "http://h"}], "paths": {
            "/a": {"get": {"operationId": "x"}},
            "/b": {"get": {"operationId": "x"}},
        }});
        let d = draft(&doc, &opts(false)).unwrap();
        let names: Vec<&str> = d.scenarios.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["x", "x_2"]);
    }

    #[test]
    fn all_unsafe_without_flag_is_an_error() {
        let doc = json!({"openapi": "3.0.0", "servers": [{"url": "http://h"}], "paths": {
            "/a": {"post": {"operationId": "make"}},
        }});
        match draft(&doc, &opts(false)) {
            Err(ImportError::NoOperations(m)) => assert!(m.contains("--include-unsafe"), "{m}"),
            other => panic!("expected NoOperations, got {other:?}"),
        }
    }

    #[test]
    fn swagger2_draft() {
        let d = draft(&petstore2(), &opts(false)).unwrap();
        assert_eq!(d.target_url, "https://pets.example.com/api");
        let names: Vec<&str> = d.scenarios.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["findPets", "addPet", "getPet"]);
        assert_eq!(rest(&d, "findPets").0, "/pets?status={{findPets_status}}");
        assert_eq!(pool(&d, "findPets_status"), (json!(["available", "sold"]), None));
        assert_eq!(rest(&d, "getPet").0, "/pets/{{getPet_petId}}");
        assert_eq!(pool(&d, "getPet_petId"), (json!([7]), None));
        assert_eq!(scenario(&d, "addPet").commented.as_deref(), Some("POST changes state"));
        assert_eq!(rest(&d, "addPet").2, Some("{\"name\":\"Rex\"}"));
    }

    #[test]
    fn openapi_drafts_meet_the_contract() {
        let count = |doc: &serde_json::Value, unsafe_: bool| {
            assert_contract(&render(&draft(doc, &opts(unsafe_)).unwrap())).scenarios.len()
        };
        assert_eq!(count(&petstore3(), false), 3);
        assert_eq!(count(&petstore3(), true), 5);
        assert_eq!(count(&petstore2(), false), 2);
    }

    #[test]
    fn body_examples_containing_braces_are_sent_verbatim() {
        let example = json!({"html": "<p>Hello {{name}}</p>"});
        let doc = json!({"openapi": "3.0.0", "servers": [{"url": "http://h"}], "paths": {"/templates": {"post": {
            "operationId": "createTemplate",
            "requestBody": {"content": {"application/json": {"example": example}}}
        }}}});
        let cfg = assert_contract(&render(&draft(&doc, &opts(true)).unwrap()));
        let pools = redliner::data::build_pools(&cfg.data).unwrap();
        let mut rng = <rand_chacha::ChaCha8Rng as rand::SeedableRng>::seed_from_u64(1);
        let sent = redliner::data::render_string(&cfg.scenarios[0].request.body, &pools, &mut rng).unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&sent).unwrap(), example);
    }

    #[test]
    fn empty_operation_ids_fall_back_to_method_and_path() {
        let doc = json!({"openapi": "3.0.0", "servers": [{"url": "http://h"}], "paths": {
            "/a": {"get": {"operationId": ""}}, "/b": {"get": {"operationId": "  "}}
        }});
        let d = draft(&doc, &opts(false)).unwrap();
        let names: Vec<&str> = d.scenarios.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["GET /a", "GET /b"]);
        assert_contract(&render(&d));
    }
}
