use clap::Parser;
use red_liner::{run, Args, Exit};

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let code: Exit = run(args).await;
    std::process::exit(code.code());
}
