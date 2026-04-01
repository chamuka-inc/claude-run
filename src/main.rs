use claude_run::cli::{parse_from_env, ParseResult};

#[tokio::main]
async fn main() {
    let cli = match parse_from_env() {
        ParseResult::Ok(cli) => *cli,
        ParseResult::Exit { message, code } => {
            if code == 0 {
                println!("{message}");
            } else {
                eprintln!("{message}");
            }
            std::process::exit(code);
        }
    };
    let code = claude_run::run(cli).await;
    std::process::exit(code);
}
