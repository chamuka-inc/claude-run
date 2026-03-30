use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = claude_run::cli::Cli::parse();
    let code = claude_run::run(cli).await;
    std::process::exit(code);
}
