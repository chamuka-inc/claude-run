pub mod cli;
pub mod config;
pub mod notify;
pub mod output;
pub mod pipeline;
pub mod rate_limit;
pub mod runner;
pub mod slugify;
pub mod verify;

use cli::Cli;
use config::Config;
use pipeline::{Pipeline, PipelineOutcome};
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

    // ── Pipeline mode ──────────────────────────────────────────
    if cli.pipeline {
        return run_pipeline(cli, config, session_name).await;
    }

    // ── Standard mode ──────────────────────────────────────────
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

/// Run the autonomous multi-instance pipeline.
async fn run_pipeline(cli: Cli, config: Config, session_name: String) -> i32 {
    let spec_path = cli
        .spec
        .clone()
        .unwrap_or_else(|| format!(".claude-run/{}/spec.md", session_name));

    output::pipeline_banner(&session_name, &spec_path, cli.verify.as_deref());

    let pipeline = Pipeline {
        config: config.clone(),
        prompt: cli.prompt.clone().unwrap(),
        spec_path,
        verify_cmd: cli.verify.clone(),
        base_session: session_name.clone(),
        extra_args: cli.extra.clone(),
        cmd: TokioCommandRunner,
    };

    match pipeline.run().await {
        PipelineOutcome::Success => {
            output::pipeline_done(&session_name);
            notify::notify(
                &format!("Pipeline complete: {session_name}"),
                config.notify,
            );
            0
        }
        PipelineOutcome::PhaseFailed { phase, exit_code } => {
            eprintln!("Pipeline failed at phase: {phase}");
            notify::notify(
                &format!("Pipeline failed at {phase}: {session_name}"),
                config.notify,
            );
            exit_code
        }
        PipelineOutcome::VerifyExhausted => {
            eprintln!("Pipeline failed: verification exhausted all rounds");
            notify::notify(
                &format!("Pipeline verify exhausted: {session_name}"),
                config.notify,
            );
            1
        }
        PipelineOutcome::ReviewRejected { round } => {
            eprintln!("Pipeline failed: review rejected after {round} rounds");
            notify::notify(
                &format!("Pipeline review rejected: {session_name}"),
                config.notify,
            );
            1
        }
    }
}
