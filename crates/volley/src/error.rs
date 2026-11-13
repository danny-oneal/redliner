//! Error type for the volley engine.

/// Errors produced by the engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("build error: {0}")]
    Build(String),
}
