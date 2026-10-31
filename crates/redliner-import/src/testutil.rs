//! Test helpers: the contract every draft must meet.

use std::sync::atomic::{AtomicUsize, Ordering};

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use serde_json::Value;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// An introspection response for the schema in the plan's Task 2.
pub const INTROSPECTION_JSON: &str = include_str!("../tests/fixtures/introspection.json");
/// A server that refuses introspection.
pub const INTROSPECTION_DISABLED_JSON: &str =
    include_str!("../tests/fixtures/introspection-disabled.json");

/// The parsed introspection fixture.
pub fn fixture_schema() -> crate::graphql::schema::Schema {
    crate::graphql::schema::parse(&serde_json::from_str(INTROSPECTION_JSON).unwrap()).unwrap()
}

/// Loads `yaml` with redliner's own config loader, builds its pools and
/// renders every scenario's templates; panics with the YAML on any failure.
pub fn assert_contract(yaml: &str) -> redliner::config::Config {
    let path = std::env::temp_dir().join(format!(
        "redliner-import-{}-{}.yaml",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&path, yaml).unwrap();
    let cfg = match redliner::config::load_config_file(&path) {
        Ok(c) => c,
        Err(e) => panic!("draft does not load: {e}\n---\n{yaml}"),
    };
    let _ = std::fs::remove_file(&path);
    let pools = match redliner::data::build_pools(&cfg.data) {
        Ok(p) => p,
        Err(e) => panic!("pools do not build: {e}\n---\n{yaml}"),
    };
    let mut rng = ChaCha8Rng::seed_from_u64(1);
    for s in &cfg.scenarios {
        let r = &s.request;
        let vars = Value::Object(r.variables.clone().into_iter().collect());
        if let Err(e) = redliner::data::render_value(&vars, &pools, &mut rng) {
            panic!("scenario {:?}: variables do not render: {e}\n---\n{yaml}", s.name);
        }
        let mut strings = vec![("path", r.path.as_str())];
        if !r.body.is_empty() {
            strings.push(("body", r.body.as_str()));
        }
        for v in r.headers.values() {
            strings.push(("header", v.as_str()));
        }
        for (what, text) in strings {
            if let Err(e) = redliner::data::render_string(text, &pools, &mut rng) {
                panic!("scenario {:?}: {what} does not render: {e}\n---\n{yaml}", s.name);
            }
        }
    }
    cfg
}
