pub mod cli;
pub mod config;
pub mod notify;
pub mod output;
pub mod rate_limit;
pub mod runner;
pub mod slugify;
pub mod verify;

use cli::Cli;
use config::Config;
use runner::{ClaudeRunner, TokioCommandRunner};
use verify::VerifyOutcome;

/// Top-level entry point. Returns the process exit code.
pub async fn run(cli: Cli) -> i32 {
    if let Err(e) = cli.validate() {
        eprintln!("{e}");
        return 1;
    }

    let config = Config::from_env();

    // Resolve session name
    let session_name = if cli.is_resume() {
        cli.resume_target().unwrap_or("").to_string()
    } else if let Some(name) = &cli.name {
        name.clone()
    } else if let Some(prompt) = &cli.prompt {
        slugify::slugify(prompt)
    } else {
        String::new()
    };

    output::banner(&session_name, cli.verify.as_deref());

    let runner = ClaudeRunner {
        config: config.clone(),
        session_name: session_name.clone(),
        extra_args: cli.extra.clone(),
        cmd: TokioCommandRunner,
    };

    // First run
    let (prompt, is_resume) = if cli.is_resume() {
        ("continue where you left off".to_string(), true)
    } else {
        (cli.prompt.clone().unwrap(), false)
    };

    if let Err(e) = runner.run_with_retry(&prompt, is_resume).await {
        notify::notify(
            &format!("Failed (exit {}): {session_name}", e.exit_code()),
            config.notify,
        );
        return e.exit_code();
    }

    // Verify loop
    if let Some(verify_cmd) = &cli.verify {
        match verify::run_verify_loop(&runner, verify_cmd).await {
            VerifyOutcome::Passed { .. } => {}
            VerifyOutcome::ExhaustedRounds => return 1,
            VerifyOutcome::ClaudeFailed { exit_code, .. } => {
                notify::notify(&format!("Fix failed: {session_name}"), config.notify);
                return exit_code;
            }
        }
    }

    output::done(&session_name);
    notify::notify(&format!("Task complete: {session_name}"), config.notify);
    0
}
