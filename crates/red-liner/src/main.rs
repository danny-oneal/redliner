// Not yet wired into the CLI; remove the allow once main uses config.
#[allow(dead_code)]
mod config;

use clap::Parser;

/// Red Liner: SLO-driven HTTP load testing.
#[derive(Parser, Debug)]
#[command(name = "red-liner", version)]
struct Cli {}

#[tokio::main]
async fn main() {
    let _cli = Cli::parse();
}
