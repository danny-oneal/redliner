//! The GraphQL schema model, parsed from an introspection response.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::ImportError;

/// The parts of a schema the generator needs.
#[derive(Debug, Clone)]
pub struct Schema {
    pub query_type: String,
    pub mutation_type: Option<String>,
    pub subscription_type: Option<String>,
    pub types: BTreeMap<String, TypeDef>,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub kind: Kind,
    /// Empty for anything but objects and interfaces.
    pub fields: Vec<Field>,
    /// Empty for anything but enums.
    pub enum_values: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Scalar,
    Object,
    Interface,
    Union,
    Enum,
    InputObject,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub args: Vec<Arg>,
    pub ty: TypeRef,
}

#[derive(Debug, Clone)]
pub struct Arg {
    pub name: String,
    pub ty: TypeRef,
}

/// A type reference: a named type wrapped in lists and non-nulls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    Named(String),
    List(Box<TypeRef>),
    NonNull(Box<TypeRef>),
}

impl TypeRef {
    /// The innermost named type.
    pub fn named(&self) -> &str {
        match self {
            TypeRef::Named(n) => n,
            TypeRef::List(t) | TypeRef::NonNull(t) => t.named(),
        }
    }

    /// Whether the outermost wrapper is non-null, i.e. a value is required.
    pub fn is_required(&self) -> bool {
        matches!(self, TypeRef::NonNull(_))
    }

    /// GraphQL type syntax, e.g. `[ID!]!`.
    pub fn sdl(&self) -> String {
        match self {
            TypeRef::Named(n) => n.clone(),
            TypeRef::List(t) => format!("[{}]", t.sdl()),
            TypeRef::NonNull(t) => format!("{}!", t.sdl()),
        }
    }
}

impl Schema {
    pub fn kind(&self, name: &str) -> Option<Kind> {
        self.types.get(name).map(|t| t.kind)
    }
}

/// Parses an introspection response. A response with `errors` and no
/// `data.__schema` is the server refusing introspection.
pub fn parse(response: &Value) -> Result<Schema, ImportError> {
    let Some(schema) = response.pointer("/data/__schema").filter(|v| v.is_object()) else {
        return Err(match response.pointer("/errors/0") {
            Some(e) => ImportError::Introspection(
                e.get("message").and_then(Value::as_str).map_or_else(|| e.to_string(), String::from),
            ),
            None => ImportError::Shape("the response has no data.__schema".into()),
        });
    };
    let root = |key: &str| {
        schema.get(key).and_then(|t| t.get("name")).and_then(Value::as_str).map(String::from)
    };
    let query_type = root("queryType").ok_or_else(|| shape("__schema has no queryType"))?;
    let list = schema
        .get("types")
        .and_then(Value::as_array)
        .ok_or_else(|| shape("__schema has no types"))?;

    let mut types = BTreeMap::new();
    for t in list {
        let name = text(t, "name")?;
        let kind = match t.get("kind").and_then(Value::as_str) {
            Some("SCALAR") => Kind::Scalar,
            Some("OBJECT") => Kind::Object,
            Some("INTERFACE") => Kind::Interface,
            Some("UNION") => Kind::Union,
            Some("ENUM") => Kind::Enum,
            Some("INPUT_OBJECT") => Kind::InputObject,
            other => return Err(ImportError::Shape(format!("type {name}: unknown kind {other:?}"))),
        };
        let fields = match t.get("fields").and_then(Value::as_array) {
            Some(fs) => fs.iter().map(field).collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        let enum_values = t
            .get("enumValues")
            .and_then(Value::as_array)
            .map(|vs| vs.iter().filter_map(|v| v.get("name")?.as_str().map(String::from)).collect())
            .unwrap_or_default();
        types.insert(name, TypeDef { kind, fields, enum_values });
    }
    Ok(Schema {
        query_type,
        mutation_type: root("mutationType"),
        subscription_type: root("subscriptionType"),
        types,
    })
}

fn shape(msg: &str) -> ImportError {
    ImportError::Shape(msg.into())
}

fn text(v: &Value, key: &str) -> Result<String, ImportError> {
    v.get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| ImportError::Shape(format!("missing {key} in {v}")))
}

