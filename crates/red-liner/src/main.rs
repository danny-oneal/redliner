use clap::Parser;

/// Red Liner: SLO-driven HTTP load testing.
#[derive(Parser, Debug)]
#[command(name = "red-liner", version)]
struct Cli {}

#[tokio::main]
async fn main() {
    let _cli = Cli::parse();
}
