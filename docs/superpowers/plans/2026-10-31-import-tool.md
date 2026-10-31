# `redliner-import` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. REQUIRED during coding: the `rust-skills` skill (where it conflicts with `CLAUDE.md`, `CLAUDE.md` wins).

**Goal:** A separate `redliner-import` binary that writes a draft Redliner config from GraphQL introspection of a live endpoint or from a Swagger 2.0 / OpenAPI 3.x document.

**Architecture:** A new workspace crate, `crates/redliner-import`, with no runtime dependency on `redliner`. Two front ends (`graphql/`, `openapi/`) each turn their source into one source-agnostic `Draft`; `draft::render` writes it as commented YAML. Tests use `redliner` as a path-only dev-dependency to prove the contract: every draft loads with `redliner::config::load_config_file` and every `{{placeholder}}` renders through `redliner::data`.

**Tech Stack:** Rust 1.88 / edition 2021, clap 4 (derive), reqwest 0.12 (rustls), serde_json, serde_yaml_ng, thiserror, tokio.

**Spec:** `docs/superpowers/specs/2026-10-31-import-tool-design.md`. Read it first: the GraphQL and OpenAPI rules there are the source of truth; tasks cite them and pin their exact values in tests.

## Global Constraints

- Work on branch `agent/import-tool` (created from `main` at `198f375`). Conventional Commits, scope `import`.
- **Do not run `cargo fmt`**; match the surrounding hand formatting. `make lint` (clippy `-D warnings`) and `make test` pass after every task.
- MSRV 1.88, **edition 2021**: no let-chains (`if let … && …`), no 2024-only syntax. CI runs `cargo +1.88 check --workspace --all-targets --locked`.
- CI runs `cargo package --workspace --locked`: the crate must be publishable, and `Cargo.lock` is committed with every dependency change.
- `redliner` and `redliner-volley` must never depend on `redliner-import`. `redliner-import` has **no** normal dependency on either; `redliner` is a path-only dev-dependency.
- No new third-party crates. Runtime deps: `clap = { version = "4.5.61", features = ["derive"] }`, `reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }` (no `json` feature: serialize bodies with `serde_json::to_vec` and set `Content-Type` yourself), `serde_json = "1.0.154"`, `serde_yaml_ng = "0.10.0"`, `thiserror = "2.0.20"`, `tokio = { version = "1.53.3", features = ["rt", "macros"] }`.
- Errors: one `thiserror` enum, `ImportError`; lowercase messages; `#[source]` on wrapped errors; no `anyhow`; no `unwrap()`/`expect()` outside tests.
- Anything that reaches the output is ordered deterministically (`BTreeMap`, or declared order). No timestamps in the output.
- Secrets never reach the output: `-H` values and the bearer token value are used only for fetching.
- Every user-visible change gets a line in `CHANGELOG.md` under `## [Unreleased]` (Task 6).

## Review Focus

