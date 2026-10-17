//! Error type for the volley engine.

/// Errors produced by the engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("http error: {0}")]
    Http(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("build error: {0}")]
    Build(String),
}
