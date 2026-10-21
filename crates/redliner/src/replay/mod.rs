//! Production-log replay: read a request log, match it to scenarios, and
//! fire it at its recorded pace.

pub mod matcher;
pub mod parse;
pub mod schedule;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("replay: read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("replay: invalid HAR: {0}")]
    Har(String),
    #[error("replay: invalid window: {0}")]
    Window(String),
    #[error("replay: line {line} ({event}) matches both scenario {first:?} and {second:?}; make the match rules disjoint")]
    Ambiguous {
        line: usize,
        event: String,
        first: String,
        second: String,
    },
}
