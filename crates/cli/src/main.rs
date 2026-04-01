const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
Usage: claude-run <command> [options]

Commands:
  retry      Run Claude with automatic rate-limit retry
  verify     Generic verify-fix loop (worker + check)
  review     Adversarial spec-compliance scorer
  pipeline   Execute a multi-stage YAML pipeline

Options:
  --help, -h       Show this help
  --version, -v    Show version

Run 'claude-run <command> --help' for details on each command.

Examples:
  claude-run retry \"implement the login feature\"
  claude-run retry --verify \"make test\" \"implement the feature\"
  claude-run verify --worker 'claude-run retry ...' --check 'make test'
  claude-run review --spec spec.md --threshold 95
  claude-run pipeline pipeline.yaml";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        eprintln!("{HELP}");
        std::process::exit(1);
    }

    let command = &args[0];
    let rest: Vec<String> = args[1..].to_vec();

    let code = match command.as_str() {
        "--help" | "-h" | "help" => {
            println!("{HELP}");
            0
        }
        "--version" | "-v" | "version" => {
            println!("claude-run {VERSION}");
            0
        }
        "retry" => claude_run_retry::run(rest).await,
        "verify" => claude_run_verify::run(rest).await,
        "review" => claude_run_review::run(rest).await,
        "pipeline" => claude_run_pipeline::run(rest).await,
        _ => {
            eprintln!("Unknown command: {command}");
            eprintln!("{HELP}");
            1
        }
    };

    std::process::exit(code);
}
