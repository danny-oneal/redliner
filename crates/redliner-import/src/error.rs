//! Import errors.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{url}: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{url}: http status {code}")]
    Status { url: String, code: u16 },
    #[error("{what}: {message}")]
    Decode { what: String, message: String },
    #[error(
        "graphql introspection failed: {0} (introspection is often disabled in production; \
         try a staging endpoint)"
    )]
    Introspection(String),
    #[error("unexpected document shape: {0}")]
    Shape(String),
    #[error("environment variable {0} is not set (named by --bearer-env)")]
    MissingEnv(String),
    #[error("invalid header {0:?}")]
    Header(String),
    #[error("cannot determine the API base URL from the spec; pass --base-url")]
    NoBaseUrl,
    #[error("nothing to scaffold: {0}")]
    NoOperations(String),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} exists; pass --force to overwrite", .0.display())]
    OutputExists(PathBuf),
}
