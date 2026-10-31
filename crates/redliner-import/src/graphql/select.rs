//! Depth-limited selection sets.

use super::schema::{Kind, Schema};

/// The lines inside a selection set for `type_name`, nested levels indented
/// two more spaces; empty for scalars, enums, input objects and unknown
/// types. `max_depth` counts `type_name` itself as level 1.
pub fn selection(schema: &Schema, type_name: &str, max_depth: usize) -> Vec<String> {
    let mut path = vec![type_name.to_string()];
    select(schema, type_name, max_depth, &mut path)
}

/// `path` holds the types from the root down to `type_name`, so a
/// self-referential type is never re-entered.
fn select(schema: &Schema, type_name: &str, depth: usize, path: &mut Vec<String>) -> Vec<String> {
    let Some(def) = schema.types.get(type_name) else {
        return Vec::new();
    };
    let mut lines = match def.kind {
        Kind::Scalar | Kind::Enum | Kind::InputObject => return Vec::new(),
        Kind::Union => return vec!["__typename".to_string()],
        Kind::Interface => vec!["__typename".to_string()],
        Kind::Object => Vec::new(),
    };
    for f in &def.fields {
        if f.args.iter().any(|a| a.ty.is_required()) {
            continue;
        }
        let target = f.ty.named();
        match schema.kind(target) {
            Some(Kind::Scalar | Kind::Enum) => lines.push(f.name.clone()),
            Some(Kind::Object | Kind::Interface | Kind::Union)
                if depth > 1 && !path.iter().any(|p| p == target) =>
            {
                path.push(target.to_string());
                let nested = select(schema, target, depth - 1, path);
                path.pop();
                if !nested.is_empty() {
                    lines.push(format!("{} {{", f.name));
                    lines.extend(nested.into_iter().map(|l| format!("  {l}")));
                    lines.push("}".to_string());
                }
            }
            _ => {}
        }
    }
    if lines.is_empty() {
        lines.push("__typename".to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphql::schema::{Arg, Field, Kind, Schema, TypeDef, TypeRef};
    use crate::testutil::fixture_schema;
    use std::collections::BTreeMap;

    #[test]
    fn selection_respects_max_depth() {
        let s = fixture_schema();
        assert_eq!(selection(&s, "Product", 1), ["id", "name", "tags"]);
        assert_eq!(
            selection(&s, "Product", 2),
            ["id", "name", "category {", "  id", "  name", "}", "tags"]
        );
        assert_eq!(
            selection(&s, "Product", 3),
            ["id", "name", "category {", "  id", "  name", "  owner {", "    id", "    name", "  }", "}", "tags"]
        );
    }

    #[test]
    fn selection_never_reenters_a_type_on_the_path() {
        assert_eq!(
            selection(&fixture_schema(), "Category", 3),
            ["id", "name", "owner {", "  id", "  name", "}", "products {", "  id", "  name", "  tags", "}"]
        );
    }

    #[test]
    fn fields_with_required_args_are_skipped() {
        let lines = selection(&fixture_schema(), "Product", 3);
        assert!(lines.iter().all(|l| !l.trim_start().starts_with("price")), "{lines:?}");
    }

    #[test]
    fn abstract_and_leaf_types() {
        let s = fixture_schema();
        assert_eq!(selection(&s, "SearchResult", 3), ["__typename"]);
        assert_eq!(selection(&s, "Node", 3), ["__typename", "id"]);
        assert!(selection(&s, "String", 3).is_empty());
        assert!(selection(&s, "Status", 3).is_empty());
    }

    #[test]
    fn nothing_selectable_falls_back_to_typename() {
        let string = TypeRef::Named("String".into());
        let mut types = BTreeMap::new();
        types.insert(
            "String".to_string(),
            TypeDef { kind: Kind::Scalar, fields: vec![], enum_values: vec![] },
        );
        types.insert(
            "Gated".to_string(),
            TypeDef {
                kind: Kind::Object,
                fields: vec![Field {
                    name: "secret".into(),
                    args: vec![Arg { name: "key".into(), ty: TypeRef::NonNull(Box::new(string.clone())) }],
                    ty: string,
                }],
                enum_values: vec![],
            },
        );
        let s = Schema { query_type: "Query".into(), mutation_type: None, subscription_type: None, types };
        assert_eq!(selection(&s, "Gated", 3), ["__typename"]);
    }
}
