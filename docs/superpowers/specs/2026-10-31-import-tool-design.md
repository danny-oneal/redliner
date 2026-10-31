# Config Importer (`redliner-import`) — Design

**Date:** 2026-10-31
**Status:** Agreed in conversation with the maintainer; implementation plan in
`docs/superpowers/plans/2026-10-31-import-tool.md`.
**Topic:** Scaffold a Redliner scenario config from a GraphQL schema (live
introspection) or an OpenAPI/Swagger document.

## Intent

Hand-writing a scenario file for an API with dozens of operations is tedious.
`redliner-import` writes a *starting point*: a draft config the user reviews,
trims, weights, and fills in, then runs with `redliner`. It never fires load
itself. Realism stays a human decision: where the importer cannot know a good
value it says so (`# TODO`) instead of guessing quietly.

The maintainer chose a separate tool over a `redliner` subcommand so importers
can multiply (GraphQL, OpenAPI, later gRPC reflection, Postman, HAR) without
growing the load tester. The YAML config is the contract between them.

## Goals

- `redliner-import graphql <url>` drafts a config from live introspection.
- `redliner-import openapi <url|file>` drafts one from Swagger 2.0, OpenAPI 3.0
  or 3.1.
- Every draft loads under `redliner --validate` unmodified, and every
  `{{placeholder}}` in it has a pool.
- `redliner` never depends on the importer; the importer never links `redliner`
  at runtime.

## Non-goals (v1)

Firing traffic; GraphQL mutations, subscriptions and fragments; synthesizing
meaningful values or request bodies from JSON Schema (examples only); external
`$ref`s; shipping the importer in release tarballs, `scripts/install.sh` or the
Docker image (v1 installs with `cargo install --path crates/redliner-import`);
other sources (gRPC reflection, Postman, HAR).

## Decisions

1. **A separate crate and binary**, `crates/redliner-import` (package and binary
   `redliner-import`, lib `redliner_import`), in this workspace with
   `version.workspace = true` so it releases in lockstep.
2. **No runtime dependency on `redliner` or `redliner-volley`.** `redliner` is a
   path-only dev-dependency (stripped by `cargo package`), used by tests to prove
   the contract: each draft loads with `redliner::config::load_config_file` and
   every template renders through `redliner::data`.
3. **No new third-party crates.** Runtime deps are a subset of what `redliner`
   already uses: clap, reqwest (rustls), serde_json, serde_yaml_ng, thiserror,
   tokio. OpenAPI is read by walking `serde_json::Value` (YAML deserializes into
   it), not a typed crate: `openapiv3` covers only 3.0, and FastAPI emits 3.1.
4. **Templated text output**, not a serde round-trip, so the draft can carry
   guidance comments. Every scalar string is written as a JSON string literal,
   which is a valid YAML double-quoted scalar, so URLs, `#`, `:` and quotes
   survive exactly.
5. **Placeholders never leave a pool empty** (list pools need values). Pools are
   named `<scenario>_<param>`, sanitized to `[A-Za-z0-9_]`. Values come from the
   spec's examples when it has them; otherwise a typed placeholder plus a
   `# TODO: real values (<type>)` comment.
6. **Safe by default.** GraphQL: Query root only. OpenAPI: GET and HEAD are live;
   POST, PUT, PATCH and DELETE are written commented out unless
   `--include-unsafe`; OPTIONS and TRACE are skipped.
7. **Secrets are never written.** `-H` values and the bearer token are used only
   to fetch; the file gets header *names* in a comment and `auth.token_env`.
8. **Deterministic output**: identical input gives a byte-identical file; no
   timestamps.
9. **A gentle default load**: `constant` at 10 rps for 30s, `weight: 1` on every
   scenario: a safe first run against an unfamiliar API.

## CLI

```
redliner-import graphql <URL>  [--max-depth N]                     [common]
redliner-import openapi <SPEC> [--base-url URL] [--include-unsafe] [common]

common:  -o, --output FILE   --force   -H, --header "Name: value" (repeatable)
         --bearer-env VAR    -k, --insecure
```

