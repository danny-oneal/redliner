//! Command-line arguments.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Scaffold a Redliner config from a GraphQL schema or an OpenAPI/Swagger
/// document. The draft is printed (or written with -o) for you to review;
/// nothing here fires load.
#[derive(Debug, Parser)]
#[command(name = "redliner-import", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Introspect a live GraphQL endpoint: one scenario per Query field.
    Graphql(GraphqlArgs),
    /// Read an OpenAPI 3.x or Swagger 2.0 document (a file or an http(s) URL):
    /// one scenario per operation.
    Openapi(OpenapiArgs),
}

#[derive(Debug, Args)]
pub struct GraphqlArgs {
    /// The GraphQL endpoint, e.g. https://api.example.com/graphql.
    pub url: String,
    /// How deep to expand nested object fields in each selection set.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=10))]
    pub max_depth: u32,
    #[command(flatten)]
    pub common: CommonArgs,
}

#[derive(Debug, Args)]
pub struct OpenapiArgs {
    /// Path or http(s) URL of the OpenAPI/Swagger document (JSON or YAML).
    pub spec: String,
    /// Target base URL; overrides the document's servers / host + basePath.
    #[arg(long)]
    pub base_url: Option<String>,
    /// Emit POST/PUT/PATCH/DELETE operations live instead of commented out.
    #[arg(long)]
    pub include_unsafe: bool,
    #[command(flatten)]
    pub common: CommonArgs,
}

#[derive(Debug, Args)]
pub struct CommonArgs {
    /// Write the draft here instead of stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Overwrite --output if it exists.
    #[arg(long)]
    pub force: bool,
    /// Extra header for fetching the schema, "Name: value" (repeatable).
    /// Only the name is written to the draft, never the value.
    #[arg(short = 'H', long = "header", value_parser = parse_header)]
    pub header: Vec<(String, String)>,
    /// Env var holding a bearer token for fetching the schema; the draft
    /// gets `auth.token_env` naming it, never the token.
    #[arg(long)]
    pub bearer_env: Option<String>,
    /// Accept invalid TLS certificates (also sets `insecure_tls` in the draft).
    #[arg(short = 'k', long)]
    pub insecure: bool,
}

fn parse_header(s: &str) -> Result<(String, String), String> {
    match s.split_once(':') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.trim().to_string())),
        _ => Err(format!("expected \"Name: value\", got {s:?}")),
    }
}