fn field(v: &Value) -> Result<Field, ImportError> {
    let args = match v.get("args").and_then(Value::as_array) {
        Some(args) => args
            .iter()
            .map(|a| Ok(Arg { name: text(a, "name")?, ty: type_ref(&a["type"])? }))
            .collect::<Result<Vec<_>, ImportError>>()?,
        None => Vec::new(),
    };
    Ok(Field { name: text(v, "name")?, args, ty: type_ref(&v["type"])? })
}

fn type_ref(v: &Value) -> Result<TypeRef, ImportError> {
    match v.get("kind").and_then(Value::as_str) {
        Some(kind @ ("NON_NULL" | "LIST")) => {
            let inner = v
                .get("ofType")
                .filter(|t| !t.is_null())
                .ok_or_else(|| ImportError::Shape(format!("{kind} type without ofType")))?;
            let inner = Box::new(type_ref(inner)?);
            Ok(if kind == "LIST" { TypeRef::List(inner) } else { TypeRef::NonNull(inner) })
        }
        _ => Ok(TypeRef::Named(text(v, "name")?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fixture_schema, INTROSPECTION_DISABLED_JSON};
    use crate::ImportError;
    use serde_json::json;

    #[test]
    fn parses_roots_and_types() {
        let s = fixture_schema();
        assert_eq!(s.query_type, "Query");
        assert_eq!(s.mutation_type.as_deref(), Some("Mutation"));
        assert_eq!(s.subscription_type, None);
        let q = &s.types["Query"];
        let names: Vec<&str> = q.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["version", "product", "products", "search", "node", "status"]);

        let product = &q.fields[1];
        assert_eq!(product.args.len(), 1);
        assert_eq!(product.args[0].name, "id");
        assert_eq!(product.args[0].ty.sdl(), "Int!");
        assert!(product.args[0].ty.is_required());

        let args: Vec<(&str, String, bool)> = q.fields[2]
            .args
            .iter()
            .map(|a| (a.name.as_str(), a.ty.sdl(), a.ty.is_required()))
            .collect();
        assert_eq!(
            args,
            [("first", "Int".to_string(), false), ("filter", "ProductFilter!".to_string(), true)]
        );
        assert_eq!(q.fields[3].ty.sdl(), "[SearchResult]");
        assert_eq!(s.kind("Product"), Some(Kind::Object));
        assert_eq!(s.types["Status"].enum_values, ["OK", "DEGRADED"]);
    }

    #[test]
    fn typeref_sdl_and_required() {
        let named = |n: &str| TypeRef::Named(n.to_string());
        let t = TypeRef::NonNull(Box::new(TypeRef::List(Box::new(TypeRef::NonNull(Box::new(
            named("ID"),
        ))))));
        assert_eq!(t.sdl(), "[ID!]!");
        assert_eq!(t.named(), "ID");
        assert!(t.is_required());
        let t = TypeRef::List(Box::new(TypeRef::NonNull(Box::new(TypeRef::List(Box::new(
            named("String"),
        ))))));
        assert_eq!(t.sdl(), "[[String]!]");
        assert!(!t.is_required());
    }

    #[test]
    fn introspection_errors_are_reported() {
        let v: serde_json::Value = serde_json::from_str(INTROSPECTION_DISABLED_JSON).unwrap();
        let e = parse(&v).unwrap_err();
        assert!(
            matches!(&e, ImportError::Introspection(m) if m == "GraphQL introspection is not allowed by Apollo Server"),
            "{e:?}"
        );
        assert!(e.to_string().contains("try a staging endpoint"), "{e}");
    }

    #[test]
    fn missing_schema_is_a_shape_error() {
        assert!(matches!(parse(&json!({"data": {}})), Err(ImportError::Shape(_))));
    }
}
