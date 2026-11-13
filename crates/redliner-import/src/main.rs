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
            // reqwest's message names only the URL; the cause is in its sources.
            let mut msg = e.to_string();
            let mut source = std::error::Error::source(&e);
            while let Some(s) = source {
                let text = s.to_string();
                if !msg.contains(&text) {
                    msg.push_str(": ");
                    msg.push_str(&text);
                }
                source = s.source();
            }
            eprintln!("redliner-import: {msg}");
            std::process::exit(2);
        }
    }
}
