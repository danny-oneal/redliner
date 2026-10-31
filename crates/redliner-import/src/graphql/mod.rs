//! The GraphQL front end: introspection to a [`Draft`](crate::draft::Draft).

pub mod schema;
pub mod select;

use serde_json::{json, Value};

use crate::draft::{Draft, PoolDraft, Protocol, RequestDraft, ScenarioDraft};
use crate::fetch::Fetcher;
use crate::ImportError;
use schema::{Field, Kind, Schema, TypeRef};
use select::selection;

/// Asks only for what the generator reads: root names, each type's kind,
/// non-deprecated fields with their args and types, and enum values.
pub const INTROSPECTION_QUERY: &str = "query RedlinerImport {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types {
      kind
      name
      fields(includeDeprecated: false) { name args { name type { ...TypeRef } } type { ...TypeRef } }
      enumValues(includeDeprecated: false) { name }
    }
  }
}
fragment TypeRef on __Type {
  kind name ofType { kind name ofType { kind name ofType { kind name ofType {
  kind name ofType { kind name ofType { kind name ofType { kind name } } } } } } }
}
";

/// Introspects the live endpoint.
pub async fn fetch_schema(f: &Fetcher, url: &str) -> Result<Schema, ImportError> {
    let resp = f.post_json(url, &json!({ "query": INTROSPECTION_QUERY })).await?;
    schema::parse(&resp)
}

/// One scenario per Query field. Each is a named operation with the field's
/// name, because the encoder sends `operationName` = scenario name.
pub fn draft(schema: &Schema, url: &str, max_depth: usize) -> Result<Draft, ImportError> {
    let root = schema
        .types
        .get(&schema.query_type)
        .filter(|t| !t.fields.is_empty())
        .ok_or_else(|| {
            ImportError::NoOperations(format!("the query root {} has no fields", schema.query_type))
        })?;
    let mut d = Draft::new(format!("GraphQL introspection of {url}"), url, Protocol::Graphql);
    for f in &root.fields {
        let name = d.scenario_name(&f.name);
        let (mut variables, mut decls, mut args) = (Vec::new(), Vec::new(), Vec::new());
        for a in f.args.iter().filter(|a| a.ty.is_required()) {
            let pool = d.add_pool(
                &format!("{}_{}", f.name, a.name),
                PoolDraft { values: vec![placeholder(schema, &a.ty)], todo: Some(a.ty.sdl()) },
            );
            variables.push((a.name.clone(), format!("{{{{{pool}}}}}")));
            decls.push(format!("${}: {}", a.name, a.ty.sdl()));
            args.push(format!("{0}: ${0}", a.name));
        }
        let query = operation(&name, f, &decls, &args, &selection(schema, f.ty.named(), max_depth));
        d.scenarios.push(ScenarioDraft {
            name,
            request: RequestDraft::Graphql { query, variables },
            commented: None,
        });
    }
    for (label, root) in [("mutations", &schema.mutation_type), ("subscriptions", &schema.subscription_type)] {
        let fields = root.as_ref().and_then(|n| schema.types.get(n)).map(|t| &t.fields);
        if let Some(fields) = fields.filter(|f| !f.is_empty()) {
            let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
            let plural = if names.len() == 1 { "" } else { "s" };
            d.notes.push(format!(
                "{label} not scaffolded: {} field{plural} ({})",
                names.len(),
                names.join(", ")
            ));
        }
    }
    Ok(d)
}

/// `query op($a: T!) {\n  field(a: $a) {\n    …\n  }\n}\n`; the parentheses
/// go when there are no variables, the braces when the field is a leaf.
fn operation(op: &str, f: &Field, decls: &[String], args: &[String], sel: &[String]) -> String {
    let paren = |v: &[String]| if v.is_empty() { String::new() } else { format!("({})", v.join(", ")) };
    let mut q = format!("query {op}{} {{\n  {}{}", paren(decls), f.name, paren(args));
    if sel.is_empty() {
        q.push('\n');
    } else {
        q.push_str(" {\n");
        for line in sel {
            q.push_str("    ");
            q.push_str(line);
            q.push('\n');
        }
        q.push_str("  }\n");
    }
    q.push_str("}\n");
    q
}