1. **Introspection disabled in production** (Apollo's default; often HTTP 400 with a JSON `errors` body): the error must quote the server's message and suggest a staging endpoint, not report a status code or a JSON-shape error. → Task 2 `introspection_errors_are_reported`, Task 3 `introspection_rejected_with_400_is_reported`.
2. **Re-running into an existing `redliner.yaml`**: `-o` must refuse to overwrite without `--force`. → Task 6 `run_writes_file_and_refuses_overwrite`.
3. **Credentials passed to fetch the schema** (`-H "X-Api-Key: …"`, `--bearer-env`): used for the request, never written to the file. → Task 6 `run_graphql_end_to_end_never_writes_secrets`.
4. **A relative server (`/v1`) in a spec loaded from a local file**: an error asking for `--base-url`, never a config whose `target.url` is `/v1`. → Task 4 `relative_server_needs_spec_url_or_override`.
5. **Cycles**: a self-referential GraphQL type (`Category.parent: Category`) and a `$ref` that points at itself must both terminate with a valid draft. → Task 2 `selection_never_reenters_a_type_on_the_path`, Task 4 `resolve_follows_local_refs_and_stops`.

---

### Task 1: Crate skeleton, `ImportError`, `Draft` and the renderer

**Files:**
- Modify: `Cargo.toml` (root: add `"crates/redliner-import"` to `members`), `Cargo.lock`
- Create: `crates/redliner-import/Cargo.toml`, `src/lib.rs`, `src/error.rs`, `src/draft.rs`, `src/testutil.rs`

**Interfaces:**
- Produces (`src/error.rs`), used by every later task:
  ```rust
  pub enum ImportError {
      Http { url: String, #[source] source: reqwest::Error },   // "{url}: {source}"
      Status { url: String, code: u16 },                         // "{url}: http status {code}"
      Decode { what: String, message: String },                  // "{what}: {message}"
      Introspection(String),  // "graphql introspection failed: {0} (introspection is often disabled in production; try a staging endpoint)"
      Shape(String),          // "unexpected document shape: {0}"
      MissingEnv(String),     // "environment variable {0} is not set (named by --bearer-env)"
      Header(String),         // "invalid header {0:?}"
      NoBaseUrl,              // "cannot determine the API base URL from the spec; pass --base-url"
      NoOperations(String),   // "nothing to scaffold: {0}"
      Io { path: PathBuf, #[source] source: std::io::Error },  // "{path}: {source}"
      OutputExists(PathBuf),  // "{0} exists; pass --force to overwrite"
  }
  ```
- Produces (`src/draft.rs`):
  ```rust
  pub enum Protocol { Graphql, Rest }
  pub struct Draft {
      pub source: String, pub target_url: String, pub protocol: Protocol,
      pub insecure_tls: bool, pub bearer_env: Option<String>, pub header_names: Vec<String>,
      pub pools: BTreeMap<String, PoolDraft>, pub scenarios: Vec<ScenarioDraft>, pub notes: Vec<String>,
  }
  pub struct PoolDraft { pub values: Vec<serde_json::Value>, pub todo: Option<String> }
  pub struct ScenarioDraft { pub name: String, pub request: RequestDraft, pub commented: Option<String> }
  pub enum RequestDraft {
      Graphql { query: String, variables: Vec<(String, String)> },
      Rest { method: String, path: String, headers: Vec<(String, String)>, body: Option<String> },
  }
  impl Draft {
      pub fn new(source: impl Into<String>, target_url: impl Into<String>, protocol: Protocol) -> Draft;
      /// Sanitizes `base`, makes it unique among `pools` (`_2`, `_3`, …), inserts, returns the name.
      pub fn add_pool(&mut self, base: &str, pool: PoolDraft) -> String;
      /// `base` unchanged if unused by `scenarios`, else `base_2`, `base_3`, …
      pub fn scenario_name(&self, base: &str) -> String;
  }
  /// Every char outside `[A-Za-z0-9_]` becomes `_`; runs of `_` collapse; leading/trailing `_` trimmed.
  pub fn sanitize_name(s: &str) -> String;
  pub fn render(d: &Draft) -> String;
  ```
  Variable and header values are the template strings themselves (`"{{product_id}}"`).
- Produces (`src/testutil.rs`, `#[cfg(test)]` only): `pub fn assert_contract(yaml: &str) -> redliner::config::Config` — writes `yaml` to a unique temp file (`std::env::temp_dir()`, process id plus an atomic counter), calls `redliner::config::load_config_file`, then `redliner::data::build_pools(&cfg.data)`, then with `ChaCha8Rng::seed_from_u64(1)` renders every scenario's `request.variables` (as one `Value::Object`) with `render_value`, and its `path`, non-empty `body` and every `headers` value with `render_string`. Any failure panics with the error and the YAML. Returns the loaded config.

- [ ] **Step 1: Scaffold the crate.** `Cargo.toml`: `name = "redliner-import"`, `version.workspace = true`, `edition = "2021"`, `rust-version = "1.88"`, `description = "Scaffold a Redliner load-test config from a GraphQL schema or an OpenAPI/Swagger document"`, `license`, `repository`, `homepage`, `readme = "../../README.md"` (all as in `crates/redliner/Cargo.toml`), `keywords = ["load-testing", "graphql", "openapi", "swagger", "scaffold"]`, `categories = ["command-line-utilities", "development-tools::testing"]`, the runtime deps from Global Constraints, and:
  ```toml
  [dev-dependencies]
  # The contract: every draft must load and render under redliner itself.
  # Path-only, so `cargo package` strips it; the binary never links redliner.
  redliner = { path = "../redliner" }
  rand = "0.9.5"
  rand_chacha = "0.9.0"
  tokio = { version = "1.53.3", features = ["rt", "macros", "net", "io-util"] }
  ```
  No `[[bin]]` yet (Task 6). `lib.rs` declares `pub mod draft; pub mod error; pub use error::ImportError; #[cfg(test)] mod testutil;`.

- [ ] **Step 2: Write the failing tests** in `draft.rs`:
  - `graphql_draft_renders_a_loadable_config`: a `Graphql` draft (`target_url "http://h/graphql"`), pool `product_id = [1]` with `todo Some("Int!")`, one scenario `product` with query `"query product($id: Int!) {\n  product(id: $id) {\n    id\n  }\n}\n"` and `variables [("id", "{{product_id}}")]`. `let cfg = assert_contract(&render(&d));` then `cfg.scenarios[0].request.variables["id"] == json!("{{product_id}}")`, `cfg.scenarios[0].request.query` equals the query exactly, and the output contains `"product_id": {type: list, values: [1]}  # TODO: real values (Int!)`.
  - `rest_draft_renders_a_loadable_config`: a `Rest` draft, one scenario `GET /a` with path `"/a/{{p}}?q={{p}}"`, header `("X-Trace", "{{p}}")`, body `None`, pool `p = ["x"]`. Contract passes; `request.method == "GET"`, `request.headers["X-Trace"] == "{{p}}"`.
  - `commented_scenario_is_the_live_block_prefixed`: the same scenario rendered live and with `commented: Some("POST changes state")`. The commented output contains the line `  # POST changes state; delete the leading "# " on the lines below to include it:`, and each following block line starts with `# `; stripping that prefix gives exactly the live block's lines. A draft with one live and one commented scenario passes the contract with `cfg.scenarios.len() == 1`.
  - `awkward_strings_round_trip_exactly`: `target_url "http://h:8080/x?y=1#frag"`, scenario name `"GET /a: #1"`, path `"/a?q={{p}}&b=x y: #z"`, body `Some("{\"q\":\"a \\\"b\\\"\\nc\"}")`. After the contract, the loaded url, name, path and body each equal the input byte for byte.
  - `render_is_deterministic`: rendering one draft twice gives equal strings; pools inserted `b` then `a` render `a` first.
  - `target_options_render`: `insecure_tls: true` loads as `cfg.target.insecure_tls`; `bearer_env Some("BENCH_TOKEN")` renders `token_env: "BENCH_TOKEN"` and loads `cfg.target.auth.auth_type == "bearer"`; `header_names ["X-Api-Key", "X-Team"]` renders exactly `  # Headers used for the import, not written here: X-Api-Key, X-Team`.
  - `names_are_sanitized_and_unique`: `sanitize_name("GET /pets/{petId}/photos_petId") == "GET_pets_petId_photos_petId"`; `add_pool("a-b", …)` then `add_pool("a_b", …)` return `"a_b"` and `"a_b_2"`; with a scenario `x` present, `scenario_name("x") == "x_2"`.

- [ ] **Step 3: Run to verify they fail** — `cargo test -p redliner-import` → FAIL to compile (`Draft`, `render` missing).

- [ ] **Step 4: Implement `error.rs` and `draft.rs`.** `render` writes exactly this layout; `{Q:x}` is `serde_json::to_string(&x)` (a JSON string literal is a valid YAML double-quoted scalar), bracketed lines appear only when their condition holds, and scenario blocks are separated by one blank line:
  ```
  # Generated by redliner-import from {source}.
  # A starting point, not a finished benchmark: review the scenarios, replace the
  # TODO pool values (or fill them with data.harvest), tune load and slos, then
  # validate it:  redliner --config <this file> --validate
  version: 1

  target:
    url: {Q:target_url}
    protocol: {graphql|rest}
  [  insecure_tls: true]
  [  auth:
      type: bearer
      token_env: {Q:bearer_env}]
  [  # Headers used for the import, not written here: {header_names joined ", "}]

  load:
    profile: constant
    rate: 10
    duration: 30s
  [
  data:
    pools:
      {Q:name}: {type: list, values: {serde_json::to_string(&values)}}[  # TODO: real values ({todo})]]

  scenarios:
    - name: {Q:name}
      weight: 1
      request:
        query: |                      ← graphql: each query line indented 8 spaces
          …
  [      variables:
          {Q:k}: {Q:v}]
        method: {Q:method}            ← rest
        path: {Q:path}
  [      headers:
          {Q:k}: {Q:v}]
  [      body: {Q:body}]

  # slos: {success_rate: 0.99, p95: 500ms}
  [
  # Notes:
  #   - {note}]
  ```
  Live scenarios come first, then commented ones (each: the reason line from Step 2, then the live block with every line prefixed `# `).

- [ ] **Step 5: Run to verify they pass** — `cargo test -p redliner-import` → PASS; `make lint` clean.

- [ ] **Step 6: Commit** — `feat(import): crate skeleton, draft model and config renderer` (include `Cargo.lock`).

---

### Task 2: GraphQL schema model and selection sets

**Files:**
- Create: `src/graphql/mod.rs` (`pub mod schema; pub mod select;` for now), `src/graphql/schema.rs`, `src/graphql/select.rs`
- Create: `tests/fixtures/introspection.json`, `tests/fixtures/introspection-disabled.json`
- Modify: `src/lib.rs` (`pub mod graphql;`)

**Interfaces:**
- Consumes: `ImportError` (Task 1).
- Produces (`schema.rs`):
  ```rust
  pub struct Schema { pub query_type: String, pub mutation_type: Option<String>,
                      pub subscription_type: Option<String>, pub types: BTreeMap<String, TypeDef> }
  pub struct TypeDef { pub kind: Kind, pub fields: Vec<Field>, pub enum_values: Vec<String> }
  pub enum Kind { Scalar, Object, Interface, Union, Enum, InputObject }
  pub struct Field { pub name: String, pub args: Vec<Arg>, pub ty: TypeRef }
  pub struct Arg { pub name: String, pub ty: TypeRef }
  pub enum TypeRef { Named(String), List(Box<TypeRef>), NonNull(Box<TypeRef>) }
  impl TypeRef { pub fn named(&self) -> &str; pub fn is_required(&self) -> bool; pub fn sdl(&self) -> String; }
  impl Schema { pub fn kind(&self, name: &str) -> Option<Kind>; }
  pub fn parse(response: &serde_json::Value) -> Result<Schema, ImportError>;
  ```
  `parse` reads `data.__schema`; if it is missing and `errors` is non-empty, `Err(Introspection(<errors[0].message>))`; if both are missing, `Err(Shape(..))`. Type kinds come from the strings `SCALAR`, `OBJECT`, `INTERFACE`, `UNION`, `ENUM`, `INPUT_OBJECT`; `NON_NULL`/`LIST` build `TypeRef` from `ofType`.
- Produces (`select.rs`): `pub fn selection(schema: &Schema, type_name: &str, max_depth: usize) -> Vec<String>` — the lines inside a selection set, nested levels indented 2 more spaces, empty for scalar, enum, input-object or unknown types.

**Fixtures.** `introspection.json` is the response a server gives to Task 3's `INTROSPECTION_QUERY` (`{"data":{"__schema":…}}`, only the fields that query asks for, built-in scalars included, fields in the order below) for:
```graphql
type Query {
  version: String!
  product(id: Int!): Product
  products(first: Int, filter: ProductFilter!): [Product!]!
  search(term: String!): [SearchResult]
  node(id: ID!): Node
  status: Status
}
type Mutation { createProduct(name: String!): Product }
type Product implements Node { id: ID!  name: String  price(currency: Currency!): Float  category: Category  tags: [String!] }
type Category { id: ID!  name: String  parent: Category  owner: Team  products: [Product] }
type Team { id: ID!  name: String }
interface Node { id: ID! }
union SearchResult = Product | Category
enum Status { OK DEGRADED }
enum Currency { USD EUR }
input ProductFilter { inStock: Boolean }
scalar DateTime
```
`introspection-disabled.json`: `{"errors":[{"message":"GraphQL introspection is not allowed by Apollo Server"}]}`.

- [ ] **Step 1: Write the failing tests.**
  - `schema.rs` — `parses_roots_and_types`: `query_type == "Query"`, `mutation_type == Some("Mutation")`, `subscription_type == None`; Query field names `["version","product","products","search","node","status"]`; `product`'s only arg is `id` with `sdl() == "Int!"` and `is_required()`; `products` args are `first` (`"Int"`, not required) and `filter` (`"ProductFilter!"`); `search`'s return `sdl() == "[SearchResult]"`.
  - `typeref_sdl_and_required`: `NonNull(List(NonNull(Named("ID"))))` gives `sdl() == "[ID!]!"`, `named() == "ID"`, required; `List(NonNull(List(Named("String"))))` gives `"[[String]!]"`, not required.
  - `introspection_errors_are_reported`: parsing the disabled fixture is `Err(ImportError::Introspection(m))` with `m == "GraphQL introspection is not allowed by Apollo Server"`, and its `to_string()` contains `"try a staging endpoint"`.
  - `missing_schema_is_a_shape_error`: `parse(&json!({"data":{}}))` is `Err(ImportError::Shape(_))`.
  - `select.rs` (fixture schema) — `selection_respects_max_depth`:
    ```rust
    assert_eq!(selection(&s, "Product", 1), ["id", "name", "tags"]);
    assert_eq!(selection(&s, "Product", 2), ["id", "name", "category {", "  id", "  name", "}", "tags"]);
    assert_eq!(selection(&s, "Product", 3),
        ["id", "name", "category {", "  id", "  name", "  owner {", "    id", "    name", "  }", "}", "tags"]);
    ```
  - `selection_never_reenters_a_type_on_the_path`: `selection(&s, "Category", 3) == ["id","name","owner {","  id","  name","}","products {","  id","  name","  tags","}"]` (`parent` and the inner `category` are skipped).
  - `fields_with_required_args_are_skipped`: no line of `selection(&s, "Product", 3)` starts with `price`.
  - `abstract_and_leaf_types`: `"SearchResult"` gives `["__typename"]`, `"Node"` gives `["__typename","id"]`, `"String"` and `"Status"` give `[]`.
  - `nothing_selectable_falls_back_to_typename`: a hand-built schema with `type Gated { secret(key: String!): String }` gives `["__typename"]`.

- [ ] **Step 2: Run to verify they fail** — `cargo test -p redliner-import graphql` → FAIL to compile.

- [ ] **Step 3: Implement `schema.rs` and `select.rs`.** `selection` applies the spec's rules recursively, carrying a path of type names that starts with `type_name`: for each field of an object or interface, skip it if any arg `is_required()`; a scalar or enum target adds the field name; an object, interface or union target adds `name {` + the nested selection (indented 2) + `}` only when `depth > 1`, the target is not on the path, and the nested selection is non-empty. An interface starts its lines with `__typename`; a union returns `["__typename"]`; an object or interface left with no lines returns `["__typename"]`.

- [ ] **Step 4: Run to verify they pass** — `cargo test -p redliner-import graphql` → PASS; `make lint` clean.

- [ ] **Step 5: Commit** — `feat(import): parse GraphQL introspection and build depth-limited selection sets`.

---

### Task 3: HTTP fetching and the GraphQL draft

**Files:**
- Create: `src/fetch.rs`
- Modify: `src/graphql/mod.rs`, `src/lib.rs` (`pub mod fetch;`), `src/testutil.rs` (add the mock server)

**Interfaces:**
- Consumes: `Draft`, `PoolDraft`, `ScenarioDraft`, `RequestDraft`, `Protocol` (Task 1); `Schema`, `TypeRef`, `Kind`, `schema::parse`, `select::selection` (Task 2).
- Produces (`fetch.rs`):
  ```rust
  pub struct FetchOpts { pub headers: Vec<(String, String)>, pub bearer_env: Option<String>, pub insecure: bool }
  pub struct Fetcher { /* reqwest::Client + reqwest::header::HeaderMap */ }
  impl Fetcher {
      pub fn new(opts: &FetchOpts) -> Result<Fetcher, ImportError>;  // MissingEnv when the var is unset or empty; Header on a bad name/value
      pub async fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<serde_json::Value, ImportError>;
      pub async fn get_text(&self, url: &str) -> Result<String, ImportError>;
  }
  ```
  `new` adds the `-H` headers and, with `bearer_env`, `Authorization: Bearer <value>`; the client has a 30 s timeout and `danger_accept_invalid_certs(opts.insecure)`. `post_json` sends JSON with `Content-Type: application/json` and returns the parsed body **whatever the status** (GraphQL errors travel in JSON on 200 and 4xx); a body that isn't JSON is `Status` on a non-2xx and `Decode` on a 2xx. `get_text` is `Status` on a non-2xx.
- Produces (`graphql/mod.rs`):
  ```rust
  pub const INTROSPECTION_QUERY: &str;  // exact text below
  pub async fn fetch_schema(f: &Fetcher, url: &str) -> Result<Schema, ImportError>;  // posts {"query": INTROSPECTION_QUERY}
  pub fn draft(schema: &Schema, url: &str, max_depth: usize) -> Result<Draft, ImportError>;
  pub fn placeholder(schema: &Schema, ty: &TypeRef) -> serde_json::Value;
  ```
- Produces (`testutil.rs`): `pub async fn spawn_mock(status: u16, content_type: &'static str, body: String) -> Mock` with `pub struct Mock { pub url: String /* http://127.0.0.1:PORT */, pub requests: Arc<Mutex<Vec<String>>> }`. It answers every request on any path with that response and records each raw request (request line, headers, body), reading until the headers and `Content-Length` body are in, like `spawn_mock` in `crates/redliner/tests/e2e.rs`.

```graphql
query RedlinerImport {
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
```

`draft` rules: source `"GraphQL introspection of {url}"`, `target_url = url`, protocol `Graphql`; one scenario per Query field, in order, named after the field; each required arg gets a pool via `add_pool(&format!("{field}_{arg}"), PoolDraft { values: vec![placeholder(..)], todo: Some(arg.ty.sdl()) })` and a variable `(arg, "{{<pool>}}")`; optional args are omitted. The query text (each line ends in `\n`):
```
query product($id: Int!) {
  product(id: $id) {
    <selection lines, each indented 4 spaces>
  }
}
```
Without required args, both parentheses are dropped (`query version {`); a scalar or enum return has no braces (`  version`). A mutation or subscription root with fields adds the note `mutations not scaffolded: N field(s) (a, b)` (`"1 field (createProduct)"` for one; likewise `subscriptions …`). No Query fields: `NoOperations("the query root <name> has no fields")`. Placeholder values per the spec: `Int` → `1`, `Float` → `1.0`, `Boolean` → `true`, `String`/`ID`/custom scalars → `"TODO"`, enum → its first value, input object → `{}`, list → `[inner]`.

- [ ] **Step 1: Write the failing tests** (fixture schema, `max_depth` 3 unless stated).
  - `draft_one_scenario_per_query_field`: scenario names `["version","product","products","search","node","status"]`, all `commented == None`.
  - `operation_is_named_after_the_field`: `version`'s query is `"query version {\n  version\n}\n"`; `product`'s is the named operation above wrapped around Task 2's depth-3 `Product` selection (indented 4).
  - `required_args_become_scenario_pools`: pool keys `["node_id","product_id","products_filter","search_term"]`; `product_id = [1]` with todo `"Int!"`, `products_filter = [{}]` / `"ProductFilter!"`, `search_term = ["TODO"]` / `"String!"`, `node_id = ["TODO"]` / `"ID!"`; `product`'s variables are `[("id","{{product_id}}")]`; `products`' query contains `$filter: ProductFilter!` and not `first`.
  - `placeholder_values_by_type`: `Int!` → `json!(1)`, `Float` → `json!(1.0)`, `Boolean!` → `json!(true)`, `String` → `"TODO"`, `ID!` → `"TODO"`, `DateTime` → `"TODO"`, `[Int!]!` → `json!([1])`, `Status` → `"OK"`, `ProductFilter!` → `json!({})`.
  - `mutations_are_noted_not_scaffolded`: `notes == ["mutations not scaffolded: 1 field (createProduct)"]`.
  - `empty_query_root_is_an_error`: a schema whose Query has no fields gives `Err(ImportError::NoOperations(_))`.
  - `graphql_draft_meets_the_contract`: `assert_contract(&render(&draft(..)?))` has 6 scenarios.
  - `fetch_schema_posts_introspection_with_headers` (`#[tokio::test]`): with env `REDLINER_IMPORT_T3_TOKEN=s3cret` and `FetchOpts { headers: vec![("X-Team".into(), "core".into())], bearer_env: Some("REDLINER_IMPORT_T3_TOKEN".into()), insecure: false }`, against `spawn_mock(200, "application/json", <fixture>)`, `fetch_schema(&f, &format!("{}/graphql", m.url))` is `Ok` with `query_type == "Query"`; the recorded request starts with `POST /graphql`, contains `__schema`, and (lowercased) contains `authorization: bearer s3cret` and `x-team: core`.
  - `missing_bearer_env_fails_before_any_request`: `Fetcher::new` with `bearer_env Some("REDLINER_IMPORT_T3_UNSET")` (removed from the env first) is `Err(MissingEnv("REDLINER_IMPORT_T3_UNSET"))`.
  - `introspection_rejected_with_400_is_reported`: the mock answers `400` with the disabled fixture; `fetch_schema` is `Err(Introspection(_))`, not `Status`.
  - `non_json_error_status_is_reported`: the mock answers `502 text/plain "bad gateway"`; `fetch_schema` is `Err(Status { code: 502, .. })`.

- [ ] **Step 2: Run to verify they fail** — `cargo test -p redliner-import` → FAIL to compile.

- [ ] **Step 3: Implement `fetch.rs`, the mock, and `graphql/mod.rs`** per the rules above. Each test that sets an env var uses its own variable name (tests run in parallel).

- [ ] **Step 4: Run to verify they pass** — `cargo test -p redliner-import` → PASS; `make lint` clean.

- [ ] **Step 5: Commit** — `feat(import): fetch over HTTP and draft scenarios from a GraphQL schema`.

---

### Task 4: OpenAPI documents, base URL, `$ref`s and parameters

**Files:**
- Create: `src/openapi/mod.rs` (`pub mod doc;` for now), `src/openapi/doc.rs`
- Create: `tests/fixtures/petstore3.yaml`, `tests/fixtures/petstore2.json`
- Modify: `src/lib.rs` (`pub mod openapi;`)

**Interfaces:**
- Consumes: `ImportError` (Task 1).
- Produces (`doc.rs`):
  ```rust
  pub enum Version { Swagger2, OpenApi3 }
  pub fn load_doc(text: &str) -> Result<serde_json::Value, ImportError>;  // JSON, else YAML (serde_yaml_ng into Value), else Decode
  pub fn version(doc: &Value) -> Result<Version, ImportError>;            // swagger "2.0" | openapi "3.*", else Shape("not a Swagger 2.0 or OpenAPI 3.x document")
  pub fn resolve<'a>(doc: &'a Value, v: &'a Value) -> Option<&'a Value>;   // local "#/..." JSON pointer (~1, ~0), ≤16 hops; external or looping → None; non-ref → Some(v)
  pub fn base_url(doc: &Value, ver: Version, spec_url: Option<&reqwest::Url>, base_override: Option<&str>) -> Result<String, ImportError>;
  pub enum Location { Path, Query, Header, Cookie }
  pub struct Param { pub name: String, pub location: Location, pub required: bool, pub values: Vec<Value>, pub todo: Option<String> }
  pub fn params(doc: &Value, ver: Version, path_item: &Value, op: &Value) -> Result<Vec<Param>, String>;
  pub(crate) fn param_values(doc: &Value, ver: Version, p: &Value) -> (Vec<Value>, Option<String>);
  pub fn body_example(doc: &Value, ver: Version, op: &Value) -> Option<Value>;
  ```
  `base_url` follows the spec (override wins, trailing `/` trimmed; 3.x `servers[0].url` with `{var}` replaced by `variables.var.default`, joined onto `spec_url` when relative; no `servers` means `/`; 2.0 `schemes[0]://host` + `basePath`, scheme `https` when absent; still relative or no host → `NoBaseUrl`). `params` returns the operation's parameters in declared order, then path-item parameters the operation doesn't override (same `name` and `in`); path parameters are always `required`; `in: body` is left to `body_example`; an external or unresolvable `$ref` is `Err("uses an external $ref (<ref>)")` and `in: formData` is `Err("uses formData parameters")`. `param_values` resolves the parameter's `schema` through `resolve` (it may be a `$ref`), then applies the spec's precedence; `todo` is `Some(<schema or 2.0 type>, "string" when absent)` only when a typed placeholder was used.

**Fixtures** (exact):
```yaml
# tests/fixtures/petstore3.yaml
openapi: 3.0.3
info: {title: Pets, version: "1"}
servers:
  - url: "https://{env}.pets.example.com/v1"
    variables: {env: {default: api}}
security: [{bearer: []}]
components:
  securitySchemes: {bearer: {type: http, scheme: bearer}}
  parameters:
    Limit: {name: limit, in: query, required: true, schema: {type: integer, default: 20}}
paths:
  /pets:
    get:
      operationId: listPets
      parameters:
        - $ref: "#/components/parameters/Limit"
        - {name: status, in: query, schema: {type: string}}
    post:
      operationId: createPet
      requestBody:
        content:
          application/json:
            example: {name: Rex, kind: dog}
  /pets/{petId}:
    parameters:
      - {name: petId, in: path, required: true, schema: {type: integer}, example: 42}
    get:
      operationId: getPetById
      parameters:
        - {name: X-Request-Id, in: header, required: true, schema: {type: string, format: uuid}}
        - {name: Authorization, in: header, required: true, schema: {type: string}}
    delete:
      operationId: deletePet
  /pets/{petId}/photos:
    get:
      parameters:
        - {name: petId, in: path, required: true, schema: {type: integer}}
        - {name: size, in: query, required: true, schema: {type: string, enum: [small, large]}}
  /legacy:
    get:
      operationId: legacy
      parameters:
        - $ref: "other.yaml#/components/parameters/X"
    options:
      operationId: legacyOptions
```
```json
{"swagger": "2.0", "info": {"title": "Pets", "version": "1"},
 "host": "pets.example.com", "basePath": "/api", "schemes": ["https", "http"],
 "paths": {
  "/pets": {
   "get": {"operationId": "findPets", "parameters": [
     {"name": "status", "in": "query", "required": true, "type": "string", "enum": ["available", "sold"]}]},
   "post": {"operationId": "addPet", "parameters": [
     {"name": "body", "in": "body", "required": true, "schema": {"example": {"name": "Rex"}}}]}},
  "/pets/{petId}": {
   "get": {"operationId": "getPet", "parameters": [
     {"name": "petId", "in": "path", "required": true, "type": "integer", "x-example": 7}]}}}}
```

- [ ] **Step 1: Write the failing tests** in `doc.rs`.
  - `load_doc_reads_json_and_yaml`: `load_doc(r#"{"a":1}"#) == json!({"a":1})`; `load_doc("a: 1\nb: [x]\n") == json!({"a":1,"b":["x"]})`; `load_doc("{")` is `Err(Decode { .. })`.
  - `version_detection`: petstore2 → `Swagger2`; petstore3 and `json!({"openapi":"3.1.0"})` → `OpenApi3`; `json!({"info":{}})` → `Err(Shape(_))`.
  - `resolve_follows_local_refs_and_stops`: in a doc where `components.parameters` holds `A: {$ref: "#/components/parameters/B"}`, `B: {name: b}` and `L: {$ref: "#/components/parameters/L"}`, `{$ref: "#/components/parameters/A"}` resolves to `{name: b}`; `L` → `None`; `"x.yaml#/a"` → `None`; `"#/paths/~1pets"` resolves the key `/pets`.
  - `base_url_from_server_variables`: petstore3 → `"https://api.pets.example.com/v1"`.
  - `base_url_from_swagger2_host`: petstore2 → `"https://pets.example.com/api"`.
  - `relative_server_needs_spec_url_or_override`: for `{openapi: "3.0.0", servers: [{url: "/v1"}]}`, `(None, None)` → `Err(NoBaseUrl)`; spec url `http://h:8080/openapi.json` → `"http://h:8080/v1"`; override `"http://x/api/"` → `"http://x/api"`. `{openapi: "3.0.0"}` with spec url `http://h/openapi.json` → `"http://h"`.
  - `params_merge_path_item_and_operation`: petstore3 `GET /pets/{petId}` → `[("X-Request-Id", Header), ("Authorization", Header), ("petId", Path)]`; `petId` values `[42]`, todo `None`; `X-Request-Id` values `["TODO"]`, todo `Some("string")`.
  - `param_value_precedence` (table; 3.x unless marked 2.0; doc `{}` except one row with `{schema: {$ref: "#/components/schemas/Id"}}` and `components.schemas.Id = {type: integer, example: 3}` → `[3]`): `{example: 5, schema: {type: integer, default: 9}}` → `[5]`; `{examples: {a: {value: "x"}, b: {value: "y"}}}` → `["x","y"]`; `{schema: {example: "s"}}` → `["s"]`; `{schema: {examples: ["p","q"]}}` → `["p","q"]`; `{schema: {type: integer, default: 20}}` → `[20]`; `{schema: {enum: ["small","large"]}}` → `["small","large"]`; 2.0 `{type: integer, "x-example": 7}` → `[7]`; 2.0 `{type: string, enum: ["available","sold"]}` → both; `{schema: {type: integer}}` → `[1]` / `Some("integer")`; `number` → `[1.0]` / `Some("number")`; `boolean` → `[true]` / `Some("boolean")`; `{schema: {type: string, format: uuid}}` → `["TODO"]` / `Some("string")`; `{}` → `["TODO"]` / `Some("string")`. All value-bearing rows have todo `None`.
  - `external_ref_skips_operation`: petstore3 `GET /legacy` → `Err(r)`, `r == "uses an external $ref (other.yaml#/components/parameters/X)"`.
  - `body_example_sources`: petstore3 `createPet` → `Some(json!({"name":"Rex","kind":"dog"}))`; a 3.x body with `examples: {one: {value: {"a":1}}}` → `Some(json!({"a":1}))`; one with only `schema.example` → that value; petstore2 `addPet` → `Some(json!({"name":"Rex"}))`; `getPetById` → `None`.

- [ ] **Step 2: Run to verify they fail** — `cargo test -p redliner-import openapi` → FAIL to compile.

- [ ] **Step 3: Implement `doc.rs`** per the interface notes and the spec's precedence list. `doc["paths"]` and other maps are `serde_json::Map`s (sorted keys), which is what makes the output order deterministic.

- [ ] **Step 4: Run to verify they pass** — `cargo test -p redliner-import openapi` → PASS; `make lint` clean.

- [ ] **Step 5: Commit** — `feat(import): read OpenAPI/Swagger documents, base URLs, refs and parameters`.

---

### Task 5: The OpenAPI draft

**Files:**
- Modify: `src/openapi/mod.rs`

**Interfaces:**
- Consumes: everything in `doc.rs` (Task 4); `Draft`, `PoolDraft`, `ScenarioDraft`, `RequestDraft`, `Protocol` (Task 1).
- Produces:
  ```rust
  pub struct OpenapiOpts { pub source: String, pub spec_url: Option<reqwest::Url>, pub base_url: Option<String>, pub include_unsafe: bool }
  pub fn draft(doc: &Value, opts: &OpenapiOpts) -> Result<Draft, ImportError>;
  ```

Rules (the spec's OpenAPI section, made exact): `Draft::new(opts.source, base_url(..)?, Rest)`. If the doc declares `security`, `components.securitySchemes` or `securityDefinitions`, the first note is `this API declares security schemes; set target.auth or pass --bearer-env`. For each path in `paths` order (the path item resolved through `resolve`), methods in the order `get, head, post, put, patch, delete`; after them, `options`/`trace` present add the note `"{path}: OPTIONS skipped"` (`TRACE` likewise). For each operation: `params` `Err(reason)` → note `"{METHOD} {path}: skipped — {reason}"` and no scenario; the name is `scenario_name(operationId or "{METHOD} {path}")`. Path params replace `{name}` in the path with `{{pool}}`; required query params append `?a={{pool}}&b={{pool}}` in order; required headers become scenario headers, except `Authorization` (any case) → note `"{scenario}: Authorization header parameter skipped; set target.auth or pass --bearer-env"`; required cookies → note `"{scenario}: cookie parameter {name} skipped"`; optional query/header/cookie params are ignored. Pools come from `add_pool(&format!("{scenario}_{param}"), PoolDraft { values, todo })`, for used params only. POST, PUT and PATCH get a body: `body_example` serialized with `serde_json::to_string`, else `"{}"` plus the note `"{scenario}: no JSON request body example; body is {} (TODO)"`; GET, HEAD and DELETE get none. POST, PUT, PATCH and DELETE get `commented: Some("{METHOD} changes state")` unless `include_unsafe`. No live scenario left: `NoOperations(format!("all {n} operations change state; pass --include-unsafe to scaffold them"))` when some were commented, else `NoOperations("the spec has no usable operations")`.

- [ ] **Step 1: Write the failing tests** (petstore3 with `include_unsafe: false`, no override, unless stated).
  - `petstore3_scenarios_in_order`: names `["listPets","createPet","getPetById","deletePet","GET /pets/{petId}/photos"]`; `createPet` and `deletePet` have `commented == Some("POST changes state")` and `Some("DELETE changes state")`; the rest `None`.
  - `petstore3_paths_and_pools`: paths `"/pets?limit={{listPets_limit}}"`, `"/pets/{{getPetById_petId}}"`, `"/pets/{{GET_pets_petId_photos_petId}}/photos?size={{GET_pets_petId_photos_size}}"`; `getPetById` headers `[("X-Request-Id","{{getPetById_X_Request_Id}}")]`; pools `listPets_limit = [20]` (todo `None`), `getPetById_petId = [42]`, `getPetById_X_Request_Id = ["TODO"]` (`Some("string")`), `GET_pets_petId_photos_petId = [1]` (`Some("integer")`), `GET_pets_petId_photos_size = ["small","large"]`, `deletePet_petId = [42]`; `createPet` adds no pool.
  - `petstore3_notes`: `notes ==` `["this API declares security schemes; set target.auth or pass --bearer-env", "GET /legacy: skipped — uses an external $ref (other.yaml#/components/parameters/X)", "/legacy: OPTIONS skipped", "getPetById: Authorization header parameter skipped; set target.auth or pass --bearer-env"]`.
  - `bodies_only_for_post_put_patch`: `createPet` body `Some("{\"kind\":\"dog\",\"name\":\"Rex\"}")`; `deletePet` and every GET have `None`.
  - `include_unsafe_makes_them_live`: with `include_unsafe: true`, every scenario has `commented == None`.
  - `duplicate_operation_ids_are_suffixed`: a doc with `GET /a` and `GET /b` both `operationId: x` gives names `["x","x_2"]`.
  - `all_unsafe_without_flag_is_an_error`: a doc with only `POST /a` is `Err(NoOperations(m))` with `m` containing `--include-unsafe`.
  - `swagger2_draft`: target url `"https://pets.example.com/api"`; names `["findPets","addPet","getPet"]`; `findPets` path `"/pets?status={{findPets_status}}"`, pool `["available","sold"]`; `getPet` path `"/pets/{{getPet_petId}}"`, pool `[7]`; `addPet` commented with body `Some("{\"name\":\"Rex\"}")`.
  - `openapi_drafts_meet_the_contract`: `assert_contract(&render(..))` gives 3 scenarios for petstore3, 5 with `include_unsafe`, and 2 for petstore2.

- [ ] **Step 2: Run to verify they fail** — `cargo test -p redliner-import openapi` → FAIL.

- [ ] **Step 3: Implement `draft`** per the rules above.

- [ ] **Step 4: Run to verify they pass** — `cargo test -p redliner-import` → PASS; `make lint` clean.

- [ ] **Step 5: Commit** — `feat(import): draft scenarios from OpenAPI and Swagger documents`.

---

### Task 6: CLI, binary, output and docs

**Files:**
- Create: `src/cli.rs`, `src/main.rs`, `tests/cli.rs`
- Modify: `crates/redliner-import/Cargo.toml` (add `[[bin]] name = "redliner-import", path = "src/main.rs"`), `src/lib.rs`, `README.md`, `CLAUDE.md`, `CHANGELOG.md`, `codebook.toml`

**Interfaces:**
- Consumes: `Fetcher`, `FetchOpts` (Task 3); `graphql::{fetch_schema, draft}` (Task 3); `openapi::{doc::load_doc, draft, OpenapiOpts}` (Tasks 4–5); `render` (Task 1).
- Produces (`cli.rs`):
  ```rust
  pub struct Cli { pub command: Command }                       // #[command(name = "redliner-import", version)]
  pub enum Command { Graphql(GraphqlArgs), Openapi(OpenapiArgs) }
  pub struct CommonArgs {
      pub output: Option<PathBuf>,              // -o, --output
      pub force: bool,                          // --force
      pub header: Vec<(String, String)>,        // -H, --header "Name: value" (repeatable; same parsing as redliner's -H)
      pub bearer_env: Option<String>,           // --bearer-env VAR
      pub insecure: bool,                       // -k, --insecure
  }
  pub struct GraphqlArgs { pub url: String, pub max_depth: u32 /* --max-depth, default 3, range 1..=10 */, pub common: CommonArgs }
  pub struct OpenapiArgs { pub spec: String, pub base_url: Option<String>, pub include_unsafe: bool, pub common: CommonArgs }
  ```
- Produces (`lib.rs`):
  ```rust
  pub struct Summary { pub scenarios: usize, pub commented: usize, pub pools: usize, pub todo_pools: usize, pub output: Option<PathBuf> }
  pub async fn run(cli: Cli, stdout: &mut impl std::io::Write) -> Result<Summary, ImportError>;
  ```
  `run` builds the draft (graphql: always fetch; openapi: a `SPEC` starting `http://` or `https://` is fetched with `get_text` and becomes `spec_url`, anything else is read as a file, `Io` on failure, with source `"OpenAPI document {spec}"`). The `Fetcher` is built **only when fetching**: a local spec with `--bearer-env` must not need the variable at import time. Then it sets `insecure_tls`, `bearer_env` and `header_names` (the `-H` names, in order) on the draft, renders, and writes to `-o` (`OutputExists` if the file exists and not `--force`; `Io` on write errors) or to `stdout`.
- `main.rs`: `#[tokio::main(flavor = "current_thread")]`; on `Ok`, print two lines to stderr, `redliner-import: {scenarios} scenario(s) ({commented} commented out), {pools} pool(s) ({todo_pools} need real values)` with ` -> {path}` appended when written to a file, then `next: review it, replace the TODO pool values, then run: redliner --config {path or <file>} --validate`; on `Err`, print `redliner-import: {e}` to stderr and exit 2.

- [ ] **Step 1: Write the failing tests.**
  - `lib.rs` — `cli_parses_both_subcommands`: `graphql http://h/graphql -H "X-A: 1" -H "X-B: 2" --bearer-env T -k -o out.yaml --force` gives url, `max_depth == 3`, headers `[("X-A","1"),("X-B","2")]`, `bearer_env Some("T")`, insecure, output, force; `openapi spec.yaml --base-url http://x --include-unsafe` gives those fields; `--max-depth 0` and `-H nocolon` fail `try_parse_from`.
  - `run_writes_file_and_refuses_overwrite`: `openapi <petstore3.yaml> --base-url http://h -o <tmp>` is `Ok` with `scenarios == 3, commented == 2`, and the file passes `assert_contract`; a second run is `Err(OutputExists(<tmp>))`; with `--force` it is `Ok`.
  - `run_graphql_end_to_end_never_writes_secrets`: env `REDLINER_IMPORT_T6_TOKEN=t0k3n-secret`; mock serves the introspection fixture; `graphql <mock>/graphql -H "X-Api-Key: k3y-secret" --bearer-env REDLINER_IMPORT_T6_TOKEN -k` into a `Vec<u8>`. The output contains `token_env: "REDLINER_IMPORT_T6_TOKEN"`, `insecure_tls: true` and `X-Api-Key`, contains neither `k3y-secret` nor `t0k3n-secret`, and passes `assert_contract`; the recorded request contains `k3y-secret`.
  - `openapi_from_url_resolves_relative_servers`: the mock serves `{"openapi":"3.0.0","servers":[{"url":"/v1"}],"paths":{"/ping":{"get":{"operationId":"ping"}}}}`; `openapi <mock>/openapi.json` loads with `cfg.target.url == format!("{}/v1", mock.url)`.
  - `openapi_file_with_bearer_env_does_not_need_the_variable`: `REDLINER_IMPORT_T6_UNSET` removed; `openapi <petstore3.yaml> --base-url http://h --bearer-env REDLINER_IMPORT_T6_UNSET` is `Ok` and writes `token_env: "REDLINER_IMPORT_T6_UNSET"`.
  - `tests/cli.rs` (through `env!("CARGO_BIN_EXE_redliner-import")`): `binary_prints_version` — stdout trimmed equals `format!("redliner-import {}", env!("CARGO_PKG_VERSION"))`; `binary_scaffolds_openapi_fixture_to_stdout` — `openapi <manifest dir>/tests/fixtures/petstore3.yaml --base-url http://h:9/api` succeeds, its stdout written to a temp file loads with `redliner::config::load_config_file` (3 scenarios), and stderr contains `next: review it`; `binary_reports_errors_with_exit_2` — `openapi /nonexistent/spec.yaml` exits with code 2 and stderr starts with `redliner-import: `.

- [ ] **Step 2: Run to verify they fail** — `cargo test -p redliner-import` → FAIL to compile.

- [ ] **Step 3: Implement `cli.rs`, `run` and `main.rs`** per the interfaces above.

- [ ] **Step 4: Run to verify they pass** — `cargo test -p redliner-import` → PASS.

- [ ] **Step 5: Document it.**
  - `README.md`: a `## Scaffold a config from a schema` section right after `## Configuration`: install (`cargo install --path crates/redliner-import --locked`), the two commands (`redliner-import graphql https://api.example.com/graphql -o redliner.yaml` and `redliner-import openapi https://api.example.com/openapi.json -o redliner.yaml`), and the follow-up: edit the `# TODO` pools, trim scenarios, then `redliner --validate`. One sentence each on mutations (not scaffolded) and unsafe methods (commented out; `--include-unsafe`).
  - `CLAUDE.md`, Architecture: replace "Two workspace crates with a one-way dependency: `redliner` → `volley`." with three crates, where `redliner` → `volley` is the only runtime dependency and `redliner-import` talks to `redliner` only through the YAML it writes (`redliner` is its dev-dependency, for the contract tests). Add a `redliner-import` bullet (front ends `graphql/` and `openapi/` produce a `Draft`; `draft::render` writes it; never add a runtime dependency on `redliner`). Commands: add `cargo test -p redliner-import`.
  - `CHANGELOG.md`, `## [Unreleased]` → `### Added` (create the subsection if absent): `` `redliner-import`, a separate tool that drafts a config from GraphQL introspection (`redliner-import graphql <url>`) or a Swagger 2.0 / OpenAPI 3.x document (`redliner-import openapi <url|file>`); install with `cargo install --path crates/redliner-import`. ``
  - `codebook.toml`: add `introspection`, `openapi` and `swagger` if missing.

- [ ] **Step 6: Verify the whole branch** — `make lint`, `make test`, and `cargo package --workspace --locked` all pass; `cargo +1.88 check --workspace --all-targets --locked` passes (if the 1.88 toolchain isn't installed locally, CI's msrv job covers it — say so in the PR).

- [ ] **Step 7: Commit** — `feat(import): redliner-import CLI with GraphQL and OpenAPI subcommands`, then `docs: document redliner-import`.

---

## Out of scope (from the spec)

Release tarballs, `scripts/install.sh` and the Docker image (a follow-up adds `-p redliner-import` to `release.yml`'s build and packaging); GraphQL mutations, subscriptions and fragments; request bodies synthesized from JSON Schema; external `$ref`s; other sources (gRPC reflection, Postman, HAR).