The draft goes to stdout unless `-o` is given; `-o` refuses to overwrite an
existing file without `--force`. A one-line summary and the next step
(`redliner --config FILE --validate`) go to stderr. Exit 0 on success, 2 on any
error. `-k` also writes `target.insecure_tls: true`; `--bearer-env VAR` also
writes `target.auth: {type: bearer, token_env: VAR}`.

## GraphQL generation

- POST a minimal introspection query. A response with `errors` and no
  `data.__schema` is an error that quotes the server: production servers often
  disable introspection.
- One scenario per non-deprecated Query-root field, named after the field. Its
  query is a **named operation with the same name** (`query product($id: Int!)
  { ... }`), because the encoder sends `operationName` = scenario name.
- Required arguments (outer `NON_NULL`) become variables bound to
  `{{<field>_<arg>}}`; optional arguments are omitted.
- Selection set: scalar and enum fields; object, interface and union fields
  descend while depth allows (`--max-depth`, default 3, counting the root
  field's return type as level 1); never re-enter a type already on the path;
  skip fields that need arguments; interfaces add `__typename`; unions select
  `__typename`; an empty selection becomes `__typename`.
- Placeholder values: Int `1`, Float `1.0`, Boolean `true`, String, ID and
  custom scalars `"TODO"`, an enum's first value, input objects `{}`, lists
  `[<inner>]`.
- Mutation and subscription roots are counted in the notes, not scaffolded.

## OpenAPI generation

- Read JSON, else YAML. `swagger: "2.0"` or `openapi: "3.x"`, else an error.
- Base URL: `--base-url` wins; 3.x `servers[0].url` with its variables replaced
  by their defaults, a relative one resolved against the spec's URL; 2.0
  `schemes[0]://host` + `basePath`. With no way to make it absolute: an error
  asking for `--base-url`.
- One scenario per path × method, ordered by path, then
  get, head, post, put, patch, delete. Named by `operationId`, else
  `"METHOD /path"`; duplicate names get `_2`, `_3`.
- Parameters: path-item parameters merged with the operation's (the operation
  wins on `(name, in)`); local `#/...` `$ref`s resolved (16 hops at most); an
  operation using an external `$ref` is skipped with a note. Path `{p}` becomes
  `{{pool}}`; a required query parameter is appended as `name={{pool}}`; a
  required header becomes a scenario header, except `Authorization` (noted:
  use `target.auth`); cookies are noted.
- Values, first found wins: `example`; every `examples.*.value`;
  `schema.example`; every `schema.examples[]` (3.1); `schema.default`; every
  `schema.enum`; for 2.0, `x-example`, `default`, `enum` on the parameter;
  otherwise a typed placeholder (integer `1`, number `1.0`, boolean `true`,
  else `"TODO"`) with a TODO comment.
- Request body (POST, PUT and PATCH; GET, HEAD and DELETE send none): the
  `application/json` content's `example`,
  else its first `examples.*.value`, else `schema.example`; for 2.0 the body
  parameter's `schema.example`; otherwise `{}` and a note.
- A spec declaring security gets a note suggesting `target.auth` /
  `--bearer-env`.
- No live operation left (all unsafe and no `--include-unsafe`) is an error that
  names the flag.

## Testing

- Contract tests: render, write to a temp file, `load_config_file`,
  `build_pools`, then render every scenario's variables, path, headers and body
  with a seeded `ChaCha8Rng`. All must succeed.
- Fixture-driven unit tests: an introspection response, a disabled-introspection
  response, an OpenAPI 3.0 YAML and a Swagger 2.0 JSON document.
- In-process TCP mocks (the pattern in `crates/redliner/tests/e2e.rs`) for live
  fetches, auth and headers.
- Binary tests through `CARGO_BIN_EXE_redliner-import`.

## Known limitations

- Placeholder values make a draft runnable, not meaningful.
- Pools are per scenario; sharing one (the same `petId` across operations) is a
  manual merge.
- OpenAPI paths come out in sorted order, not document order: `serde_json` maps
  sort their keys.
- Not in the release artifacts yet; install with `cargo install`.
