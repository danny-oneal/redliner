//! Red Liner: SLO-driven HTTP load testing (library half; `main.rs` is a shim).

pub mod checks;
pub mod cli;
pub mod config;
pub mod data;
pub mod encode;
pub mod mix;
pub mod phases;
pub mod run;
pub mod slo;

pub use cli::Args;
pub use run::{run, Exit};
