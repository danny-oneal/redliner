use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = redliner_import::cli::Cli::parse();
    match redliner_import::run(cli, &mut std::io::stdout().lock()).await {
        Ok(s) => {
            let dest = s.output.as_ref().map(|p| p.display().to_string());
            eprintln!(
                "redliner-import: {} scenario(s) ({} commented out), {} pool(s) ({} need real values){}",
                s.scenarios,
                s.commented,
                s.pools,
                s.todo_pools,
                dest.as_ref().map(|p| format!(" -> {p}")).unwrap_or_default()
            );
            eprintln!(
                "next: review it, replace the TODO pool values, then run: redliner --config {} --validate",
                dest.as_deref().unwrap_or("<file>")
            );
        }
        Err(e) => {
            eprintln!("redliner-import: {e}");
            std::process::exit(2);
        }
    }
}
