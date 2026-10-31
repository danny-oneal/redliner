//! Redliner's config importer: drafts a scenario config from a GraphQL schema
//! (live introspection) or an OpenAPI/Swagger document. The YAML it writes is
//! the only interface to `redliner`; nothing here links it at runtime.

pub mod draft;
pub mod error;

pub use error::ImportError;

#[cfg(test)]
mod testutil;
