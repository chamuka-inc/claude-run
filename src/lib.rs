pub mod cli;
pub mod config;
pub mod notify;
pub mod output;
pub mod pipeline;
pub mod prompts;
pub mod rate_limit;
pub mod runner;
pub mod slugify;
pub mod stage;
pub mod verdict;
pub mod verifier;
pub mod yaml_pipeline;

use cli::Cli;
use config::Config;
use pipeline::{Pipeline, PipelineOutcome, PipelineRunner, PipelineStep};
use runner::TokioCommandRunner;
use stage::Stage;
use verifier::{VerdictParser, Verifier};

/// Top-level entry point. Returns the process exit code.
pub async fn run(cli: Cli) -> i32 {
    if let Err(e) = cli.validate() {
        eprintln!("{e}");
        return 1;
    }

    let config = Config::from_env();

    // Load pipeline from YAML or build from CLI flags
    let (pipeline, session_name) = if let Some(ref path) = cli.pipeline {
        match yaml_pipeline::load_pipeline(std::path::Path::new(path)) {
            Ok(p) => {
                let name = cli.name.clone().unwrap_or_else(|| {
                    std::path::Path::new(path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("pipeline")
                        .to_string()
                });
                (p, name)
            }
            Err(e) => {
                eprintln!("Error loading pipeline from {path}:\n{e}");
                return 1;
            }
        }
    } else {
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
        let pipeline = build_pipeline(&cli, &config);
        (pipeline, session_name)
    };

    output::banner(
        &session_name,
        cli.verify.as_deref(),
        cli.av_banner(),
        cli.pipeline.as_deref(),
    );

    let runner = PipelineRunner {
        config: config.clone(),
        base_session: session_name.clone(),
        extra_args: cli.extra.clone(),
        cmd: TokioCommandRunner,
    };

    let outcome = runner.run(&pipeline).await;

    match &outcome {
        PipelineOutcome::Success => {
            output::done(&session_name);
            notify::notify(&format!("Task complete: {session_name}"), config.notify);
        }
        PipelineOutcome::VerifyExhausted => {
            notify::notify(
                &format!("Verification exhausted: {session_name}"),
                config.notify,
            );
        }
        PipelineOutcome::StageFailed { .. } => {
            notify::notify(
                &format!("Failed (exit {}): {session_name}", outcome.exit_code()),
                config.notify,
            );
        }
    }

    outcome.exit_code()
}

/// Build a Pipeline from CLI flags.
fn build_pipeline(cli: &Cli, config: &Config) -> Pipeline {
    let prompt = if cli.is_resume() {
        "continue where you left off".to_string()
    } else {
        cli.prompt.clone().unwrap()
    };

    let worker = Stage::claude_worker(&prompt);

    // Determine the verifier (if any)
    let verifier = build_verifier(cli);

    let steps = match verifier {
        Some(v) => {
            let max_rounds = if cli.av {
                cli.av_rounds.unwrap_or(config.av_rounds)
            } else {
                config.verify_max
            };
            vec![PipelineStep::VerifyLoop {
                worker,
                verifier: v,
                max_rounds,
            }]
        }
        None => vec![PipelineStep::Run(worker)],
    };

    Pipeline { steps }
}

/// Build a Verifier from CLI flags.
fn build_verifier(cli: &Cli) -> Option<Verifier> {
    let shell_verifier = cli.verify.as_ref().map(|cmd| Verifier::Shell {
        command: cmd.clone(),
    });

    let av_verifier = if cli.av {
        let original_prompt = cli.prompt.as_deref().unwrap_or("");
        let review_prompt = prompts::build_review_prompt(original_prompt, cli.av_spec.as_deref());

        let threshold = cli.av_threshold.unwrap_or(95);

        let reviewer_stage = Stage::claude_reviewer(review_prompt, "-av", cli.av_model.clone());

        Some(Verifier::Claude {
            stage: reviewer_stage,
            verdict_parser: VerdictParser::ScoreThreshold { threshold },
        })
    } else {
        None
    };

    match (shell_verifier, av_verifier) {
        (Some(shell), Some(av)) => Some(Verifier::Chain(vec![shell, av])),
        (Some(v), None) | (None, Some(v)) => Some(v),
        (None, None) => None,
    }
}