/// A value of type `ty` that passes the server's type check; meaningful
/// values are the user's job (the pool carries a TODO).
pub fn placeholder(schema: &Schema, ty: &TypeRef) -> Value {
    match ty {
        TypeRef::NonNull(t) => placeholder(schema, t),
        TypeRef::List(t) => Value::Array(vec![placeholder(schema, t)]),
        TypeRef::Named(n) => match schema.types.get(n) {
            Some(t) if t.kind == Kind::Enum => {
                t.enum_values.first().map_or_else(|| json!("TODO"), |v| json!(v))
            }
            Some(t) if t.kind == Kind::InputObject => json!({}),
            _ => match n.as_str() {
                "Int" => json!(1),
                "Float" => json!(1.0),
                "Boolean" => json!(true),
                _ => json!("TODO"),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft::{render, RequestDraft};
    use crate::fetch::{FetchOpts, Fetcher};
    use crate::graphql::schema::{Kind, Schema, TypeDef, TypeRef};
    use crate::graphql::select::selection;
    use crate::testutil::{
        assert_contract, fixture_schema, spawn_mock, INTROSPECTION_DISABLED_JSON, INTROSPECTION_JSON,
    };
    use crate::ImportError;
    use serde_json::json;
    use std::collections::BTreeMap;

    const URL: &str = "http://h/graphql";

    /// A type from GraphQL syntax, e.g. `[Int!]!`.
    fn t(sdl: &str) -> TypeRef {
        if let Some(inner) = sdl.strip_suffix('!') {
            return TypeRef::NonNull(Box::new(t(inner)));
        }
        if let Some(inner) = sdl.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            return TypeRef::List(Box::new(t(inner)));
        }
        TypeRef::Named(sdl.to_string())
    }

    fn request<'a>(d: &'a crate::draft::Draft, name: &str) -> (&'a str, &'a [(String, String)]) {
        let s = d.scenarios.iter().find(|s| s.name == name).expect("scenario");
        match &s.request {
            RequestDraft::Graphql { query, variables } => (query.as_str(), variables.as_slice()),
            other => panic!("not graphql: {other:?}"),
        }
    }

    fn no_auth() -> FetchOpts {
        FetchOpts { headers: vec![], bearer_env: None, insecure: false }
    }

    #[test]
    fn draft_one_scenario_per_query_field() {
        let d = draft(&fixture_schema(), URL, 3).unwrap();
        let names: Vec<&str> = d.scenarios.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["version", "product", "products", "search", "node", "status"]);
        assert!(d.scenarios.iter().all(|s| s.commented.is_none()));
        assert_eq!(d.target_url, URL);
        assert_eq!(d.source, "GraphQL introspection of http://h/graphql");
    }

    #[test]
    fn operation_is_named_after_the_field() {
        let s = fixture_schema();
        let d = draft(&s, URL, 3).unwrap();
        assert_eq!(request(&d, "version").0, "query version {\n  version\n}\n");
        let sel: String =
            selection(&s, "Product", 3).iter().map(|l| format!("    {l}\n")).collect();
        assert_eq!(
            request(&d, "product").0,
            format!("query product($id: Int!) {{\n  product(id: $id) {{\n{sel}  }}\n}}\n")
        );
    }

    #[test]
    fn required_args_become_scenario_pools() {
        let d = draft(&fixture_schema(), URL, 3).unwrap();
        let keys: Vec<&str> = d.pools.keys().map(String::as_str).collect();
        assert_eq!(keys, ["node_id", "product_id", "products_filter", "search_term"]);
        let pool = |v: serde_json::Value, todo: &str| PoolDraft { values: vec![v], todo: Some(todo.into()) };
        assert_eq!(d.pools["product_id"], pool(json!(1), "Int!"));
        assert_eq!(d.pools["products_filter"], pool(json!({}), "ProductFilter!"));
        assert_eq!(d.pools["search_term"], pool(json!("TODO"), "String!"));
        assert_eq!(d.pools["node_id"], pool(json!("TODO"), "ID!"));
        assert_eq!(request(&d, "product").1, [("id".to_string(), "{{product_id}}".to_string())]);
        let products = request(&d, "products").0;
        assert!(products.contains("$filter: ProductFilter!"), "{products}");
        assert!(!products.contains("first"), "{products}");
    }

    #[test]
    fn placeholder_values_by_type() {
        let s = fixture_schema();
        for (ty, want) in [
            ("Int!", json!(1)),
            ("Float", json!(1.0)),
            ("Boolean!", json!(true)),
            ("String", json!("TODO")),
            ("ID!", json!("TODO")),
            ("DateTime", json!("TODO")),
            ("[Int!]!", json!([1])),
            ("Status", json!("OK")),
            ("ProductFilter!", json!({})),
        ] {
            assert_eq!(placeholder(&s, &t(ty)), want, "{ty}");
        }
    }

    #[test]
    fn mutations_are_noted_not_scaffolded() {
        let d = draft(&fixture_schema(), URL, 3).unwrap();
        assert_eq!(d.notes, ["mutations not scaffolded: 1 field (createProduct)"]);
    }

    #[test]
    fn empty_query_root_is_an_error() {
        let mut types = BTreeMap::new();
        types.insert("Query".to_string(), TypeDef { kind: Kind::Object, fields: vec![], enum_values: vec![] });
        let s = Schema { query_type: "Query".into(), mutation_type: None, subscription_type: None, types };
        assert!(matches!(draft(&s, URL, 3), Err(ImportError::NoOperations(_))));
    }

    #[test]
    fn graphql_draft_meets_the_contract() {
        let cfg = assert_contract(&render(&draft(&fixture_schema(), URL, 3).unwrap()));
        assert_eq!(cfg.scenarios.len(), 6);
    }

    #[tokio::test]
    async fn fetch_schema_posts_introspection_with_headers() {
        std::env::set_var("REDLINER_IMPORT_T3_TOKEN", "s3cret");
        let m = spawn_mock(200, "application/json", INTROSPECTION_JSON.to_string()).await;
        let f = Fetcher::new(&FetchOpts {
            headers: vec![("X-Team".into(), "core".into())],
            bearer_env: Some("REDLINER_IMPORT_T3_TOKEN".into()),
            insecure: false,
        })
        .unwrap();
        let s = fetch_schema(&f, &format!("{}/graphql", m.url)).await.unwrap();
        assert_eq!(s.query_type, "Query");
        let reqs = m.requests.lock().unwrap();
        assert!(reqs[0].starts_with("POST /graphql"), "{}", reqs[0]);
        assert!(reqs[0].contains("__schema"), "{}", reqs[0]);
        let lower = reqs[0].to_lowercase();
        assert!(lower.contains("authorization: bearer s3cret"), "{lower}");
        assert!(lower.contains("x-team: core"), "{lower}");
    }

    #[tokio::test]
    async fn introspection_rejected_with_400_is_reported() {
        let m = spawn_mock(400, "application/json", INTROSPECTION_DISABLED_JSON.to_string()).await;
        let f = Fetcher::new(&no_auth()).unwrap();
        let r = fetch_schema(&f, &format!("{}/graphql", m.url)).await;
        assert!(matches!(r, Err(ImportError::Introspection(_))), "{r:?}");
    }

    #[tokio::test]
    async fn non_json_error_status_is_reported() {
        let m = spawn_mock(502, "text/plain", "bad gateway".to_string()).await;
        let f = Fetcher::new(&no_auth()).unwrap();
        let r = fetch_schema(&f, &format!("{}/graphql", m.url)).await;
        assert!(matches!(r, Err(ImportError::Status { code: 502, .. })), "{r:?}");
    }

    #[tokio::test]
    async fn wrong_path_reports_the_status() {
        let m = spawn_mock(404, "application/json", r#"{"message":"not found"}"#.to_string()).await;
        let f = Fetcher::new(&no_auth()).unwrap();
        let r = fetch_schema(&f, &format!("{}/nope", m.url)).await;
        assert!(matches!(r, Err(ImportError::Status { code: 404, .. })), "{r:?}");
    }

    #[tokio::test]
    async fn a_redirect_is_named_not_reported_as_disabled_introspection() {
        // Following a 301 turns the POST into a GET, which a GraphQL server
        // rejects with `errors`; that read as "introspection is disabled".
        let target = spawn_mock(400, "application/json", INTROSPECTION_DISABLED_JSON.to_string()).await;
        let to = format!("{}/graphql", target.url);
        let m = crate::testutil::spawn_mock_with(301, "text/plain", String::new(), format!("Location: {to}\r\n")).await;
        let f = Fetcher::new(&no_auth()).unwrap();
        let r = fetch_schema(&f, &format!("{}/graphql", m.url)).await;
        assert!(matches!(&r, Err(ImportError::Redirect { location, .. }) if *location == to), "{r:?}");
    }
}
