// Not yet wired into the CLI (a later task consumes the model).
#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod encode;
#[allow(dead_code)]
mod mix;

use clap::Parser;

/// Red Liner: SLO-driven HTTP load testing.
#[derive(Parser, Debug)]
#[command(name = "red-liner", version)]
struct Cli {}

#[tokio::main]
async fn main() {
    let _cli = Cli::parse();
}
